//! Detectors for log-shaped content: ANSI-colored terminal output, stack
//! traces (Python/Java/Rust/Go flavors), and interactive shell sessions.
//!
//! These are grouped together because they share the same downstream
//! planning treatment (`Representation` rule e in `morph-detect`'s planner)
//! and because visually they're all "console output", but the actual
//! detection signatures are quite different per kind, hence separate
//! functions rather than one shared heuristic.

use std::sync::LazyLock;

use regex::Regex;

static ANSI_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\x1b\[[0-9;]*[A-Za-z]").expect("valid regex"));

/// Result of a log-family heuristic scan. All three detectors in this module
/// share this shape; kind-specific extra metrics aren't needed downstream
/// beyond confidence (`has_ansi_codes`/`line_count` in `ContentMetrics`
/// already cover what the planner needs).
#[derive(Debug, Clone, Copy, Default)]
pub struct LogDetection {
    pub confidence: f32,
}

/// Detects ANSI-colored terminal output via `ESC[...m`-style escape
/// sequences (SGR codes, cursor movement, etc.). Presence of even one such
/// sequence is strong, almost unambiguous evidence — plain text and every
/// other content kind this crate detects never legitimately contains them.
pub fn detect_terminal_log(text: &str) -> LogDetection {
    let matches = ANSI_RE.find_iter(text).count();
    if matches == 0 {
        return LogDetection { confidence: 0.0 };
    }
    let line_count = text.lines().count().max(1) as f32;
    let density = (matches as f32 / line_count).min(2.0);
    let confidence = (0.5 + density * 0.3).min(0.97);
    LogDetection { confidence }
}

static PY_TRACEBACK_HEADER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^Traceback \(most recent call last\):").expect("valid regex")
});
static PY_FILE_LINE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?m)^\s*File "[^"]+", line \d+"#).expect("valid regex"));
static JAVA_AT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*at [\w$.<>]+\([^)]*\)").expect("valid regex"));
static RUST_PANIC_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"panicked at").expect("valid regex"));
static GO_GOROUTINE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^goroutine \d+ \[[^\]]+\]:").expect("valid regex"));

/// Detects stack traces across the four ecosystems the spec calls out:
/// Python (`Traceback (most recent call last):` + `File "...", line N`),
/// Java (`\tat pkg.Class.method(...)`), Rust (`panicked at`), and Go
/// (`goroutine N [running]:`). Any single strong marker is enough; repeated
/// frame lines push confidence higher.
pub fn detect_stack_trace(text: &str) -> LogDetection {
    let mut hits = 0u32;
    if PY_TRACEBACK_HEADER_RE.is_match(text) {
        hits += 3;
    }
    hits += PY_FILE_LINE_RE.find_iter(text).count() as u32;
    hits += JAVA_AT_RE.find_iter(text).count() as u32;
    if RUST_PANIC_RE.is_match(text) {
        hits += 3;
    }
    if GO_GOROUTINE_RE.is_match(text) {
        hits += 3;
    }

    if hits == 0 {
        return LogDetection { confidence: 0.0 };
    }
    let confidence = (0.45 + hits as f32 * 0.12).min(0.97);
    LogDetection { confidence }
}

