use crate::layers::{LayerM, MantleOverlay};
use fuser::{FileAttr, FileType, Filesystem, ReplyAttr, ReplyDirectory, ReplyEntry, Request};
use parking_lot::RwLock;
use std::ffi::OsStr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub mod ops;

pub(crate) const TTL: Duration = Duration::from_secs(1);

pub(crate) fn create_file_attr(
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
    pub(crate) layer_m: Arc<RwLock<LayerM>>,
    pub(crate) overlay: Arc<MantleOverlay>,
}

impl MantleFS {
    pub fn new(layer_m: Arc<RwLock<LayerM>>, overlay: Arc<MantleOverlay>) -> Self {
        MantleFS { layer_m, overlay }
    }

    pub(crate) fn ensure_stat_fetched(&self, ino: u64) {
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
                use std::os::unix::fs::MetadataExt;
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

    pub(crate) fn update_parent_times(&self, parent_ino: u64) {
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

    pub(crate) fn resolve_backend_parent(&self, ino: u64) -> u64 {
        let layer_d = self.overlay.layer_d.read();
        layer_d.directory_redirects.get(&ino).copied().unwrap_or(ino)
    }

    pub(crate) fn get_merged_backend_attr(&self, ino: u64) -> Option<fuser::FileAttr> {
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
    fn lookup(&mut self, req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        ops::lookup::lookup(self, req, parent, name, reply);
    }

    fn getattr(&mut self, req: &Request, ino: u64, reply: ReplyAttr) {
        ops::getattr::getattr(self, req, ino, reply);
    }

    fn readdir(&mut self, req: &Request, ino: u64, fh: u64, offset: i64, reply: ReplyDirectory) {
        ops::readdir::readdir(self, req, ino, fh, offset, reply);
    }

    fn mkdir(&mut self, req: &Request, parent: u64, name: &OsStr, mode: u32, umask: u32, reply: ReplyEntry) {
        ops::mkdir::mkdir(self, req, parent, name, mode, umask, reply);
    }

    fn create(&mut self, req: &Request, parent: u64, name: &OsStr, mode: u32, umask: u32, flags: i32, reply: fuser::ReplyCreate) {
        ops::create::create(self, req, parent, name, mode, umask, flags, reply);
    }

    fn unlink(&mut self, req: &Request, parent: u64, name: &OsStr, reply: fuser::ReplyEmpty) {
        ops::unlink::unlink(self, req, parent, name, reply);
    }

    fn rmdir(&mut self, req: &Request, parent: u64, name: &OsStr, reply: fuser::ReplyEmpty) {
        ops::rmdir::rmdir(self, req, parent, name, reply);
    }

    fn setattr(
        &mut self,
        req: &Request,
        ino: u64,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<fuser::TimeOrNow>,
        mtime: Option<fuser::TimeOrNow>,
        ctime: Option<SystemTime>,
        fh: Option<u64>,
        crtime: Option<SystemTime>,
        chgtime: Option<SystemTime>,
        bkuptime: Option<SystemTime>,
        flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        ops::setattr::setattr(
            self, req, ino, mode, uid, gid, size, atime, mtime, ctime, fh, crtime, chgtime, bkuptime, flags, reply,
        );
    }

    fn rename(
        &mut self,
        req: &Request,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        flags: u32,
        reply: fuser::ReplyEmpty,
    ) {
        ops::rename::rename(self, req, parent, name, newparent, newname, flags, reply);
    }
}
