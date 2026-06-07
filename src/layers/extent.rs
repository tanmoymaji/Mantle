#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Extent {
    /// Points to data in the original external drive
    Backend {
        file_offset: u64,
        ino: u64,
        offset: u64,
        length: u64,
    },
    /// Points to newly written data in the NVMe Cache
    LayerC {
        file_offset: u64,
        cache_id: u64,
        offset: u64,
        length: u64,
    },
}

impl Extent {
    pub fn length(&self) -> u64 {
        match self {
            Extent::Backend { length, .. } => *length,
            Extent::LayerC { length, .. } => *length,
        }
    }

    pub fn file_offset(&self) -> u64 {
        match self {
            Extent::Backend { file_offset, .. } => *file_offset,
            Extent::LayerC { file_offset, .. } => *file_offset,
        }
    }

    /// Checks if this extent is physically contiguous with the next extent,
    /// and coalesces them into a single extent if possible.
    pub fn try_coalesce(&self, next: &Extent) -> Option<Extent> {
        match (self, next) {
            (
                Extent::Backend {
                    file_offset: f1,
                    ino: i1,
                    offset: o1,
                    length: l1,
                },
                Extent::Backend {
                    file_offset: f2,
                    ino: i2,
                    offset: o2,
                    length: l2,
                },
            ) if i1 == i2 && (*o1 + *l1) == *o2 && (*f1 + *l1) == *f2 => Some(Extent::Backend {
                file_offset: *f1,
                ino: *i1,
                offset: *o1,
                length: l1 + l2,
            }),
            (
                Extent::LayerC {
                    file_offset: f1,
                    cache_id: c1,
                    offset: o1,
                    length: l1,
                },
                Extent::LayerC {
                    file_offset: f2,
                    cache_id: c2,
                    offset: o2,
                    length: l2,
                },
            ) if c1 == c2 && (*o1 + *l1) == *o2 && (*f1 + *l1) == *f2 => Some(Extent::LayerC {
                file_offset: *f1,
                cache_id: *c1,
                offset: *o1,
                length: l1 + l2,
            }),
            _ => None,
        }
    }
}

/// Represents a reference counting operation to be applied to a cache block.
/// When extents are split or eclipsed, the underlying Layer C cache blocks must be tracked
/// to ensure garbage collection behaves correctly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefAction {
    /// Increment the reference count for the cache block with the given ID.
    Increment(u64),
    /// Decrement the reference count for the cache block with the given ID.
    Decrement(u64),
}

/// A sequential, non-overlapping list of extents representing a file's physical data map.
/// The extents are ordered by their `file_offset`.
#[derive(Debug, Clone, Default)]
pub struct ExtentList {
    pub extents: Vec<Extent>,
}

impl ExtentList {
    pub fn new() -> Self {
        Self {
            extents: Vec::new(),
        }
    }

