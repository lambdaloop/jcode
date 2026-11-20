//! Hide context-fold reference tags from displayed prose.
//!
//! Model-driven context folding labels each transcript message with a
//! `[context-ref: mNNNNN]` tag so the model can name fold ranges. Models
//! sometimes echo those tags in their replies. They are addressing metadata
//! for the model, not content for the user, so the renderer drops them.

const OPEN: &str = "[context-ref: m";

/// Remove `[context-ref: mNNNNN]` tags and the horizontal whitespace before
/// them. Borrows the input unchanged when no tag is present.
pub fn strip_context_refs(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains(OPEN) {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(OPEN) {
        let tail = &rest[at + OPEN.len()..];
        let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 || !tail[digits..].starts_with(']') {
            out.push_str(&rest[..at + OPEN.len()]);
            rest = tail;
            continue;
        }
        out.push_str(rest[..at].trim_end_matches([' ', '\t']));
        rest = &tail[digits + 1..];
    }
    out.push_str(rest);
    std::borrow::Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::strip_context_refs;

    #[test]
    fn removes_tags_and_leaves_other_text() {
        assert_eq!(strip_context_refs("Done. [context-ref: m01379]"), "Done.");
        assert_eq!(strip_context_refs("a [context-ref: m00001][context-ref: m00002]\nb"), "a\nb");
        assert_eq!(strip_context_refs("keep [context-ref: mx] and [context-ref: m12"), "keep [context-ref: mx] and [context-ref: m12");
        assert!(matches!(strip_context_refs("plain"), std::borrow::Cow::Borrowed("plain")));
    }
}
