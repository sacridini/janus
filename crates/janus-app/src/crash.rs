//! Crash reports (IDEIAS.md, phase 58): what a failure leaves for the next start and for an issue. A panic writes
//! `crashes/crash-<date>-<time>.txt` in Janus's data folder (the versions, the system, the GPU, the thread, the
//! message, the backtrace and the last lines of the log ring); on Windows a native failure (GDAL, a driver, an
//! invalid access) writes a minidump and a short text the same way, from the process's own unhandled-exception
//! filter. Everywhere, a marker `running-<pid>.json` is kept while the window is open and removed on a clean exit:
//! one whose process is gone means Janus did not close normally (a native crash on Linux and macOS, a kill), and
//! the next start says so (crash_ui.rs). Nothing is sent anywhere; the text is shown before an issue is opened.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// The lines the log ring keeps (the last ones go into a report).
pub const LOG_LINES: usize = 200;
/// A log line is cut at this many characters.
const LOG_LINE_CHARS: usize = 400;
/// Reports and dumps older than this are deleted at the start.
const KEEP_DAYS: u64 = 30;
/// A process writes this many reports at most (a thread panicking in a loop does not fill the disk).
const MAX_REPORTS: usize = 10;
/// Where an issue is opened, and how much of the report goes into its URL (GitHub refuses URLs of about 8 KB).
const ISSUE_URL: &str = "https://github.com/sacridini/janus/issues/new";
const ISSUE_BODY_CHARS: usize = 6000;
const ISSUE_BODY_ENCODED: usize = 7000;

static LOG: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
static GPU: Mutex<String> = Mutex::new(String::new());
static THREADS: AtomicUsize = AtomicUsize::new(0);
static DIR: Mutex<Option<PathBuf>> = Mutex::new(None);
static WRITTEN: AtomicUsize = AtomicUsize::new(0);

// ---- time ----

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// (year, month, day, hour, minute, second) in UTC.
fn utc(secs: u64) -> (i32, u32, u32, u32, u32, u32) {
    let (y, m, d) = janus_cube::dates::civil_from_days((secs / 86400) as i64);
    let r = secs % 86400;
    (y, m, d, (r / 3600) as u32, (r % 3600 / 60) as u32, (r % 60) as u32)
}

/// "20261118-102345", as the report's file name carries it.
pub fn stamp(secs: u64) -> String {
    let (y, mo, d, h, mi, s) = utc(secs);
    format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}")
}

/// "2026-11-18 10:23:45 UTC".
pub fn pretty(secs: u64) -> String {
    let (y, mo, d, h, mi, s) = utc(secs);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02} UTC")
}

// ---- what the report says about this process ----

/// The folder of the reports: `crashes` in the data folder, or the one the self-test set.
pub fn dir() -> PathBuf {
    let set = DIR.try_lock().ok().and_then(|d| d.clone());
    set.unwrap_or_else(|| crate::settings::app_data_dir().join("crashes"))
}

/// Another folder for the reports (the self-test's), None for the usual one.
pub fn set_dir(dir: Option<PathBuf>) {
    if let Ok(mut d) = DIR.lock() {
        *d = dir;
    }
    refresh_native();
}

/// The adapter and backend of the GPU in use, once it is made.
pub fn set_gpu(name: &str, backend: &str) {
    if let Ok(mut g) = GPU.lock() {
        *g = format!("{name} ({backend})");
    }
    refresh_native();
}

/// The processing threads in use.
pub fn set_threads(n: usize) {
    THREADS.store(n, Ordering::Relaxed);
    refresh_native();
}

/// The lines a report states about this build and machine.
pub fn environment() -> String {
    let gpu = GPU.try_lock().map(|g| g.clone()).unwrap_or_default();
    let gpu = if gpu.is_empty() { "not created yet".to_string() } else { gpu };
    format!(
        "Janus:      {}\nZeit:       {} (zeit-core {})\nGDAL:       {}\nSystem:     {} {}\nGPU:        {}\nThreads:    {}\n",
        env!("CARGO_PKG_VERSION"),
        janus_zeit::ZEIT_VERSION,
        janus_zeit::ZEIT_CORE_REV,
        janus_cube::gdal_version(),
        std::env::consts::OS,
        std::env::consts::ARCH,
        gpu,
        THREADS.load(Ordering::Relaxed),
    )
}

