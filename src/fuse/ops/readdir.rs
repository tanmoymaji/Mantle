use crate::fuse::MantleFS;
use fuser::{FileType, ReplyDirectory, Request};

pub fn readdir(fs: &mut MantleFS, _req: &Request, ino: u64, _fh: u64, offset: i64, mut reply: ReplyDirectory) {
    let redirected_ino = {
        let layer_d = fs.overlay.layer_d.read();
        layer_d.directory_redirects.get(&ino).copied()
    };
    let ino_for_layer_m = redirected_ino.unwrap_or(ino);

    let layer_f_parent = if redirected_ino.is_some() {
        fs.overlay
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
        let layer = fs.layer_m.read();
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

                if fs.overlay.layer_t.read().is_tombstoned(child_ino)
                    || fs.overlay.layer_a.read().is_deleted(child_ino)
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
                let parent = fs
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
        let layer_f = fs.overlay.layer_f.read();
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
