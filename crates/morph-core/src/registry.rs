use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// A read-heavy, name-keyed registry of trait objects (providers, protocol
/// adapters, renderers, ...). Backed by `RwLock` rather than a lock-free map:
/// registries are populated at startup/hot-reload and read on every request,
/// so writes are rare and reads never block each other.
pub struct Registry<T: ?Sized> {
    entries: RwLock<HashMap<String, Arc<T>>>,
}

impl<T: ?Sized> Default for Registry<T> {
    fn default() -> Self {
        Registry {
            entries: RwLock::new(HashMap::new()),
        }
    }
}

impl<T: ?Sized> Registry<T> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, name: impl Into<String>, entry: Arc<T>) {
        self.entries
            .write()
            .expect("registry lock poisoned")
            .insert(name.into(), entry);
    }

    pub fn get(&self, name: &str) -> Option<Arc<T>> {
        self.entries
            .read()
            .expect("registry lock poisoned")
            .get(name)
            .cloned()
    }

    pub fn names(&self) -> Vec<String> {
        self.entries
            .read()
            .expect("registry lock poisoned")
            .keys()
            .cloned()
            .collect()
    }

    pub fn len(&self) -> usize {
        self.entries.read().expect("registry lock poisoned").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    trait Greeter: Send + Sync {
        fn greet(&self) -> String;
    }

    struct Hello;
    impl Greeter for Hello {
        fn greet(&self) -> String {
            "hello".into()
        }
    }

    #[test]
    fn register_and_get() {
        let reg: Registry<dyn Greeter> = Registry::new();
        reg.register("hello", Arc::new(Hello));
        assert_eq!(reg.get("hello").unwrap().greet(), "hello");
        assert!(reg.get("missing").is_none());
        assert_eq!(reg.names(), vec!["hello".to_string()]);
    }
}
