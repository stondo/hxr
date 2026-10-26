//! Core data model shared by all harness adapters.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// One resumable session from any harness.
#[derive(Debug, Clone)]
pub struct Session {
    /// Harness-internal session identifier passed to the resume command.
    pub id: String,
    /// Harness name (adapter [`crate::adapter::Harness::name`]).
    pub harness: String,
    /// Working directory the session belongs to / should be resumed in.
    pub cwd: PathBuf,
    /// Display title (may be empty; UI falls back to placeholder).
    pub title: String,
    /// Last activity, unix epoch milliseconds.
    pub updated_ms: i64,
    /// Optional preview metadata (model, git branch, cost, ...) — read by the
    /// preview pane in a later phase.
    #[allow(dead_code)]
    pub extra: BTreeMap<String, String>,
}
