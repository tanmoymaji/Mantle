use fuser::{FileAttr, FileType};
use jwalk::WalkDir;
use log::{info, warn};
use parking_lot::RwLock;
use rustc_hash::FxHashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub type Inode = u64;
pub const ROOT_INODE: Inode = 1;
pub const FETCH_BATCH_SIZE: usize = 512;

/// Layer M: Backend Metadata Index
/// Scans the backend and tracks inode metadata with lazy stat loading.
#[derive(Debug, Clone)]
pub struct Metadata {
    pub ino: Inode,
    pub parent: Inode,
    pub name: String,
    pub kind: FileType,
    pub size: u64,
    pub mtime: SystemTime,
    pub atime: SystemTime,
    pub ctime: SystemTime,
    pub stat_fetched: bool, // Flag to enable lazy loading
}

impl Metadata {
    pub fn as_file_attr(&self) -> FileAttr {
        FileAttr {
            ino: self.ino,
            size: self.size,
            blocks: (self.size + 511) / 512,
            atime: self.atime,
            mtime: self.mtime,
            ctime: self.ctime,
            crtime: self.ctime,
            kind: self.kind,
            perm: if self.kind == FileType::Directory {
                0o755
            } else {
                0o644
            },
            nlink: if self.kind == FileType::Directory {
                2
            } else {
                1
            },
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
            rdev: 0,
            flags: 0,
            blksize: 4096,
        }
    }
}

/// Layer M: Backend Inode Catalog
/// Provides inode lookup, directory structure, and lazy stat fetch support.
pub struct LayerM {
    pub backend_root: PathBuf,
    // Monotonic inode allocator for the in-memory index.
    next_ino: AtomicU64,
    pub inodes: FxHashMap<Inode, Metadata>,
    pub children: FxHashMap<Inode, Vec<Inode>>,
    pub name_index: FxHashMap<Inode, FxHashMap<String, Inode>>,
}

impl LayerM {
    pub fn new<P: AsRef<Path>>(backend_root: P) -> anyhow::Result<Self> {
        let mut layer = Self {
            backend_root: backend_root.as_ref().to_path_buf(),
            next_ino: AtomicU64::new(ROOT_INODE),
            inodes: FxHashMap::default(),
            children: FxHashMap::default(),
            name_index: FxHashMap::default(),
        };

        layer.scan_backend()?;
        Ok(layer)
    }

    fn allocate_ino(&self) -> Inode {
        self.next_ino.fetch_add(1, Ordering::SeqCst)
    }

    fn scan_backend(&mut self) -> anyhow::Result<()> {
        info!(
            "Layer M: Starting parallel backend scan of {:?}",
            self.backend_root
        );
        let start = std::time::Instant::now();

        let root_meta = Metadata {
            ino: ROOT_INODE,
            parent: ROOT_INODE,
            name: "".to_string(),
            kind: FileType::Directory,
            size: 4096, // Just a default size for dir
            mtime: SystemTime::now(),
            atime: SystemTime::now(),
            ctime: SystemTime::now(),
            stat_fetched: true,
        };
        self.inodes.insert(ROOT_INODE, root_meta);
        self.children.insert(ROOT_INODE, Vec::new());
        self.next_ino.store(ROOT_INODE + 1, Ordering::SeqCst);

        let mut path_to_ino: FxHashMap<PathBuf, Inode> = FxHashMap::default();
        path_to_ino.insert(self.backend_root.clone(), ROOT_INODE);

        // We use jwalk which parallelizes the walk and prevents explicit stats!
        for entry in WalkDir::new(&self.backend_root)
            .skip_hidden(false)
            .sort(false)
        {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    warn!("Layer M: Failed to access entry during scan: {}", e);
                    continue;
                }
            };

            let path = entry.path();
            if path == self.backend_root {
                continue; // Skip root as it's already added
            }

            let parent_path = path.parent().unwrap_or(Path::new("")).to_path_buf();

            let parent_ino = *path_to_ino
                .entry(parent_path)
                .or_insert_with(|| self.allocate_ino());
            let ino = *path_to_ino
                .entry(path.clone())
                .or_insert_with(|| self.allocate_ino());

