use rustc_hash::FxHashSet;

/// Layer T: Tombstones
/// Hides files/folders from the backend that have been moved, overwritten, or otherwise replaced.
pub struct LayerT {
    pub tombstoned_inodes: FxHashSet<u64>,
}

impl LayerT {
    pub fn new() -> Self {
        Self {
            tombstoned_inodes: FxHashSet::default(),
        }
    }

    pub fn tombstone(&mut self, ino: u64) {
        self.tombstoned_inodes.insert(ino);
    }

    pub fn is_tombstoned(&self, ino: u64) -> bool {
        self.tombstoned_inodes.contains(&ino)
    }
}
