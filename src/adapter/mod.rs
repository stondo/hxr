//! Harness adapters: one implementation per coding-agent CLI.
//!
//! An adapter is a read-only view over one harness session store plus the
//! knowledge of how to resume one of its sessions. Adapters must be failure
//! isolated: a broken store yields an error (or zero sessions) and never
//! affects the other adapters.

pub mod claude;
pub mod codex;
#[cfg(test)]
mod fixtures_test;
pub mod kimi;
pub mod omp;
pub mod opencode;
pub mod pi;
pub mod qwen;
pub mod reasonix;
pub mod util;

use anyhow::{Context, Result};
use std::path::PathBuf;

use crate::model::Session;

pub trait Harness: Send + Sync {
    /// Stable lowercase harness name, also used as the tab name.
    fn name(&self) -> &'static str;

    /// The CLI binary this adapter resumes through (used for detection).
    fn binary(&self) -> &'static str;

    /// Read-only scan of the harness session store. Individual bad rows are
    /// skipped by the adapter; an `Err` means the whole store is unreadable.
    fn discover(&self) -> Result<Vec<Session>>;

    /// Build the resume command (argv including the binary, plus the working
    /// directory to run it in).
    fn resume(&self, session: &Session, extra: &[String]) -> (Vec<String>, PathBuf);
}

/// Adapter registry in tab order.
pub fn registry() -> Vec<Box<dyn Harness>> {
    vec![
        Box::new(opencode::Opencode),
        Box::new(claude::Claude),
        Box::new(codex::Codex),
        Box::new(pi::Pi),
        Box::new(omp::Omp),
        Box::new(qwen::Qwen),
        Box::new(kimi::Kimi),
        Box::new(reasonix::Reasonix),
    ]
}

/// Root home for harness stores: `$HXR_HOME` (tests / sandboxes) or `$HOME`.
pub fn home_dir() -> Result<std::path::PathBuf> {
    std::env::var_os("HXR_HOME")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
        .context("hxr: neither HXR_HOME nor HOME is set")
}

/// True if `bin` resolves to an executable file on PATH.
pub fn on_path(bin: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| {
        let f = dir.join(bin);
        std::fs::metadata(&f)
            .map(|m| m.is_file() && (m.permissions().mode() & 0o111) != 0)
            .unwrap_or(false)
    })
}
