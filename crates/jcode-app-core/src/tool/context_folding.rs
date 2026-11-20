//! Model-driven folding tools operate only on the live session manager.

use super::{Tool, ToolContext, ToolOutput};
use crate::compaction::CompactionManager;
use crate::config::CompactionMode;
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Clone, Copy)]
pub(super) enum FoldAction {
    Compress,
    Decompress,
    Status,
}

pub(super) struct ContextFoldingTool {
    compaction: Arc<RwLock<CompactionManager>>,
    action: FoldAction,
}

impl ContextFoldingTool {
    pub(super) fn new(compaction: Arc<RwLock<CompactionManager>>, action: FoldAction) -> Self {
        Self { compaction, action }
    }
}

#[derive(Deserialize)]
struct CompressInput {
    start: String,
    end: String,
    summary: String,
}

#[derive(Deserialize)]
struct DecompressInput {
    fold_id: u64,
}

#[async_trait]
impl Tool for ContextFoldingTool {
    fn name(&self) -> &str {
        match self.action {
            FoldAction::Compress => "compress",
            FoldAction::Decompress => "decompress",
            FoldAction::Status => "context_status",
        }
    }

    fn description(&self) -> &str {
        match self.action {
            FoldAction::Compress => "Fold a completed context range into a concise summary. Use the start and end message anchors shown in context. Preserve decisions, constraints, and facts needed for future work. Originals remain recoverable with decompress.",
            FoldAction::Decompress => "Restore the original context covered by a fold ID.",
            FoldAction::Status => "Inspect live context usage and recoverable context folds.",
        }
    }

    fn model_visible(&self) -> bool {
        self.compaction
            .try_read()
            .is_ok_and(|manager| manager.mode() == CompactionMode::ModelDriven)
    }

    fn parameters_schema(&self) -> Value {
        let mut schema = json!({
            "type": "object",
            "properties": {"intent": super::intent_schema_property()},
            "required": ["intent"]
        });
        match self.action {
            FoldAction::Compress => {
                schema["properties"]["start"] = json!({"type": "string", "description": "Inclusive start message anchor."});
                schema["properties"]["end"] = json!({"type": "string", "description": "Inclusive end message anchor."});
                schema["properties"]["summary"] = json!({"type": "string", "description": "Replacement summary retaining essential context."});
                schema["required"] = json!(["intent", "start", "end", "summary"]);
            }
            FoldAction::Decompress => {
                schema["properties"]["fold_id"] = json!({"type": "integer", "minimum": 0, "description": "Fold ID returned by compress or context_status."});
                schema["required"] = json!(["intent", "fold_id"]);
            }
            FoldAction::Status => {}
        }
        schema
    }

    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
        let output = match self.action {
            FoldAction::Compress => {
                let args: CompressInput = serde_json::from_value(input)?;
                let fold_id = self.compaction.write().await
                    .compress_context(&args.start, &args.end, args.summary)
                    .map_err(anyhow::Error::msg)?;
                json!({"fold_id": fold_id})
            }
            FoldAction::Decompress => {
                let args: DecompressInput = serde_json::from_value(input)?;
                self.compaction.write().await.decompress_context(args.fold_id)
                    .map_err(anyhow::Error::msg)?;
                json!({"fold_id": args.fold_id, "decompressed": true})
            }
            FoldAction::Status => self.compaction.read().await.context_fold_status(),
        };
        Ok(ToolOutput::new(output.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_expose_required_arguments() {
        let manager = Arc::new(RwLock::new(CompactionManager::new()));
        for (action, name, required) in [
            (FoldAction::Compress, "compress", json!(["intent", "start", "end", "summary"])),
            (FoldAction::Decompress, "decompress", json!(["intent", "fold_id"])),
            (FoldAction::Status, "context_status", json!(["intent"])),
        ] {
            let tool = ContextFoldingTool::new(manager.clone(), action);
            assert_eq!(tool.name(), name);
            let schema = tool.parameters_schema();
            assert_eq!(schema["required"], required);
            for field in required.as_array().unwrap() {
                assert!(schema["properties"].get(field.as_str().unwrap()).is_some());
            }
        }
    }

    #[tokio::test]
    async fn visibility_is_nonblocking_when_manager_is_locked() {
        let manager = Arc::new(RwLock::new(CompactionManager::new()));
        let tool = ContextFoldingTool::new(manager.clone(), FoldAction::Status);
        let _guard = manager.write().await;
        assert!(!tool.model_visible());
    }

    fn context() -> ToolContext {
        ToolContext {
            session_id: "fold-tool-unit-test".into(),
            message_id: "message".into(),
            tool_call_id: "call".into(),
            working_dir: None,
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: super::super::ToolExecutionMode::Direct,
        }
    }

    /// Tools capture the manager of the registry that built them, so every
    /// registry clone serving the same session must hand them that same
    /// manager. A fresh manager per clone left `compress` reading an empty
    /// transcript while the agent folded its own copy.
    #[test]
    fn registry_clones_share_the_session_manager() {
        let registry = super::super::Registry::empty();
        let clone = registry.clone();
        assert!(Arc::ptr_eq(&registry.compaction(), &clone.compaction()));
        let independent = registry.independent();
        assert!(!Arc::ptr_eq(&registry.compaction(), &independent.compaction()));
    }

    #[tokio::test]
    async fn execution_mutates_shared_live_manager() {
        let manager = Arc::new(RwLock::new(CompactionManager::new()));
        {
            let mut guard = manager.write().await;
            guard.set_mode(CompactionMode::ModelDriven);
            guard.prepare_context_folds(&[
                crate::message::Message::user(&"old detail ".repeat(100)),
                crate::message::Message::assistant_text("done"),
                crate::message::Message::user("next task"),
                crate::message::Message::assistant_text("working"),
            ]);
        }
        let compress = ContextFoldingTool::new(manager.clone(), FoldAction::Compress);
        assert!(compress.model_visible());
        let result = compress.execute(json!({"intent":"save space", "start":"m00001", "end":"m00001", "summary":"Old work completed."}), context()).await.unwrap();
        let fold_id = serde_json::from_str::<Value>(&result.output).unwrap()["fold_id"].as_u64().unwrap();
        let status = ContextFoldingTool::new(manager.clone(), FoldAction::Status);
        let result = status.execute(json!({"intent":"inspect context"}), context()).await.unwrap();
        let value: Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(value["folds"][0]["id"], fold_id);
        assert_eq!(value["folds"][0]["active"], true);
        let decompress = ContextFoldingTool::new(manager.clone(), FoldAction::Decompress);
        decompress.execute(json!({"intent":"restore", "fold_id":fold_id}), context()).await.unwrap();
        assert_eq!(manager.read().await.context_fold_status()["folds"][0]["active"], false);
        assert!(decompress.execute(json!({"fold_id":fold_id}), context()).await.is_err());
        manager.write().await.set_mode(CompactionMode::Reactive);
        assert!(!compress.model_visible());
    }

    #[test]
    fn decompress_rejects_invalid_fold_ids() {
        for value in [json!(-1), json!(1.5), json!("1"), Value::Null] {
            assert!(serde_json::from_value::<DecompressInput>(json!({"fold_id": value})).is_err());
        }
    }
}
