use crate::fuse::MantleFS;
use fuser::{ReplyAttr, Request};
use libc::ENOENT;
use super::super::{TTL, create_file_attr};

pub fn getattr(fs: &mut MantleFS, _req: &Request, ino: u64, reply: ReplyAttr) {
    // Overlay properties take precedence over backend properties.
    {
        let layer_f = fs.overlay.layer_f.read();
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
    if fs.overlay.layer_t.read().is_tombstoned(ino)
        || fs.overlay.layer_a.read().is_deleted(ino)
    {
        reply.error(ENOENT);
        return;
    }

    fs.ensure_stat_fetched(ino);
    if let Some(attr) = fs.get_merged_backend_attr(ino) {
        reply.attr(&TTL, &attr);
    } else {
        reply.error(ENOENT);
    }
}