static SHELL_PROMPT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^[ \t]*[$#>][ \t]+\S").expect("valid regex"));

/// Detects an interactive shell session transcript: `$`/`#`/`>` prompt lines
/// interleaved with their command output. A file that is *only* prompt
/// lines back-to-back (e.g. a plain list of commands, no output) is scored
/// lower, since that's a script rather than a captured session.
pub fn detect_shell_session(text: &str) -> LogDetection {
    let line_count = text.lines().count().max(1);
    let prompt_lines = SHELL_PROMPT_RE.find_iter(text).count();
    if prompt_lines == 0 {
        return LogDetection { confidence: 0.0 };
    }

    let non_prompt_lines = line_count.saturating_sub(prompt_lines);
    let mixed_bonus = if non_prompt_lines > 0 { 0.25 } else { 0.0 };
    let density = prompt_lines as f32 / line_count as f32;
    let confidence = (0.3 + density * 0.4 + mixed_bonus).min(0.95);
    LogDetection { confidence }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- ANSI / terminal log ---

    #[test]
    fn ansi_colored_output_detected() {
        let text =
            "\u{1b}[32mOK\u{1b}[0m running task\n\u{1b}[31mERROR\u{1b}[0m something failed\n";
        let d = detect_terminal_log(text);
        assert!(d.confidence > 0.35, "confidence was {}", d.confidence);
    }

    #[test]
    fn ansi_cursor_movement_detected() {
        let text = "\u{1b}[2K\u{1b}[1Gbuilding... 42%\n\u{1b}[2K\u{1b}[1Gbuilding... 87%\n";
        let d = detect_terminal_log(text);
        assert!(d.confidence > 0.35, "confidence was {}", d.confidence);
    }

    #[test]
    fn single_ansi_sequence_detected() {
        let text = "\u{1b}[1mBold header\u{1b}[0m\nplain output line\nanother plain line\n";
        let d = detect_terminal_log(text);
        assert!(d.confidence > 0.35, "confidence was {}", d.confidence);
    }

    #[test]
    fn plain_text_has_no_ansi() {
        let text = "Build succeeded with no warnings and no errors reported by the toolchain.";
        let d = detect_terminal_log(text);
        assert_eq!(d.confidence, 0.0);
    }

    // --- stack traces ---

    #[test]
    fn python_traceback_detected() {
        let text = "Traceback (most recent call last):\n  File \"app.py\", line 10, in <module>\n    main()\n  File \"app.py\", line 6, in main\n    raise ValueError(\"boom\")\nValueError: boom\n";
        let d = detect_stack_trace(text);
        assert!(d.confidence > 0.35, "confidence was {}", d.confidence);
    }

    #[test]
    fn java_stack_trace_detected() {
        let text = "Exception in thread \"main\" java.lang.NullPointerException\n\tat com.example.App.run(App.java:42)\n\tat com.example.App.main(App.java:10)\n";
        let d = detect_stack_trace(text);
        assert!(d.confidence > 0.35, "confidence was {}", d.confidence);
    }

    #[test]
    fn rust_panic_detected() {
        let text = "thread 'main' panicked at src/main.rs:5:9:\ncalled `Option::unwrap()` on a `None` value\nnote: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n";
        let d = detect_stack_trace(text);
        assert!(d.confidence > 0.35, "confidence was {}", d.confidence);
    }

    #[test]
    fn go_panic_detected() {
        let text = "panic: runtime error: index out of range\n\ngoroutine 1 [running]:\nmain.main()\n\t/app/main.go:12 +0x1d\n";
        let d = detect_stack_trace(text);
        assert!(d.confidence > 0.35, "confidence was {}", d.confidence);
    }

    #[test]
    fn plain_text_has_no_trace() {
        let text = "Everything ran fine and the program exited normally with status zero.";
        let d = detect_stack_trace(text);
        assert_eq!(d.confidence, 0.0);
    }

    // --- shell sessions ---

    #[test]
    fn shell_session_with_mixed_output_detected() {
        let text = "$ ls -la\ntotal 12\ndrwxr-xr-x  3 user user 4096 Jan  1 00:00 .\n$ pwd\n/home/user/project\n";
        let d = detect_shell_session(text);
        assert!(d.confidence > 0.35, "confidence was {}", d.confidence);
    }

    #[test]
    fn root_prompt_session_detected() {
        let text = "# apt-get update\nReading package lists... Done\n# apt-get install -y curl\nSetting up curl ...\n";
        let d = detect_shell_session(text);
        assert!(d.confidence > 0.35, "confidence was {}", d.confidence);
    }

    #[test]
    fn windows_style_prompt_session_detected() {
        let text = "> dir\n Volume in drive C has no label.\n> echo done\ndone\n";
        let d = detect_shell_session(text);
        assert!(d.confidence > 0.35, "confidence was {}", d.confidence);
    }

    #[test]
    fn plain_prose_has_no_shell_prompts() {
        let text = "Every morning I check my email before starting on the day's tasks.";
        let d = detect_shell_session(text);
        assert_eq!(d.confidence, 0.0);
    }
}
