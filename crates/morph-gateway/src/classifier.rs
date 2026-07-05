use morph_core::content::ContentKind;
use morph_core::traits::Classifier;

/// Combines the native `DefaultClassifier` with any WASM classifier
/// plugins: every classifier is asked, and every `(kind, confidence)` pair
/// any of them returns is pooled together before `morph_detect::analyze`
/// picks the top-scoring one. This is what lets a plugin either add
/// coverage for a content kind Morph doesn't know natively, or simply offer
/// a second opinion on one it does.
pub struct CompositeClassifier {
    classifiers: Vec<std::sync::Arc<dyn Classifier>>,
}

impl CompositeClassifier {
    pub fn new(classifiers: Vec<std::sync::Arc<dyn Classifier>>) -> Self {
        CompositeClassifier { classifiers }
    }
}

impl Classifier for CompositeClassifier {
    fn name(&self) -> &str {
        "composite"
    }

    fn classify(&self, text: &str) -> Vec<(ContentKind, f32)> {
        let mut all: Vec<(ContentKind, f32)> = self
            .classifiers
            .iter()
            .flat_map(|c| c.classify(text))
            .collect();
        all.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct Fixed(ContentKind, f32);
    impl Classifier for Fixed {
        fn name(&self) -> &str {
            "fixed"
        }
        fn classify(&self, _text: &str) -> Vec<(ContentKind, f32)> {
            vec![(self.0, self.1)]
        }
    }

    #[test]
    fn pools_and_sorts_by_confidence_descending() {
        let composite = CompositeClassifier::new(vec![
            Arc::new(Fixed(ContentKind::Markdown, 0.4)),
            Arc::new(Fixed(ContentKind::Json, 0.9)),
        ]);
        let result = composite.classify("irrelevant");
        assert_eq!(result[0], (ContentKind::Json, 0.9));
        assert_eq!(result[1], (ContentKind::Markdown, 0.4));
    }
}
