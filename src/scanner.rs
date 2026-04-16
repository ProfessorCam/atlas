use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::SystemTime;
use crossbeam_channel::Sender;

// ---------------------------------------------------------------------------
// Skip-set: paths that should never be traversed
// ---------------------------------------------------------------------------

/// Filesystem types listed in /proc/mounts that a disk-space analyser should
/// never enter.  Includes:
///   • pseudo/virtual FSes  (proc, sysfs, cgroup…) — report nonsense sizes
///   • squashfs             (snap packages)         — real data lives in
///                                                    /var/lib/snapd/snaps/;
///                                                    counting the mounts too
///                                                    would double-count
const SKIP_FS_TYPES: &[&str] = &[
    "proc",
    "sysfs",
    "devtmpfs",
    "cgroup",
    "cgroup2",
    "debugfs",
    "tracefs",
    "securityfs",
    "devpts",
    "hugetlbfs",
    "binfmt_misc",
    "mqueue",
    "fusectl",
    "bpf",
    "pstore",
    "autofs",
    "squashfs", // snap loop mounts
    "overlay",  // container layers
];

/// Hard-coded path prefixes that are always virtual regardless of mount table.
const ALWAYS_SKIP_PREFIXES: &[&str] =
    &["/proc", "/sys", "/dev", "/run/user", "/dev/pts", "/dev/shm"];

/// Build a set of absolute paths that the scanner must not enter.
///
/// Reads `/proc/mounts` and collects every mount point whose filesystem type
/// is in `SKIP_FS_TYPES`.  The hard-coded prefixes above are always included
/// as a fallback for systems where `/proc/mounts` is unreadable.
pub fn build_skip_set() -> HashSet<PathBuf> {
    let mut skip: HashSet<PathBuf> =
        ALWAYS_SKIP_PREFIXES.iter().map(PathBuf::from).collect();

    #[cfg(target_os = "linux")]
    if let Ok(text) = std::fs::read_to_string("/proc/mounts") {
        for line in text.lines() {
            // Format: device  mount-point  fs-type  options  dump  pass
            let mut cols = line.splitn(4, ' ');
            let _dev   = cols.next().unwrap_or("");
            let mnt    = cols.next().unwrap_or("");
            let fstype = cols.next().unwrap_or("");

            if !mnt.is_empty() && SKIP_FS_TYPES.iter().any(|&t| fstype == t) {
                skip.insert(PathBuf::from(mnt));
            }
        }
    }

    skip
}

/// Returns `true` when `path` should be skipped: either it is an exact entry
/// in the skip set, or it falls under one of the always-skip prefixes.
fn should_skip(path: &Path, skip: &HashSet<PathBuf>) -> bool {
    if skip.contains(path) {
        return true;
    }
    if let Some(s) = path.to_str() {
        for prefix in ALWAYS_SKIP_PREFIXES {
            if s.starts_with(&format!("{}/", prefix)) {
                return true;
            }
        }
    }
    false
}

/// A single node in the file tree (file, directory, or synthetic entry).
#[derive(Debug, Clone)]
pub struct FileEntry {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
    /// True while this entry is a placeholder during an in-progress scan.
    pub is_unscanned: bool,
    pub children: Vec<Arc<FileEntry>>,
    pub file_count: u64,
    pub modified: Option<SystemTime>,
}

impl FileEntry {
    pub fn extension(&self) -> Option<&str> {
        if self.is_dir || self.is_unscanned {
            return None;
        }
        self.path.extension().and_then(|e| e.to_str())
    }
}

/// Messages sent from the background scanner to the UI thread.
pub enum ScanMessage {
    /// Incremental progress update (current path being entered).
    Progress { path: PathBuf, bytes: u64, files: u64 },
    /// Partial tree available for display right now.
    Update(Arc<FileEntry>),
    /// Scan fully complete.
    Done(Arc<FileEntry>),
    /// Fatal error.
    Error(String),
}

/// Cancellation token.
#[derive(Clone)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        CancelToken(Arc::new(AtomicBool::new(false)))
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Spawn a background scan.
pub fn start_scan(root_path: PathBuf, tx: Sender<ScanMessage>, cancel: CancelToken) {
    std::thread::spawn(move || {
        // Build the skip set once — reads /proc/mounts, O(mounts) cost.
        let skip = build_skip_set();
        match scan_root_incremental(&root_path, &tx, &cancel, &skip) {
            Ok(entry) => {
                let _ = tx.send(ScanMessage::Done(Arc::new(entry)));
            }
            Err(e) => {
                if !cancel.is_cancelled() {
                    let _ = tx.send(ScanMessage::Error(e.to_string()));
                }
            }
        }
    });
}

