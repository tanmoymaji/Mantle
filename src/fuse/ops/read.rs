use crate::fuse::MantleFS;
use crate::layers::extent::Extent;
use fuser::{ReplyData, Request};
use std::fs::File;
use std::os::unix::fs::FileExt;

/// Handles a FUSE `read` request.
/// Resolves the read window against Layer F, Layer D, and Layer M. Merges the results of
/// NVMe cache (Layer C) extents with physical disk (Layer M) backends, implicitly handling holes
/// and dependencies dynamically.
pub fn read(
    fs: &mut MantleFS,
    _req: &Request,
    ino: u64,
    _fh: u64,
    offset: i64,
    size: u32,
    _flags: i32,
    _lock_owner: Option<u64>,
    reply: ReplyData,
) {
    if offset < 0 {
        reply.error(libc::EINVAL);
        return;
    }
    let offset = offset as u64;
    let size = size as u64;

    let mut is_layer_f = false;
    let mut file_size = 0;

    {
        let layer_f = fs.overlay.layer_f.read();
        if let Some(meta) = layer_f.inodes.get(&ino) {
            is_layer_f = true;
            file_size = meta.size;
        }
    }

    if !is_layer_f {
        // Backend file
        let override_size = {
            let layer_s = fs.overlay.layer_s.read();
            layer_s.overrides.get(&ino).and_then(|s| s.size)
        };
        if let Some(s) = override_size {
            file_size = s;
        } else {
            let layer_m = fs.layer_m.read();
            if let Some(meta) = layer_m.get_metadata(ino) {
                file_size = meta.size;
            } else {
                reply.error(libc::ENOENT);
                return;
            }
        }
    }

    if offset >= file_size {
        reply.data(&[]);
        return;
    }

    let read_size = (file_size - offset).min(size) as usize;
    let mut buffer = vec![0u8; read_size];

    let apply_extents = |buffer: &mut [u8],
                         extents: &[Extent],
                         layer_c: &crate::layers::LayerC,
                         layer_m: &crate::layers::LayerM| {
        for ext in extents {
            let ext_start = ext.file_offset();
            let ext_end = ext_start.saturating_add(ext.length());

            let read_end = offset.saturating_add(read_size as u64);
            // Check if this extent overlaps the requested read window
            if ext_end <= offset || ext_start >= read_end {
                continue;
            }

            // Calculate the overlap window
            let overlap_start = ext_start.max(offset);
            let overlap_end = ext_end.min(read_end);
            let overlap_len = (overlap_end - overlap_start) as usize;

            let buf_offset = (overlap_start - offset) as usize;
            let ext_internal_offset = overlap_start - ext_start;

            match ext {
                Extent::Backend {
                    ino: b_ino,
                    offset: b_offset,
                    ..
                } => {
                    let path = layer_m.get_full_path(*b_ino);
                    if let Ok(file) = File::open(&path) {
                        // Read exactly the overlapped part
                        let read_pos = b_offset + ext_internal_offset;
                        let _ = file.read_exact_at(
                            &mut buffer[buf_offset..buf_offset + overlap_len],
                            read_pos,
                        );
                    }
                }
                Extent::LayerC {
                    cache_id,
                    offset: c_offset,
                    ..
                } => {
                    let read_pos = c_offset + ext_internal_offset;
                    if let Some(data) = layer_c.read_block(*cache_id, read_pos, overlap_len) {
                        buffer[buf_offset..buf_offset + overlap_len].copy_from_slice(&data);
                    }
                }
            }
        }
    };

    let layer_c = fs.overlay.layer_c.read();
    let layer_m = fs.layer_m.read();

    if is_layer_f {
        // 1. Base Layer (Dependencies)
        {
            let layer_d = fs.overlay.layer_d.read();
            if let Some(deps) = layer_d.dependencies.get(&ino) {
                apply_extents(&mut buffer, &deps.extents, &layer_c, &layer_m);
            }
        }
        // 2. Cache Layer (File Extents)
        {
            let layer_f = fs.overlay.layer_f.read();
            if let Some(exts) = layer_f.file_extents.get(&ino) {
                apply_extents(&mut buffer, &exts.extents, &layer_c, &layer_m);
            }
        }
    } else {
        // Backend file
        // 1. Base Layer (Read directly from Backend file)
        let path = layer_m.get_full_path(ino);
        if let Ok(file) = File::open(&path) {
            let _ = file.read_exact_at(&mut buffer, offset);
            // Implicitly handles sparse files: reading less bytes or failing to read leaves the
            // unread portions of the buffer initialized with zeroes.
        }

        // 2. Cache Layer (Modified Extents)
        {
            let layer_d = fs.overlay.layer_d.read();
            if let Some(exts) = layer_d.modified_extents.get(&ino) {
                apply_extents(&mut buffer, &exts.extents, &layer_c, &layer_m);
            }
        }
    }

    reply.data(&buffer);
}
