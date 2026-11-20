//! Model-driven folding tools operate only on the live session manager.

use super::{Tool, ToolContext, ToolOutput, WeakRegistry};
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
    registry: WeakRegistry,
    action: FoldAction,
}

impl ContextFoldingTool {
    pub(super) fn new(registry: WeakRegistry, action: FoldAction) -> Self {
        Self { registry, action }
    }

    /// Resolve the manager of the registry this call runs in.
    ///
    /// The registry a tool executes on is not always the one the agent tracks:
    /// failover rebuilds an agent on a fresh registry, and per-client registries
    /// keep their own managers. Agents publish theirs per session, so prefer
    /// that and fall back to the executing registry.
    fn manager(&self, session_id: &str) -> Option<Arc<RwLock<CompactionManager>>> {
        crate::compaction::live_session::resolve(session_id)
            .or_else(|| self.registry.upgrade().map(|registry| registry.compaction()))
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
        self.registry
            .upgrade()
            .is_some_and(|registry| registry.compaction()
                .try_read()
                .is_ok_and(|manager| manager.mode() == CompactionMode::ModelDriven))
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

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let manager = self.manager(&ctx.session_id)
            .ok_or_else(|| anyhow::anyhow!("This tool is no longer attached to a live session"))?;
        let output = match self.action {
            FoldAction::Compress => {
                let args: CompressInput = serde_json::from_value(input)?;
                let fold_id = manager.write().await
                    .compress_context(&args.start, &args.end, args.summary)
                    .map_err(anyhow::Error::msg)?;
                crate::compaction::live_session::mark_dirty(&ctx.session_id);
                json!({"fold_id": fold_id})
            }
            FoldAction::Decompress => {
                let args: DecompressInput = serde_json::from_value(input)?;
                manager.write().await.decompress_context(args.fold_id)
                    .map_err(anyhow::Error::msg)?;
                crate::compaction::live_session::mark_dirty(&ctx.session_id);
                json!({"fold_id": args.fold_id, "decompressed": true})
            }
            FoldAction::Status => manager.read().await.context_fold_status(),
        };
        Ok(ToolOutput::new(output.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::Registry;

    /// A registry wired like a live session: session tools registered against
    /// its own manager, so resolving through the registry and through a clone
    /// of it agree.
    async fn session_registry() -> Registry {
        let registry = Registry::empty();
        let mut tools = std::collections::HashMap::new();
        registry.insert_session_tools(&mut tools);
        *registry.tools.write().await = tools;
        registry
    }

    async fn registered(registry: &Registry, name: &str) -> Arc<dyn Tool> {
        registry.tools.read().await.get(name).cloned().expect("session tool registered")
    }

    #[tokio::test]
    async fn schemas_expose_required_arguments() {
        let registry = session_registry().await;
        for (name, required) in [
            ("compress", json!(["intent", "start", "end", "summary"])),
            ("decompress", json!(["intent", "fold_id"])),
            ("context_status", json!(["intent"])),
        ] {
            let tool = registered(&registry, name).await;
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
        let registry = session_registry().await;
        let manager = registry.compaction();
        let tool = registered(&registry, "context_status").await;
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

    /// Registry clones serve one session and must share its manager, otherwise
    /// the agent folds its own copy while every tool context sees an empty one.
    #[test]
    fn registry_clones_share_the_session_manager() {
        let registry = Registry::empty();
        assert!(Arc::ptr_eq(&registry.compaction(), &registry.clone().compaction()));
        assert!(!Arc::ptr_eq(&registry.compaction(), &registry.independent().compaction()));
    }

    /// The tool executes inside a registry clone, as it does on the server, and
    /// must still fold the transcript the agent tracks.
    #[tokio::test]
    async fn execution_folds_the_manager_of_its_executing_registry() {
        let registry = session_registry().await;
        let manager = registry.compaction();
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
        let compress = ContextFoldingTool::new(registry.clone().downgrade(), FoldAction::Compress);
        assert!(compress.model_visible());
        let result = compress.execute(json!({"intent":"save space", "start":"m00001", "end":"m00001", "summary":"Old work completed."}), context()).await.unwrap();
        let fold_id = serde_json::from_str::<Value>(&result.output).unwrap()["fold_id"].as_u64().unwrap();
        // The agent owns the session write, so a fold must flag it for persistence.
        assert!(crate::compaction::live_session::take_dirty(&context().session_id));
        let status = ContextFoldingTool::new(registry.clone().downgrade(), FoldAction::Status);
        let result = status.execute(json!({"intent":"inspect context"}), context()).await.unwrap();
        let value: Value = serde_json::from_str::<Value>(&result.output).unwrap();
        assert_eq!(value["folds"][0]["id"], fold_id);
        assert_eq!(value["folds"][0]["active"], true);
        let decompress = ContextFoldingTool::new(registry.downgrade(), FoldAction::Decompress);
        decompress.execute(json!({"intent":"restore", "fold_id":fold_id}), context()).await.unwrap();
        assert_eq!(manager.read().await.context_fold_status()["folds"][0]["active"], false);
        assert!(decompress.execute(json!({"fold_id":fold_id}), context()).await.is_err());
        manager.write().await.set_mode(CompactionMode::Reactive);
        assert!(!compress.model_visible());
    }

    /// A tool can execute on a registry that never tracked the transcript, so the
    /// manager the agent published for its session wins over the registry's.
    #[tokio::test]
    async fn execution_prefers_the_published_session_manager() {
        let registry = session_registry().await;
        let session = "fold-tool-published-manager-test";
        let live = registry.compaction();
        {
            let mut guard = live.write().await;
            guard.set_mode(CompactionMode::ModelDriven);
            guard.prepare_context_folds(&[
                crate::message::Message::user("first"),
                crate::message::Message::assistant_text("second"),
            ]);
        }
        crate::compaction::live_session::publish(session, &live);
        let status = ContextFoldingTool::new(registry.downgrade(), FoldAction::Status);
        let mut ctx = context();
        ctx.session_id = session.to_string();
        let result = status.execute(json!({"intent":"inspect context"}), ctx).await.unwrap();
        let value: Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(value["message_count"], 2);
    }

    #[test]
    fn decompress_rejects_invalid_fold_ids() {
        for value in [json!(-1), json!(1.5), json!("1"), Value::Null] {
            assert!(serde_json::from_value::<DecompressInput>(json!({"fold_id": value})).is_err());
        }
    }
}
