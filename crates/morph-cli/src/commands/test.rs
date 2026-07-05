//! `morph test`: a minimal, self-contained smoke test of the local
//! detect → plan → render pipeline. No config file, no network call — this
//! checks that Morph's own code works on this machine (fonts load,
//! rendering produces valid output), not that a provider is reachable
//! (that's what `morph doctor` is for).

use morph_core::content::ContentKind;
use morph_core::representation::{RasterFormat, RenderOptions, Theme};
use morph_core::traits::Classifier;

struct Case {
    name: &'static str,
    text: &'static str,
    expect_kind: ContentKind,
}

const CASES: &[Case] = &[
    Case {
        name: "markdown",
        text: "# Title\n\n- one\n- two\n\nSome prose here.",
        expect_kind: ContentKind::Markdown,
    },
    Case {
        name: "json",
        text: "{\"a\": 1, \"b\": [1, 2, 3]}",
        expect_kind: ContentKind::Json,
    },
    Case {
        name: "code",
        text: "fn main() {\n    let x = 1;\n    println!(\"{}\", x);\n}\n",
        expect_kind: ContentKind::Code,
    },
];

pub fn execute() -> anyhow::Result<()> {
    let classifier = morph_detect::DefaultClassifier::new();
    let renderers = morph_render::default_renderers();
    let mut failures = 0;

    for case in CASES {
        let candidates = classifier.classify(case.text);
        let (kind, confidence) = candidates
            .first()
            .copied()
            .unwrap_or((ContentKind::PlainText, 0.0));
        if kind != case.expect_kind {
            println!(
                "  [fail] {}: expected {:?}, classifier said {:?} ({:.2})",
                case.name, case.expect_kind, kind, confidence
            );
            failures += 1;
            continue;
        }

        let Some(renderer) = renderers.iter().find(|r| r.supports(kind)) else {
            println!(
                "  [fail] {}: no renderer registered for {:?}",
                case.name, kind
            );
            failures += 1;
            continue;
        };
        let content = morph_core::content::DetectedContent {
            kind,
            ..morph_core::content::DetectedContent::plain(case.text)
        };
        let opts = RenderOptions {
            theme: Theme::Dark,
            max_width_px: 800,
            scale: 1.0,
            format: RasterFormat::Png,
        };
        match renderer.render(&content, &opts) {
            Ok(asset) if !asset.bytes.is_empty() && asset.width > 0 && asset.height > 0 => {
                println!(
                    "  [ok]   {} -> {:?}, rendered {} bytes ({}x{})",
                    case.name,
                    kind,
                    asset.bytes.len(),
                    asset.width,
                    asset.height
                );
            }
            Ok(_) => {
                println!(
                    "  [fail] {}: renderer produced an empty/zero-sized asset",
                    case.name
                );
                failures += 1;
            }
            Err(e) => {
                println!("  [fail] {}: render error: {e}", case.name);
                failures += 1;
            }
        }
    }

    println!();
    if failures == 0 {
        println!("All {} local pipeline checks passed.", CASES.len());
        Ok(())
    } else {
        anyhow::bail!("{failures}/{} local pipeline checks failed", CASES.len());
    }
}
