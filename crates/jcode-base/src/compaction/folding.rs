use super::*;
use jcode_compaction_core::folding::{self, ContextFold, FoldRenderer};

#[derive(Default)]
pub(super) struct FoldingState {
    pub folds: Vec<ContextFold>,
    history: Vec<Message>,
    prefix_chars: Vec<usize>,
    renderer: FoldRenderer,
    last_nudge: usize,
}

impl CompactionManager {
    pub(super) fn restore_context_folds(&mut self, state: &crate::session::StoredCompactionState) {
        self.folding = FoldingState::default();
        self.folding.folds.clone_from(&state.folds);
        self.folding.folds.sort_unstable_by_key(|fold| fold.start);
        let mut next = 0;
        self.folding.folds.retain(|fold| {
            // Do not judge the range here: a provisional agent seeds while its
            // transcript is still a single placeholder message, so every fold
            // looked out of range and was dropped. `prepare_context_folds`
            // prunes folds that no longer fit the real transcript.
            if fold.start > fold.end || (fold.active && fold.start < next) {
                return false;
            }
            if fold.active {
                next = fold.end + 1;
            }
            true
        });
    }

    /// Refresh only appended payloads. Session reseeds clear this derived cache.
    pub fn prepare_context_folds(&mut self, messages: &[Message]) {
        if self.mode != crate::config::CompactionMode::ModelDriven && self.folding.folds.is_empty() { return; }
        let state = &mut self.folding;
        let len = state.history.len();
        if len > messages.len() || (len > 0 && !state.history[len - 1].content.shares_storage_with(&messages[len - 1].content)) {
            state.history.clear();
            state.prefix_chars.clear();
            state.renderer.clear();
        }
        if state.prefix_chars.is_empty() { state.prefix_chars.push(0); }
        for message in &messages[state.history.len()..] {
            state.prefix_chars.push(state.prefix_chars.last().copied().unwrap_or(0).saturating_add(message_char_count(message)));
            state.history.push(message.clone());
        }
        state.folds.retain(|f| f.end < messages.len());
    }

    pub(super) fn folded_chars_saved(&self) -> usize {
        self.folding.folds.iter().filter(|f| f.active && f.start >= self.compacted_count)
            .map(|f| {
                let prefix = &self.folding.prefix_chars;
                prefix.get(f.end + 1).zip(prefix.get(f.start))
                    .map(|(end, start)| end.saturating_sub(*start).saturating_sub(f.summary.len() + 128))
                    .unwrap_or(0)
            }).sum()
    }

    pub fn compress_context(&mut self, start: &str, end: &str, summary: String) -> Result<u64, String> {
        if self.mode != crate::config::CompactionMode::ModelDriven { return Err("Enable model_driven compaction first".into()); }
        let start = folding::parse_ref(start)?;
        let end = folding::parse_ref(end)?;
        if start < self.compacted_count {
            return Err(format!(
                "Range starts behind the summary boundary. Start at {} or later.",
                folding::message_ref(self.compacted_count)
            ));
        }
        let id = folding::compress(&mut self.folding.folds, &self.folding.history, start, end, self.compacted_count, summary)?;
        self.fold_changed("model_driven");
        Ok(id)
    }

    pub fn decompress_context(&mut self, fold_id: u64) -> Result<(), String> {
        if self.mode != crate::config::CompactionMode::ModelDriven {
            return Err("Enable model_driven compaction first".into());
        }
        let index = self.folding.folds.iter().position(|f| f.id == fold_id && f.active).ok_or("Active fold not found")?;
        let fold = &self.folding.folds[index];
        if fold.start < self.compacted_count { return Err("Fold is behind the emergency compaction boundary. Use conversation_search to retrieve originals.".into()); }
        self.folding.folds[index].active = false;
        if self.token_estimate_with(&self.folding.history) as f32 >= self.token_budget as f32 * CRITICAL_THRESHOLD {
            self.folding.folds[index].active = true;
            return Err("Restoring this fold would exceed the context budget. Fold other history first or use conversation_search to retrieve a smaller excerpt.".into());
        }
        self.fold_changed("decompress");
        Ok(())
    }

