//! Shared async agent turn: one user prompt → assistant text + token usage.

use yoagent::agent::Agent;
use yoagent::*;

/// Run a single prompt and collect assistant text (no REPL formatting).
pub async fn run_prompt(agent: &mut Agent, input: &str) -> (Usage, String) {
    let mut rx = agent.prompt(input).await;
    let mut last_usage = Usage::default();
    let mut text = String::new();

    while let Some(event) = rx.recv().await {
        match event {
            AgentEvent::MessageUpdate {
                delta: StreamDelta::Text { delta },
                ..
            } => {
                text.push_str(&delta);
            }
            AgentEvent::AgentEnd { messages } => {
                for msg in messages.iter().rev() {
                    if let AgentMessage::Llm(Message::Assistant { usage, .. }) = msg {
                        last_usage = usage.clone();
                        break;
                    }
                }
            }
            _ => {}
        }
    }

    (last_usage, text)
}
