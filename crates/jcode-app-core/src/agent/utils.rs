use crate::session::GitState;
use std::path::Path;
use std::process::Command;

pub(super) fn trace_enabled() -> bool {
    match std::env::var("JCODE_TRACE") {
        Ok(value) => {
            let value = value.trim();
            !value.is_empty() && value != "0" && value.to_lowercase() != "false"
        }
        Err(_) => false,
    }
}

pub(super) fn git_state_for_dir(dir: &Path) -> Option<GitState> {
    let root = git_output(dir, &["rev-parse", "--show-toplevel"])?;
    let head = git_output(dir, &["rev-parse", "HEAD"]);
    let branch = git_output(dir, &["rev-parse", "--abbrev-ref", "HEAD"]);
    let dirty = git_output(dir, &["status", "--porcelain"]).map(|out| !out.is_empty());

    Some(GitState {
        root,
        head,
        branch,
        dirty,
    })
}

fn git_output(dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Drop `[context-ref: mNNNNN]` tags the model echoed into its reply text.
/// They address transcript messages for folding and are re-added on every
/// request, so persisting them only leaks them into transcripts and mirrors.
pub(super) fn strip_echoed_context_refs(blocks: &mut [crate::message::ContentBlock]) {
    for block in blocks {
        if let crate::message::ContentBlock::Text { text, .. } = block
            && let std::borrow::Cow::Owned(clean) = jcode_render_core::strip_context_refs(text)
        {
            *text = clean;
        }
    }
}

#[cfg(test)]
mod context_ref_tests {
    use crate::message::ContentBlock;

    #[test]
    fn persisted_assistant_text_has_no_context_ref() {
        let mut blocks = vec![ContentBlock::Text { text: "Done. [context-ref: m00013]".into(), cache_control: None }];
        super::strip_echoed_context_refs(&mut blocks);
        let ContentBlock::Text { text, .. } = &blocks[0] else { panic!() };
        assert_eq!(text, "Done.");
    }
}
