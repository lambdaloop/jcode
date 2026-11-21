//! How a provider wants the model's reasoning echoed back on later turns.
//!
//! Replay is a property of the wire format a provider speaks, not of its
//! display name: orchestrators such as `MultiProvider` report a stable but
//! human-facing name ("Claude") while their underlying runtime speaks the
//! Anthropic wire format. Deciding replay from the name therefore misses the
//! orchestrator entirely and the model re-plans every turn.

/// Wire format used to replay an assistant turn's reasoning.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ReasoningReplay {
    /// The provider cannot take reasoning back: keep a readable history trace
    /// only.
    #[default]
    None,
    /// Anthropic native signed thinking blocks (`ContentBlock::AnthropicThinking`).
    AnthropicThinking,
    /// OpenAI native reasoning items, carried separately from the readable text.
    OpenAiReasoning,
    /// Plain readable reasoning text sent back as ordinary content.
    ReadableText,
}

impl ReasoningReplay {
    /// Whether the provider can accept reasoning echoed back on a later turn.
    pub fn stores_context(self) -> bool {
        !matches!(self, Self::None)
    }

    /// Whether replaying this reasoning also preserves the readable text, so a
    /// separate history-only trace would duplicate it.
    pub fn replay_is_readable(self) -> bool {
        matches!(self, Self::ReadableText | Self::AnthropicThinking)
    }
}

#[cfg(test)]
mod tests {
    use super::ReasoningReplay;

    #[test]
    fn stores_context_only_for_replayable_kinds() {
        assert!(!ReasoningReplay::None.stores_context());
        assert!(ReasoningReplay::AnthropicThinking.stores_context());
        assert!(ReasoningReplay::OpenAiReasoning.stores_context());
        assert!(ReasoningReplay::ReadableText.stores_context());
    }

    #[test]
    fn openai_reasoning_is_not_readable() {
        assert!(!ReasoningReplay::OpenAiReasoning.replay_is_readable());
        assert!(ReasoningReplay::AnthropicThinking.replay_is_readable());
        assert!(ReasoningReplay::ReadableText.replay_is_readable());
    }
}
