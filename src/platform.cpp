#include "platform.hpp"

#include <cstdio>
#include <cstdlib>
#include <filesystem>

#ifdef _WIN32
#define NOMINMAX
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <objbase.h>
#include <shellapi.h>
#include <shlobj.h>
#include <shobjidl.h>
#include <winioctl.h>
#else
#include <fstream>
#include <memory>
#include <spawn.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>
#ifdef __APPLE__
#include <mach-o/dyld.h>
#else
#include <sys/sysmacros.h>
#endif
extern char** environ;
#endif

namespace fs = std::filesystem;

namespace platform {

#ifdef _WIN32

static std::string toUtf8(const wchar_t* w) {
    int len = WideCharToMultiByte(CP_UTF8, 0, w, -1, nullptr, 0, nullptr, nullptr);
    if (len <= 1) return {};
    std::string out(size_t(len - 1), '\0');
    WideCharToMultiByte(CP_UTF8, 0, w, -1, out.data(), len, nullptr, nullptr);
    return out;
}

static std::wstring toWide(const std::string& s) {
    int len = MultiByteToWideChar(CP_UTF8, 0, s.c_str(), -1, nullptr, 0);
    if (len <= 1) return {};
    std::wstring out(size_t(len - 1), L'\0');
    MultiByteToWideChar(CP_UTF8, 0, s.c_str(), -1, out.data(), len);
    return out;
}

void init() { CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE); }

std::vector<std::string> commandLineArgs(int, char**) {
    // main()'s argv uses the ANSI codepage; non-ASCII paths would break.
    int n = 0;
    LPWSTR* wargv = CommandLineToArgvW(GetCommandLineW(), &n);
    std::vector<std::string> out;
    for (int i = 1; i < n; ++i) out.push_back(toUtf8(wargv[i]));
    LocalFree(wargv);
    return out;
}

void attachParentConsole() {
    // Output already redirected (pipe/file): use it as is.
    HANDLE out = GetStdHandle(STD_OUTPUT_HANDLE);
    if (out && out != INVALID_HANDLE_VALUE && GetFileType(out) != FILE_TYPE_UNKNOWN) return;
    if (AttachConsole(ATTACH_PARENT_PROCESS)) {
        FILE* f;
        freopen_s(&f, "CONOUT$", "w", stdout);
        freopen_s(&f, "CONOUT$", "w", stderr);
        SetConsoleOutputCP(CP_UTF8);
        std::printf("\n");
    }
}

std::string exeDir() {
    wchar_t buf[MAX_PATH];
    GetModuleFileNameW(nullptr, buf, MAX_PATH);
    return fs::path(buf).parent_path().u8string();
}

std::string appDataDir() {
    PWSTR p = nullptr;
    fs::path dir;
    if (SUCCEEDED(SHGetKnownFolderPath(FOLDERID_LocalAppData, 0, nullptr, &p))) dir = fs::path(p) / "tsv";
    CoTaskMemFree(p);
    if (dir.empty()) dir = fs::temp_directory_path() / "tsv";
    std::error_code ec;
    fs::create_directories(dir, ec);
    return dir.u8string();
}

static std::vector<std::string> runDialog(bool folders) {
    std::vector<std::string> out;
    IFileOpenDialog* dlg = nullptr;
    if (FAILED(CoCreateInstance(CLSID_FileOpenDialog, nullptr, CLSCTX_ALL, IID_PPV_ARGS(&dlg))))
        return out;
    DWORD opts = 0;
    dlg->GetOptions(&opts);
    opts |= FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST;
    opts |= folders ? FOS_PICKFOLDERS : (FOS_ALLOWMULTISELECT | FOS_FILEMUSTEXIST);
    dlg->SetOptions(opts);
    if (!folders) {
        COMDLG_FILTERSPEC filters[] = {
            {L"Rasters", L"*.tif;*.tiff;*.vrt;*.img;*.jp2;*.nc;*.hdf;*.h5;*.dat;*.bil;*.asc"},
            {L"All files", L"*.*"}};
        dlg->SetFileTypes(2, filters);
        dlg->SetTitle(L"Open raster(s): 1 multiband file or several files (1 per date)");
    } else {
        dlg->SetTitle(L"Open a time series folder (1 file per date)");
    }
    if (SUCCEEDED(dlg->Show(nullptr))) {
        IShellItemArray* items = nullptr;
        if (SUCCEEDED(dlg->GetResults(&items))) {
            DWORD count = 0;
            items->GetCount(&count);
            for (DWORD i = 0; i < count; ++i) {
                IShellItem* item = nullptr;
                if (SUCCEEDED(items->GetItemAt(i, &item))) {
                    PWSTR p = nullptr;
                    if (SUCCEEDED(item->GetDisplayName(SIGDN_FILESYSPATH, &p))) {
                        out.push_back(toUtf8(p));
                        CoTaskMemFree(p);
                    }
                    item->Release();
                }
            }
            items->Release();
        }
    }
    dlg->Release();
    return out;
}

