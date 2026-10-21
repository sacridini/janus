//! The disk caches of the cache folder (overviews, .januscube; full-resolution caches, .janusfull) share one
//! budget, regulated from the disk that holds the folder (src/disk_cache.cpp). Automatic (the default): a tenth of
//! the disk, 8 to 256 GB; or a size set in Settings. Either way a reserve of the disk stays free (a tenth, 10 to
//! 100 GB): as the disk fills up the budget shrinks, and the least recently opened series leave first.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

pub const OVERVIEW_EXT: &str = "januscube";
pub const FULLRES_EXT: &str = "janusfull";

const GB: u64 = 1 << 30;

pub fn auto_cap(disk_bytes: u64) -> u64 {
    if disk_bytes == 0 { 64 * GB } else { (disk_bytes / 10).clamp(8 * GB, 256 * GB) }
}

pub fn reserve(disk_bytes: u64) -> u64 {
    if disk_bytes == 0 { 0 } else { (disk_bytes / 10).clamp(10 * GB, 100 * GB) }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Budget {
    /// what the caches may take now
    pub bytes: u64,
    /// automatic or set by hand
    pub cap: u64,
    /// what they take
    pub used: u64,
    pub overview: u64,
    pub full_res: u64,
    /// the folder's disk (0: unknown)
    pub disk: u64,
    pub available: u64,
    pub reserve: u64,
    pub automatic: bool,
}

impl Budget {
    /// Shrunk to keep the reserve free.
    pub fn by_disk(&self) -> bool {
        self.bytes < self.cap
    }
}

/// The size and free space of the disk holding `dir`.
pub fn disk_space(dir: &Path) -> Option<(u64, u64)> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        unsafe extern "system" {
            fn GetDiskFreeSpaceExW(dir: *const u16, avail: *mut u64, total: *mut u64, free: *mut u64) -> i32;
        }
        let wide: Vec<u16> = dir.as_os_str().encode_wide().chain([0]).collect();
        let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
        // SAFETY: a NUL-terminated path and three out values.
        let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut avail, &mut total, &mut free) };
        (ok != 0).then_some((total, avail))
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: a NUL-terminated path and the struct to fill.
        let ok = unsafe { libc::statvfs(c.as_ptr(), &mut st) } == 0;
        ok.then(|| (st.f_blocks as u64 * st.f_frsize as u64, st.f_bavail as u64 * st.f_frsize as u64))
    }
}

/// `setting`: bytes, 0 = automatic.
pub fn budget(dir: &Path, setting: u64) -> Budget {
    let (disk, available) = disk_space(dir).unwrap_or((0, 0));
    let overview = folder_bytes(dir, OVERVIEW_EXT);
    let full_res = folder_bytes(dir, FULLRES_EXT);
    let used = overview + full_res;
    let automatic = setting == 0;
    let cap = if automatic { auto_cap(disk) } else { setting };
    let reserve = reserve(disk);
    let mut bytes = cap;
    if disk > 0 {
        let room = (used + available).saturating_sub(reserve);
        bytes = cap.min(room);
    }
    Budget { bytes, cap, used, overview, full_res, disk, available, reserve, automatic }
}

fn is_cache(p: &Path) -> bool {
    p.extension().is_some_and(|e| e == OVERVIEW_EXT || e == FULLRES_EXT)
}

/// Bytes of the folder's files with extension `ext`.
pub fn folder_bytes(dir: &Path, ext: &str) -> u64 {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == ext))
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

/// Removes the least recently opened caches not in use until the folder's take at most `max_bytes`; returns what
/// they take then.
pub fn prune(dir: &Path, max_bytes: u64) -> u64 {
    let mut entries: Vec<(PathBuf, std::time::SystemTime, u64)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| is_cache(&e.path()))
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((e.path(), m.modified().ok()?, m.len()))
        })
        .collect();
    let mut total: u64 = entries.iter().map(|e| e.2).sum();
    entries.sort_by_key(|e| e.1);
    for (p, _, size) in entries {
        if total <= max_bytes {
            break;
        }
        if in_use(&p) || std::fs::remove_file(&p).is_err() {
            continue;
        }
        total -= size;
    }
    total
}

/// Room for `need` more bytes in the budget, older caches pruned to make it; else why it cannot be had.
pub fn make_room(dir: &Path, setting: u64, need: u64) -> Result<(), String> {
    let b = budget(dir, setting);
    let gb = |v: u64| v as f64 / GB as f64;
    if need > b.bytes {
        return Err(if b.by_disk() {
            format!(
                "needs {:.1} GB; the disk has {:.1} GB free and Janus keeps {:.0} GB of it free",
                gb(need),
                gb(b.available),
                gb(b.reserve)
            )
        } else {
            format!("needs {:.1} GB, more than the cache budget ({:.0} GB)", gb(need), gb(b.bytes))
        });
    }
    if prune(dir, b.bytes - need) + need > b.bytes {
        return Err(format!(
            "needs {:.1} GB; the cache budget ({:.0} GB) is taken by caches in use",
            gb(need),
            gb(b.bytes)
        ));
    }
    Ok(())
}

/// Cache files open in this process, by file name (the folder is the same for all): never pruned or cleared.
static IN_USE: LazyLock<Mutex<HashMap<String, usize>>> = LazyLock::new(Default::default);

fn name_of(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

pub fn retain(path: &Path) {
    *IN_USE.lock().unwrap().entry(name_of(path)).or_default() += 1;
}

pub fn release(path: &Path) {
    let mut m = IN_USE.lock().unwrap();
    let name = name_of(path);
    if let Some(n) = m.get_mut(&name) {
        *n -= 1;
        if *n == 0 {
            m.remove(&name);
        }
    }
}

pub fn in_use(path: &Path) -> bool {
    IN_USE.lock().unwrap().get(&name_of(path)).is_some_and(|&n| n > 0)
}

/// The cache was opened: it leaves last (the least recently opened go first).
pub fn touch(path: &Path) {
    if let Ok(f) = std::fs::File::options().write(true).open(path) {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps() {
        assert_eq!(auto_cap(0), 64 * GB);
        assert_eq!(auto_cap(500 * GB), 50 * GB);
        assert_eq!(auto_cap(10 * 1024 * GB), 256 * GB);
        assert_eq!(reserve(2048 * GB), 100 * GB);
        assert_eq!(reserve(50 * GB), 10 * GB);
    }

    #[test]
    fn prune_keeps_what_is_in_use() {
        let dir = std::env::temp_dir().join(format!("janus-prune-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.januscube"), dir.join("b.janusfull"));
        std::fs::write(&a, vec![0u8; 1000]).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&b, vec![0u8; 1000]).unwrap();
        std::fs::write(dir.join("other.txt"), vec![0u8; 5000]).unwrap();
        retain(&a);
        // the oldest (a) is in use: b goes instead
        assert_eq!(prune(&dir, 1000), 1000);
        assert!(a.exists() && !b.exists());
        release(&a);
        assert_eq!(prune(&dir, 0), 0);
        assert!(!a.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
