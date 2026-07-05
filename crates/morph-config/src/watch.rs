use std::path::{Path, PathBuf};

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::watch;
use tracing::{info, warn};

use crate::error::Result;
use crate::load;
use crate::schema::Config;

/// Watches `morph.toml` for changes and republishes a parsed `Config` on a
/// `tokio::sync::watch` channel, so every part of the gateway that holds a
/// `watch::Receiver<Config>` picks up new settings without a restart.
///
/// A config edit that fails to parse is logged and ignored — the previous
/// good config keeps serving traffic rather than the gateway crashing or
/// silently running with a partially-applied config.
pub struct ConfigWatcher {
    _watcher: RecommendedWatcher,
    receiver: watch::Receiver<Config>,
}

impl ConfigWatcher {
    pub fn spawn(path: impl AsRef<Path>) -> Result<Self> {
        let path: PathBuf = path.as_ref().to_path_buf();
        let initial = load::load(&path)?;
        let (tx, rx) = watch::channel(initial);

        let watch_path = path.clone();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
            let event = match res {
                Ok(event) => event,
                Err(err) => {
                    warn!(error = %err, "config watcher error");
                    return;
                }
            };
            if !event.paths.iter().any(|p| p == &watch_path) {
                return;
            }
            match load::load(&watch_path) {
                Ok(new_config) => {
                    info!(path = %watch_path.display(), "config reloaded");
                    // `watch::Sender::send` is synchronous; safe to call from
                    // notify's callback thread directly.
                    let _ = tx.send(new_config);
                }
                Err(err) => {
                    warn!(error = %err, "config reload failed, keeping previous config");
                }
            }
        })?;

        // Watch the parent directory rather than the file itself: editors
        // frequently replace files via rename-on-save, which some platforms
        // report as the watched inode disappearing rather than a `Modify`
        // event on it.
        let watch_dir = path.parent().unwrap_or_else(|| Path::new("."));
        watcher.watch(watch_dir, RecursiveMode::NonRecursive)?;

        Ok(ConfigWatcher {
            _watcher: watcher,
            receiver: rx,
        })
    }

    pub fn receiver(&self) -> watch::Receiver<Config> {
        self.receiver.clone()
    }

    pub fn current(&self) -> Config {
        self.receiver.borrow().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn picks_up_edits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("morph.toml");
        std::fs::write(&path, "listen = \"0.0.0.0:8080\"\n").unwrap();

        let watcher = ConfigWatcher::spawn(&path).unwrap();
        let mut rx = watcher.receiver();
        assert_eq!(rx.borrow().listen, "0.0.0.0:8080");

        std::fs::write(&path, "listen = \"0.0.0.0:9999\"\n").unwrap();

        let changed = tokio::time::timeout(Duration::from_secs(5), rx.changed()).await;
        assert!(changed.is_ok(), "expected a config change notification");
        assert_eq!(rx.borrow().listen, "0.0.0.0:9999");
    }
}
