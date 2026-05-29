use crate::fuse::MantleFS;
use fuser::{FileType, ReplyEmpty, Request};
use libc::ENOENT;
use std::ffi::OsStr;

pub fn unlink(fs: &mut MantleFS, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEmpty) {
    // Find the inode
    let mut target_ino = None;
    let mut is_layer_f = false;

    // Search Layer F first
    {
        let layer_f = fs.overlay.layer_f.read();
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
        let parent_for_layer_m = fs.resolve_backend_parent(parent);

        let layer_m = fs.layer_m.read();
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
            let mut layer_f = fs.overlay.layer_f.write();
            layer_f.remove_child(parent, ino, name);
            layer_f.inodes.remove(&ino);
            // Extent cleanup would happen here
        } else {
            let mut layer_a = fs.overlay.layer_a.write();
            layer_a.mark_deleted(ino);
        }

        fs.update_parent_times(parent);

        reply.ok();
    } else {
        reply.error(ENOENT);
    }
}