// ---- the log ring ----

/// A line for the ring: the last LOG_LINES go into a crash report. Cheap; may be called from any thread.
pub fn log(line: &str) {
    let (_, _, _, h, mi, s) = utc(now_secs());
    let mut text = format!("[{h:02}:{mi:02}:{s:02}] ");
    text.extend(line.chars().map(|c| if c == '\n' || c == '\r' { ' ' } else { c }).take(LOG_LINE_CHARS));
    if let Ok(mut q) = LOG.lock() {
        push_bounded(&mut q, text, LOG_LINES);
    }
}

fn push_bounded(q: &mut VecDeque<String>, line: String, max: usize) {
    while q.len() >= max {
        q.pop_front();
    }
    q.push_back(line);
}

/// The ring's lines, oldest first; empty when another thread holds it (a panic never waits).
fn log_snapshot() -> Vec<String> {
    LOG.try_lock().map(|q| q.iter().cloned().collect()).unwrap_or_default()
}

// ---- the report ----

/// What a failure knows of itself.
pub struct Facts<'a> {
    /// "panic", "unclean exit"
    pub kind: &'a str,
    pub secs: u64,
    pub thread: &'a str,
    pub message: &'a str,
    pub location: &'a str,
    pub backtrace: &'a str,
    pub log: &'a [String],
}

/// The report's text.
pub fn format_report(f: &Facts) -> String {
    let mut t =
        format!("Janus crash report\n==================\nTime (UTC): {}\nKind:       {}\n", pretty(f.secs), f.kind);
    t += &environment();
    if !f.thread.is_empty() {
        t += &format!("Thread:     {}\n", f.thread);
    }
    t += &format!("Message:    {}\n", f.message);
    if !f.location.is_empty() {
        t += &format!("Location:   {}\n", f.location);
    }
    if !f.backtrace.is_empty() {
        t += &format!("\nBacktrace\n---------\n{}\n", f.backtrace.trim_end());
    }
    t += "\nLast log lines\n--------------\n";
    if f.log.is_empty() {
        t += "(none)\n";
    }
    let from = f.log.len().saturating_sub(LOG_LINES);
    for l in &f.log[from..] {
        t += l;
        t.push('\n');
    }
    t
}

/// The file written: `crash-<stamp>.<ext>`, or `crash-<stamp>-<n>.<ext>` when that name is taken.
pub fn write_file(dir: &Path, secs: u64, ext: &str, bytes: &[u8]) -> Option<PathBuf> {
    use std::io::Write;
    std::fs::create_dir_all(dir).ok()?;
    for n in 0..100 {
        let name =
            if n == 0 { format!("crash-{}.{ext}", stamp(secs)) } else { format!("crash-{}-{n}.{ext}", stamp(secs)) };
        let path = dir.join(name);
        if let Ok(mut f) = std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            return f.write_all(bytes).is_ok().then_some(path);
        }
    }
    None
}

/// The hook: a panic's report in the folder, then the hook that was there (the default one prints to stderr). It
/// touches no state of the app (it may be borrowed) and never waits for a lock.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        panic_report(info);
        previous(info);
    }));
}

/// A backtrace taken in the hook starts with the capture's and the panic machinery's own frames: the text from the
/// frame that panicked (after the last `core::panicking` one among the first frames); whole when none is found.
fn trim_backtrace(bt: &str) -> String {
    let lines: Vec<&str> = bt.lines().collect();
    let is_frame = |l: &str| l.trim_start().split_once(':').is_some_and(|(n, _)| n.parse::<u32>().is_ok());
    let last = lines
        .iter()
        .take(40)
        .rposition(|l| is_frame(l) && (l.contains("core::panicking::") || l.contains("std::panicking::begin_panic")));
    let Some(last) = last else { return bt.to_string() };
    match lines[last + 1..].iter().position(|l| is_frame(l)) {
        Some(k) => lines[last + 1 + k..].join("\n"),
        None => bt.to_string(),
    }
}

