//! Whether a file sits on a spinning disk (src/platform.cpp, isOnRotationalDisk): then one sequential reader, and
//! random reads wait for the overview (session.rs).

/// Windows: the volume's seek penalty (IOCTL_STORAGE_QUERY_PROPERTY). Network and UNC paths count as no penalty.
#[cfg(windows)]
pub fn is_on_rotational_disk(path: &std::path::Path) -> bool {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    #[repr(C)]
    struct PropertyQuery {
        property_id: u32,
        query_type: u32,
        additional: [u8; 4],
    }
    #[repr(C)]
    #[derive(Default)]
    struct SeekPenalty {
        version: u32,
        size: u32,
        incurs_seek_penalty: u8,
        _pad: [u8; 3],
    }
    unsafe extern "system" {
        fn GetVolumePathNameW(file: *const u16, volume: *mut u16, len: u32) -> i32;
        fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            sa: *mut c_void,
            disp: u32,
            flags: u32,
            tmpl: *mut c_void,
        ) -> isize;
        fn DeviceIoControl(
            h: isize,
            code: u32,
            inb: *const c_void,
            insz: u32,
            outb: *mut c_void,
            outsz: u32,
            ret: *mut u32,
            ov: *mut c_void,
        ) -> i32;
        fn CloseHandle(h: isize) -> i32;
    }
    const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;
    const STORAGE_DEVICE_SEEK_PENALTY_PROPERTY: u32 = 7;
    const FILE_SHARE_READ_WRITE: u32 = 3;
    const OPEN_EXISTING: u32 = 3;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut vol = [0u16; 260];
    // SAFETY: NUL-terminated input, output buffer of the length given.
    if unsafe { GetVolumePathNameW(wide.as_ptr(), vol.as_mut_ptr(), vol.len() as u32) } == 0 {
        return false;
    }
    let len = vol.iter().position(|&c| c == 0).unwrap_or(0);
    let vol = String::from_utf16_lossy(&vol[..len]);
    // "C:\" -> "\\.\C:"; network and UNC volumes have no drive letter
    if vol.len() < 2 || vol.as_bytes()[1] != b':' {
        return false;
    }
    let dev: Vec<u16> = format!(r"\\.\{}", &vol[..2]).encode_utf16().chain([0]).collect();
    // SAFETY: plain Win32 calls on a handle closed below.
    unsafe {
        let h = CreateFileW(
            dev.as_ptr(),
            0,
            FILE_SHARE_READ_WRITE,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        );
        if h == -1 {
            return false;
        }
        let q = PropertyQuery { property_id: STORAGE_DEVICE_SEEK_PENALTY_PROPERTY, query_type: 0, additional: [0; 4] };
        let mut d = SeekPenalty::default();
        let mut bytes = 0u32;
        let ok = DeviceIoControl(
            h,
            IOCTL_STORAGE_QUERY_PROPERTY,
            &q as *const _ as *const c_void,
            size_of::<PropertyQuery>() as u32,
            &mut d as *mut _ as *mut c_void,
            size_of::<SeekPenalty>() as u32,
            &mut bytes,
            std::ptr::null_mut(),
        );
        CloseHandle(h);
        ok != 0 && d.incurs_seek_penalty != 0
    }
}

/// Linux: /sys/dev/block/MAJOR:MINOR/queue/rotational, a partition's in its parent disk's folder.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn is_on_rotational_disk(path: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(m) = std::fs::metadata(path) else { return false };
    let dev = m.dev();
    let (major, minor) = (((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff), (dev & 0xff) | ((dev >> 12) & !0xff));
    let base = format!("/sys/dev/block/{major}:{minor}");
    ["/queue/rotational", "/../queue/rotational"]
        .iter()
        .find_map(|rel| std::fs::read_to_string(format!("{base}{rel}")).ok())
        .is_some_and(|s| s.trim() == "1")
}

/// Macs with Apple Silicon have SSDs; external HDDs are rare.
#[cfg(target_os = "macos")]
pub fn is_on_rotational_disk(_path: &std::path::Path) -> bool {
    false
}