    fn fold_changed(&mut self, trigger: &str) {
        self.observed_input_tokens = None;
        self.last_compaction = Some(CompactionEvent {
            trigger: trigger.into(), pre_tokens: None, post_tokens: None, tokens_saved: None,
            duration_ms: Some(0), messages_dropped: None, messages_compacted: None,
            summary_chars: Some(self.folding.folds.iter().filter(|f| f.active).map(|f| f.summary.len()).sum()),
            active_messages: None,
        });
    }

    pub fn context_fold_status(&self) -> serde_json::Value {
        serde_json::json!({"mode":self.mode.as_str(), "message_count":self.folding.history.len(),
            "first_ref":folding::message_ref(self.compacted_count),
            "context_budget":self.token_budget, "estimated_tokens":self.token_estimate_with(&self.folding.history),
            "folds":self.folding.folds.iter().map(|f| serde_json::json!({"id":f.id,"start":folding::message_ref(f.start),"end":folding::message_ref(f.end),"active":f.active})).collect::<Vec<_>>()})
    }

    /// A persisted reminder keeps the prefix append-only between fold mutations.
    pub fn context_fold_nudge(&mut self) -> Option<String> {
        if self.mode != crate::config::CompactionMode::ModelDriven { return None; }
        let count = self.total_turns;
        if self.context_usage_with(&self.folding.history) < 0.60 || count < self.folding.last_nudge.saturating_add(8) { return None; }
        self.folding.last_nudge = count;
        Some("<system-reminder>Context is filling. Use compress with start/end context-ref labels and a concise, accurate summary of completed work. Preserve decisions, paths, constraints and unfinished tasks. Originals remain searchable with conversation_search, and decompress restores a fold. Keep recent work verbatim.</system-reminder>".into())
    }

    pub fn persisted_state_matches(&self, state: Option<&crate::session::StoredCompactionState>) -> bool {
        match state {
            None => self.active_summary.is_none() && self.folding.folds.is_empty(),
            Some(state) => self.compacted_count == state.compacted_count && self.folding.folds == state.folds
                && match &self.active_summary {
                    Some(summary) => summary.text == state.summary_text && summary.openai_encrypted_content == state.openai_encrypted_content
                        && summary.covers_up_to_turn == state.covers_up_to_turn && summary.original_turn_count == state.original_turn_count,
                    None => state.summary_text.is_empty() && state.openai_encrypted_content.is_none() && state.compacted_count == 0,
                },
        }
    }

    pub fn persisted_state(&self) -> Option<crate::session::StoredCompactionState> {
        if self.active_summary.is_none() && self.folding.folds.is_empty() { return None; }
        let summary = self.active_summary.as_ref();
        Some(crate::session::StoredCompactionState {
            summary_text: summary.map_or_else(String::new, |s| s.text.clone()),
            openai_encrypted_content: summary.and_then(|s| s.openai_encrypted_content.clone()),
            covers_up_to_turn: summary.map(|s| s.covers_up_to_turn).unwrap_or(0),
            original_turn_count: summary.map(|s| s.original_turn_count).unwrap_or(0),
            compacted_count: self.compacted_count, folds: self.folding.folds.clone(),
        })
    }

    pub fn messages_for_api_with(&mut self, all_messages: &[Message]) -> Vec<Message> {
        self.check_and_apply_compaction_with(all_messages);
        self.discard_oversized_openai_native_compaction();
        self.prepare_context_folds(all_messages);
        let mut result = Vec::new();
        if let Some(summary) = &self.active_summary {
            let block = summary.openai_encrypted_content.as_ref().map(|text| ContentBlock::OpenAICompaction { encrypted_content: text.to_string() })
                .unwrap_or_else(|| ContentBlock::Text { text: compacted_summary_text_block(&summary.text), cache_control: None });
            result.push(Message { role: Role::User, content: vec![block].into(), timestamp: None, tool_duration_ms: None });
        }
        if self.mode == crate::config::CompactionMode::ModelDriven || !self.folding.folds.is_empty() {
            result.extend(self.folding.renderer.render(all_messages, &self.folding.folds, self.compacted_count, self.mode == crate::config::CompactionMode::ModelDriven));
        } else { result.extend(self.active_messages(all_messages).iter().cloned()); }
        result
    }
}
