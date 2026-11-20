//! Local, reversible context folding. Originals remain owned by the session.
use crate::message_char_count;
use jcode_message_types::{ContentBlock, Message};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextFold {
    pub id: u64,
    /// Zero-based inclusive transcript ordinals, never provider-view indices.
    pub start: usize,
    pub end: usize,
    pub summary: String,
    pub active: bool,
}

/// Incrementally retains tagged visible payloads, not another copy of originals.
#[derive(Default)]
pub struct FoldRenderer {
    tagged: HashMap<usize, (Message, Message, usize)>,
    summaries: HashMap<u64, Message>,
    generation: usize,
}

pub fn parse_ref(value: &str) -> Result<usize, String> {
    let invalid = || "Expected a message reference such as m00001".to_string();
    let digits = value.strip_prefix('m').ok_or_else(invalid)?;
    match digits.parse::<usize>() {
        Ok(ordinal) => ordinal.checked_sub(1).ok_or_else(invalid),
        Err(_) => Err(invalid()),
    }
}

pub fn message_ref(index: usize) -> String { format!("m{:05}", index + 1) }

/// Expand a range until every included tool call and result stays together.
/// Run only when the model requests a fold, never on the append-only hot path.
pub fn safe_range(messages: &[Message], mut start: usize, mut end: usize) -> Result<(usize, usize), String> {
    if start > end || end >= messages.len() { return Err("Invalid message range".into()); }
    let mut uses = HashMap::new();
    let mut results = HashMap::new();
    for (index, message) in messages.iter().enumerate() {
        for block in &message.content {
            match block {
                ContentBlock::ToolUse { id, .. } => { uses.insert(id.as_str(), index); }
                ContentBlock::ToolResult { tool_use_id, .. } => { results.insert(tool_use_id.as_str(), index); }
                _ => {}
            }
        }
    }
    loop {
        let previous = (start, end);
        for (id, &index) in &uses {
            if (start..=end).contains(&index) {
                let result = results.get(id).ok_or("Cannot fold an unfinished tool call")?;
                start = start.min(*result);
                end = end.max(*result);
            }
        }
        for (id, &index) in &results {
            if (start..=end).contains(&index) {
                let call = uses.get(id).ok_or("Cannot fold an orphan tool result")?;
                start = start.min(*call);
                end = end.max(*call);
            }
        }
        if previous == (start, end) { return Ok(previous); }
    }
}

pub fn compress(folds: &mut Vec<ContextFold>, messages: &[Message], start: usize, end: usize, min_start: usize, summary: String) -> Result<u64, String> {
    if summary.trim().is_empty() || summary.len() > 64 * 1024 {
        return Err("Summary must be nonempty and at most 64 KiB".into());
    }
    let (start, end) = safe_range(messages, start, end)?;
    if start < min_start { return Err("Tool-pair expansion would cross the fallback compaction boundary".into()); }
    // Never hide the current request or the tool call executing this mutation.
    if end + 2 >= messages.len() { return Err("Keep the latest two messages verbatim".into()); }
    if folds.iter().any(|f| f.active && f.start <= end && start <= f.end && !(start <= f.start && f.end <= end)) {
        return Err("Include the whole active fold or decompress it first.".into());
    }
    let original_chars: usize = messages[start..=end].iter().map(message_char_count).sum();
    let already_saved: usize = folds.iter().filter(|f| f.active && start <= f.start && f.end <= end)
        .map(|f| messages[f.start..=f.end].iter().map(message_char_count).sum::<usize>().saturating_sub(f.summary.len() + 128)).sum();
    if summary.len() + 128 >= original_chars.saturating_sub(already_saved) { return Err("Summary must be smaller than the currently visible range".into()); }
    let id = folds.iter().map(|f| f.id).max().unwrap_or(0).checked_add(1).ok_or("Fold ID exhausted")?;
    folds.retain(|f| !(start <= f.start && f.end <= end));
    let at = folds.partition_point(|fold| fold.start <= start);
    folds.insert(at, ContextFold { id, start, end, summary, active: true });
    Ok(id)
}

impl FoldRenderer {
    pub fn clear(&mut self) { *self = Self::default(); }

    pub fn render(&mut self, messages: &[Message], folds: &[ContextFold], offset: usize, refs: bool) -> Vec<Message> {
        self.generation = self.generation.wrapping_add(1);
        let active = folds.iter().filter(|f| f.active && f.start >= offset && f.start <= f.end && f.end < messages.len());
        let mut output = Vec::new();
        let mut index = offset.min(messages.len());
        let mut visible_summaries = HashSet::new();
        for fold in active {
            if fold.start < index { continue; }
            while index < fold.start {
                output.push(self.tagged_message(index, &messages[index], refs));
                index += 1;
            }
            let summary = self.summaries.entry(fold.id).or_insert_with(|| {
                let mut message = Message::user(&format!("<context-fold id=\"{}\" range=\"{}:{}\">\n{}\n</context-fold>", fold.id, message_ref(fold.start), message_ref(fold.end), fold.summary));
                message.timestamp = None;
                message
            });
            visible_summaries.insert(fold.id);
            output.push(summary.clone());
            index = fold.end + 1;
        }
        while index < messages.len() {
            output.push(self.tagged_message(index, &messages[index], refs));
            index += 1;
        }
        self.tagged.retain(|_, (_, _, seen)| *seen == self.generation);
        self.summaries.retain(|id, _| visible_summaries.contains(id));
        output
    }

    fn tagged_message(&mut self, index: usize, original: &Message, refs: bool) -> Message {
        if !refs || original.is_internal_system_reminder() { return original.clone(); }
        if let Some((source, tagged, seen)) = self.tagged.get_mut(&index) {
            if source.content.shares_storage_with(&original.content) && source.timestamp == original.timestamp && source.tool_duration_ms == original.tool_duration_ms && source.role == original.role { *seen = self.generation; return tagged.clone(); }
        }
        let mut tagged = original.clone();
        let reference = format!("[context-ref: {}]", message_ref(index));
        if let Some(ContentBlock::ToolResult { content, .. }) = tagged.content.iter_mut().find(|b| matches!(b, ContentBlock::ToolResult { .. })) {
            content.push('\n');
            content.push_str(&reference);
        } else if let Some(at) = tagged.content.iter().position(|b| matches!(b, ContentBlock::ToolUse { .. })) {
            // tool_use must close the assistant turn, or Anthropic reads the
            // following tool_result turn as a prefill continuation.
            tagged.content.insert(at, ContentBlock::Text { text: reference, cache_control: None });
        } else {
            tagged.content.push(ContentBlock::Text { text: reference, cache_control: None });
        }
        self.tagged.insert(index, (original.clone(), tagged.clone(), self.generation));
        tagged
    }
}

#[cfg(test)]
mod tests;
