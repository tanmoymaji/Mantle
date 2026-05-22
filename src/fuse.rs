use crate::layer_m::LayerM;
use fuser::{Filesystem, ReplyAttr, ReplyDirectory, ReplyEntry, Request};
use libc::ENOENT;
use parking_lot::RwLock;
use std::ffi::OsStr;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

const TTL: Duration = Duration::from_secs(1);

pub struct MantleFS {
    layer_m: Arc<RwLock<LayerM>>,
}

impl MantleFS {
    pub fn new(layer_m: Arc<RwLock<LayerM>>) -> Self {
        MantleFS { layer_m }
    }

    fn ensure_stat_fetched(&self, ino: u64) {
        let (path, needs_fetch) = {
            let layer = self.layer_m.read();
            if let Some(meta) = layer.inodes.get(&ino) {
                if meta.stat_fetched {
                    return;
                }
                (layer.get_full_path(ino), true)
            } else {
                return; // Inode doesn't exist
            }
        };

        if needs_fetch {
            if let Ok(fs_meta) = std::fs::symlink_metadata(&path) {
                let mut layer = self.layer_m.write();
                if let Some(meta) = layer.inodes.get_mut(&ino) {
                    meta.size = fs_meta.len();
                    meta.mtime = fs_meta.modified().unwrap_or(UNIX_EPOCH);
                    meta.atime = fs_meta.accessed().unwrap_or(UNIX_EPOCH);
                    meta.ctime = fs_meta.created().unwrap_or(meta.mtime);
                    meta.stat_fetched = true;
                }
            }
        }
    }
}

impl Filesystem for MantleFS {
    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let name_str = name.to_string_lossy();

        let ino_opt = {
            let layer = self.layer_m.read();
            layer.lookup_ino(parent, &name_str)
        };

        if let Some(ino) = ino_opt {
            self.ensure_stat_fetched(ino);
            let layer = self.layer_m.read();
            if let Some(meta) = layer.get_metadata(ino) {
                reply.entry(&TTL, &meta.as_file_attr(), 0);
                return;
            }
        }
        reply.error(ENOENT);
    }

    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        self.ensure_stat_fetched(ino);
        let layer = self.layer_m.read();
        if let Some(meta) = layer.get_metadata(ino) {
            reply.attr(&TTL, &meta.as_file_attr());
        } else {
            reply.error(ENOENT);
        }
    }

    fn readdir(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        let layer = self.layer_m.read();
        if let Some(children) = layer.children.get(&ino) {
            if offset < 1 {
                if reply.add(ino, 1, fuser::FileType::Directory, ".") {
                    reply.ok();
                    return;
                }
            }
            if offset < 2 {
                let parent = layer.get_metadata(ino).map(|m| m.parent).unwrap_or(1);
                if reply.add(parent, 2, fuser::FileType::Directory, "..") {
                    reply.ok();
                    return;
                }
            }

            for (i, &child_ino) in children.iter().enumerate() {
                let entry_offset = (i + 3) as i64;
                if entry_offset <= offset {
                    continue; // Skip already returned entries
                }

                if let Some(child_meta) = layer.get_metadata(child_ino) {
                    if reply.add(child_ino, entry_offset, child_meta.kind, &child_meta.name) {
                        break; // Buffer is full, kernel will call again with this offset
                    }
                }
            }
        }
        reply.ok();
    }
}
