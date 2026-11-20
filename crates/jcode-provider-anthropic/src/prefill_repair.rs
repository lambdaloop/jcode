//! Recovery from Anthropic rejecting a request as an assistant prefill.

use super::*;

/// Compact role and block-kind summary of the last few messages, for logging
/// why Anthropic rejected a request. Contains no message text.
pub fn describe_tail(messages: &[ApiMessage], count: usize) -> String {
    let start = messages.len().saturating_sub(count);
    messages[start..]
        .iter()
        .map(|message| {
            let kinds: Vec<&str> = message
                .content
                .iter()
                .map(|block| match block {
                    ApiContentBlock::Text { .. } => "text",
                    ApiContentBlock::ToolUse { .. } => "tool_use",
                    ApiContentBlock::ToolResult { .. } => "tool_result",
                    ApiContentBlock::Thinking { .. } => "thinking",
                    ApiContentBlock::Image { .. } => "image",
                    ApiContentBlock::Raw(value) => value
                        .get("type")
                        .and_then(|kind| kind.as_str())
                        .unwrap_or("raw"),
                    #[allow(unreachable_patterns)]
                    _ => "other",
                })
                .collect();
            format!("{}[{}]", message.role, kinds.join(","))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Recover once from Anthropic rejecting the final turn as a prefill. A user
/// turn containing only tool results can still trigger this upstream error.
/// Keep those results first and add an explicit continuation to the same turn.
pub fn repair_rejected_prefill(messages: &mut Vec<ApiMessage>, error: &str) -> bool {
    if !error.contains("does not support assistant message prefill") {
        return false;
    }
    let continuation = ApiContentBlock::Text {
        text: CONTINUATION_USER_TURN.to_string(),
        cache_control: None,
    };
    match messages.last_mut() {
        Some(tail) if tail.role == "assistant" => {
            // An interrupted native call can look like a paused turn, even
            // when the server has no actual continuation for it.
            if is_paused_server_tool_turn(tail) {
                tail.content.pop();
            }
            if tail.content.is_empty() {
                messages.pop();
            }
            if let Some(tail) = messages.last_mut().filter(|tail| tail.role == "user") {
                tail.content.push(continuation);
            } else {
                messages.push(ApiMessage {
                    role: "user".into(),
                    content: vec![continuation],
                });
            }
            true
        }
        Some(tail)
            if tail.role == "user"
                && !tail.content.is_empty()
                && tail.content.iter().all(|block| matches!(block, ApiContentBlock::ToolResult { .. })) =>
        {
            tail.content.push(continuation);
            true
        }
        _ => false,
    }
}

