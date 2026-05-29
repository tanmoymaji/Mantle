use crate::layers::extent::Extent;
use fuser::FileType;
use rustc_hash::FxHashMap;
use std::time::SystemTime;

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};

pub struct NewInodeMeta {
    pub name: OsString,
    pub parent: u64,
    pub kind: FileType,
    pub size: u64,
    pub mtime: SystemTime,
    pub ctime: SystemTime,
    pub atime: SystemTime,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
}

/// Layer F: New Files/Folders
/// Tracks completely new files and directories that do not exist in the backend.
pub struct LayerF {
    pub inodes: FxHashMap<u64, NewInodeMeta>,
    pub children: FxHashMap<u64, BTreeMap<i64, u64>>,
    pub name_index: FxHashMap<u64, FxHashMap<OsString, u64>>,
    pub file_extents: FxHashMap<u64, Vec<Extent>>,
    next_ino: u64,
    next_offset: i64,
}

impl LayerF {
    pub fn new(start_ino: u64) -> Self {
        Self {
            inodes: FxHashMap::default(),
            children: FxHashMap::default(),
            name_index: FxHashMap::default(),
            file_extents: FxHashMap::default(),
            next_ino: start_ino,
            next_offset: 1_000_000_000_000_000_000,
        }
    }

    pub fn allocate_inode(&mut self) -> u64 {
        let ino = self.next_ino;
        self.next_ino += 1;
        ino
    }

    pub fn add_child(&mut self, parent: u64, child: u64, name: OsString) {
        let offset = self.next_offset;
        self.next_offset += 1;
        self.children
            .entry(parent)
            .or_default()
            .insert(offset, child);

        self.name_index
            .entry(parent)
            .or_default()
            .insert(name, child);
    }

    pub fn remove_child(&mut self, parent: u64, child: u64, name: &OsStr) {
        let mut remove_from_children = false;
        if let Some(names) = self.name_index.get_mut(&parent) {
            if names.get(name) == Some(&child) {
                names.remove(name);
                remove_from_children = true;
            }
        }

        if remove_from_children {
            if let Some(children) = self.children.get_mut(&parent) {
                let mut key_to_remove = None;
                for (&offset, &ino) in children.iter() {
                    if ino == child {
                        key_to_remove = Some(offset);
                        break;
                    }
                }
                if let Some(key) = key_to_remove {
                    children.remove(&key);
                }
            }
        }
    }
}
