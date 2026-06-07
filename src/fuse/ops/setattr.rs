use super::super::{TTL, create_file_attr};
use crate::fuse::MantleFS;
use fuser::{ReplyAttr, Request};
use libc::ENOENT;
use std::time::SystemTime;

pub fn setattr(
    fs: &mut MantleFS,
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
        let mut layer_f = fs.overlay.layer_f.write();
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
                if mtime.is_none() {
                    meta.mtime = sys_now;
                }
                if ctime.is_none() {
                    meta.ctime = sys_now;
                }
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
                ino,
                meta.size,
                meta.kind,
                meta.mtime,
                meta.ctime,
                meta.atime,
                meta.mode as u16,
                meta.uid,
                meta.gid,
            ));
        }
    } // layer_f lock is dropped here

    let mut ref_actions = Vec::new();
    if let Some(new_size) = size {
        let mut layer_f = fs.overlay.layer_f.write();
        if layer_f.inodes.contains_key(&ino) {
            if let Some(extents) = layer_f.file_extents.get_mut(&ino) {
                ref_actions = extents.truncate_past(new_size);
            }
        } else {
            drop(layer_f);
            if let Some(extents) = fs.overlay.layer_d.write().modified_extents.get_mut(&ino) {
                ref_actions = extents.truncate_past(new_size);
            }
        }
    }

    if !ref_actions.is_empty() {
        use crate::layers::extent::RefAction;
        let mut layer_c = fs.overlay.layer_c.write();
        for action in ref_actions.drain(..) {
            match action {
                RefAction::Increment(id) => layer_c.increment_ref(id),
                RefAction::Decrement(id) => layer_c.decrement_ref(id),
            }
        }
    }

    if let Some(a) = attr {
        reply.attr(&TTL, &a);
        return;
    }

    if fs.overlay.layer_t.read().is_tombstoned(ino) || fs.overlay.layer_a.read().is_deleted(ino) {
        reply.error(ENOENT);
        return;
    }

    // Fetch real backend metadata to ensure we're modifying the true size/permissions
    fs.ensure_stat_fetched(ino);

    // Priority 2: If the inode is in Layer M (backend), record the changes in Layer S (Overrides).
    let layer = fs.layer_m.read();
    if layer.get_metadata(ino).is_some() {
        drop(layer);

        {
            let mut layer_s = fs.overlay.layer_s.write();
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
                if mtime.is_none() {
                    entry.mtime = Some(sys_now);
                }
                if ctime.is_none() {
                    entry.ctime = Some(sys_now);
                }
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
            if let Some(mut extents) = fs.overlay.layer_d.write().modified_extents.remove(&ino) {
                ref_actions.extend(extents.truncate_past(0));
            }
        }

        if !ref_actions.is_empty() {
            use crate::layers::extent::RefAction;
            let mut layer_c = fs.overlay.layer_c.write();
            for action in ref_actions {
                match action {
                    RefAction::Increment(id) => layer_c.increment_ref(id),
                    RefAction::Decrement(id) => layer_c.decrement_ref(id),
                }
            }
        }

        if let Some(attr) = fs.get_merged_backend_attr(ino) {
            reply.attr(&TTL, &attr);
        } else {
            reply.error(ENOENT);
        }
    } else {
        reply.error(ENOENT);
    }
}
