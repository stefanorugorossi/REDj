use std::collections::BTreeMap;

use crate::{TrackId, TrackInfo};

/// The analyzed tracks available to a session, by id.
#[derive(Debug, Clone, Default)]
pub struct Library {
    tracks: BTreeMap<TrackId, TrackInfo>,
}

impl Library {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, info: TrackInfo) {
        self.tracks.insert(info.id.clone(), info);
    }

    pub fn get(&self, id: &TrackId) -> Option<&TrackInfo> {
        self.tracks.get(id)
    }

    /// Looks a track up by full id or by a unique id prefix (at least 4
    /// characters), which is convenient for humans writing plans.
    pub fn resolve(&self, id: &str) -> Option<&TrackInfo> {
        if let Some(t) = self.tracks.get(&TrackId(id.to_owned())) {
            return Some(t);
        }
        if id.len() < 4 {
            return None;
        }
        let mut it = self.tracks.values().filter(|t| t.id.0.starts_with(id));
        match (it.next(), it.next()) {
            (Some(t), None) => Some(t),
            _ => None,
        }
    }

    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &TrackInfo> {
        self.tracks.values()
    }
}

impl FromIterator<TrackInfo> for Library {
    fn from_iter<I: IntoIterator<Item = TrackInfo>>(iter: I) -> Self {
        let mut lib = Library::new();
        for t in iter {
            lib.insert(t);
        }
        lib
    }
}
