//! What depends on the system (src/platform.cpp): the Files panel's roots, opening a folder in the file manager.

/// The home folder, then the drives (Windows) or the root and the mounted volumes.
pub fn root_folders() -> Vec<String> {
    let mut out = Vec::new();
    #[cfg(windows)]
    {
        if let Some(home) = std::env::var_os("USERPROFILE") {
            out.push(home.to_string_lossy().into_owned());
        }
        unsafe extern "system" {
            fn GetLogicalDrives() -> u32;
        }
        // the drives the system lists (no access to them: a disconnected share must not stall)
        let mask = unsafe { GetLogicalDrives() };
        for i in 0..26u8 {
            if mask & (1 << i) != 0 {
                out.push(format!("{}:\\", char::from(b'A' + i)));
            }
        }
    }
    #[cfg(not(windows))]
    {
        out.push(std::env::var("HOME").unwrap_or_else(|_| "/".into()));
        out.push("/".into());
        #[cfg(target_os = "macos")]
        out.push("/Volumes".into());
        #[cfg(not(target_os = "macos"))]
        for m in ["/media", "/mnt"] {
            if std::fs::read_dir(m).is_ok_and(|mut d| d.next().is_some()) {
                out.push(m.into());
            }
        }
    }
    out
}

/// The folder (or file) in the system's file manager.
pub fn open_in_explorer(path: &str) {
    #[cfg(windows)]
    let opener = "explorer";
    #[cfg(target_os = "macos")]
    let opener = "open";
    #[cfg(all(unix, not(target_os = "macos")))]
    let opener = "xdg-open";
    let _ = std::process::Command::new(opener).arg(path).spawn();
}