std::vector<std::string> openFilesDialog() { return runDialog(false); }

std::string openFolderDialog() {
    auto r = runDialog(true);
    return r.empty() ? std::string() : r[0];
}

std::vector<std::string> rootFolders() {
    std::vector<std::string> out;
    PWSTR p = nullptr;
    if (SUCCEEDED(SHGetKnownFolderPath(FOLDERID_Profile, 0, nullptr, &p))) out.push_back(toUtf8(p));
    CoTaskMemFree(p);
    const DWORD mask = GetLogicalDrives();
    for (int i = 0; i < 26; ++i)
        if (mask & (1u << i)) out.push_back(std::string(1, char('A' + i)) + ":\\");
    return out;
}

std::string getEnv(const char* name) {
    const std::wstring w = toWide(name);
    const DWORD n = GetEnvironmentVariableW(w.c_str(), nullptr, 0);
    if (n == 0) return {};
    std::wstring v(n, L'\0');
    GetEnvironmentVariableW(w.c_str(), v.data(), n);
    v.resize(n - 1);
    return toUtf8(v.c_str());
}

void openInExplorer(const std::string& path) {
    ShellExecuteW(nullptr, L"open", toWide(path).c_str(), nullptr, nullptr, SW_SHOWNORMAL);
}

bool isOnRotationalDisk(const std::string& path) {
    wchar_t volPath[MAX_PATH];
    if (!GetVolumePathNameW(toWide(path).c_str(), volPath, MAX_PATH)) return false;
    // "C:\" -> "\\.\C:"
    std::wstring vol = volPath;
    if (vol.size() < 2 || vol[1] != L':') return false; // network/UNC: treat as no seek penalty
    std::wstring dev = L"\\\\.\\" + vol.substr(0, 2);
    HANDLE h = CreateFileW(dev.c_str(), 0, FILE_SHARE_READ | FILE_SHARE_WRITE, nullptr, OPEN_EXISTING, 0, nullptr);
    if (h == INVALID_HANDLE_VALUE) return false;
    STORAGE_PROPERTY_QUERY q{};
    q.PropertyId = StorageDeviceSeekPenaltyProperty;
    q.QueryType = PropertyStandardQuery;
    DEVICE_SEEK_PENALTY_DESCRIPTOR d{};
    DWORD bytes = 0;
    BOOL ok = DeviceIoControl(h, IOCTL_STORAGE_QUERY_PROPERTY, &q, sizeof(q), &d, sizeof(d), &bytes, nullptr);
    CloseHandle(h);
    return ok && d.IncursSeekPenalty;
}

#else // ---------------------------------------------------------------------
// Linux and macOS.

void init() {}
std::vector<std::string> commandLineArgs(int argc, char** argv) {
    return std::vector<std::string>(argv + 1, argv + argc);
}
void attachParentConsole() {} // a terminal program already has its console

std::string exeDir() {
#ifdef __APPLE__
    char buf[4096];
    uint32_t size = sizeof(buf);
    if (_NSGetExecutablePath(buf, &size) == 0) return fs::canonical(buf).parent_path().string();
    return fs::current_path().string();
#else
    return fs::canonical("/proc/self/exe").parent_path().string();
#endif
}

std::string appDataDir() {
    // macOS: ~/Library/Application Support/tsv; Linux: $XDG_DATA_HOME/tsv (~/.local/share/tsv).
    const char* home = std::getenv("HOME");
    const fs::path h = home ? home : "/tmp";
#ifdef __APPLE__
    fs::path dir = h / "Library" / "Application Support" / "tsv";
#else
    const char* xdg = std::getenv("XDG_DATA_HOME");
    fs::path dir = (xdg && *xdg ? fs::path(xdg) : h / ".local" / "share") / "tsv";
#endif
    std::error_code ec;
    fs::create_directories(dir, ec);
    return dir.string();
}

static bool onPath(const char* program) {
    const char* path = std::getenv("PATH");
    if (!path) return false;
    std::string p = path;
    size_t start = 0;
    while (start <= p.size()) {
        const size_t end = p.find(':', start);
        const std::string dir = p.substr(start, end == std::string::npos ? std::string::npos : end - start);
        if (!dir.empty() && access((fs::path(dir) / program).c_str(), X_OK) == 0) return true;
        if (end == std::string::npos) break;
        start = end + 1;
    }
    return false;
}

