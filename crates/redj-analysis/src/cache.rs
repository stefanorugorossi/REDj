//! Analysis cache: one JSON file per track, named by content hash.

use std::path::{Path, PathBuf};

use redj_core::TrackInfo;

use crate::ANALYZER_VERSION;

/// Directory of cached [`TrackInfo`] records.
#[derive(Debug, Clone)]
pub struct Cache {
    dir: PathBuf,
}

impl Cache {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn file(&self, hash: &str) -> PathBuf {
        self.dir.join(format!("{hash}.json"))
    }

    /// Returns the cached record if present and produced by the current
    /// analyzer version.
    pub fn load(&self, hash: &str) -> Option<TrackInfo> {
        let text = std::fs::read_to_string(self.file(hash)).ok()?;
        let info: TrackInfo = serde_json::from_str(&text).ok()?;
        (info.analyzer_version == ANALYZER_VERSION).then_some(info)
    }

    pub fn store(&self, hash: &str, info: &TrackInfo) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let tmp = self.dir.join(format!("{hash}.json.tmp"));
        std::fs::write(&tmp, serde_json::to_vec_pretty(info)?)?;
        std::fs::rename(tmp, self.file(hash))
    }
}
