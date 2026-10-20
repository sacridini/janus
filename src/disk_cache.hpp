#pragma once

#include <cstdint>
#include <string>

// The disk caches of the cache folder (overviews, .januscube; full-resolution
// caches, .janusfull) share one budget, regulated from the disk that holds the
// folder. Automatic (the default): a tenth of the disk, 8 to 256 GB; or a size
// set in Settings. Either way a reserve of the disk stays free (a tenth, 10 to
// 100 GB): as the disk fills up the budget shrinks, and the least recently
// opened series leave first. Checked at startup, before each cache file is
// written, when a series is closed and once a minute (App::pumpDiskCache).
// Basemap tiles keep GDAL's own limit (512 MB, basemap.cpp).
namespace diskcache {

constexpr const char* kOverviewExt = ".januscube";
constexpr const char* kFullResExt = ".janusfull";

uint64_t autoCap(uint64_t diskBytes);
uint64_t reserve(uint64_t diskBytes);

struct Budget {
    uint64_t bytes = 0;              // what the caches may take now
    uint64_t cap = 0;                // automatic or set by hand
    uint64_t used = 0;               // what they take
    uint64_t overview = 0, fullRes = 0;
    uint64_t disk = 0, available = 0; // the folder's disk (0: unknown)
    uint64_t reserve = 0;
    bool automatic = true;
    bool byDisk() const { return bytes < cap; } // shrunk to keep the reserve free
};

// `setting`: bytes, 0 = automatic.
Budget budget(const std::string& cacheDir, uint64_t setting);

// Removes the least recently opened caches not in use until the folder's take
// at most maxBytes; returns what they take then.
uint64_t prune(const std::string& cacheDir, uint64_t maxBytes);
// Room for `need` more bytes in the budget, older caches pruned to make it;
// false (and why) if it cannot be had.
bool makeRoom(const std::string& cacheDir, uint64_t setting, uint64_t need, std::string& why);

// Bytes of the folder's files with extension `ext`.
uint64_t folderBytes(const std::string& cacheDir, const char* ext);

// Cache files open in this process (by path): never pruned or cleared.
void retain(const std::string& path);
void release(const std::string& path);
bool inUse(const std::string& path);

} // namespace diskcache