// Runs a helper program (no shell: arguments are passed as they are) and
// returns its standard output.
static std::string runCapture(const std::vector<std::string>& args) {
    int fds[2];
    if (pipe(fds) != 0) return {};
    posix_spawn_file_actions_t fa;
    posix_spawn_file_actions_init(&fa);
    posix_spawn_file_actions_adddup2(&fa, fds[1], 1);
    posix_spawn_file_actions_addclose(&fa, fds[0]);
    std::vector<std::string> a = args;
    std::vector<char*> av;
    for (std::string& x : a) av.push_back(x.data());
    av.push_back(nullptr);
    pid_t pid = -1;
    const int rc = posix_spawnp(&pid, av[0], &fa, nullptr, av.data(), environ);
    posix_spawn_file_actions_destroy(&fa);
    close(fds[1]);
    std::string out;
    if (rc == 0) {
        char buf[4096];
        ssize_t n;
        while ((n = read(fds[0], buf, sizeof(buf))) > 0) out.append(buf, size_t(n));
        int st = 0;
        waitpid(pid, &st, 0);
    }
    close(fds[0]);
    return out;
}

static std::vector<std::string> splitLines(const std::string& s) {
    std::vector<std::string> out;
    size_t start = 0;
    while (start < s.size()) {
        size_t end = s.find('\n', start);
        if (end == std::string::npos) end = s.size();
        std::string line = s.substr(start, end - start);
        if (!line.empty() && line.back() == '\r') line.pop_back();
        if (!line.empty()) out.push_back(line);
        start = end + 1;
    }
    return out;
}

// Native dialogs through the desktop's own helpers (no GUI toolkit linked into
// tsv): zenity (GNOME) or kdialog (KDE) on Linux, AppleScript on macOS. Without
// one of them the dialogs return nothing; the Files panel still works.
static std::vector<std::string> runDialog(bool folders) {
#ifdef __APPLE__
    const std::string script = folders ? "POSIX path of (choose folder)"
                                       : "set fs to choose file with multiple selections allowed\n"
                                         "set out to \"\"\n"
                                         "repeat with f in fs\nset out to out & POSIX path of f & linefeed\nend repeat\n"
                                         "out";
    return splitLines(runCapture({"osascript", "-e", script}));
#else
    if (onPath("zenity")) {
        if (folders) return splitLines(runCapture({"zenity", "--file-selection", "--directory"}));
        return splitLines(runCapture({"zenity", "--file-selection", "--multiple", "--separator=\n"}));
    }
    if (onPath("kdialog")) {
        if (folders) return splitLines(runCapture({"kdialog", "--getexistingdirectory"}));
        return splitLines(runCapture({"kdialog", "--getopenfilename", ".", "", "--multiple", "--separate-output"}));
    }
    return {};
#endif
}

std::vector<std::string> openFilesDialog() { return runDialog(false); }
std::string openFolderDialog() {
    auto r = runDialog(true);
    return r.empty() ? std::string() : r[0];
}

void openInExplorer(const std::string& path) {
#ifdef __APPLE__
    const char* opener = "open";
#else
    const char* opener = "xdg-open";
#endif
    std::vector<std::string> a = {opener, path};
    std::vector<char*> av = {a[0].data(), a[1].data(), nullptr};
    pid_t pid = -1;
    if (posix_spawnp(&pid, opener, nullptr, nullptr, av.data(), environ) == 0) {
        int st = 0;
        waitpid(pid, &st, 0); // both return as soon as the file manager / app is launched
    }
}

std::vector<std::string> rootFolders() {
    const char* home = std::getenv("HOME");
    std::vector<std::string> out = {home ? home : "/", "/"};
#ifdef __APPLE__
    out.push_back("/Volumes");
#else
    for (const char* m : {"/media", "/mnt"}) {
        std::error_code ec;
        if (fs::is_directory(m, ec) && !fs::is_empty(m, ec)) out.push_back(m);
    }
#endif
    return out;
}

std::string getEnv(const char* name) {
    const char* v = std::getenv(name);
    return v ? v : "";
}

bool isOnRotationalDisk(const std::string& path) {
#ifdef __APPLE__
    (void)path; // Macs with Apple Silicon have SSDs; external HDDs are rare
    return false;
#else
    // /sys/dev/block/MAJOR:MINOR is the device (or partition) holding the file;
    // a partition's queue/ lives in its parent disk's folder.
    struct stat st {};
    if (stat(path.c_str(), &st) != 0) return false;
    const std::string dev = "/sys/dev/block/" + std::to_string(major(st.st_dev)) + ":" + std::to_string(minor(st.st_dev));
    for (const char* rel : {"/queue/rotational", "/../queue/rotational"}) {
        std::ifstream f(dev + rel);
        int r = 0;
        if (f >> r) return r == 1;
    }
    return false;
#endif
}

#endif

} // namespace platform

std::tm platform::localTime(std::time_t t) {
    std::tm tm{};
#ifdef _WIN32
    localtime_s(&tm, &t);
#else
    localtime_r(&t, &tm);
#endif
    return tm;
}
