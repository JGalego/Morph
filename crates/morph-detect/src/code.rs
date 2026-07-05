//! Source code detection and best-effort language guessing.
//!
//! Two independent signals feed the confidence score: *structural* cues that
//! are language-agnostic (indentation runs, brace/semicolon density,
//! assignment-shaped lines) and *lexical* cues (keywords/punctuation drawn
//! from the same per-language marker tables used for language guessing).
//! Combining both means a snippet in a language we don't have explicit
//! markers for still registers as "code" from structure alone, while a
//! snippet that's all keywords but no structure (e.g. prose that mentions
//! "class" and "function") doesn't dominate the score.

use std::sync::LazyLock;

use regex::Regex;

static ASSIGNMENT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^\s*[A-Za-z_$][A-Za-z0-9_.]*\s*(?::=|[-+*/]?=)\s*\S").expect("valid regex")
});

/// Generic keywords/punctuation that show up across most C-like and
/// scripting languages. Used only to decide "this looks like code at all",
/// never to pick a specific language.
const GENERIC_CODE_MARKERS: &[&str] = &[
    "function",
    "return ",
    "class ",
    "import ",
    "def ",
    "public ",
    "private ",
    "static ",
    "const ",
    "let ",
    "var ",
    "void ",
    "null",
    "true",
    "false",
    "package ",
    "namespace ",
    "interface ",
    "struct ",
    "#include",
];

/// One language's distinctive markers, split into `strong` (near-unambiguous)
/// and `weak` (common, but shared with other languages) evidence.
struct LangRule {
    name: &'static str,
    strong: &'static [&'static str],
    weak: &'static [&'static str],
}

static LANG_RULES: &[LangRule] = &[
    LangRule {
        name: "rust",
        strong: &[
            "fn main(",
            "println!(",
            "let mut ",
            "impl ",
            "::<",
            "pub fn ",
        ],
        weak: &["use std::", "pub struct", "#[derive", "match ", " -> "],
    },
    LangRule {
        name: "python",
        strong: &["def ", "elif ", "__init__", "self."],
        weak: &[
            "import ", "print(", "lambda ", "None", "True", "False", ":\n",
        ],
    },
    LangRule {
        name: "typescript",
        strong: &[
            "interface ",
            "implements ",
            "as const",
            ": string",
            ": number",
            ": boolean",
            "export type",
        ],
        weak: &["function ", "=>", "const ", "public ", "private "],
    },
    LangRule {
        name: "javascript",
        strong: &["console.log", "require(", "module.exports", "=>"],
        weak: &["function ", "const ", "let ", "===", "!=="],
    },
    LangRule {
        name: "go",
        strong: &["package main", "func main(", "fmt.Println", ":="],
        weak: &["import (", "defer ", "chan ", "range "],
    },
    LangRule {
        name: "java",
        strong: &[
            "public class ",
            "public static void main",
            "System.out.println",
        ],
        weak: &["import java.", "extends ", "implements ", "private "],
    },
    LangRule {
        name: "csharp",
        strong: &["using System;", "Console.WriteLine", "static void Main"],
        weak: &["namespace ", "public class", "public "],
    },
    LangRule {
        name: "cpp",
        strong: &[
            "#include <iostream>",
            "std::",
            "cout <<",
            "using namespace std",
        ],
        weak: &["template<", "class ", "public:", "private:"],
    },
    LangRule {
        name: "c",
        strong: &["#include <stdio.h>", "printf(", "malloc("],
        weak: &["#include <", "int main(", "NULL", "struct "],
    },
    LangRule {
        name: "ruby",
        strong: &["def ", "puts ", "elsif", "end\n"],
        weak: &["require '", "do |", ".each", "@"],
    },
    LangRule {
        name: "php",
        strong: &["<?php", "$_", "->"],
        weak: &["echo ", "namespace ", "function "],
    },
    LangRule {
        name: "shell",
        strong: &["#!/bin/bash", "#!/usr/bin/env"],
        weak: &["fi\n", "then\n", "$(", "export ", "if [", "$1"],
    },
    LangRule {
        name: "sql",
        strong: &[
            "SELECT ",
            "INSERT INTO",
            "CREATE TABLE",
            "UPDATE ",
            "DELETE FROM",
        ],
        weak: &["FROM ", "WHERE ", "JOIN ", "GROUP BY", "ORDER BY"],
    },
    LangRule {
        name: "html",
        strong: &["<!DOCTYPE html", "<html", "<head>", "<body"],
        weak: &["<div", "<span", "<p>", "<a href"],
    },
    LangRule {
        name: "css",
        strong: &["@media", "margin:", "padding:", "color:", "font-size:"],
        weak: &["px;", "em;", "{\n", "#id", ".class"],
    },
];

