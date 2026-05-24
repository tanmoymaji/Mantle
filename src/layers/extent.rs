#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Extent {
    /// Points to data in the original external drive
    Backend { ino: u64, offset: u64, length: u64 },
    /// Points to newly written data in the NVMe Cache
    LayerC {
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

    /// Checks if this extent is physically contiguous with the next extent,
    /// and coalesces them into a single extent if possible.
    pub fn try_coalesce(&self, next: &Extent) -> Option<Extent> {
        match (self, next) {
            (
                Extent::Backend {
                    ino: i1,
                    offset: o1,
                    length: l1,
                },
                Extent::Backend {
                    ino: i2,
                    offset: o2,
                    length: l2,
                },
            ) if i1 == i2 && (*o1 + *l1) == *o2 => Some(Extent::Backend {
                ino: *i1,
                offset: *o1,
                length: l1 + l2,
            }),
            (
                Extent::LayerC {
                    cache_id: c1,
                    offset: o1,
                    length: l1,
                },
                Extent::LayerC {
                    cache_id: c2,
                    offset: o2,
                    length: l2,
                },
            ) if c1 == c2 && (*o1 + *l1) == *o2 => Some(Extent::LayerC {
                cache_id: *c1,
                offset: *o1,
                length: l1 + l2,
            }),
            _ => None,
        }
    }
}
