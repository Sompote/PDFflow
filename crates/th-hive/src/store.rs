//! Session persistence (`ctx.sessions`): one JSONL file per session under a root folder,
//! or memory only when no folder is set.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use crate::events::Emit;
use crate::session::{now_ms, Appended, Session, SessionMeta};

pub struct SessionStore {
    root: RwLock<Option<PathBuf>>,
    /// `session/event`: every appended event of every session.
    pub bus: Arc<Emit<Appended>>,
    counter: AtomicU64,
}

impl SessionStore {
    pub fn new(root: Option<PathBuf>) -> SessionStore {
        SessionStore { root: RwLock::new(root), bus: Arc::new(Emit::new("session/event")), counter: AtomicU64::new(1) }
    }

    pub fn root(&self) -> Option<PathBuf> {
        self.root.read().ok().and_then(|r| r.clone())
    }

    /// Moves new sessions to another folder (the application switched project).
    pub fn set_root(&self, root: Option<PathBuf>) {
        if let Ok(mut r) = self.root.write() {
            *r = root;
        }
    }

    /// A fresh id: `<prefix>-<time>-<n>`.
    pub fn new_id(&self, prefix: &str) -> String {
        format!("{prefix}-{}-{}", now_ms(), self.counter.fetch_add(1, Ordering::Relaxed))
    }

    pub fn path_of(&self, id: &str) -> Option<PathBuf> {
        self.root().map(|r| r.join(format!("{id}.jsonl")))
    }

    pub fn create(&self, meta: SessionMeta) -> Result<Arc<Session>, String> {
        match self.path_of(&meta.id) {
            Some(p) => Session::create(&p, meta, self.bus.clone()),
            None => Ok(Session::memory(meta, self.bus.clone())),
        }
    }

    pub fn open(&self, id: &str) -> Result<Arc<Session>, String> {
        let p = self.path_of(id).ok_or("sessions are kept in memory only")?;
        Session::open(&p, self.bus.clone())
    }

    pub fn open_path(&self, p: &Path) -> Result<Arc<Session>, String> {
        Session::open(p, self.bus.clone())
    }

    /// Session ids on disk, newest first.
    pub fn list(&self) -> Vec<String> {
        let Some(r) = self.root() else { return vec![] };
        let mut v: Vec<(std::time::SystemTime, String)> = std::fs::read_dir(r)
            .map(|rd| {
                rd.flatten()
                    .filter_map(|e| {
                        let n = e.file_name().to_string_lossy().to_string();
                        let id = n.strip_suffix(".jsonl")?.to_string();
                        Some((e.metadata().and_then(|m| m.modified()).ok()?, id))
                    })
                    .collect()
            })
            .unwrap_or_default();
        v.sort_by(|a, b| b.0.cmp(&a.0));
        v.into_iter().map(|(_, id)| id).collect()
    }
}
