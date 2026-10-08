#pragma once

// Web basemap (satellite imagery or a map) drawn under the series: XYZ tiles
// in Web Mercator (EPSG:3857) read through GDAL's WMS driver (TMS service:
// libcurl and a disk cache of its own, no new dependency) in a pool of its
// own, kept on the GPU (LRU) and drawn on the map, the active layer's pixel
// grid, through a warp grid from that grid to Web Mercator (reproject.hpp).
// Only the zoom level that matches the screen and the tiles in view are read;
// requests that left the screen are dropped before they are sent. Nothing here
// exists, or touches the network, until a source is picked.

#include <atomic>
#include <cstdint>
#include <functional>
#include <memory>
#include <string>
#include <unordered_map>
#include <vector>

#include "cube.hpp"
#include "gpu.hpp"
#include "reproject.hpp"

class JobPool;

struct BasemapSource {
    std::string id;          // "esri", "eox", "osm" or "custom" (kept in the settings)
    std::string name;        // as listed
    std::string url;         // XYZ template: {z}, {x}, {y} ({-y}: rows from the bottom, {s}: "a")
    int maxZoom = 19;
    int tileSize = 256;      // pixels per side (256 or 512)
    std::string attribution; // drawn on the map and in the exports whenever the basemap is on
    std::string terms;       // licence / terms of use, for the interface
    int connections = 4;     // concurrent requests
    int cacheDays = 30;      // how long a tile stays in the disk cache
};

// Esri World Imagery, Sentinel-2 cloudless 2016 by EOX, OpenStreetMap.
const std::vector<BasemapSource>& basemapPresets();
// Why a custom XYZ template cannot be used ("" if it can).
std::string basemapUrlProblem(const std::string& url);

class Basemap {
public:
    // `cacheDir`: GDAL's tile cache (a folder per source inside it); `wake`
    // is called from the reader threads when a tile is ready.
    Basemap(const BasemapSource& src, const std::string& cacheDir, std::function<void()> wake);
    ~Basemap();
    Basemap(const Basemap&) = delete;
    Basemap& operator=(const Basemap&) = delete;

    const BasemapSource& source() const { return src_; }

    // Main thread. Places Web Mercator on the map: the warp grid from the
    // active layer's pixels to the tiles (kept while the layer stays active).
    // False, with the reason, if the layer has no georeferencing or CRS.
    bool setMap(const CubeInfo& active, std::string& why);
    bool hasMap() const { return map_ != nullptr; }
    const Reprojection* map() const { return map_.get(); }
    // GPU resources (tiles, the warp grid), on the main thread: before the
    // object is handed to another thread to be destroyed.
    void releaseGpu();

    // Once per frame (whatever the number of map panels calling update()):
    // requests not renewed for 2 frames are dropped before being sent.
    void tick();
    // A map panel's view: `view` = map-space rectangle (x0, y0, x1, y1, in
    // the active layer's pixels), `pxPerMapPx` = target pixels per map pixel.
    // Requests the tiles it lacks at the matching zoom level (and a few of a
    // coarser one first, a quick preview); returns that level (-1: none).
    int update(const double view[4], double pxPerMapPx);
    // Uploads the tiles that were read (up to `maxUploads`). True if any.
    bool uploadReady(int maxUploads);
    // The cached tiles covering the view, coarser levels first (they fill what
    // the wanted level still lacks): f(tex, quad = map-space rectangle, warp).
    void forEachVisible(const double view[4], double pxPerMapPx,
                        const std::function<void(GpuTex, const double* quad, const WarpParams&)>& f);

    // State, for the interface.
    bool offline() const;        // the last request did not reach the server
    std::string error() const;   // the last server error (e.g. "HTTP 403"), "" if none
    int inflight() const;
    int gpuTiles() const { return int(gpu_.size()); }
    size_t gpuBytes() const { return gpuBytes_; }
    int loads() const { return loads_; }
    double avgLoadMs() const { return loads_ ? loadMsSum_ / loads_ : 0.0; }
    double firstTileMs() const { return firstTileMs_; } // since the map was placed (-1: none yet)
    static int datasetsOpened(); // GDAL WMS datasets opened by this process (tests: none while off)

private:
    struct Shared;
    struct Tile {
        GpuTex tex = 0;
        uint64_t lastUsed = 0;
        uint64_t quadFor = ~0ull; // mapVersion_ quad/warp were computed for
        bool drawable = false;
        double quad[4] = {0, 0, 0, 0};
        WarpParams warp;
    };
    // Zoom level and Web Mercator box (x0, y0, x1, y1; y up) of a view.
    bool viewTiles(const double view[4], double pxPerMapPx, int& z, double box[4]) const;
    void tileRange(int z, const double box[4], int r[4]) const;
    void request(int z, const double box[4], int priority);
    void place(int z, int x, int y, Tile& t);
    void evict();

    BasemapSource src_;
    std::shared_ptr<Shared> sh_;
    std::unique_ptr<Reprojection> map_; // map space -> the Web Mercator frame below
    uint64_t mapFor_ = 0;               // CubeInfo id of the active layer it was made for
    uint64_t mapVersion_ = 0;
    double frame_[3] = {0, 0, 1};       // the frame's geotransform: left x, top y, pixel size (m)
    int frameW_ = 1, frameH_ = 1;
    double placedAt_ = 0;               // when the map was placed (ms, steady clock)
    double firstTileMs_ = -1;
    uint64_t frameNo_ = 0;
    std::unordered_map<uint64_t, Tile> gpu_;
    size_t gpuBytes_ = 0, maxGpuBytes_ = 128ull << 20;
    double loadMsSum_ = 0;
    int loads_ = 0;
    std::unique_ptr<JobPool> pool_;     // last: destroyed first (its jobs use the members above)
};
