use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;
use morph_core::error::GatewayError;
use morph_core::request::{CanonicalRequest, CanonicalResponse};
use morph_core::traits::Middleware;
use serde::Serialize;

#[derive(Serialize)]
struct RequestRecord<'a> {
    event: &'static str,
    request_id: &'a str,
    protocol: &'a str,
    model: &'a str,
    message_count: usize,
    stream: bool,
}

#[derive(Serialize)]
struct ResponseRecord<'a> {
    event: &'static str,
    request_id: &'a str,
    stop_reason: &'a morph_core::request::StopReason,
    latency_ms: u128,
    input_tokens: u32,
    output_tokens: u32,
}

/// Appends one JSON line per request/response event to a file, for
/// benchmarking and traffic-shape analysis. Deliberately records shape and
/// timing only — never prompt/response *content* — regardless of any
/// logging configuration, since recorded files are meant to be safe to
/// share/retain for longer than live logs.
pub struct RequestRecordingMiddleware {
    path: PathBuf,
    file: Mutex<std::fs::File>,
}

impl RequestRecordingMiddleware {
    pub fn new(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(RequestRecordingMiddleware {
            path,
            file: Mutex::new(file),
        })
    }

    fn write_line(&self, line: &str) {
        let mut file = self.file.lock().expect("recording file lock poisoned");
        if let Err(e) = writeln!(file, "{line}") {
            tracing::warn!(path = %self.path.display(), error = %e, "failed to write request recording");
        }
    }
}

#[async_trait]
impl Middleware for RequestRecordingMiddleware {
    fn name(&self) -> &str {
        "request_recording"
    }

    async fn on_request(&self, req: &CanonicalRequest) -> Result<(), GatewayError> {
        let record = RequestRecord {
            event: "request",
            request_id: &req.metadata.request_id,
            protocol: &req.metadata.ingress_protocol,
            model: &req.model,
            message_count: req.messages.len(),
            stream: req.stream,
        };
        if let Ok(line) = serde_json::to_string(&record) {
            self.write_line(&line);
        }
        Ok(())
    }

    async fn on_response(
        &self,
        req: &CanonicalRequest,
        resp: &CanonicalResponse,
    ) -> Result<(), GatewayError> {
        let latency_ms = req
            .metadata
            .received_at
            .elapsed()
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let record = ResponseRecord {
            event: "response",
            request_id: &req.metadata.request_id,
            stop_reason: &resp.stop_reason,
            latency_ms,
            input_tokens: resp.usage.input_tokens,
            output_tokens: resp.usage.output_tokens,
        };
        if let Ok(line) = serde_json::to_string(&record) {
            self.write_line(&line);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use morph_core::message::Role;
    use morph_core::request::{RequestMetadata, Usage};
    use std::time::SystemTime;

    fn sample_request() -> CanonicalRequest {
        CanonicalRequest {
            model: "gpt-4o".to_string(),
            messages: vec![morph_core::message::Message::user("hi")],
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

    #[tokio::test]
    async fn writes_one_line_per_event_without_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("record.jsonl");
        let mw = RequestRecordingMiddleware::new(&path).unwrap();

        let req = sample_request();
        mw.on_request(&req).await.unwrap();
        mw.on_response(
            &req,
            &CanonicalResponse {
                id: "resp-1".to_string(),
                model: "gpt-4o".to_string(),
                content: vec![morph_core::message::ContentBlock::text("secret answer")],
                stop_reason: morph_core::request::StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 5,
                    output_tokens: 7,
                },
            },
        )
        .await
        .unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(!contents.contains("secret answer"));
        assert!(!contents.contains("\"hi\""));
        assert!(lines[0].contains("\"event\":\"request\""));
        assert!(lines[1].contains("\"event\":\"response\""));
        let _ = Role::User;
    }
}
