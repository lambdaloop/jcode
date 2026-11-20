//! Tools whose state belongs to a single session rather than the base cache.

use super::*;
use context_folding::{ContextFoldingTool, FoldAction};

impl Registry {
    pub(super) fn insert_session_tools(&self, tools: &mut HashMap<String, Arc<dyn Tool>>) {
        Self::insert_tool(tools, "batch", batch::BatchTool::new(self.downgrade()));
        Self::insert_tool(tools, "ipython", ipython::IpythonTool::new(self.downgrade()));
        Self::insert_tool(
            tools,
            "conversation_search",
            conversation_search::ConversationSearchTool::new(self.compaction.clone()),
        );
        for action in [FoldAction::Compress, FoldAction::Decompress, FoldAction::Status] {
            let tool = ContextFoldingTool::new(self.downgrade(), action);
            let name = tool.name().to_owned();
            Self::insert_tool(tools, &name, tool);
        }
        // Codemode is opt-in. When off, it never reaches the model.
        if codemode::enabled() {
            Self::insert_tool(tools, "codemode", codemode::CodemodeTool::new(self.downgrade()));
        }
        // Disabled integration discovery must never contact its endpoint.
        if crate::config::config().sponsors.enabled {
            Self::insert_tool(tools, "integration_tools", discover::DiscoverToolsTool::new());
        }
    }
}
