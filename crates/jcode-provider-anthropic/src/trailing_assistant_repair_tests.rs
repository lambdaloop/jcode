//! Regression coverage for #600.
//!
//! Anthropic rejects a request whose final message is an assistant turn on
//! models without prefill support (notably `claude-fable-5`). The reload
//! auto-resume path starts a turn with empty user content and delivers its
//! continuation as a *system reminder*, so the transcript still ends on the
//! interrupted assistant turn. `format_messages` must repair that shape.

use super::*;
use jcode_message_types::{ContentBlock, Message, Role};

fn text_msg(role: Role, text: &str) -> Message {
    Message {
        role,
        content: vec![ContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        }]
        .into(),
        timestamp: None,
        tool_duration_ms: None,
    }
}

fn roles(messages: &[ApiMessage]) -> Vec<&str> {
    messages.iter().map(|m| m.role.as_str()).collect()
}

const PREFILL_ERROR: &str = "anthropic api error (400 bad request): this model does not support assistant message prefill. the conversation must end with a user message.";

#[test]
fn rejected_tool_result_tail_retries_with_explicit_user_text() {
    let mut messages = vec![ApiMessage {
        role: "user".into(),
        content: vec![ApiContentBlock::ToolResult {
            tool_use_id: "toolu_bash".into(),
            content: ToolResultContent::Text("output".into()),
            is_error: false,
        }],
    }];
    let original = serde_json::to_value(&messages).unwrap();
    assert!(!repair_rejected_prefill(&mut messages, "unrelated 400"));
    assert_eq!(serde_json::to_value(&messages).unwrap(), original);

    assert!(repair_rejected_prefill(&mut messages, PREFILL_ERROR));
    let retry = serde_json::to_value(&messages).unwrap();
    assert_eq!(roles(&messages), ["user"]);
    assert_eq!(retry[0]["content"][0], original[0]["content"][0]);
    assert_eq!(retry[0]["content"][1]["text"], CONTINUATION_USER_TURN);
    assert!(!repair_rejected_prefill(&mut messages, PREFILL_ERROR));
}

#[test]
fn rejected_native_pause_drops_unanswered_call_and_keeps_user_turn() {
    let mut messages = format_messages(&[text_msg(Role::User, "search")], false);
    messages.push(ApiMessage {
        role: "assistant".into(),
        content: vec![ApiContentBlock::Raw(json!({"type": "server_tool_use", "id": "srvtoolu_1"}))],
    });
    assert!(repair_rejected_prefill(&mut messages, PREFILL_ERROR));
    assert_eq!(roles(&messages), ["user"]);
    let retry = serde_json::to_value(&messages).unwrap();
    assert_eq!(retry[0]["content"][0]["text"], "search");
    assert_eq!(retry[0]["content"][1]["text"], CONTINUATION_USER_TURN);
    assert!(!repair_rejected_prefill(&mut messages, PREFILL_ERROR));
}

#[test]
fn transcript_ending_on_assistant_gets_a_continuation_user_turn() {
    let formatted = format_messages(
        &[
            text_msg(Role::User, "Q1"),
            text_msg(Role::Assistant, "A1 (interrupted by reload)"),
        ],
        false,
    );

    assert_eq!(roles(&formatted), vec!["user", "assistant", "user"]);
    let last = formatted.last().expect("trailing turn");
    match last.content.first().expect("content block") {
        ApiContentBlock::Text { text, .. } => assert_eq!(text, CONTINUATION_USER_TURN),
        _ => panic!("expected a text block in the continuation turn"),
    }
}

#[test]
fn transcript_already_ending_on_user_is_left_alone() {
    let formatted = format_messages(
        &[
            text_msg(Role::User, "Q1"),
            text_msg(Role::Assistant, "A1"),
            text_msg(Role::User, "Q2"),
        ],
        false,
    );

    assert_eq!(roles(&formatted), vec!["user", "assistant", "user"]);
    match formatted.last().expect("trailing turn").content.first() {
        Some(ApiContentBlock::Text { text, .. }) => assert_eq!(text, "Q2"),
        _ => panic!("expected the original user text"),
    }
}

#[test]
fn dangling_tool_use_repair_already_ends_on_user_and_is_not_double_patched() {
    // An assistant turn with an unanswered tool_use already gets a synthetic
    // user tool_result appended, so no extra continuation turn is needed.
    let messages = vec![
        text_msg(Role::User, "Q1"),
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "toolu_1".to_string(),
                name: "bash".to_string(),
                input: json!({"command": "ls"}),
                thought_signature: None,
            }]
            .into(),
            timestamp: None,
            tool_duration_ms: None,
        },
    ];

    let formatted = format_messages(&messages, false);
    assert_eq!(roles(&formatted), vec!["user", "assistant", "user"]);
    assert!(matches!(
        formatted.last().expect("trailing turn").content.first(),
        Some(ApiContentBlock::ToolResult { .. })
    ));
}
