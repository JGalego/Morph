use std::sync::LazyLock;

use morph_core::error::GatewayError;
use morph_core::message::ContentBlock;
use morph_core::request::{CanonicalRequest, CanonicalResponse};
use morph_core::traits::Transformer;
use regex::Regex;

struct Pattern {
    regex: &'static LazyLock<Regex>,
    replacement: &'static str,
}

static OPENAI_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"sk-[A-Za-z0-9]{20,}").expect("static regex"));
static AWS_ACCESS_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"AKIA[0-9A-Z]{16}").expect("static regex"));
static BEARER_TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Bearer [A-Za-z0-9\-_.]{10,}").expect("static regex"));
static EMAIL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\w.+-]+@[\w-]+\.[\w.-]+").expect("static regex"));

fn patterns() -> &'static [Pattern] {
    static PATTERNS: LazyLock<[Pattern; 4]> = LazyLock::new(|| {
        [
            Pattern {
                regex: &OPENAI_KEY,
                replacement: "[REDACTED_API_KEY]",
            },
            Pattern {
                regex: &AWS_ACCESS_KEY,
                replacement: "[REDACTED_AWS_KEY]",
            },
            Pattern {
                regex: &BEARER_TOKEN,
                replacement: "Bearer [REDACTED_TOKEN]",
            },
            Pattern {
                regex: &EMAIL,
                replacement: "[REDACTED_EMAIL]",
            },
        ]
    });
    &*PATTERNS
}

fn redact_text(text: &str) -> String {
    let mut out = text.to_string();
    for pattern in patterns() {
        out = pattern
            .regex
            .replace_all(&out, pattern.replacement)
            .into_owned();
    }
    out
}

fn redact_block(block: ContentBlock) -> ContentBlock {
    match block {
        ContentBlock::Text(t) => ContentBlock::text(redact_text(&t.text)),
        other => other,
    }
}

/// Scrubs common secret-shaped substrings (API keys, AWS access keys,
/// bearer tokens, email addresses) from request/response text content
/// before it's forwarded upstream or handed back to the client. A
/// best-effort safety net, not a substitute for not putting secrets in
/// prompts in the first place — pattern matching can't catch everything.
pub struct RedactionTransformer;

impl Transformer for RedactionTransformer {
    fn name(&self) -> &str {
        "redaction"
    }

    fn transform_request(
        &self,
        mut req: CanonicalRequest,
    ) -> Result<CanonicalRequest, GatewayError> {
        for message in &mut req.messages {
            message.content = std::mem::take(&mut message.content)
                .into_iter()
                .map(redact_block)
                .collect();
        }
        if let Some(system) = &req.system {
            req.system = Some(redact_text(system));
        }
        Ok(req)
    }

    fn transform_response(
        &self,
        mut resp: CanonicalResponse,
    ) -> Result<CanonicalResponse, GatewayError> {
        resp.content = std::mem::take(&mut resp.content)
            .into_iter()
            .map(redact_block)
            .collect();
        Ok(resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use morph_core::message::Message;
    use morph_core::request::RequestMetadata;
    use std::time::SystemTime;

    fn req_with_text(text: &str) -> CanonicalRequest {
        CanonicalRequest {
            model: "gpt-4o".to_string(),
            messages: vec![Message::user(text)],
            system: None,
            tools: vec![],
            tool_choice: None,
            temperature: None,
            top_p: None,
            max_tokens: None,
            stream: false,
            stop: vec![],
            response_format: None,
            reasoning: None,
            metadata: RequestMetadata {
                request_id: "req-1".to_string(),
                ingress_protocol: "openai_chat".to_string(),
                received_at: SystemTime::now(),
            },
            extra: serde_json::Value::Null,
        }
    }

    #[test]
    fn redacts_openai_style_api_key() {
        let redactor = RedactionTransformer;
        let req = req_with_text("my key is sk-abcdefghijklmnopqrstuvwxyz123456");
        let out = redactor.transform_request(req).unwrap();
        let text = out.messages[0].text_content();
        assert!(text.contains("[REDACTED_API_KEY]"));
        assert!(!text.contains("sk-abcdefghijklmnopqrstuvwxyz123456"));
    }

    #[test]
    fn redacts_email_addresses() {
        let redactor = RedactionTransformer;
        let req = req_with_text("contact me at jane.doe@example.com please");
        let out = redactor.transform_request(req).unwrap();
        assert!(out.messages[0].text_content().contains("[REDACTED_EMAIL]"));
    }

    #[test]
    fn leaves_ordinary_text_untouched() {
        let redactor = RedactionTransformer;
        let req = req_with_text("what's the weather like today?");
        let out = redactor.transform_request(req).unwrap();
        assert_eq!(
            out.messages[0].text_content(),
            "what's the weather like today?"
        );
    }
}