            let file_type = entry.file_type();
            let kind = if file_type.is_dir() {
                FileType::Directory
            } else if file_type.is_symlink() {
                FileType::Symlink
            } else {
                FileType::RegularFile
            };

            // LAZY LOADING: We do NOT fetch real size or mtime here!
            let meta = Metadata {
                ino,
                parent: parent_ino,
                name: entry.file_name().to_string_lossy().to_string(),
                kind,
                size: if kind == FileType::Directory { 4096 } else { 0 },
                mtime: UNIX_EPOCH,
                atime: UNIX_EPOCH,
                ctime: UNIX_EPOCH,
                stat_fetched: false,
            };

            let name = meta.name.clone();
            self.inodes.insert(ino, meta);
            self.children.entry(parent_ino).or_default().push(ino);
            if kind == FileType::Directory {
                self.children.entry(ino).or_default();
            }
            self.name_index
                .entry(parent_ino)
                .or_default()
                .insert(name, ino);
        }

        info!(
            "Layer M: Scan complete in {:?}. Loaded {} files/folders.",
            start.elapsed(),
            self.inodes.len()
        );
        Ok(())
    }

    pub fn get_metadata(&self, ino: Inode) -> Option<&Metadata> {
        self.inodes.get(&ino)
    }

    pub fn get_full_path(&self, ino: Inode) -> PathBuf {
        let mut parts = Vec::new();
        let mut current = ino;
        while current != ROOT_INODE {
            if let Some(meta) = self.inodes.get(&current) {
                parts.push(meta.name.clone());
                current = meta.parent;
            } else {
                break;
            }
        }
        parts.reverse();
        let mut path = self.backend_root.clone();
        for p in parts {
            path.push(p);
        }
        path
    }

    pub fn lookup_ino(&self, parent: Inode, name: &str) -> Option<Inode> {
        self.name_index
            .get(&parent)
            .and_then(|m| m.get(name))
            .copied()
    }

    /// Background stat fetch for entries discovered without metadata.
    pub fn start_background_fetch(shared: Arc<RwLock<LayerM>>) {
        std::thread::spawn(move || {
            let start = std::time::Instant::now();
            let mut pending = Vec::new();

            // Gather all pending inodes quickly
            {
                let layer = shared.read();
                for (ino, meta) in &layer.inodes {
                    if !meta.stat_fetched {
                        pending.push(*ino);
                    }
                }
            }

            // Sort by inode to align with discovery order and mitigate completely random I/O
            pending.sort_unstable();

            let total_pending = pending.len();

            info!(
                "Layer M Background: Fetching stats for {} items...",
                total_pending
            );

            let mut success_count = 0;

            for chunk in pending.chunks(FETCH_BATCH_SIZE) {
                // Fetch stats without locks
                let mut batch = Vec::new();
                for &ino in chunk {
                    let path = {
                        let layer = shared.read();
                        if layer.inodes.get(&ino).map_or(false, |m| m.stat_fetched) {
                            continue;
                        }
                        layer.get_full_path(ino)
                    };

                    if let Ok(fs_meta) = std::fs::symlink_metadata(&path) {
                        batch.push((ino, fs_meta));
                    }
                }

                success_count += batch.len();

                // Apply batch under one write lock
                if !batch.is_empty() {
                    let mut layer = shared.write();
                    for (ino, fs_meta) in batch {
                        if let Some(meta) = layer.inodes.get_mut(&ino) {
                            meta.size = fs_meta.len();
                            meta.mtime = fs_meta.modified().unwrap_or(UNIX_EPOCH);
                            meta.atime = fs_meta.accessed().unwrap_or(UNIX_EPOCH);
                            meta.ctime = fs_meta.created().unwrap_or(meta.mtime);
                            meta.stat_fetched = true;
                        }
                    }
                }
            }

            info!(
                "Layer M Background: Processed {} items in {:?} ({} succeeded)",
                total_pending,
                start.elapsed(),
                success_count
            );
        });
    }
}