fn panic_report(info: &std::panic::PanicHookInfo) {
    thread_local! {
        static BUSY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if BUSY.with(|b| b.replace(true)) || WRITTEN.fetch_add(1, Ordering::Relaxed) >= MAX_REPORTS {
        return;
    }
    let payload = info.payload();
    let message = payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "(no message)".into());
    let location = info.location().map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column())).unwrap_or_default();
    let thread = std::thread::current();
    let backtrace = trim_backtrace(&std::backtrace::Backtrace::force_capture().to_string());
    let secs = now_secs();
    let log = log_snapshot();
    let text = format_report(&Facts {
        kind: "panic",
        secs,
        thread: thread.name().unwrap_or("(unnamed)"),
        message: &message,
        location: &location,
        backtrace: &backtrace,
        log: &log,
    });
    write_file(&dir(), secs, "txt", text.as_bytes());
    BUSY.with(|b| b.set(false));
}

// ---- the marker of a run that has not ended ----

/// A `running-<pid>.json`: a window open (removed when it closes normally).
#[derive(Clone, Debug, PartialEq)]
pub struct Marker {
    pub pid: u32,
    pub started: u64,
    pub version: String,
    pub path: PathBuf,
}

fn marker_path(dir: &Path, pid: u32) -> PathBuf {
    dir.join(format!("running-{pid}.json"))
}

pub fn write_marker(dir: &Path, pid: u32, started: u64) {
    let _ = std::fs::create_dir_all(dir);
    let json = serde_json::json!({ "pid": pid, "started": started, "version": env!("CARGO_PKG_VERSION") });
    let _ = std::fs::write(marker_path(dir, pid), json.to_string());
}

pub fn remove_marker(dir: &Path, pid: u32) {
    let _ = std::fs::remove_file(marker_path(dir, pid));
}

fn read_markers(dir: &Path) -> Vec<Marker> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    let mut out: Vec<Marker> = rd
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with("running-") && n.ends_with(".json")
        })
        .filter_map(|e| {
            let v: serde_json::Value = serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok()?;
            Some(Marker {
                pid: v.get("pid")?.as_u64()? as u32,
                started: v.get("started")?.as_u64()?,
                version: v.get("version").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                path: e.path(),
            })
        })
        .collect();
    out.sort_by_key(|m| m.started);
    out
}

/// Whether a process of this id is running.
pub fn process_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, GetLastError};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        const STILL_ACTIVE: u32 = 259;
        // SAFETY: a handle opened, asked once and closed.
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return GetLastError() == ERROR_ACCESS_DENIED;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(h, &mut code) != 0;
            CloseHandle(h);
            !ok || code == STILL_ACTIVE
        }
    }
    #[cfg(unix)]
    {
        // SAFETY: signal 0 only checks that the process exists.
        unsafe { libc::kill(pid as i32, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) }
    }
}

/// The markers of runs that are gone (not this process, not alive): Janus did not close normally.
pub fn dead_markers(dir: &Path, own_pid: u32, alive: &dyn Fn(u32) -> bool) -> Vec<Marker> {
    read_markers(dir).into_iter().filter(|m| m.pid != own_pid && !alive(m.pid)).collect()
}

// ---- the start after a failure ----

/// What the notice says: the last run that did not end normally.
#[derive(Clone, Debug)]
pub struct Closed {
    /// when the report was written (its file's time)
    pub when: u64,
    pub text: String,
    pub file: PathBuf,
    /// the dead markers, removed when the notice is dismissed
    pub markers: Vec<PathBuf>,
    /// runs before it that also did not end normally
    pub earlier: usize,
}

