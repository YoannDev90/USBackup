use std::sync::OnceLock;
use tokio::sync::{Mutex, MutexGuard};

/// Global lock serializing every interactive TTY session (dialoguer
/// prompts and the configuration wizard). Two USB keys plugged at once
/// would otherwise interleave prompts on the same terminal.
///
/// Uses a tokio Mutex so the guard can be held across `.await` points
/// (the wizard) while keeping the future `Send` for `tokio::spawn`.
///
/// Acquire it at the top-level entry point; callees (e.g. the wizard)
/// run under the caller's guard and must NOT lock again (deadlock).
/// From a `spawn_blocking` thread use
/// `Handle::current().block_on(tui_lock::lock())`.
pub async fn lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().await
}
