use crate::layers::{LayerM, MantleOverlay};
use fuser::{FileAttr, FileType, Filesystem, ReplyAttr, ReplyDirectory, ReplyEntry, Request};
use libc::ENOENT;
use parking_lot::RwLock;
use std::ffi::OsStr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::os::unix::fs::MetadataExt;

const TTL: Duration = Duration::from_secs(1);

fn create_file_attr(
    ino: u64,
    size: u64,
    kind: FileType,
    mtime: SystemTime,
    ctime: SystemTime,
    atime: SystemTime,
    mode: u16,
    uid: u32,
    gid: u32,
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
        perm: mode,
        nlink: if kind == FileType::Directory { 2 } else { 1 },
        uid,
        gid,
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
                    meta.ctime = UNIX_EPOCH + std::time::Duration::new(fs_meta.ctime().max(0) as u64, fs_meta.ctime_nsec().clamp(0, 999_999_999) as u32);
                    meta.mode = fs_meta.mode();
                    meta.uid = fs_meta.uid();
                    meta.gid = fs_meta.gid();
                    meta.stat_fetched = true;
                }
            }
        }
    }

    fn update_parent_times(&self, parent_ino: u64) {
        let now = SystemTime::now();
        let mut in_layer_f = false;

        {
            let mut layer_f = self.overlay.layer_f.write();
            if let Some(meta) = layer_f.inodes.get_mut(&parent_ino) {
                meta.mtime = now;
                meta.ctime = now;
                in_layer_f = true;
            }
        }

        if !in_layer_f {
            let mut layer_s = self.overlay.layer_s.write();
            let entry = layer_s
                .overrides
                .entry(parent_ino)
                .or_insert_with(|| crate::layers::layer_s::StatOverride::default());
            entry.mtime = Some(now);
            entry.ctime = Some(now);
        }
    }

    fn resolve_backend_parent(&self, ino: u64) -> u64 {
        let layer_d = self.overlay.layer_d.read();
        layer_d.directory_redirects.get(&ino).copied().unwrap_or(ino)
    }

    pub fn get_merged_backend_attr(&self, ino: u64) -> Option<fuser::FileAttr> {
        let meta = {
            let layer_m = self.layer_m.read();
            layer_m.get_metadata(ino).cloned()
        };

        if let Some(meta) = meta {
            let mut final_size = meta.size;
            let mut final_mtime = meta.mtime;
            let mut final_ctime = meta.ctime;
            let mut final_atime = meta.atime;
            let mut final_mode = (meta.mode & 0o7777) as u16;
            let mut final_uid = meta.uid;
            let mut final_gid = meta.gid;

            {
                let layer_s = self.overlay.layer_s.read();
                if let Some(override_stat) = layer_s.overrides.get(&ino) {
                    if let Some(s) = override_stat.size {
                        final_size = s;
                    }
                    if let Some(m) = override_stat.mtime {
                        final_mtime = m;
                    }
                    if let Some(c) = override_stat.ctime {
                        final_ctime = c;
                    }
                    if let Some(a) = override_stat.atime {
                        final_atime = a;
                    }
                    if let Some(m) = override_stat.mode {
                        final_mode = m as u16;
                    }
                    if let Some(u) = override_stat.uid {
                        final_uid = u;
                    }
                    if let Some(g) = override_stat.gid {
                        final_gid = g;
                    }
                }
            }

            Some(create_file_attr(
                ino,
                final_size,
                meta.kind,
                final_mtime,
                final_ctime,
                final_atime,
                final_mode,
                final_uid,
                final_gid,
            ))
        } else {
            None
        }
    }
}

impl Filesystem for MantleFS {
    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        // Priority 1: Newly created or modified files in the overlay (Layer F)
        // must take precedence over backend files.
        {
            let layer_f = self.overlay.layer_f.read();
            if let Some(&child_ino) = layer_f.name_index.get(&parent).and_then(|m| m.get(name)) {
                if let Some(meta) = layer_f.inodes.get(&child_ino) {
                    let attr = create_file_attr(
                        child_ino, meta.size, meta.kind, meta.mtime, meta.ctime, meta.atime,
                        meta.mode as u16, meta.uid, meta.gid,
                    );
                    reply.entry(&TTL, &attr, 0);
                    return;
                }
            }
        }