    /// Overwrites existing extents with a new extent.
    /// Splits and removes any overlapping extents, returning actions to update cache references.
    pub fn overwrite(&mut self, new_ext: Extent) -> Vec<RefAction> {
        let mut actions = Vec::new();
        let new_start = new_ext.file_offset();
        let new_end = new_start.saturating_add(new_ext.length());
        let mut i = 0;

        while i < self.extents.len() {
            let ext = &self.extents[i];
            let start = ext.file_offset();
            let end = start.saturating_add(ext.length());

            if end <= new_start || start >= new_end {
                // No overlap
                i += 1;
                continue;
            }

            if start >= new_start && end <= new_end {
                // Completely eclipsed
                if let Extent::LayerC { cache_id, .. } = self.extents[i] {
                    actions.push(RefAction::Decrement(cache_id));
                }
                self.extents.remove(i);
                continue;
            }

            if start < new_start && end > new_end {
                // The new extent splits this extent perfectly in half
                let left = self.slice_extent(ext, start, new_start);
                let right = self.slice_extent(ext, new_end, end);

                let cache_id = if let Extent::LayerC { cache_id, .. } = self.extents[i] {
                    Some(cache_id)
                } else {
                    None
                };

                self.extents.remove(i);
                if let Some(r) = right {
                    self.extents.insert(i, r);
                    if let Some(cid) = cache_id {
                        actions.push(RefAction::Increment(cid));
                    }
                }
                if let Some(l) = left {
                    self.extents.insert(i, l);
                    i += 1; // Skip the left piece we just inserted
                }
                continue;
            }

            if start < new_start && end <= new_end {
                // Overlaps the right side of the existing extent
                if let Some(left) = self.slice_extent(ext, start, new_start) {
                    self.extents[i] = left;
                    i += 1;
                } else {
                    if let Extent::LayerC { cache_id, .. } = self.extents[i] {
                        actions.push(RefAction::Decrement(cache_id));
                    }
                    self.extents.remove(i);
                }
                continue;
            }

            if start >= new_start && end > new_end {
                // Overlaps the left side of the existing extent
                if let Some(right) = self.slice_extent(ext, new_end, end) {
                    self.extents[i] = right;
                    i += 1;
                } else {
                    if let Extent::LayerC { cache_id, .. } = self.extents[i] {
                        actions.push(RefAction::Decrement(cache_id));
                    }
                    self.extents.remove(i);
                }
                continue;
            }
        }

        // Insert the new extent in sorted order
        let insert_idx = self
            .extents
            .partition_point(|e| e.file_offset() < new_start);
        self.extents.insert(insert_idx, new_ext);

        actions.extend(self.coalesce());
        actions
    }

    /// Slices an extent, returning a new Extent covering [start, end)
    /// Returns None if the requested range is invalid or empty.
    fn slice_extent(&self, ext: &Extent, start: u64, end: u64) -> Option<Extent> {
        if start >= end {
            return None;
        }
        match *ext {
            Extent::Backend {
                file_offset,
                ino,
                offset,
                length: _,
            } => {
                let shift = start.saturating_sub(file_offset);
                Some(Extent::Backend {
                    file_offset: start,
                    ino,
                    offset: offset.saturating_add(shift),
                    length: end - start,
                })
            }
            Extent::LayerC {
                file_offset,
                cache_id,
                offset,
                length: _,
            } => {
                let shift = start.saturating_sub(file_offset);
                Some(Extent::LayerC {
                    file_offset: start,
                    cache_id,
                    offset: offset.saturating_add(shift),
                    length: end - start,
                })
            }
        }
    }

    /// Truncates all extents past the given size.
    /// Any extent partially intersecting `size` is sliced.
    /// Returns a list of RefActions to decrement references for dropped cache extents.
    pub fn truncate_past(&mut self, size: u64) -> Vec<RefAction> {
        let mut actions = Vec::new();
        let mut i = 0;
        while i < self.extents.len() {
            let start = self.extents[i].file_offset();
            let end = start.saturating_add(self.extents[i].length());
            if start >= size {
                if let Extent::LayerC { cache_id, .. } = self.extents[i] {
                    actions.push(RefAction::Decrement(cache_id));
                }
                self.extents.remove(i);
            } else if end > size {
                if let Some(sliced) = self.slice_extent(&self.extents[i], start, size) {
                    self.extents[i] = sliced;
                    i += 1;
                } else {
                    if let Extent::LayerC { cache_id, .. } = self.extents[i] {
                        actions.push(RefAction::Decrement(cache_id));
                    }
                    self.extents.remove(i);
                }
            } else {
                i += 1;
            }
        }
        actions
    }

    /// Scans the extent list and merges contiguous extents that map to consecutive backend or cache offsets.
    /// Returns a list of RefActions to decrement references for merged cache blocks.
    pub fn coalesce(&mut self) -> Vec<RefAction> {
        let mut actions = Vec::new();
        let mut i = 0;
        while i + 1 < self.extents.len() {
            if let Some(merged) = self.extents[i].try_coalesce(&self.extents[i + 1]) {
                self.extents[i] = merged;
                if let Extent::LayerC { cache_id, .. } = self.extents[i + 1] {
                    actions.push(RefAction::Decrement(cache_id));
                }
                self.extents.remove(i + 1);
            } else {
                i += 1;
            }
        }
        actions
    }
}
