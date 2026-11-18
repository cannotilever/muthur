//! LLM chat core types.
pub mod sse;

#[derive(Clone, Debug, PartialEq)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<serde_json::Value>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub role: Role,
    pub content: MessageContent,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChatRequest {
    pub system: String,
    pub messages: Vec<Message>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Stop {
    End,
    MaxTokens,
    ToolUse,
    Refusal,
    Other(String),
}

impl Stop {
    pub fn log_str(&self) -> &str {
        match self {
            Stop::End => "end_turn",
            Stop::MaxTokens => "max_tokens",
            Stop::ToolUse => "tool_use",
            Stop::Refusal => "refusal",
            Stop::Other(s) => s,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LlmError {
    NoCarrier,
    Auth,
    Overloaded,
    Malformed,
    SignalLost,
    Transmission,
    Restricted,
    NoAccessCode,
    NotConfigured,
}

impl LlmError {
    pub fn reason(&self) -> &'static str {
        match self {
            LlmError::NoCarrier => "NO CARRIER",
            LlmError::Auth => "AUTHORIZATION REJECTED",
            LlmError::Overloaded => "SYSTEM OVERLOADED",
            LlmError::Malformed => "MALFORMED INQUIRY",
            LlmError::SignalLost => "SIGNAL LOST",
            LlmError::Transmission => "TRANSMISSION ERROR",
            LlmError::Restricted => "DIRECTIVE RESTRICTED",
            LlmError::NoAccessCode => "NO ACCESS CODE",
            LlmError::NotConfigured => "NOT CONFIGURED",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum StreamEvent {
    Text(String),
    ToolCall { id: String, name: String, input: serde_json::Value },
    /// `raw`: Anthropic content blocks as they'd appear non-streamed; None for openai.
    Done { stop: Stop, model: Option<String>, raw: Option<Vec<serde_json::Value>> },
    Error(LlmError),
}

pub trait StreamParser: Send {
    fn feed(&mut self, rec: &sse::SseRecord) -> Vec<StreamEvent>;
    /// Body ended. Emits Error(Transmission) if no Done was produced.
    fn finish(&mut self) -> Vec<StreamEvent>;
    /// One-line token usage for muthur.log; None if the protocol reports none.
    fn usage_summary(&self) -> Option<String> {
        None
    }
}

/// Install the ring crypto provider; safe to call repeatedly.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reasons_are_uppercase_ascii() {
        for e in [LlmError::NoCarrier, LlmError::Auth, LlmError::Overloaded, LlmError::Malformed, LlmError::SignalLost, LlmError::Transmission, LlmError::Restricted, LlmError::NoAccessCode, LlmError::NotConfigured] {
            let r = e.reason();
            assert!(!r.is_empty() && r.chars().all(|c| c.is_ascii_uppercase() || c == ' '), "{r}");
        }
    }
    #[test] fn crypto_provider_install_is_idempotent() { install_crypto_provider(); install_crypto_provider(); }
}
