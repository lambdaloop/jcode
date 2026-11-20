use super::*;
use jcode_compaction_core::folding::ContextFold;
use std::sync::Arc;

fn state() -> StoredCompactionState {
    StoredCompactionState {
        summary_text: String::new(),
        openai_encrypted_content: None,
        covers_up_to_turn: 0,
        original_turn_count: 0,
        compacted_count: 0,
        folds: vec![ContextFold {
            id: 7,
            start: 0,
            end: 1,
            summary: "folded context".into(),
            active: true,
        }],
    }
}

fn session() -> Session {
    let mut session = Session::create_with_id("session_fold_persistence".into(), None, None);
    for _ in 0..3 {
        session.add_message(Role::User, vec![ContentBlock::Text {
            text: "original transcript".into(), cache_control: None,
        }]);
    }
    session.compaction = Some(Arc::new(state()));
    session
}

#[test]
fn legacy_compaction_defaults_to_empty_folds_and_omits_empty_field() {
    let legacy = serde_json::json!({
        "summary_text": "legacy", "covers_up_to_turn": 2,
        "original_turn_count": 2, "compacted_count": 2,
    });
    let state: StoredCompactionState = serde_json::from_value(legacy.clone()).unwrap();
    assert!(state.folds.is_empty());
    assert_eq!(serde_json::to_value(state).unwrap(), legacy);
}

#[test]
fn folds_roundtrip_with_inactive_state_and_originals() {
    let mut session = session();
    Arc::make_mut(session.compaction.as_mut().unwrap()).folds[0].active = false;
    let json = serde_json::to_vec(&session).unwrap();
    let loaded: Session = serde_json::from_slice(&json).unwrap();
    assert_eq!(loaded.compaction, session.compaction);
    assert_eq!(loaded.messages.len(), 3);
    assert_eq!(
        serde_json::to_value(&loaded.messages[0].content).unwrap(),
        serde_json::to_value(&session.messages[0].content).unwrap(),
    );
}

#[test]
fn folds_persist_through_snapshot_and_journal() -> anyhow::Result<()> {
    struct RestoreHome(Option<std::ffi::OsString>);
    impl Drop for RestoreHome {
        fn drop(&mut self) {
            if let Some(home) = &self.0 {
                crate::env::set_var("JCODE_HOME", home);
            } else {
                crate::env::remove_var("JCODE_HOME");
            }
        }
    }
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir()?;
    let _restore = RestoreHome(std::env::var_os("JCODE_HOME"));
    crate::env::set_var("JCODE_HOME", home.path());
    let mut session = session();
    Arc::make_mut(session.compaction.as_mut().unwrap()).folds[0].active = false;
    session.add_message(Role::User, vec![ContentBlock::Text {
        text: "x".repeat(1024 * 1024), cache_control: None,
    }]);
    session.save()?;
    assert_eq!(Session::load(&session.id)?.compaction, session.compaction);
    Arc::make_mut(session.compaction.as_mut().unwrap()).folds[0].active = true;
    session.add_message(Role::User, vec![]);
    session.save()?;
    let mut resumed = Session::load(&session.id)?;
    assert_eq!(resumed.compaction, session.compaction);
    // Reload interrupts the reload tool itself, so recovery inserts a missing
    // tool result after the folded range and checkpoints the repaired history.
    let mut repaired = session.messages[2].clone();
    repaired.id = "message_fold_repaired_tool_output".into();
    resumed.insert_message(3, repaired);
    resumed.save()?;
    let reloaded = Session::load(&session.id)?;
    assert_eq!(reloaded.compaction, session.compaction);
    assert_eq!(reloaded.messages[3].id, "message_fold_repaired_tool_output");
    Ok(())
}

#[test]
fn fold_summaries_are_redacted_without_mutating_shared_source() {
    let mut session = session();
    let secret = "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123";
    Arc::make_mut(session.compaction.as_mut().unwrap()).folds[0].summary = secret.into();
    let exported = session.redacted_for_export();
    assert_ne!(exported.compaction.as_ref().unwrap().folds[0].summary, secret);
    assert_eq!(session.compaction.as_ref().unwrap().folds[0].summary, secret);
}

#[test]
fn transcript_edits_preserve_only_unaffected_fold_ranges() {
    let original = session();
    let mut inserted = original.clone();
    inserted.insert_message(1, original.messages[0].clone());
    assert!(inserted.compaction.as_ref().unwrap().folds.is_empty());
    let mut before = original.clone();
    before.insert_message(0, original.messages[2].clone());
    let shifted = &before.compaction.as_ref().unwrap().folds[0];
    assert_eq!((shifted.start, shifted.end), (1, 2));
    let mut replaced = original.clone();
    replaced.replace_messages(original.messages.clone());
    assert!(replaced.compaction.as_ref().unwrap().folds.is_empty());
    assert_eq!(original.compaction.as_ref().unwrap().folds.len(), 1);
}

#[test]
fn truncate_retains_only_fully_in_bounds_inclusive_ranges() {
    let mut session = session();
    let original = session.clone();
    let mut removed = state().folds.remove(0);
    removed.id = 8;
    removed.end = 2;
    Arc::make_mut(session.compaction.as_mut().unwrap()).folds.push(removed);
    session.truncate_messages(2);
    assert_eq!(session.compaction.as_ref().unwrap().folds, original.compaction.as_ref().unwrap().folds);
    session.truncate_messages(1);
    assert!(session.compaction.as_ref().unwrap().folds.is_empty());
    assert_eq!(original.compaction.as_ref().unwrap().folds.len(), 1);
}
