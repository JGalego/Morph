use serde::{Deserialize, Serialize};

/// What a target model/provider can actually do. The representation planner
/// and pipeline consult this before ever choosing an image-bearing
/// representation or forwarding tool calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capabilities {
    pub streaming: bool,
    pub tools: bool,
    pub vision: bool,
    pub structured_output: bool,
    pub reasoning: bool,
    pub max_context_tokens: Option<u32>,
}

impl Default for Capabilities {
    fn default() -> Self {
        Capabilities {
            streaming: true,
            tools: false,
            vision: false,
            structured_output: false,
            reasoning: false,
            max_context_tokens: None,
        }
    }
}