/// The reports (name, time) of `dir` written at `since` or later, newest first.
fn reports_since(dir: &Path, since: u64) -> Vec<(u64, PathBuf)> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    let mut v: Vec<(u64, PathBuf)> = rd
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with("crash-") && n.ends_with(".txt")
        })
        .filter_map(|e| {
            let t = e.metadata().ok()?.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs();
            (t >= since).then_some((t, e.path()))
        })
        .collect();
    v.sort_by(|a, b| b.cmp(a));
    v
}

/// Whether the last run that left a marker behind ended badly, and its report: the newest written during that run
/// (a panic, a native crash on Windows), else one written now from the marker alone.
pub fn check_previous(dir: &Path, own_pid: u32, alive: &dyn Fn(u32) -> bool, now: u64) -> Option<Closed> {
    let dead = dead_markers(dir, own_pid, alive);
    let last = dead.last()?;
    let (when, file) = match reports_since(dir, last.started).into_iter().next() {
        Some(r) => r,
        None => {
            let message = format!(
                "Janus {} (started {}) did not close normally and left no report: it was ended from outside, or a \
                 native failure stopped it (Linux and macOS leave no more than this).",
                last.version,
                pretty(last.started)
            );
            let text = format_report(&Facts {
                kind: "unclean exit",
                secs: now,
                thread: "",
                message: &message,
                location: "",
                backtrace: "",
                log: &[],
            });
            let file = write_file(dir, now, "txt", text.as_bytes())?;
            (now, file)
        }
    };
    let text = std::fs::read_to_string(&file).ok()?;
    Some(Closed { when, text, file, markers: dead.iter().map(|m| m.path.clone()).collect(), earlier: dead.len() - 1 })
}

/// The notice dismissed: the dead markers go, so the next start says nothing.
pub fn dismiss(closed: &Closed) {
    for m in &closed.markers {
        let _ = std::fs::remove_file(m);
    }
}

/// Reports and dumps older than KEEP_DAYS deleted.
pub fn remove_old(dir: &Path, now: u64) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let old = e
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .is_some_and(|t| now.saturating_sub(t.as_secs()) > KEEP_DAYS * 86400);
        if name.starts_with("crash-") && old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// At the start of a window: the old reports deleted, what the last run left, then this run's marker.
pub fn start_window() -> Option<Closed> {
    let (dir, pid, now) = (dir(), std::process::id(), now_secs());
    let _ = std::fs::create_dir_all(&dir);
    remove_old(&dir, now);
    let closed = check_previous(&dir, pid, &process_alive, now);
    write_marker(&dir, pid, now);
    closed
}

/// The window closed normally: this run's marker goes.
pub fn clean_exit() {
    remove_marker(&dir(), std::process::id());
}

// ---- an issue ----

/// Text for a URL's query: everything but the unreserved characters as %XX.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// GitHub's new-issue page with the title and the report filled in, the report cut to what a URL carries (the
/// note says where the whole of it is).
pub fn issue_url(title: &str, report: &str) -> String {
    let mut body = String::new();
    let mut encoded = 0;
    let mut cut = false;
    for (n, c) in report.chars().enumerate() {
        let len = percent_encode(c.encode_utf8(&mut [0; 4])).len();
        if n >= ISSUE_BODY_CHARS || encoded + len > ISSUE_BODY_ENCODED {
            cut = true;
            break;
        }
        body.push(c);
        encoded += len;
    }
    if cut {
        body += "\n\n[cut here: the whole report is in Janus's crashes folder]\n";
    }
    format!("{ISSUE_URL}?title={}&body={}", percent_encode(title), percent_encode(&body))
}

// ---- --crash-test ----

/// `--crash-test panic|native`: the failure, on the main thread.
pub fn provoke(mode: &str) {
    match mode {
        "panic" => panic!("--crash-test panic: a panic on purpose"),
        "native" => {
            log("--crash-test native: a write to a null pointer");
            // SAFETY: none; this is the invalid access the option asks for.
            unsafe { std::ptr::null_mut::<u8>().write_volatile(1) };
        }
        _ => {}
    }
}

// ---- native failures (Windows) ----

