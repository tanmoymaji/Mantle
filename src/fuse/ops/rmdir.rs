use crate::fuse::MantleFS;
use fuser::{FileType, ReplyEmpty, Request};
use libc::ENOENT;
use std::ffi::OsStr;

pub fn rmdir(fs: &mut MantleFS, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEmpty) {
    let mut target_ino = None;
    let mut is_layer_f = false;

    // Search Layer F
    {
        let layer_f = fs.overlay.layer_f.read();
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
        let parent_for_layer_m = fs.resolve_backend_parent(parent);

        let layer_m = fs.layer_m.read();
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
        if let Some(children) = fs.overlay.layer_f.read().children.get(&ino) {
            if !children.is_empty() {
                is_empty = false;
            }
        }

        // Check Layer M children
        if is_empty && !is_layer_f {
            if let Some(children) = fs.layer_m.read().children.get(&ino) {
                for &child_ino in children {
                    // If it's not deleted or tombstoned, the dir is not empty
                    if !fs.overlay.layer_a.read().is_deleted(child_ino)
                        && !fs.overlay.layer_t.read().is_tombstoned(child_ino)
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
            let mut layer_f = fs.overlay.layer_f.write();
            layer_f.remove_child(parent, ino, name);
            layer_f.inodes.remove(&ino);
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