/// Result of scanning one text segment for source-code signals.
#[derive(Debug, Clone, Default)]
pub struct CodeDetection {
    pub confidence: f32,
    /// Best-effort language guess (e.g. "rust", "python"), `None` if no
    /// language table matched strongly enough to guess.
    pub language: Option<String>,
}

/// Scores `text` for how strongly it resembles source code, and attaches a
/// best-effort language guess.
pub fn detect(text: &str) -> CodeDetection {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return CodeDetection {
            confidence: 0.0,
            language: None,
        };
    }

    let lines: Vec<&str> = text.lines().collect();
    let line_count = lines.len().max(1) as f32;

    let indented = lines
        .iter()
        .filter(|l| l.starts_with("    ") || l.starts_with('\t'))
        .count() as f32;
    let brace_lines = lines
        .iter()
        .filter(|l| {
            let t = l.trim();
            t == "{" || t == "}" || t.ends_with('{') || t.ends_with('}') || t.ends_with("};")
        })
        .count() as f32;
    let semicolon_lines = lines.iter().filter(|l| l.trim_end().ends_with(';')).count() as f32;
    let assignment_lines = ASSIGNMENT_RE.find_iter(text).count() as f32;

    let structure_signal = ((indented / line_count) * 0.4)
        + ((brace_lines / line_count).min(1.0) * 0.5)
        + ((semicolon_lines / line_count).min(1.0) * 0.5)
        + ((assignment_lines / line_count).min(1.0) * 0.3);

    let generic_hits = GENERIC_CODE_MARKERS
        .iter()
        .filter(|m| text.contains(*m))
        .count() as f32;
    let lang_hits = LANG_RULES
        .iter()
        .flat_map(|r| r.strong.iter().chain(r.weak.iter()))
        .filter(|m| text.contains(**m))
        .count() as f32;
    let keyword_signal = ((generic_hits + lang_hits) / 6.0).min(1.0);

    let confidence = (structure_signal * 0.5 + keyword_signal * 0.5).min(1.0);

    CodeDetection {
        confidence,
        language: guess_language(text),
    }
}

/// Best-effort language guess via weighted marker matching. Returns `None`
/// if nothing scored any evidence at all.
pub fn guess_language(text: &str) -> Option<String> {
    let mut best: Option<(&'static str, i32)> = None;
    for rule in LANG_RULES {
        let mut score = 0i32;
        for m in rule.strong {
            if text.contains(m) {
                score += 3;
            }
        }
        for m in rule.weak {
            if text.contains(m) {
                score += 1;
            }
        }
        if score > 0 {
            let better = match best {
                Some((_, b)) => score > b,
                None => true,
            };
            if better {
                best = Some((rule.name, score));
            }
        }
    }
    best.map(|(name, _)| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_rust_function() {
        let text = "pub fn add(a: i32, b: i32) -> i32 {\n    let mut sum = a;\n    sum += b;\n    println!(\"{}\", sum);\n    sum\n}\n";
        let d = detect(text);
        assert!(d.confidence > 0.35, "confidence was {}", d.confidence);
        assert_eq!(d.language, Some("rust".to_string()));
    }

    #[test]
    fn detects_python_function() {
        let text = "class Greeter:\n    def __init__(self, name):\n        self.name = name\n\n    def greet(self):\n        print(f\"Hello, {self.name}\")\n";
        let d = detect(text);
        assert!(d.confidence > 0.35, "confidence was {}", d.confidence);
        assert_eq!(d.language, Some("python".to_string()));
    }

    #[test]
    fn detects_javascript_function() {
        let text =
            "function add(a, b) {\n  const sum = a + b;\n  console.log(sum);\n  return sum;\n}\n";
        let d = detect(text);
        assert!(d.confidence > 0.35, "confidence was {}", d.confidence);
        assert_eq!(d.language, Some("javascript".to_string()));
    }

    #[test]
    fn detects_go_function() {
        let text =
            "package main\n\nimport \"fmt\"\n\nfunc main() {\n\tx := 42\n\tfmt.Println(x)\n}\n";
        let d = detect(text);
        assert!(d.confidence > 0.35, "confidence was {}", d.confidence);
        assert_eq!(d.language, Some("go".to_string()));
    }

    #[test]
    fn plain_prose_scores_low_and_has_no_language() {
        let text = "The quick brown fox jumps over the lazy dog while everyone watches quietly.";
        let d = detect(text);
        assert!(d.confidence < 0.35, "confidence was {}", d.confidence);
        assert_eq!(d.language, None);
    }
}
