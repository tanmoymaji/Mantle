use crate::fuse::MantleFS;
use fuser::{FileType, ReplyCreate, Request};
use std::ffi::OsStr;
use std::time::SystemTime;
use super::super::{TTL, create_file_attr};

pub fn create(
    fs: &mut MantleFS,
    req: &Request,
    parent: u64,
    name: &OsStr,
    mode: u32,
    umask: u32,
    _flags: i32,
    reply: ReplyCreate,
) {
    let mut layer_f = fs.overlay.layer_f.write();
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

    fs.update_parent_times(parent);

    reply.created(&TTL, &attr, 0, 0, 0);
}
