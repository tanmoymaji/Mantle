use rustc_hash::FxHashMap;
use std::time::SystemTime;

/// Layer S: Stat/Metadata Overrides
/// Tracks isolated metadata changes (timestamps, size, permissions, ownership) for backend files,
/// without requiring a full copy-up or inode change.
#[derive(Default, Clone)]
pub struct StatOverride {
    pub mtime: Option<SystemTime>,
    pub atime: Option<SystemTime>,
    pub ctime: Option<SystemTime>,
    pub size: Option<u64>,
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
}

pub struct LayerS {
    // Maps backend Inode to its metadata overrides
    pub overrides: FxHashMap<u64, StatOverride>,
}

impl LayerS {
    pub fn new() -> Self {
        Self {
            overrides: FxHashMap::default(),
        }
    }
}
