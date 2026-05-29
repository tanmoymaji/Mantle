use crate::fuse::MantleFS;
use fuser::{ReplyEntry, Request};
use libc::ENOENT;
use std::ffi::OsStr;
use super::super::{TTL, create_file_attr};

pub fn lookup(fs: &mut MantleFS, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
    // Priority 1: Newly created or modified files in the overlay (Layer F)
    // must take precedence over backend files.
    {
        let layer_f = fs.overlay.layer_f.read();
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

    let parent_for_layer_m = fs.resolve_backend_parent(parent);

    // Priority 2: If it's not a newly created file, fall back to the backend drive (Layer M).
    let ino_opt = {
        let layer = fs.layer_m.read();
        layer.lookup_ino(parent_for_layer_m, name)
    };

    if let Some(ino) = ino_opt {
        // Hide backend inodes that have been marked for deletion or overwritten
        if fs.overlay.layer_t.read().is_tombstoned(ino)
            || fs.overlay.layer_a.read().is_deleted(ino)
        {
            reply.error(ENOENT);
            return;
        }

        fs.ensure_stat_fetched(ino);
        if let Some(attr) = fs.get_merged_backend_attr(ino) {
            reply.entry(&TTL, &attr, 0);
            return;
        }
    }

    reply.error(ENOENT);
}
