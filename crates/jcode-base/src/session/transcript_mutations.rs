use super::{Session, StoredMessage};
use std::sync::Arc;

impl Session {
    fn retain_transcript_folds(&mut self, len: usize) {
        if let Some(state) = self.compaction.as_mut() {
            if state.folds.iter().any(|fold| fold.start >= len || fold.end >= len) {
                Arc::make_mut(state)
                    .folds
                    .retain(|fold| fold.start < len && fold.end < len);
            }
        }
    }

    pub fn insert_message(&mut self, index: usize, message: StoredMessage) {
        self.messages.insert(index, message);
        self.retain_transcript_folds(0);
        self.mark_memory_profile_dirty();
        self.mark_messages_full_dirty();
    }

    pub fn replace_messages(&mut self, messages: Vec<StoredMessage>) {
        self.messages = messages;
        self.retain_transcript_folds(0);
        self.mark_memory_profile_dirty();
        self.mark_messages_full_dirty();
    }

    pub fn truncate_messages(&mut self, len: usize) {
        if len < self.messages.len() {
            self.messages.truncate(len);
            self.retain_transcript_folds(len);
            self.mark_memory_profile_dirty();
            self.mark_messages_full_dirty();
        }
    }
}
