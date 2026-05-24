use rustc_hash::FxHashSet;

/// Layer A: Backend Deletions
/// Tracks Inodes from the backend that have been marked for deletion.
pub struct LayerA {
    pub deleted_inodes: FxHashSet<u64>,
}

impl LayerA {
    pub fn new() -> Self {
        Self {
            deleted_inodes: FxHashSet::default(),
        }
    }

    pub fn mark_deleted(&mut self, ino: u64) {
        self.deleted_inodes.insert(ino);
    }

    pub fn is_deleted(&self, ino: u64) -> bool {
        self.deleted_inodes.contains(&ino)
    }
}
