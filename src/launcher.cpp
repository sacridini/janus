// tsv.com: console launcher (same idea as Visual Studio's devenv.com).
// In a terminal, "tsv" resolves to .com before .exe (PATHEXT order):
// --help/--version are printed to the console itself; anything else opens the
// GUI (tsv.exe) with the same arguments and returns the prompt immediately.
#define NOMINMAX
#define WIN32_LEAN_AND_MEAN
#include <windows.h>

#include <cstdio>
#include <string>

#include "usage.hpp"

static std::string toUtf8(const wchar_t* w) {
    int len = WideCharToMultiByte(CP_UTF8, 0, w, -1, nullptr, 0, nullptr, nullptr);
    std::string s(size_t(len > 0 ? len - 1 : 0), '\0');
    if (len > 1) WideCharToMultiByte(CP_UTF8, 0, w, -1, s.data(), len, nullptr, nullptr);
    return s;
}

int wmain(int argc, wchar_t** argv) {
    SetConsoleOutputCP(CP_UTF8);
    for (int i = 1; i < argc; ++i) {
        const std::string a = toUtf8(argv[i]);
        if (a == "-h" || a == "--help" || a == "/?") {
            std::fputs(kUsage, stdout);
            return 0;
        }
        if (a == "--version") {
            std::printf("tsv %s\n", TSV_VERSION);
            return 0;
        }
        if (isValueOption(a.c_str())) {
            if (i + 1 >= argc) {
                std::fprintf(stderr, "tsv: %s needs a value\n", a.c_str());
                return 2;
            }
            ++i;
        } else if (a.rfind("--", 0) == 0) {
            std::fprintf(stderr, "tsv: unknown option: %s\n\n%s", a.c_str(), kUsage);
            return 2;
        }
    }

    wchar_t self[MAX_PATH];
    GetModuleFileNameW(nullptr, self, MAX_PATH);
    std::wstring exe = self;
    exe.replace(exe.size() - 4, 4, L".exe");

    // Forward the original command line, minus the program name.
    const wchar_t* rest = GetCommandLineW();
    if (*rest == L'"') {
        ++rest;
        while (*rest && *rest != L'"') ++rest;
        if (*rest) ++rest;
    } else {
        while (*rest && *rest != L' ' && *rest != L'\t') ++rest;
    }
    std::wstring cmd = L"\"" + exe + L"\"" + rest;

    STARTUPINFOW si{};
    si.cb = sizeof(si);
    PROCESS_INFORMATION pi{};
    if (!CreateProcessW(exe.c_str(), cmd.data(), nullptr, nullptr, FALSE, 0, nullptr, nullptr, &si, &pi)) {
        std::fprintf(stderr, "tsv: could not start %s (error %lu)\n", toUtf8(exe.c_str()).c_str(), GetLastError());
        return 1;
    }
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    return 0;
}
