use rustc_hash::FxHashMap;
use std::sync::atomic::{AtomicU64, Ordering};

/// Layer C: Block Cache
/// Stores newly created blocks/chunks and their reference count.
pub struct LayerC {
    // Maps cache_id to CacheBlockMeta (which will eventually hold NVMe paths)
    pub blocks: FxHashMap<u64, CacheBlockMeta>,
    next_id: AtomicU64,
}

#[derive(Debug, Clone)]
pub struct CacheBlockMeta {
    pub ref_count: u64,
    pub length: u64,
}

impl LayerC {
    pub fn new() -> Self {
        Self {
            blocks: FxHashMap::default(),
            next_id: AtomicU64::new(1),
        }
    }

    pub fn allocate_block(&mut self, length: u64) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.blocks.insert(
            id,
            CacheBlockMeta {
                ref_count: 1,
                length,
            },
        );
        id
    }

    pub fn increment_ref(&mut self, cache_id: u64) {
        if let Some(meta) = self.blocks.get_mut(&cache_id) {
            meta.ref_count += 1;
        }
    }

    pub fn decrement_ref(&mut self, cache_id: u64) {
        let mut remove = false;
        if let Some(meta) = self.blocks.get_mut(&cache_id) {
            if meta.ref_count > 0 {
                meta.ref_count -= 1;
            }
            if meta.ref_count == 0 {
                remove = true;
            }
        }
        if remove {
            self.blocks.remove(&cache_id);
            // TODO: In the future, delete the corresponding file from NVMe here.
        }
    }
}