/// On Windows: the filter that writes the minidump. Elsewhere nothing: the marker covers them.
pub fn install_native_handler() {
    #[cfg(windows)]
    native::install();
}

fn refresh_native() {
    #[cfg(windows)]
    native::refresh();
}

#[cfg(windows)]
mod native {
    use std::fmt::Write as _;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Mutex, OnceLock};

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, HMODULE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CREATE_ALWAYS, CreateFileW, FILE_ATTRIBUTE_NORMAL, WriteFile};
    use windows_sys::Win32::System::Diagnostics::Debug::{
        EXCEPTION_POINTERS, MINIDUMP_EXCEPTION_INFORMATION, MiniDumpNormal, MiniDumpWithThreadInfo, MiniDumpWriteDump,
        SetUnhandledExceptionFilter,
    };
    use windows_sys::Win32::System::LibraryLoader::{
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, GetModuleFileNameW,
        GetModuleHandleExW,
    };
    use windows_sys::Win32::System::SystemInformation::GetSystemTime;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId};

    const GENERIC_WRITE: u32 = 0x4000_0000;
    const EXCEPTION_EXECUTE_HANDLER: i32 = 1;

    /// The folder (UTF-16) and the lines about this build, made before the failure: the filter allocates nothing it
    /// can avoid.
    static DIR: OnceLock<Vec<u16>> = OnceLock::new();
    static HEADER: Mutex<String> = Mutex::new(String::new());
    static BUSY: AtomicBool = AtomicBool::new(false);

    pub fn install() {
        let dir: Vec<u16> = super::dir().to_string_lossy().encode_utf16().collect();
        let _ = std::fs::create_dir_all(super::dir());
        let _ = DIR.set(dir);
        refresh();
        // SAFETY: a filter of the right signature, kept for the life of the process.
        unsafe { SetUnhandledExceptionFilter(Some(filter)) };
    }

    pub fn refresh() {
        let text = super::environment();
        if let Ok(mut h) = HEADER.lock() {
            *h = text;
        }
    }

    /// A file written without allocating: the text formatted into a buffer on the stack, flushed when it fills.
    struct Out {
        h: HANDLE,
        buf: [u8; 2048],
        n: usize,
    }

    impl Out {
        fn flush(&mut self) {
            if self.n > 0 {
                let mut written = 0u32;
                // SAFETY: the buffer's first n bytes, a handle open for writing.
                unsafe { WriteFile(self.h, self.buf.as_ptr(), self.n as u32, &mut written, std::ptr::null_mut()) };
                self.n = 0;
            }
        }
    }

    impl std::fmt::Write for Out {
        fn write_str(&mut self, s: &str) -> std::fmt::Result {
            for chunk in s.as_bytes().chunks(self.buf.len()) {
                if self.n + chunk.len() > self.buf.len() {
                    self.flush();
                }
                self.buf[self.n..self.n + chunk.len()].copy_from_slice(chunk);
                self.n += chunk.len();
            }
            Ok(())
        }
    }

    /// A short ASCII text on the stack (a file name).
    struct Name {
        b: [u8; 64],
        n: usize,
    }

    impl std::fmt::Write for Name {
        fn write_str(&mut self, s: &str) -> std::fmt::Result {
            let room = self.b.len() - self.n;
            let k = s.len().min(room);
            self.b[self.n..self.n + k].copy_from_slice(&s.as_bytes()[..k]);
            self.n += k;
            Ok(())
        }
    }

    /// The file `<folder>\crash-<stamp>.<ext>` created: its handle.
    fn create(stamp: &str, ext: &str) -> Option<HANDLE> {
        let dir = DIR.get()?;
        let mut path = [0u16; 700];
        if dir.len() + 40 > path.len() {
            return None;
        }
        path[..dir.len()].copy_from_slice(dir);
        let mut name = Name { b: [0; 64], n: 0 };
        let _ = write!(name, "\\crash-{stamp}.{ext}");
        for (k, b) in name.b[..name.n].iter().enumerate() {
            path[dir.len() + k] = u16::from(*b);
        }
        // SAFETY: a NUL-terminated path in a buffer that outlives the call.
        let h = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_WRITE,
                0,
                std::ptr::null(),
                CREATE_ALWAYS,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        (h != INVALID_HANDLE_VALUE).then_some(h)
    }

    fn exception_name(code: u32) -> &'static str {
        match code {
            0xC000_0005 => "access violation",
            0xC000_00FD => "stack overflow",
            0xC000_0094 => "integer division by zero",
            0xC000_001D => "illegal instruction",
            0xC000_0006 => "in-page error",
            0xC000_0409 => "stack buffer overrun",
            0xE06D_7363 => "C++ exception",
            0x8000_0003 => "breakpoint",
            _ => "exception",
        }
    }

    /// The failure: the minidump and a short text, then the process ends with no dialog of Windows Error Reporting.
    unsafe extern "system" fn filter(info: *const EXCEPTION_POINTERS) -> i32 {
        if BUSY.swap(true, Ordering::SeqCst) || info.is_null() {
            return EXCEPTION_EXECUTE_HANDLER;
        }
        // SAFETY: Windows hands the filter valid pointers for the failure.
        unsafe {
            let mut now = std::mem::zeroed();
            GetSystemTime(&mut now);
            let mut name = Name { b: [0; 64], n: 0 };
            let _ = write!(
                name,
                "{:04}{:02}{:02}-{:02}{:02}{:02}",
                now.wYear, now.wMonth, now.wDay, now.wHour, now.wMinute, now.wSecond
            );
            let stamp = std::str::from_utf8(&name.b[..name.n]).unwrap_or("unknown");
            let record = (*info).ExceptionRecord;
            let (code, address) = if record.is_null() {
                (0, 0)
            } else {
                ((*record).ExceptionCode as u32, (*record).ExceptionAddress as usize)
            };

            let dumped = if let Some(h) = create(stamp, "dmp") {
                let exception = MINIDUMP_EXCEPTION_INFORMATION {
                    ThreadId: GetCurrentThreadId(),
                    ExceptionPointers: info as *mut EXCEPTION_POINTERS,
                    ClientPointers: 0,
                };
                let ok = MiniDumpWriteDump(
                    GetCurrentProcess(),
                    GetCurrentProcessId(),
                    h,
                    MiniDumpNormal | MiniDumpWithThreadInfo,
                    &exception,
                    std::ptr::null(),
                    std::ptr::null(),
                );
                CloseHandle(h);
                ok != 0
            } else {
                false
            };

            if let Some(h) = create(stamp, "txt") {
                let mut out = Out { h, buf: [0; 2048], n: 0 };
                let _ = write!(
                    out,
                    "Janus crash report\n==================\nTime (UTC): {:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC\n\
                     Kind:       native exception\n",
                    now.wYear, now.wMonth, now.wDay, now.wHour, now.wMinute, now.wSecond
                );
                if let Ok(header) = HEADER.try_lock() {
                    let _ = out.write_str(&header);
                }
                let _ = writeln!(out, "Message:    {} (0x{code:08X})", exception_name(code));
                if code == 0xC000_0005 && !record.is_null() && (*record).NumberParameters >= 2 {
                    let kind = match (*record).ExceptionInformation[0] {
                        0 => "read of",
                        1 => "write to",
                        8 => "execution of",
                        _ => "access to",
                    };
                    let _ = writeln!(out, "Access:     {kind} address 0x{:X}", (*record).ExceptionInformation[1]);
                }
                let _ = write!(out, "Address:    0x{address:X}");
                // the module the address is in, and the offset in it
                let mut module: HMODULE = std::ptr::null_mut();
                let flags = GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT;
                if GetModuleHandleExW(flags, address as *const u16, &mut module) != 0 {
                    let mut file = [0u16; 520];
                    let n = GetModuleFileNameW(module, file.as_mut_ptr(), file.len() as u32) as usize;
                    let file = &file[..n.min(520)];
                    let from = file.iter().rposition(|&c| c == u16::from(b'\\')).map_or(0, |p| p + 1);
                    let _ = out.write_str(" (");
                    for &c in &file[from..] {
                        let _ = out.write_char(char::from_u32(u32::from(c)).filter(char::is_ascii).unwrap_or('?'));
                    }
                    let _ = write!(out, " + 0x{:X})", address.wrapping_sub(module as usize));
                }
                let _ = write!(out, "\nThread id:  {}\n", GetCurrentThreadId());
                if dumped {
                    let _ = writeln!(out, "Minidump:   crash-{stamp}.dmp, in this folder");
                } else {
                    let _ = out.write_str("Minidump:   could not be written\n");
                }
                let _ = out.write_str("\nLast log lines\n--------------\n");
                match super::LOG.try_lock() {
                    Ok(q) => {
                        for l in q.iter() {
                            let _ = out.write_str(l);
                            let _ = out.write_char('\n');
                        }
                    }
                    Err(_) => {
                        let _ = out.write_str("(the log was busy)\n");
                    }
                }
                out.flush();
                CloseHandle(h);
            }
        }
        EXCEPTION_EXECUTE_HANDLER
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("janus-crash-test-{name}-{}-{}", std::process::id(), now_secs()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn crash_report_has_its_fields() {
        let log = vec!["[10:00:00] opened a series".to_string(), "[10:00:01] export started".to_string()];
        let text = format_report(&Facts {
            kind: "panic",
            secs: 1_794_000_000,
            thread: "main",
            message: "index out of bounds",
            location: "src/map.rs:10:5",
            backtrace: "   0: janus::map::draw\n",
            log: &log,
        });
        for field in [
            "Janus crash report",
            "Time (UTC): 2026-",
            "Kind:       panic",
            &format!("Janus:      {}", env!("CARGO_PKG_VERSION")),
            &format!("zeit-core {}", janus_zeit::ZEIT_CORE_REV),
            "GDAL:       ",
            &format!("System:     {} {}", std::env::consts::OS, std::env::consts::ARCH),
            "GPU:        ",
            "Threads:    ",
            "Thread:     main",
            "Message:    index out of bounds",
            "Location:   src/map.rs:10:5",
            "janus::map::draw",
            "[10:00:01] export started",
        ] {
            assert!(text.contains(field), "{field:?} missing from\n{text}");
        }
    }

    #[test]
    fn crash_backtrace_starts_at_the_panic() {
        let bt = "   0: std::backtrace::Backtrace::force_capture\n             at lib.rs:1\n   1: janus::crash::hook\n   \
                  2: core::panicking::panic_fmt\n             at panicking.rs:80\n   3: janus::app::ui\n   4: egui::run\n";
        assert_eq!(trim_backtrace(bt), "   3: janus::app::ui\n   4: egui::run");
        assert_eq!(trim_backtrace("   0: a\n   1: b\n"), "   0: a\n   1: b\n");
    }

    #[test]
    fn crash_log_ring_is_bounded() {
        let mut q = VecDeque::new();
        for i in 0..(LOG_LINES + 50) {
            push_bounded(&mut q, format!("line {i}"), LOG_LINES);
        }
        assert_eq!(q.len(), LOG_LINES);
        assert_eq!(q.front().unwrap(), "line 50");
        assert_eq!(q.back().unwrap(), &format!("line {}", LOG_LINES + 49));
        // a report carries at most the last LOG_LINES of what it is given
        let many: Vec<String> = (0..LOG_LINES + 30).map(|i| format!("l{i}")).collect();
        let text = format_report(&Facts {
            kind: "panic",
            secs: 0,
            thread: "",
            message: "m",
            location: "",
            backtrace: "",
            log: &many,
        });
        assert!(text.contains("\nl229\n") && text.contains("\nl30\n") && !text.contains("\nl29\n"));
    }

    #[test]
    fn crash_file_names_and_times() {
        assert_eq!(stamp(0), "19700101-000000");
        assert_eq!(stamp(1_763_461_425), "20251118-102345");
        assert_eq!(pretty(1_763_461_425), "2025-11-18 10:23:45 UTC");
        let dir = temp("names");
        let a = write_file(&dir, 1_763_461_425, "txt", b"one").unwrap();
        let b = write_file(&dir, 1_763_461_425, "txt", b"two").unwrap();
        assert_eq!(a.file_name().unwrap().to_string_lossy(), "crash-20251118-102345.txt");
        assert_eq!(b.file_name().unwrap().to_string_lossy(), "crash-20251118-102345-1.txt");
        assert_eq!(std::fs::read(&a).unwrap(), b"one");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn crash_marker_of_a_dead_process_is_found() {
        let dir = temp("dead");
        let now = now_secs();
        write_marker(&dir, 111, now - 100);
        write_marker(&dir, 222, now - 50);
        write_marker(&dir, 333, now - 10);
        // 222 is alive, 333 is this process
        let alive = |pid: u32| pid == 222;
        let dead = dead_markers(&dir, 333, &alive);
        assert_eq!(dead.iter().map(|m| m.pid).collect::<Vec<_>>(), vec![111]);
        // no report was left: one is written from the marker
        let closed = check_previous(&dir, 333, &alive, now).expect("a run that did not end");
        assert!(closed.text.contains("Kind:       unclean exit"), "{}", closed.text);
        assert_eq!(closed.earlier, 0);
        assert!(closed.file.exists());
        // dismissed: the dead marker goes, the live ones stay, and the next start says nothing
        dismiss(&closed);
        assert!(check_previous(&dir, 333, &alive, now).is_none());
        assert!(marker_path(&dir, 222).exists() && marker_path(&dir, 333).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn crash_report_of_the_run_that_ended_is_the_one_shown() {
        let dir = temp("panic");
        let now = now_secs();
        write_marker(&dir, 111, now - 100);
        let text = format_report(&Facts {
            kind: "panic",
            secs: now,
            thread: "main",
            message: "boom",
            location: "a.rs:1:1",
            backtrace: "",
            log: &[],
        });
        let file = write_file(&dir, now, "txt", text.as_bytes()).unwrap();
        let closed = check_previous(&dir, 7, &|_| false, now).unwrap();
        assert_eq!(closed.file, file);
        assert!(closed.text.contains("Message:    boom"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn crash_clean_exit_leaves_nothing() {
        let dir = temp("clean");
        write_marker(&dir, 444, now_secs());
        remove_marker(&dir, 444);
        assert!(check_previous(&dir, 1, &|_| false, now_secs()).is_none());
        // this very process is alive
        write_marker(&dir, std::process::id(), now_secs());
        assert!(dead_markers(&dir, 1, &process_alive).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn crash_old_reports_are_deleted() {
        let dir = temp("old");
        let file = write_file(&dir, 1_000_000_000, "txt", b"old").unwrap();
        let other = dir.join("notes.txt");
        std::fs::write(&other, "keep").unwrap();
        // their files' times are now: 31 days later they are old, 29 days later they are not
        remove_old(&dir, now_secs() + 29 * 86400);
        assert!(file.exists());
        remove_old(&dir, now_secs() + 31 * 86400);
        assert!(!file.exists() && other.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn crash_issue_url_is_encoded_and_cut() {
        assert_eq!(percent_encode("a b&c=d\n\u{e9}"), "a%20b%26c%3Dd%0A%C3%A9");
        let url = issue_url("Janus closed", "line 1\nline 2");
        assert!(url.starts_with("https://github.com/sacridini/janus/issues/new?title=Janus%20closed&body="));
        assert!(url.ends_with("line%201%0Aline%202"), "{url}");
        let long = "x".repeat(20_000);
        let url = issue_url("t", &long);
        assert!(url.len() < 8000, "{} bytes", url.len());
        assert!(url.contains("cut%20here"));
        // accents and newlines take more room encoded: still within the URL's limit
        let hard = "\u{e9}\n".repeat(5000);
        let url = issue_url("t", &hard);
        assert!(url.len() < 8000, "{} bytes", url.len());
        assert!(url.contains("cut%20here"));
    }
}
