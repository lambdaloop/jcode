use super::*;

#[test]
fn coalesced_folds_restore_the_entire_original_range() {
    let history = vec![Message::user(&"old detail ".repeat(100)); 4];
    let mut folds = Vec::new();
    let first = compress(&mut folds, &history, 0, 0, 0, "first".into()).unwrap();
    let merged = compress(&mut folds, &history, 0, 1, 0, "merged".into()).unwrap();
    assert_ne!(first, merged);
    assert_eq!(folds.len(), 1);
    let mut renderer = FoldRenderer::default();
    assert_eq!(renderer.render(&history, &folds, 0, true).len(), 3);
    folds[0].active = false;
    let restored = renderer.render(&history, &folds, 0, false);
    assert_eq!(serde_json::to_vec(&history).unwrap(), serde_json::to_vec(&restored).unwrap());
}

#[test]
fn folded_prefix_is_byte_stable_after_append_and_reversible() {
    let mut history = vec![Message::user(&"original ".repeat(100)), Message::user("recent"), Message::user("new")];
    let mut folds = Vec::new();
    compress(&mut folds, &history, 0, 0, 0, "summary".into()).unwrap();
    let mut renderer = FoldRenderer::default();
    let before = renderer.render(&history, &folds, 0, true);
    history.push(Message::user("append"));
    let after = renderer.render(&history, &folds, 0, true);
    assert_eq!(serde_json::to_vec(&before).unwrap(), serde_json::to_vec(&after[..before.len()]).unwrap());
    assert!(before[1].content.shares_storage_with(&after[1].content));
    folds[0].active = false;
    let restored = renderer.render(&history, &folds, 0, true);
    assert!(serde_json::to_string(&restored[0]).unwrap().contains("original "));
    assert!(!serde_json::to_string(&history[0]).unwrap().contains("context-ref"));
}

#[test]
fn ref_tag_never_follows_a_tool_use_block() {
    let mut call = Message::user("thinking aloud");
    call.content.push(ContentBlock::ToolUse { id: "a".into(), name: "tool".into(), input: serde_json::json!({}), thought_signature: None });
    let history = vec![call, Message::user("recent")];
    let rendered = FoldRenderer::default().render(&history, &[], 0, true);
    let blocks: Vec<_> = rendered[0].content.iter().collect();
    assert!(matches!(blocks.last(), Some(ContentBlock::ToolUse { .. })), "{blocks:?}");
    assert!(serde_json::to_string(&rendered[0]).unwrap().contains("context-ref: m00001"));
}

#[test]
fn range_expands_across_all_results_of_a_parallel_call() {
    let mut call = Message::user("call");
    for id in ["a", "b"] {
        call.content.push(ContentBlock::ToolUse { id: id.into(), name: "tool".into(), input: serde_json::json!({}), thought_signature: None });
    }
    let result = |id: &str| {
        let mut message = Message::user("result");
        message.content = vec![ContentBlock::ToolResult { tool_use_id: id.into(), content: "output".into(), is_error: None }].into();
        message
    };
    let history = vec![call, result("a"), result("b")];
    assert_eq!(safe_range(&history, 1, 1), Ok((0, 2)));
    assert!(safe_range(&history[..2], 0, 1).is_err());
}
