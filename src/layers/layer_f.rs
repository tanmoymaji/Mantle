use crate::layers::extent::Extent;
use fuser::FileType;
use rustc_hash::FxHashMap;
use std::time::SystemTime;

pub struct NewInodeMeta {
    pub name: String,
    pub parent: u64,
    pub kind: FileType,
    pub size: u64,
    pub mtime: SystemTime,
    pub ctime: SystemTime,
    pub atime: SystemTime,
}

/// Layer F: New Files/Folders
/// Tracks completely new files and directories that do not exist in the backend.
pub struct LayerF {
    pub inodes: FxHashMap<u64, NewInodeMeta>,
    pub children: FxHashMap<u64, Vec<u64>>,
    pub file_extents: FxHashMap<u64, Vec<Extent>>,
    next_ino: u64,
}

impl LayerF {
    pub fn new(start_ino: u64) -> Self {
        Self {
            inodes: FxHashMap::default(),
            children: FxHashMap::default(),
            file_extents: FxHashMap::default(),
            next_ino: start_ino,
        }
    }

    pub fn allocate_inode(&mut self) -> u64 {
        let ino = self.next_ino;
        self.next_ino += 1;
        ino
    }
}
