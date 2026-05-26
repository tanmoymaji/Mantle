use crate::layers::{LayerM, MantleOverlay};
use fuser::{FileAttr, FileType, Filesystem, ReplyAttr, ReplyDirectory, ReplyEntry, Request};
use libc::ENOENT;
use parking_lot::RwLock;
use std::ffi::OsStr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const TTL: Duration = Duration::from_secs(1);

fn create_file_attr(
    ino: u64,
    size: u64,
    kind: FileType,
    mtime: SystemTime,
    ctime: SystemTime,
    atime: SystemTime,
) -> FileAttr {
    FileAttr {
        ino,
        size,
        blocks: (size + 511) / 512,
        atime,
        mtime,
        ctime,
        crtime: ctime,
        kind,
        perm: if kind == FileType::Directory {
            0o755
        } else {
            0o644
        },
        nlink: if kind == FileType::Directory { 2 } else { 1 },
        uid: 1000,
        gid: 1000,
        rdev: 0,
        blksize: 4096,
        flags: 0,
    }
}

pub struct MantleFS {
    layer_m: Arc<RwLock<LayerM>>,
    overlay: Arc<MantleOverlay>,
}

impl MantleFS {
    pub fn new(layer_m: Arc<RwLock<LayerM>>, overlay: Arc<MantleOverlay>) -> Self {
        MantleFS { layer_m, overlay }
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

        // Priority 1: Newly created or modified files in the overlay (Layer F)
        // must take precedence over backend files.
        {
            let layer_f = self.overlay.layer_f.read();
            if let Some(children) = layer_f.children.get(&parent) {
                for (_, &child_ino) in children.iter() {
                    if let Some(meta) = layer_f.inodes.get(&child_ino) {
                        if meta.name == name_str {
                            let attr = create_file_attr(
                                child_ino, meta.size, meta.kind, meta.mtime, meta.ctime, meta.atime,
                            );
                            reply.entry(&TTL, &attr, 0);
                            return;
                        }
                    }
                }
            }
        }

        // Priority 2: If it's not a newly created file, fall back to the backend drive (Layer M).
        let ino_opt = {
            let layer = self.layer_m.read();
            layer.lookup_ino(parent, &name_str)
        };

        if let Some(ino) = ino_opt {
            // Hide backend inodes that have been marked for deletion or overwritten
            if self.overlay.layer_t.read().is_tombstoned(ino)
                || self.overlay.layer_a.read().is_deleted(ino)
            {
                reply.error(ENOENT);
                return;
            }

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
        // Overlay properties take precedence over backend properties.
        {
            let layer_f = self.overlay.layer_f.read();
            if let Some(meta) = layer_f.inodes.get(&ino) {
                let attr = create_file_attr(
                    ino, meta.size, meta.kind, meta.mtime, meta.ctime, meta.atime,
                );
                reply.attr(&TTL, &attr);
                return;
            }
        }

        // Hide backend inodes that have been marked for deletion or overwritten
        if self.overlay.layer_t.read().is_tombstoned(ino)
            || self.overlay.layer_a.read().is_deleted(ino)
        {
            reply.error(ENOENT);
            return;
        }

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
        // Backend (Layer M) offsets are always < 1 Quintillion.
        // New files (Layer F) are assigned offsets starting at 1 Quintillion.
        if offset < 1_000_000_000_000_000_000 {
            let layer = self.layer_m.read();
            if let Some(children) = layer.children.get(&ino) {
                if offset < 1 {
                    if reply.add(ino, 1, FileType::Directory, ".") {
                        reply.ok();
                        return;
                    }
                }
                if offset < 2 {
                    let parent = layer.get_metadata(ino).map(|m| m.parent).unwrap_or(1);
                    if reply.add(parent, 2, FileType::Directory, "..") {
                        reply.ok();
                        return;
                    }
                }

                for (i, &child_ino) in children.iter().enumerate() {
                    let entry_offset = (i + 3) as i64;
                    if entry_offset <= offset {
                        continue;
                    }

                    if self.overlay.layer_t.read().is_tombstoned(child_ino)
                        || self.overlay.layer_a.read().is_deleted(child_ino)
                    {
                        continue;
                    }

                    if let Some(child_meta) = layer.get_metadata(child_ino) {
                        if reply.add(child_ino, entry_offset, child_meta.kind, &child_meta.name) {
                            reply.ok();
                            return;
                        }
                    }
                }
            } else {
                if offset < 1 {
                    if reply.add(ino, 1, FileType::Directory, ".") {
                        reply.ok();
                        return;
                    }
                }
                if offset < 2 {
                    let parent = self
                        .overlay
                        .layer_f
                        .read()
                        .inodes
                        .get(&ino)
                        .map(|m| m.parent)
                        .unwrap_or(1);
                    if reply.add(parent, 2, FileType::Directory, "..") {
                        reply.ok();
                        return;
                    }
                }
            }
        }

        {
            let layer_f = self.overlay.layer_f.read();
            if let Some(children) = layer_f.children.get(&ino) {
                for (&child_offset, &child_ino) in children.range(offset..) {
                    if let Some(child_meta) = layer_f.inodes.get(&child_ino) {
                        // Pass child_offset + 1 so the OS knows to start AFTER this element next time
                        if reply.add(
                            child_ino,
                            child_offset + 1,
                            child_meta.kind,
                            &child_meta.name,
                        ) {
                            reply.ok();
                            return;
                        }
                    }
                }
            }
        }

        reply.ok();
    }

    fn mkdir(
        &mut self,
        _req: &Request,
        parent: u64,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        let mut layer_f = self.overlay.layer_f.write();
        let ino = layer_f.allocate_inode();
        let name_str = name.to_string_lossy().to_string();

        let now = SystemTime::now();
        let meta = crate::layers::layer_f::NewInodeMeta {
            name: name_str,
            parent,
            kind: FileType::Directory,
            size: 4096,
            mtime: now,
            ctime: now,
            atime: now,
        };

        layer_f.inodes.insert(ino, meta);
        layer_f.add_child(parent, ino);

        let attr = create_file_attr(ino, 4096, FileType::Directory, now, now, now);
        reply.entry(&TTL, &attr, 0);
    }

    fn create(
        &mut self,
        _req: &Request,
        parent: u64,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        _flags: i32,
        reply: fuser::ReplyCreate,
    ) {
        let mut layer_f = self.overlay.layer_f.write();
        let ino = layer_f.allocate_inode();
        let name_str = name.to_string_lossy().to_string();

        let now = SystemTime::now();
        let meta = crate::layers::layer_f::NewInodeMeta {
            name: name_str,
            parent,
            kind: FileType::RegularFile,
            size: 0,
            mtime: now,
            ctime: now,
            atime: now,
        };

        layer_f.inodes.insert(ino, meta);
        layer_f.add_child(parent, ino);

        let attr = create_file_attr(ino, 0, FileType::RegularFile, now, now, now);
        reply.created(&TTL, &attr, 0, 0, 0);
    }

    fn unlink(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: fuser::ReplyEmpty) {
        let name_str = name.to_string_lossy();

        // Find the inode
        let mut target_ino = None;
        let mut is_layer_f = false;

        // Search Layer F first
        {
            let layer_f = self.overlay.layer_f.read();
            if let Some(children) = layer_f.children.get(&parent) {
                for (_, &child_ino) in children.iter() {
                    if let Some(meta) = layer_f.inodes.get(&child_ino) {
                        if meta.name == name_str && meta.kind == FileType::RegularFile {
                            target_ino = Some(child_ino);
                            is_layer_f = true;
                            break;
                        }
                    }
                }
            }
        }

        // Search Layer M if not found
        if target_ino.is_none() {
            let layer_m = self.layer_m.read();
            if let Some(ino) = layer_m.lookup_ino(parent, &name_str) {
                if let Some(meta) = layer_m.get_metadata(ino) {
                    if meta.kind == FileType::RegularFile {
                        target_ino = Some(ino);
                    }
                }
            }
        }

        if let Some(ino) = target_ino {
            if is_layer_f {
                let mut layer_f = self.overlay.layer_f.write();
                layer_f.inodes.remove(&ino);
                layer_f.remove_child(parent, ino);
                // Extent cleanup would happen here
            } else {
                let mut layer_a = self.overlay.layer_a.write();
                layer_a.mark_deleted(ino);
            }
            reply.ok();
        } else {
            reply.error(ENOENT);
        }
    }

    fn rmdir(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: fuser::ReplyEmpty) {
        let name_str = name.to_string_lossy();

        let mut target_ino = None;
        let mut is_layer_f = false;

        // Search Layer F
        {
            let layer_f = self.overlay.layer_f.read();
            if let Some(children) = layer_f.children.get(&parent) {
                for (_, &child_ino) in children.iter() {
                    if let Some(meta) = layer_f.inodes.get(&child_ino) {
                        if meta.name == name_str && meta.kind == FileType::Directory {
                            target_ino = Some(child_ino);
                            is_layer_f = true;
                            break;
                        }
                    }
                }
            }
        }

        // Search Layer M
        if target_ino.is_none() {
            let layer_m = self.layer_m.read();
            if let Some(ino) = layer_m.lookup_ino(parent, &name_str) {
                if let Some(meta) = layer_m.get_metadata(ino) {
                    if meta.kind == FileType::Directory {
                        target_ino = Some(ino);
                    }
                }
            }
        }

        if let Some(ino) = target_ino {
            // Need to verify it's empty
            let mut is_empty = true;

            // Check Layer F children
            if let Some(children) = self.overlay.layer_f.read().children.get(&ino) {
                if !children.is_empty() {
                    is_empty = false;
                }
            }

            // Check Layer M children
            if is_empty && !is_layer_f {
                if let Some(children) = self.layer_m.read().children.get(&ino) {
                    for &child_ino in children {
                        // If it's not deleted or tombstoned, the dir is not empty
                        if !self.overlay.layer_a.read().is_deleted(child_ino)
                            && !self.overlay.layer_t.read().is_tombstoned(child_ino)
                        {
                            is_empty = false;
                            break;
                        }
                    }
                }
            }

            if !is_empty {
                reply.error(libc::ENOTEMPTY);
                return;
            }

            if is_layer_f {
                let mut layer_f = self.overlay.layer_f.write();
                layer_f.inodes.remove(&ino);
                layer_f.remove_child(parent, ino);
            } else {
                let mut layer_a = self.overlay.layer_a.write();
                layer_a.mark_deleted(ino);
            }
            reply.ok();
        } else {
            reply.error(ENOENT);
        }
    }
}