        let parent_for_layer_m = self.resolve_backend_parent(parent);

        // Priority 2: If it's not a newly created file, fall back to the backend drive (Layer M).
        let ino_opt = {
            let layer = self.layer_m.read();
            layer.lookup_ino(parent_for_layer_m, name)
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
            if let Some(attr) = self.get_merged_backend_attr(ino) {
                reply.entry(&TTL, &attr, 0);
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
                    meta.mode as u16, meta.uid, meta.gid,
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
        if let Some(attr) = self.get_merged_backend_attr(ino) {
            reply.attr(&TTL, &attr);
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
        let redirected_ino = {
            let layer_d = self.overlay.layer_d.read();
            layer_d.directory_redirects.get(&ino).copied()
        };
        let ino_for_layer_m = redirected_ino.unwrap_or(ino);

        let layer_f_parent = if redirected_ino.is_some() {
            self.overlay
                .layer_f
                .read()
                .inodes
                .get(&ino)
                .map(|m| m.parent)
        } else {
            None
        };

        // Backend (Layer M) offsets are always < 1 Quintillion.
        // New files (Layer F) are assigned offsets starting at 1 Quintillion.
        if offset < 1_000_000_000_000_000_000 {
            let layer = self.layer_m.read();
            if let Some(children) = layer.children.get(&ino_for_layer_m) {
                if offset < 1 {
                    if reply.add(ino, 1, FileType::Directory, ".") {
                        reply.ok();
                        return;
                    }
                }
                if offset < 2 {
                    let parent = layer_f_parent.unwrap_or_else(|| {
                        layer
                            .get_metadata(ino_for_layer_m)
                            .map(|m| m.parent)
                            .unwrap_or(1)
                    });
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
        req: &Request,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        reply: ReplyEntry,
    ) {
        let mut layer_f = self.overlay.layer_f.write();
        let ino = layer_f.allocate_inode();

        let now = SystemTime::now();
        let meta = crate::layers::layer_f::NewInodeMeta {
            name: name.to_os_string(),
            parent,
            kind: FileType::Directory,
            size: 4096,
            mtime: now,
            ctime: now,
            atime: now,
            mode: (mode & !umask) & 0o7777,
            uid: req.uid(),
            gid: req.gid(),
        };

        layer_f.inodes.insert(ino, meta);
        layer_f.add_child(parent, ino, name.to_os_string());

        let attr = create_file_attr(ino, 4096, FileType::Directory, now, now, now, ((mode & !umask) & 0o7777) as u16, req.uid(), req.gid());

        drop(layer_f); // Prevent deadlock with update_parent_times

        self.update_parent_times(parent);

        reply.entry(&TTL, &attr, 0);
    }

    fn create(
        &mut self,
        req: &Request,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        _flags: i32,
        reply: fuser::ReplyCreate,
    ) {
        let mut layer_f = self.overlay.layer_f.write();
        let ino = layer_f.allocate_inode();

        let now = SystemTime::now();
        let meta = crate::layers::layer_f::NewInodeMeta {
            name: name.to_os_string(),
            parent,
            kind: FileType::RegularFile,
            size: 0,
            mtime: now,
            ctime: now,
            atime: now,
            mode: (mode & !umask) & 0o7777,
            uid: req.uid(),
            gid: req.gid(),
        };

        layer_f.inodes.insert(ino, meta);
        layer_f.add_child(parent, ino, name.to_os_string());

        let attr = create_file_attr(ino, 0, FileType::RegularFile, now, now, now, ((mode & !umask) & 0o7777) as u16, req.uid(), req.gid());

        drop(layer_f); // Prevent deadlock with update_parent_times

        self.update_parent_times(parent);

        reply.created(&TTL, &attr, 0, 0, 0);
    }

    fn unlink(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: fuser::ReplyEmpty) {
        // Find the inode
        let mut target_ino = None;
        let mut is_layer_f = false;

        // Search Layer F first
        {
            let layer_f = self.overlay.layer_f.read();
            if let Some(&child_ino) = layer_f.name_index.get(&parent).and_then(|m| m.get(name)) {
                if let Some(meta) = layer_f.inodes.get(&child_ino) {
                    if meta.kind == FileType::RegularFile {
                        target_ino = Some(child_ino);
                        is_layer_f = true;
                    }
                }
            }
        }

        // Search Layer M if not found
        if target_ino.is_none() {
            let parent_for_layer_m = self.resolve_backend_parent(parent);

            let layer_m = self.layer_m.read();
            if let Some(ino) = layer_m.lookup_ino(parent_for_layer_m, name) {
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
                layer_f.remove_child(parent, ino, name);
                layer_f.inodes.remove(&ino);
                // Extent cleanup would happen here
            } else {
                let mut layer_a = self.overlay.layer_a.write();
                layer_a.mark_deleted(ino);
            }

            self.update_parent_times(parent);

            reply.ok();
        } else {
            reply.error(ENOENT);
        }
    }

    fn rmdir(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: fuser::ReplyEmpty) {

        let mut target_ino = None;
        let mut is_layer_f = false;

        // Search Layer F
        {
            let layer_f = self.overlay.layer_f.read();
            if let Some(&child_ino) = layer_f.name_index.get(&parent).and_then(|m| m.get(name)) {
                if let Some(meta) = layer_f.inodes.get(&child_ino) {
                    if meta.kind == FileType::Directory {
                        target_ino = Some(child_ino);
                        is_layer_f = true;
                    }
                }
            }
        }

        // Search Layer M
        if target_ino.is_none() {
            let parent_for_layer_m = self.resolve_backend_parent(parent);

            let layer_m = self.layer_m.read();
            if let Some(ino) = layer_m.lookup_ino(parent_for_layer_m, name) {
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
                layer_f.remove_child(parent, ino, name);
                layer_f.inodes.remove(&ino);
            } else {
                let mut layer_a = self.overlay.layer_a.write();
                layer_a.mark_deleted(ino);
            }

            self.update_parent_times(parent);

            reply.ok();
        } else {
            reply.error(ENOENT);
        }
    }

    fn setattr(
        &mut self,
        _req: &Request,
        ino: u64,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<fuser::TimeOrNow>,
        mtime: Option<fuser::TimeOrNow>,
        ctime: Option<SystemTime>,
        _fh: Option<u64>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        let mut attr = None;

        // Priority 1: Check if the inode exists in Layer F (newly created or fully copied files).
        {
            let mut layer_f = self.overlay.layer_f.write();
            if let Some(meta) = layer_f.inodes.get_mut(&ino) {
                if let Some(t) = atime {
                    meta.atime = match t {
                        fuser::TimeOrNow::SpecificTime(st) => st,
                        fuser::TimeOrNow::Now => SystemTime::now(),
                    };
                }
                if let Some(t) = mtime {
                    meta.mtime = match t {
                        fuser::TimeOrNow::SpecificTime(st) => st,
                        fuser::TimeOrNow::Now => SystemTime::now(),
                    };
                }
                if let Some(c) = ctime {
                    meta.ctime = c;
                }
                if let Some(s) = size {
                    meta.size = s;
                    let sys_now = SystemTime::now();
                    if mtime.is_none() { meta.mtime = sys_now; }
                    if ctime.is_none() { meta.ctime = sys_now; }
                }
                if let Some(m) = mode {
                    meta.mode = m & 0o7777;
                }
                if let Some(u) = uid {
                    meta.uid = u;
                }
                if let Some(g) = gid {
                    meta.gid = g;
                }

                attr = Some(create_file_attr(
                    ino, meta.size, meta.kind, meta.mtime, meta.ctime, meta.atime,
                    meta.mode as u16, meta.uid, meta.gid,
                ));
            }
        } // layer_f lock is dropped here

        // Invalidate extents if the file is truncated to 0 bytes, 
        // to prevent stale data blocks from leaking into future appends.
        if size == Some(0) {
            let layer_f = self.overlay.layer_f.read();
            if layer_f.inodes.contains_key(&ino) {
                // If it's in layer_f, remove its extents. We can safely drop read and grab write.
                drop(layer_f);
                self.overlay.layer_f.write().file_extents.remove(&ino);
            } else {
                drop(layer_f);
                // For layer_m files backed by Layer S overrides, clear modified extents.
                self.overlay.layer_d.write().modified_extents.remove(&ino);
            }
        }

        if let Some(a) = attr {
            reply.attr(&TTL, &a);
            return;
        }

        if self.overlay.layer_t.read().is_tombstoned(ino)
            || self.overlay.layer_a.read().is_deleted(ino)
        {
            reply.error(ENOENT);
            return;
        }

        // Fetch real backend metadata to ensure we're modifying the true size/permissions
        self.ensure_stat_fetched(ino);

        // Priority 2: If the inode is in Layer M (backend), record the changes in Layer S (Overrides).
        let layer = self.layer_m.read();
        if layer.get_metadata(ino).is_some() {
            drop(layer);

            {
                let mut layer_s = self.overlay.layer_s.write();
                let entry = layer_s
                    .overrides
                    .entry(ino)
                    .or_insert_with(|| crate::layers::layer_s::StatOverride::default());

                if let Some(t) = atime {
                    entry.atime = Some(match t {
                        fuser::TimeOrNow::SpecificTime(st) => st,
                        fuser::TimeOrNow::Now => SystemTime::now(),
                    });
                }
                if let Some(t) = mtime {
                    entry.mtime = Some(match t {
                        fuser::TimeOrNow::SpecificTime(st) => st,
                        fuser::TimeOrNow::Now => SystemTime::now(),
                    });
                }
                if let Some(c) = ctime {
                    entry.ctime = Some(c);
                }
                if let Some(s) = size {
                    entry.size = Some(s);
                    let sys_now = SystemTime::now();
                    if mtime.is_none() { entry.mtime = Some(sys_now); }
                    if ctime.is_none() { entry.ctime = Some(sys_now); }
                }
                if let Some(m) = mode {
                    entry.mode = Some(m & 0o7777);
                }
                if let Some(u) = uid {
                    entry.uid = Some(u);
                }
                if let Some(g) = gid {
                    entry.gid = Some(g);
                }
            } // Drop layer_s lock

            if size == Some(0) {
                // Truncating a backend file directly via Layer S.
                // We must clear its modified extents in Layer D.
                self.overlay.layer_d.write().modified_extents.remove(&ino);
            }

            if let Some(attr) = self.get_merged_backend_attr(ino) {
                reply.attr(&TTL, &attr);
            } else {
                reply.error(ENOENT);
            }
        } else {
            reply.error(ENOENT);
        }
    }

    fn rename(
        &mut self,
        _req: &Request,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        flags: u32,
        reply: fuser::ReplyEmpty,
    ) {
        // Reject unsupported flags (we only support RENAME_NOREPLACE which is 1)
        if (flags & !1) != 0 {
            reply.error(libc::EINVAL);
            return;
        }

        let mut source_ino = None;
        let mut is_layer_f = false;

        {
            let layer_f = self.overlay.layer_f.read();
            if let Some(&child_ino) = layer_f.name_index.get(&parent).and_then(|m| m.get(name)) {
                source_ino = Some(child_ino);
                is_layer_f = true;
            }
        }

        if source_ino.is_none() {
            let parent_for_layer_m = self.resolve_backend_parent(parent);

            let layer_m = self.layer_m.read();
            if let Some(ino) = layer_m.lookup_ino(parent_for_layer_m, name) {
                if !self.overlay.layer_t.read().is_tombstoned(ino)
                    && !self.overlay.layer_a.read().is_deleted(ino)
                {
                    source_ino = Some(ino);
                }
            }
        }

        let Some(ino) = source_ino else {
            reply.error(ENOENT);
            return;
        };

        // Handle Overwrite (Unlink target if exists)
        let mut dest_exists = false;
        let mut dest_ino = None;
        let mut dest_is_layer_f = false;

        {
            let layer_f = self.overlay.layer_f.read();
            if let Some(&child_ino) = layer_f.name_index.get(&newparent).and_then(|m| m.get(newname)) {
                dest_ino = Some(child_ino);
                dest_is_layer_f = true;
                dest_exists = true;
            }
        }

        if !dest_exists {
            let newparent_for_layer_m = self.resolve_backend_parent(newparent);

            let layer_m = self.layer_m.read();
            if let Some(d_ino) = layer_m.lookup_ino(newparent_for_layer_m, newname) {
                if !self.overlay.layer_t.read().is_tombstoned(d_ino)
                    && !self.overlay.layer_a.read().is_deleted(d_ino)
                {
                    dest_ino = Some(d_ino);
                }
            }
        }

        if let Some(d_ino) = dest_ino {
            // RENAME_NOREPLACE is flag 1
            if (flags & 1) != 0 {
                reply.error(libc::EEXIST);
                return;
            }

            if d_ino == ino {
                reply.ok();
                return;
            }
            if dest_is_layer_f {
                let mut layer_f = self.overlay.layer_f.write();
                layer_f.remove_child(newparent, d_ino, newname);
                layer_f.inodes.remove(&d_ino);
            } else {
                let mut layer_a = self.overlay.layer_a.write();
                layer_a.mark_deleted(d_ino);
            }
        }

        // Perform the Move
        if is_layer_f {
            // Case 1: Layer F Move
            {
                let mut layer_f = self.overlay.layer_f.write();
                layer_f.remove_child(parent, ino, name);

                if let Some(meta) = layer_f.inodes.get_mut(&ino) {
                    meta.name = newname.to_os_string();
                    meta.parent = newparent;
                }

                layer_f.add_child(newparent, ino, newname.to_os_string());
            }
        } else {
            // Case 2: Layer M Move (Tombstone + New Layer F + Layer D Dependency)
            {
                let mut layer_t = self.overlay.layer_t.write();
                layer_t.tombstone(ino);
            }

            self.ensure_stat_fetched(ino);

            let (size, kind, old_mtime, old_atime, old_mode, old_uid, old_gid) = {
                let mut size = 0;
                let mut kind = FileType::RegularFile;
                let mut old_mtime = SystemTime::now();
                let mut old_atime = SystemTime::now();
                let mut old_mode = 0o644;
                let mut old_uid = 1000;
                let mut old_gid = 1000;

                {
                    let layer_m = self.layer_m.read();
                    if let Some(meta) = layer_m.get_metadata(ino) {
                        size = meta.size;
                        kind = meta.kind;
                        old_mtime = meta.mtime;
                        old_atime = meta.atime;
                        old_mode = meta.mode;
                        old_uid = meta.uid;
                        old_gid = meta.gid;
                    }
                }

                let layer_s = self.overlay.layer_s.read();
                if let Some(override_stat) = layer_s.overrides.get(&ino) {
                    if let Some(m) = override_stat.mtime {
                        old_mtime = m;
                    }
                    if let Some(a) = override_stat.atime {
                        old_atime = a;
                    }
                    if let Some(s) = override_stat.size {
                        size = s;
                    }
                    if let Some(m) = override_stat.mode {
                        old_mode = m;
                    }
                    if let Some(u) = override_stat.uid {
                        old_uid = u;
                    }
                    if let Some(g) = override_stat.gid {
                        old_gid = g;
                    }
                }
                
                (size, kind, old_mtime, old_atime, old_mode, old_uid, old_gid)
            };

            let (new_ino, kind, size) = {
                let mut layer_f = self.overlay.layer_f.write();
                let new_ino = layer_f.allocate_inode();

                let now = SystemTime::now();
                let meta = crate::layers::layer_f::NewInodeMeta {
                    name: newname.to_os_string(),
                    parent: newparent,
                    kind,
                    size,
                    mtime: old_mtime,
                    ctime: now,
                    atime: old_atime,
                    mode: old_mode & 0o7777,
                    uid: old_uid,
                    gid: old_gid,
                };

                layer_f.inodes.insert(new_ino, meta);
                layer_f.add_child(newparent, new_ino, newname.to_os_string());
                (new_ino, kind, size)
            };

            if kind == FileType::Directory {
                let mut layer_d = self.overlay.layer_d.write();
                layer_d.add_redirect(new_ino, ino);
            } else {
                use crate::layers::Extent;
                let mut layer_d = self.overlay.layer_d.write();
                layer_d.add_dependency(
                    new_ino,
                    vec![Extent::Backend {
                        ino,
                        offset: 0,
                        length: size,
                    }],
                );
            }
        }

        self.update_parent_times(parent);
        if parent != newparent {
            self.update_parent_times(newparent);
        }

        reply.ok();
    }
}
