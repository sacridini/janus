#include "disk_cache.hpp"

#include <algorithm>
#include <cstdio>
#include <filesystem>
#include <map>
#include <mutex>
#include <vector>

namespace fs = std::filesystem;

namespace {

constexpr uint64_t GB = 1ull << 30;

std::mutex gInUseM;
std::map<std::string, int> gInUse; // by file name: the folder is the same for all

std::string nameOf(const std::string& path) { return fs::u8path(path).filename().u8string(); }

bool isCache(const fs::path& p) {
    const fs::path e = p.extension();
    return e == diskcache::kOverviewExt || e == diskcache::kFullResExt;
}

} // namespace

namespace diskcache {

uint64_t autoCap(uint64_t diskBytes) {
    if (diskBytes == 0) return 64 * GB; // unknown disk
    return std::clamp(diskBytes / 10, 8 * GB, 256 * GB);
}

uint64_t reserve(uint64_t diskBytes) {
    return diskBytes == 0 ? 0 : std::clamp(diskBytes / 10, 10 * GB, 100 * GB);
}

Budget budget(const std::string& cacheDir, uint64_t setting) {
    Budget b;
    std::error_code ec;
    const fs::space_info sp = fs::space(fs::u8path(cacheDir), ec);
    if (!ec) {
        b.disk = sp.capacity;
        b.available = sp.available;
    }
    b.overview = folderBytes(cacheDir, kOverviewExt);
    b.fullRes = folderBytes(cacheDir, kFullResExt);
    b.used = b.overview + b.fullRes;
    b.automatic = setting == 0;
    b.cap = b.automatic ? autoCap(b.disk) : setting;
    b.reserve = reserve(b.disk);
    b.bytes = b.cap;
    if (b.disk) {
        const uint64_t room = b.used + b.available > b.reserve ? b.used + b.available - b.reserve : 0;
        b.bytes = std::min(b.cap, room);
    }
    return b;
}

uint64_t prune(const std::string& cacheDir, uint64_t maxBytes) {
    struct Entry { fs::path p; fs::file_time_type t; uint64_t size; };
    std::vector<Entry> entries;
    uint64_t total = 0;
    std::error_code ec;
    for (auto& e : fs::directory_iterator(fs::u8path(cacheDir), ec)) {
        if (!isCache(e.path())) continue;
        entries.push_back({e.path(), e.last_write_time(ec), e.file_size(ec)});
        total += entries.back().size;
    }
    std::sort(entries.begin(), entries.end(), [](const Entry& a, const Entry& b) { return a.t < b.t; });
    for (const Entry& e : entries) {
        if (total <= maxBytes) break;
        if (inUse(e.p.u8string()) || !fs::remove(e.p, ec)) continue;
        total -= e.size;
    }
    return total;
}

bool makeRoom(const std::string& cacheDir, uint64_t setting, uint64_t need, std::string& why) {
    const Budget b = budget(cacheDir, setting);
    char msg[192];
    if (need > b.bytes) {
        if (b.byDisk())
            std::snprintf(msg, sizeof(msg), "needs %.1f GB; the disk has %.1f GB free and Janus keeps %.0f GB of it free",
                          need / double(GB), b.available / double(GB), b.reserve / double(GB));
        else
            std::snprintf(msg, sizeof(msg), "needs %.1f GB, more than the cache budget (%.0f GB)", need / double(GB),
                          b.bytes / double(GB));
        why = msg;
        return false;
    }
    if (prune(cacheDir, b.bytes - need) + need > b.bytes) {
        std::snprintf(msg, sizeof(msg), "needs %.1f GB; the cache budget (%.0f GB) is taken by caches in use",
                      need / double(GB), b.bytes / double(GB));
        why = msg;
        return false;
    }
    return true;
}

uint64_t folderBytes(const std::string& cacheDir, const char* ext) {
    uint64_t total = 0;
    std::error_code ec;
    for (auto& e : fs::directory_iterator(fs::u8path(cacheDir), ec))
        if (e.path().extension() == ext) total += e.file_size(ec);
    return total;
}

void retain(const std::string& path) {
    std::lock_guard<std::mutex> lk(gInUseM);
    ++gInUse[nameOf(path)];
}

void release(const std::string& path) {
    std::lock_guard<std::mutex> lk(gInUseM);
    auto it = gInUse.find(nameOf(path));
    if (it != gInUse.end() && --it->second <= 0) gInUse.erase(it);
}

bool inUse(const std::string& path) {
    std::lock_guard<std::mutex> lk(gInUseM);
    auto it = gInUse.find(nameOf(path));
    return it != gInUse.end() && it->second > 0;
}

} // namespace diskcache
