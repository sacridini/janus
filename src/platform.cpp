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

void init() {}
std::vector<std::string> commandLineArgs(int argc, char** argv) {
    return std::vector<std::string>(argv + 1, argv + argc);
}
void attachParentConsole() {}
std::string exeDir() { return fs::canonical("/proc/self/exe").parent_path().string(); }
std::string appDataDir() {
    const char* home = std::getenv("HOME");
    fs::path dir = fs::path(home ? home : "/tmp") / ".cache" / "tsv";
    std::error_code ec;
    fs::create_directories(dir, ec);
    return dir.string();
}
std::vector<std::string> openFilesDialog() { return {}; }
std::string openFolderDialog() { return {}; }
bool isOnRotationalDisk(const std::string&) { return false; }

#endif

} // namespace platform
