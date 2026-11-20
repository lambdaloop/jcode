//! The compaction manager that is currently serving a live session.
//!
//! Fold tools mutate the manager that holds the session transcript, but they run
//! on a registry that is not always the one the agent tracks. Failover rebuilds
//! an agent on a fresh registry, and per-client registries keep their own
//! managers, so a tool that pins `registry.compaction()` can fold a manager no
//! agent ever fed. Agents publish their manager here every turn; tools resolve
//! it by session id.

use super::CompactionManager;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use tokio::sync::RwLock;

pub type SharedCompaction = Arc<RwLock<CompactionManager>>;

fn table() -> &'static Mutex<HashMap<String, Weak<RwLock<CompactionManager>>>> {
    static TABLE: OnceLock<Mutex<HashMap<String, Weak<RwLock<CompactionManager>>>>> =
        OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Sessions whose fold state changed since the last time it was persisted.
fn dirty() -> &'static Mutex<std::collections::HashSet<String>> {
    static DIRTY: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    DIRTY.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

fn lock() -> std::sync::MutexGuard<'static, HashMap<String, Weak<RwLock<CompactionManager>>>> {
    match table().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Record `manager` as the live manager for `session_id`.
pub fn publish(session_id: &str, manager: &SharedCompaction) {
    let mut entries = lock();
    // Drop entries whose agent is gone so a long-lived server does not grow.
    entries.retain(|_, weak| weak.strong_count() > 0);
    entries.insert(session_id.to_string(), Arc::downgrade(manager));
}

/// The live manager for `session_id`, if an agent published one.
pub fn resolve(session_id: &str) -> Option<SharedCompaction> {
    let entries = lock();
    entries.get(session_id).and_then(Weak::upgrade)
}

/// Note that `session_id` folded or unfolded context and owes a session write.
///
/// Fold state reaches the session file through the agent that owns it, and a
/// tool has no handle on that agent. The tool marks the session here and the
/// next turn persists it.
pub fn mark_dirty(session_id: &str) {
    match dirty().lock() {
        Ok(mut sessions) => {
            sessions.insert(session_id.to_string());
        }
        Err(poisoned) => {
            poisoned.into_inner().insert(session_id.to_string());
        }
    }
}

/// Clear the pending write for `session_id`, reporting whether one was owed.
pub fn take_dirty(session_id: &str) -> bool {
    match dirty().lock() {
        Ok(mut sessions) => sessions.remove(session_id),
        Err(poisoned) => poisoned.into_inner().remove(session_id),
    }
}
