use crate::fuse::MantleFS;
use fuser::{FileType, ReplyEmpty, Request};
use libc::{EEXIST, EINVAL, ENOENT};
use std::ffi::OsStr;
use std::time::SystemTime;

pub fn rename(
    fs: &mut MantleFS,
    _req: &Request,
    parent: u64,
    name: &OsStr,
    newparent: u64,
    newname: &OsStr,
    flags: u32,
    reply: ReplyEmpty,
) {
    // Reject unsupported flags (we only support RENAME_NOREPLACE which is 1)
    if (flags & !1) != 0 {
        reply.error(EINVAL);
        return;
    }

    let mut source_ino = None;
    let mut is_layer_f = false;

    {
        let layer_f = fs.overlay.layer_f.read();
        if let Some(&child_ino) = layer_f.name_index.get(&parent).and_then(|m| m.get(name)) {
            source_ino = Some(child_ino);
            is_layer_f = true;
        }
    }

    if source_ino.is_none() {
        let parent_for_layer_m = fs.resolve_backend_parent(parent);

        let layer_m = fs.layer_m.read();
        if let Some(ino) = layer_m.lookup_ino(parent_for_layer_m, name) {
            if !fs.overlay.layer_t.read().is_tombstoned(ino)
                && !fs.overlay.layer_a.read().is_deleted(ino)
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
        let layer_f = fs.overlay.layer_f.read();
        if let Some(&child_ino) = layer_f
            .name_index
            .get(&newparent)
            .and_then(|m| m.get(newname))
        {
            dest_ino = Some(child_ino);
            dest_is_layer_f = true;
            dest_exists = true;
        }
    }

    if !dest_exists {
        let newparent_for_layer_m = fs.resolve_backend_parent(newparent);

        let layer_m = fs.layer_m.read();
        if let Some(d_ino) = layer_m.lookup_ino(newparent_for_layer_m, newname) {
            if !fs.overlay.layer_t.read().is_tombstoned(d_ino)
                && !fs.overlay.layer_a.read().is_deleted(d_ino)
            {
                dest_ino = Some(d_ino);
            }
        }
    }

    if let Some(d_ino) = dest_ino {
        // RENAME_NOREPLACE is flag 1
        if (flags & 1) != 0 {
            reply.error(EEXIST);
            return;
        }

        if d_ino == ino {
            reply.ok();
            return;
        }
        if dest_is_layer_f {
            let mut layer_f = fs.overlay.layer_f.write();
            layer_f.remove_child(newparent, d_ino, newname);
            layer_f.inodes.remove(&d_ino);
        } else {
            let mut layer_a = fs.overlay.layer_a.write();
            layer_a.mark_deleted(d_ino);
        }
    }

    // Perform the Move
    if is_layer_f {
        // Case 1: Layer F Move
        {
            let mut layer_f = fs.overlay.layer_f.write();
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
            let mut layer_t = fs.overlay.layer_t.write();
            layer_t.tombstone(ino);
        }

        fs.ensure_stat_fetched(ino);

        let (size, kind, old_mtime, old_atime, old_mode, old_uid, old_gid) = {
            let mut size = 0;
            let mut kind = FileType::RegularFile;
            let mut old_mtime = SystemTime::now();
            let mut old_atime = SystemTime::now();
            let mut old_mode = 0o644;
            let mut old_uid = 1000;
            let mut old_gid = 1000;

            {
                let layer_m = fs.layer_m.read();
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

            let layer_s = fs.overlay.layer_s.read();
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
            let mut layer_f = fs.overlay.layer_f.write();
            // Preserve the original inode number to prevent kernel dcache invalidation errors
            let new_ino = ino;

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
            let mut layer_d = fs.overlay.layer_d.write();
            layer_d.add_redirect(new_ino, ino);
        } else {
            use crate::layers::extent::{Extent, ExtentList};
            let mut layer_d = fs.overlay.layer_d.write();
            layer_d.add_dependency(
                new_ino,
                ExtentList {
                    extents: vec![Extent::Backend {
                        file_offset: 0,
                        ino,
                        offset: 0,
                        length: size,
                    }],
                },
            );

            // Transplant pending modifications so they aren't lost
            if let Some(modified) = layer_d.modified_extents.remove(&ino) {
                let mut layer_f = fs.overlay.layer_f.write();
                layer_f.file_extents.insert(new_ino, modified);
            }
        }
    }

    fs.update_parent_times(parent);
    if parent != newparent {
        fs.update_parent_times(newparent);
    }

    reply.ok();
}
