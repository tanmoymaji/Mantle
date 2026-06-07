use crate::fuse::MantleFS;
use crate::layers::extent::Extent;
use fuser::{ReplyWrite, Request};
use std::time::SystemTime;

/// Handles a FUSE `write` request.
/// Writes the provided data block to the NVMe cache (Layer C), updating the associated
/// metadata in the overlay layers. If the file is tracked by Layer F (new file), its extents
/// are updated directly. Otherwise, it updates the modified extents in Layer D (for backend files)
/// and triggers the RefActions required to increment or decrement the reference counts for cache blocks.
pub fn write(
    fs: &mut MantleFS,
    _req: &Request,
    ino: u64,
    _fh: u64,
    offset: i64,
    data: &[u8],
    _write_flags: u32,
    _flags: i32,
    _lock_owner: Option<u64>,
    reply: ReplyWrite,
) {
    if offset < 0 {
        reply.error(libc::EINVAL);
        return;
    }
    let offset = offset as u64;
    let length = data.len() as u64;
    let now = SystemTime::now();

    // 1. Write the data block to Layer C
    let cache_id = {
        let mut layer_c = fs.overlay.layer_c.write();
        layer_c.write_block(data.to_vec())
    };

    let new_extent = Extent::LayerC {
        file_offset: offset,
        cache_id,
        offset: 0,
        length,
    };

    // 2. Update Metadata & Track Extents
    let mut updated = false;

    let mut ref_actions = Vec::new();

    // Check Layer F
    {
        let mut layer_f = fs.overlay.layer_f.write();
        if let Some(meta) = layer_f.inodes.get_mut(&ino) {
            meta.mtime = now;
            meta.ctime = now;
            let new_end = offset.saturating_add(length);
            if new_end > meta.size {
                meta.size = new_end;
            }

            let extents = layer_f.file_extents.entry(ino).or_default();
            ref_actions = extents.overwrite(new_extent.clone());
            updated = true;
        }
    }

    if !updated {
        // Must be a backend file (Layer M)
        {
            let mut layer_d = fs.overlay.layer_d.write();
            let extents = layer_d.modified_extents.entry(ino).or_default();
            ref_actions = extents.overwrite(new_extent);
        }

        let mut layer_s = fs.overlay.layer_s.write();
        let override_stat = layer_s.overrides.entry(ino).or_default();

        // We need to know the original size to update it correctly if we exceed it.
        // If Layer S has a size, use it. Otherwise fetch from Layer M.
        let mut current_size = override_stat.size.unwrap_or(0);
        if override_stat.size.is_none() {
            let layer_m = fs.layer_m.read();
            if let Some(meta) = layer_m.get_metadata(ino) {
                current_size = meta.size;
            }
        }

        let new_end = offset.saturating_add(length);
        if new_end > current_size {
            override_stat.size = Some(new_end);
        }

        override_stat.mtime = Some(now);
        override_stat.ctime = Some(now);
    }

    // 3. Apply Ref Actions to LayerC
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

    reply.written(length as u32);
}
