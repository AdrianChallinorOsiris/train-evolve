//! Shared async agent turn: one user prompt → assistant text + token usage.

use yoagent::agent::Agent;
use yoagent::*;

/// Error info extracted from an API failure (bad key, rate limit, network, etc.).
#[derive(Debug, Clone)]
pub struct ApiError {
    pub message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ApiError {}

/// Maps provider overload signals (e.g. HTTP 529) to a short operator-facing message.
pub fn format_api_error_for_user(provider_message: &str) -> String {
    let lower = provider_message.to_ascii_lowercase();
    if provider_message.contains("529") || lower.contains("overloaded") {
        "Anthropic overloaded".to_string()
    } else {
        provider_message.to_string()
    }
}

/// Result of a single agent turn.
pub struct PromptResult {
    pub usage: Usage,
    pub text: String,
    /// If the LLM ended with `StopReason::Error`, this holds the error message.
    pub api_error: Option<ApiError>,
}

/// Run a single prompt and collect assistant text (no REPL formatting).
///
/// Returns a [`PromptResult`] that includes any API-level error the provider
/// reported (bad key, rate limit, overloaded, etc.).  Callers should inspect
/// `api_error` and decide how to proceed.
pub async fn run_prompt(agent: &mut Agent, input: &str) -> PromptResult {
    let mut rx = agent.prompt(input).await;
    let mut last_usage = Usage::default();
    let mut text = String::new();
    let mut api_error: Option<ApiError> = None;
    let mut got_any_event = false;

    while let Some(event) = rx.recv().await {
        got_any_event = true;
        match event {
            AgentEvent::MessageUpdate {
                delta: StreamDelta::Text { delta },
                ..
            } => {
                text.push_str(&delta);
            }
            AgentEvent::AgentEnd { messages } => {
                for msg in messages.iter().rev() {
                    if let AgentMessage::Llm(Message::Assistant {
                        usage,
                        stop_reason,
                        error_message,
                        ..
                    }) = msg
                    {
                        last_usage = usage.clone();
                        if *stop_reason == StopReason::Error {
                            let err_text = error_message
                                .clone()
                                .unwrap_or_else(|| "unknown API error".into());
                            api_error = Some(ApiError { message: err_text });
                        }
                        break;
                    }
                }
            }
            _ => {}
        }
    }

    // If we received zero events the channel closed immediately — likely a
    // network error or the provider rejected the request before streaming.
    if !got_any_event && api_error.is_none() {
        api_error = Some(ApiError {
            message: "no response from API (network error or invalid API key?)".into(),
        });
    }

    PromptResult {
        usage: last_usage,
        text,
        api_error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_error_display() {
        let e = ApiError {
            message: "rate limited".into(),
        };
        assert_eq!(e.to_string(), "rate limited");
    }

    #[test]
    fn format_api_error_529_is_overloaded() {
        assert_eq!(
            format_api_error_for_user("status 529 from upstream"),
            "Anthropic overloaded"
        );
    }

    #[test]
    fn format_api_error_overloaded_keyword() {
        assert_eq!(
            format_api_error_for_user("Service temporarily overloaded"),
            "Anthropic overloaded"
        );
    }

    #[test]
    fn format_api_error_other_passthrough() {
        assert_eq!(
            format_api_error_for_user("invalid x-api-key"),
            "invalid x-api-key"
        );
    }
}
