use crate::layers::extent::Extent;
use rustc_hash::FxHashMap;

/// Layer D: Dependency Graph
/// Tracks backend files that have been modified (overwritten, appended, or partially copied).
pub struct LayerD {
    // Maps backend Inode to its updated list of extents (for in-place writes)
    pub modified_extents: FxHashMap<u64, Vec<Extent>>,

    // Maps Layer F Inode to the Backend Extents it depends on (for moved/copied files)
    pub dependencies: FxHashMap<u64, Vec<Extent>>,

    // Maps Layer F Directory Inode to Backend Directory Inode (for renamed directories)
    pub directory_redirects: FxHashMap<u64, u64>,
}

impl LayerD {
    pub fn new() -> Self {
        Self {
            modified_extents: FxHashMap::default(),
            dependencies: FxHashMap::default(),
            directory_redirects: FxHashMap::default(),
        }
    }

    /// Adds a dependency mapping from a new Layer F inode to backend extents.
    /// **Invariant:** `layer_f_ino` MUST be an inode in `Layer F` (not `Layer M`).
    /// The `extents` MUST map to valid regions in backend files (`Layer M`).
    pub fn add_dependency(&mut self, layer_f_ino: u64, extents: Vec<Extent>) {
        self.dependencies.insert(layer_f_ino, extents);
    }

    /// Adds a directory redirection mapping from a new Layer F directory to a backend directory.
    /// **Invariant:** `layer_f_ino` MUST be a directory in `Layer F` created via rename/move.
    /// `backend_ino` MUST be a valid directory in `Layer M`.
    pub fn add_redirect(&mut self, layer_f_ino: u64, backend_ino: u64) {
        self.directory_redirects.insert(layer_f_ino, backend_ino);
    }

    /// Removes a dependency mapping (e.g. upon truncation).
    pub fn remove_dependency(&mut self, layer_f_ino: u64) {
        self.dependencies.remove(&layer_f_ino);
    }
}
