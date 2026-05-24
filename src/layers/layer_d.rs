use crate::layers::extent::Extent;
use rustc_hash::FxHashMap;

/// Layer D: Dependency Graph
/// Tracks backend files that have been modified (overwritten, appended, or partially copied).
pub struct LayerD {
    // Maps backend Inode to its updated list of extents
    pub modified_extents: FxHashMap<u64, Vec<Extent>>,
}

impl LayerD {
    pub fn new() -> Self {
        Self {
            modified_extents: FxHashMap::default(),
        }
    }
}