/// Scan `root_path`, emitting Update messages after each top-level child finishes.
fn scan_root_incremental(
    root_path: &Path,
    tx: &Sender<ScanMessage>,
    cancel: &CancelToken,
    skip: &HashSet<PathBuf>,
) -> Result<FileEntry, std::io::Error> {
    let meta = std::fs::symlink_metadata(root_path)?;
    let name = path_name(root_path);

    // Plain file — nothing incremental to do
    if !meta.is_dir() {
        let size = meta.len();
        return Ok(FileEntry {
            path: root_path.to_path_buf(),
            name,
            size,
            is_dir: false,
            is_unscanned: false,
            children: vec![],
            file_count: 1,
            modified: meta.modified().ok(),
        });
    }

    // Collect first-level children (non-symlinks, non-virtual-FS)
    let mut child_paths: Vec<PathBuf> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(root_path) {
        for entry in rd.flatten() {
            let p = entry.path();
            if let Ok(m) = std::fs::symlink_metadata(&p) {
                if m.file_type().is_symlink() {
                    continue;
                }
                // Skip virtual/squashfs mount points
                if m.is_dir() && should_skip(&p, skip) {
                    continue;
                }
                child_paths.push(p);
            }
        }
    }
    child_paths.sort_unstable();

    let num_children = child_paths.len();

    // Estimate a placeholder size per child so the treemap looks reasonable
    // before real sizes are known.
    let placeholder_size: u64 = if num_children > 0 {
        if let Some((total, avail)) = get_disk_info(root_path) {
            ((total.saturating_sub(avail)) / num_children as u64).max(1024 * 1024)
        } else {
            64 * 1024 * 1024 // 64 MiB fallback
        }
    } else {
        0
    };

    // Build placeholder list
    let placeholders: Vec<Arc<FileEntry>> = child_paths
        .iter()
        .map(|p| {
            Arc::new(FileEntry {
                path: p.clone(),
                name: path_name(p),
                size: placeholder_size,
                is_dir: std::fs::metadata(p).map(|m| m.is_dir()).unwrap_or(false),
                is_unscanned: true,
                children: vec![],
                file_count: 0,
                modified: None,
            })
        })
        .collect();

    // Emit initial view with all placeholders visible
    if !placeholders.is_empty() {
        let total_placeholder: u64 = placeholders.iter().map(|p| p.size).sum();
        let initial = Arc::new(FileEntry {
            path: root_path.to_path_buf(),
            name: name.clone(),
            size: total_placeholder,
            is_dir: true,
            is_unscanned: false,
            children: placeholders.clone(),
            file_count: 0,
            modified: meta.modified().ok(),
        });
        let _ = tx.send(ScanMessage::Update(initial));
    }

    // Scan each child; emit an Update after each one completes
    let bytes_count = Arc::new(AtomicU64::new(0));
    let files_count = Arc::new(AtomicU64::new(0));

    let mut completed: Vec<Arc<FileEntry>> = Vec::new();
    let mut total_size: u64 = 0;
    let mut total_files: u64 = 0;

    for (i, path) in child_paths.iter().enumerate() {
        if cancel.is_cancelled() {
            break;
        }

        let _ = tx.send(ScanMessage::Progress {
            path: path.clone(),
            bytes: bytes_count.load(Ordering::Relaxed),
            files: files_count.load(Ordering::Relaxed),
        });

        match scan_dir(path, tx, cancel, &bytes_count, &files_count, skip) {
            Ok(child) => {
                total_size += child.size;
                total_files += child.file_count;
                completed.push(Arc::new(child));
            }
            Err(_) => continue,
        }

        if cancel.is_cancelled() {
            break;
        }

        // Assemble partial root: completed entries + remaining placeholders
        let mut partial_children: Vec<Arc<FileEntry>> = completed.clone();
        for j in (i + 1)..num_children {
            partial_children.push(Arc::clone(&placeholders[j]));
        }

        sort_children(&mut partial_children);

        let partial_root = Arc::new(FileEntry {
            path: root_path.to_path_buf(),
            name: name.clone(),
            size: total_size + placeholders[i + 1..num_children].iter().map(|p| p.size).sum::<u64>(),
            is_dir: true,
            is_unscanned: false,
            children: partial_children,
            file_count: total_files,
            modified: meta.modified().ok(),
        });

        let _ = tx.send(ScanMessage::Update(partial_root));
    }

    // Final result without placeholders
    let mut final_children = completed;
    final_children.sort_unstable_by(|a, b| b.size.cmp(&a.size));

    Ok(FileEntry {
        path: root_path.to_path_buf(),
        name,
        size: total_size,
        is_dir: true,
        is_unscanned: false,
        children: final_children,
        file_count: total_files,
        modified: meta.modified().ok(),
    })
}

/// Recursively scan a path without emitting incremental updates (used for
/// individual children during the top-level incremental scan).
fn scan_dir(
    path: &Path,
    tx: &Sender<ScanMessage>,
    cancel: &CancelToken,
    bytes_scanned: &Arc<AtomicU64>,
    files_scanned: &Arc<AtomicU64>,
    skip: &HashSet<PathBuf>,
) -> Result<FileEntry, std::io::Error> {
    if cancel.is_cancelled() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "cancelled",
        ));
    }

    let meta = std::fs::symlink_metadata(path)?;
    let name = path_name(path);

    if !meta.is_dir() {
        let size = meta.len();
        bytes_scanned.fetch_add(size, Ordering::Relaxed);
        files_scanned.fetch_add(1, Ordering::Relaxed);
        return Ok(FileEntry {
            path: path.to_path_buf(),
            name,
            size,
            is_dir: false,
            is_unscanned: false,
            children: vec![],
            file_count: 1,
            modified: meta.modified().ok(),
        });
    }

    let rd = match std::fs::read_dir(path) {
        Ok(rd) => rd,
        Err(_) => {
            return Ok(FileEntry {
                path: path.to_path_buf(),
                name,
                size: 0,
                is_dir: true,
                is_unscanned: false,
                children: vec![],
                file_count: 0,
                modified: meta.modified().ok(),
            });
        }
    };

    let _ = tx.send(ScanMessage::Progress {
        path: path.to_path_buf(),
        bytes: bytes_scanned.load(Ordering::Relaxed),
        files: files_scanned.load(Ordering::Relaxed),
    });

    let mut children: Vec<Arc<FileEntry>> = Vec::new();
    let mut total_size: u64 = 0;
    let mut total_files: u64 = 0;

    for entry in rd {
        if cancel.is_cancelled() {
            break;
        }
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let child_path = entry.path();
        let child_meta = match std::fs::symlink_metadata(&child_path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if child_meta.file_type().is_symlink() {
            continue;
        }
        // Skip virtual/squashfs mount points at any depth
        if child_meta.is_dir() && should_skip(&child_path, skip) {
            continue;
        }
        match scan_dir(&child_path, tx, cancel, bytes_scanned, files_scanned, skip) {
            Ok(child) => {
                total_size += child.size;
                total_files += child.file_count;
                children.push(Arc::new(child));
            }
            Err(_) => continue,
        }
    }

    children.sort_unstable_by(|a, b| b.size.cmp(&a.size));

    Ok(FileEntry {
        path: path.to_path_buf(),
        name,
        size: total_size,
        is_dir: true,
        is_unscanned: false,
        children,
        file_count: total_files,
        modified: meta.modified().ok(),
    })
}

/// Sort children: real entries by size desc, unscanned at the end.
pub fn sort_children(children: &mut Vec<Arc<FileEntry>>) {
    children.sort_unstable_by(|a, b| {
        match (a.is_unscanned, b.is_unscanned) {
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            _ => b.size.cmp(&a.size),
        }
    });
}

/// Walk a path to find a descendant by a sequence of name segments.
pub fn find_descendant(root: &Arc<FileEntry>, path_segments: &[String]) -> Option<Arc<FileEntry>> {
    if path_segments.is_empty() {
        return Some(Arc::clone(root));
    }
    for child in &root.children {
        if child.name == path_segments[0] {
            return find_descendant(child, &path_segments[1..]);
        }
    }
    None
}

/// Return a new tree with the node at `remove_path` removed and ancestor
/// sizes updated. Returns `None` if `remove_path` == the root itself.
pub fn remove_from_tree(root: &Arc<FileEntry>, remove_path: &Path) -> Option<Arc<FileEntry>> {
    if root.path == remove_path {
        return None;
    }

    let mut new_children: Vec<Arc<FileEntry>> = Vec::new();
    let mut size_delta: i64 = 0;
    let mut file_delta: i64 = 0;

    for child in &root.children {
        if child.path == remove_path {
            size_delta -= child.size as i64;
            file_delta -= child.file_count as i64;
            // don't push — this is the deleted entry
        } else if remove_path.starts_with(&child.path) && !child.path.as_os_str().is_empty() {
            let old_size = child.size;
            let old_files = child.file_count;
            match remove_from_tree(child, remove_path) {
                Some(updated) => {
                    size_delta += updated.size as i64 - old_size as i64;
                    file_delta += updated.file_count as i64 - old_files as i64;
                    new_children.push(updated);
                }
                None => {
                    // Shouldn't reach here since child.path != remove_path
                    size_delta -= old_size as i64;
                    file_delta -= old_files as i64;
                }
            }
        } else {
            new_children.push(Arc::clone(child));
        }
    }

    new_children.sort_unstable_by(|a, b| {
        match (a.is_unscanned, b.is_unscanned) {
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            _ => b.size.cmp(&a.size),
        }
    });

    Some(Arc::new(FileEntry {
        path: root.path.clone(),
        name: root.name.clone(),
        size: ((root.size as i64) + size_delta).max(0) as u64,
        is_dir: root.is_dir,
        is_unscanned: root.is_unscanned,
        children: new_children,
        file_count: ((root.file_count as i64) + file_delta).max(0) as u64,
        modified: root.modified,
    }))
}

/// Get (total_bytes, available_bytes) for the filesystem containing `path`.
#[cfg(unix)]
pub fn get_disk_info(path: &Path) -> Option<(u64, u64)> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c_path.as_ptr(), &mut stat) == 0 {
            let bsize = stat.f_frsize as u64;
            if bsize == 0 {
                return None;
            }
            let total = stat.f_blocks.checked_mul(bsize)?;
            let available = stat.f_bavail.checked_mul(bsize)?;
            Some((total, available))
        } else {
            None
        }
    }
}

#[cfg(not(unix))]
pub fn get_disk_info(_path: &Path) -> Option<(u64, u64)> {
    None
}

fn path_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}
