# Architecture

[README](../README.md) · [Install](install.md) · [Usage](usage.md) · [Building](building.md) · [Architecture](architecture.md)

```
 files (GDAL) ──► Overview (CPU, [t][y][x] float32) ──► GpuCube (R32F texture array)
       │               │ disk cache (%LOCALAPPDATA%\Janus\cache)      │
       │               └─ instant approximate series                  ├─► statistics shader (1 pass)
       │                                                              └─► display shader (stretch, colormap, modes)
       ├──► FullResCache: every date at full resolution on the local disk (built with the overview on an HDD)
       │         └─ read first by the two below for the dates it has
       ├──► TileManager: full-resolution tiles of the visible area ──► LRU cache on the GPU
       └──► exact pixel series / ROI (interactive pool, cancellable)

 XYZ tiles (GDAL WMS/TMS, curl) ──► Basemap: tiles of the view at the screen's zoom ──► LRU on the GPU,
                                    drawn first, through a warp grid (active layer's grid -> EPSG:3857)
```

| File | Role |
|---|---|
| `src/cube.*` | Layer, date and band discovery; per-thread GDAL reader (nodata → NaN, scale/offset, normalized difference, QA mask) |
| `src/overview.*` | Reduced cube built in parallel, following the focused date; disk cache |
| `src/fullres_cache.*` | Full-resolution cache: lossless 64×64 blocks per date on the local disk, built with the overview (HDD), read for series, ROI and tiles |
| `src/gpu.hpp` | Renderer-neutral GPU layer: cube, per-pixel temporal statistics, map drawing, textures (`GpuTex`) |
| `src/gpu_gl.cpp`, `src/gpu_metal.mm` | Its OpenGL 3.3 (GLSL) and Metal (MSL, shared-memory buffers) backends |
| `src/render_backend*` | Window and context, ImGui renderer backend, present (main window and detached panels): OpenGL or Metal |
| `src/tiles.*` | Detail tiles per zoom level; drops requests that left the screen |
| `src/session.*` | One open series: thread pools, exact series, ROI |
| `src/stats.*` | OLS, Sen's slope, percentiles |
| `src/app.*` | User interface (ImGui/ImPlot) |
| `src/app_layers.cpp` | Several series as layers (alignment, active layer, other layers' series), Layers and Files panels |
| `src/app_classes.cpp` | Categorical series: detection, class colours and names, legend, class statistics |
| `src/app_swipe.cpp` | Swipe: the comparison drawn like a map panel into its own target, divider, View menu entries and keys of swipe and transect |
| `src/app_analysis.cpp` | Analysis panel: a series by day of the year (years overlaid, climatology, year × day heatmap), class shares over time and transitions (counted on the overview, a few dates per frame), scatter of two layers/dates (density, least squares) |
| `src/app_transect.cpp` | Space-time transect: sampling along the line (overview, then full resolution in the background), the Transect panel (image drawn by the map renderer) |
| `src/file_browser.*` | Lazily listed folder tree (rasters only by default); reveals (expands and scrolls to) the series opened; favourites; a folder's preview as a series (`describeSeriesFiles`) |
| `src/app_export.cpp` | Export: map as PNG (offscreen render at 1–4×, marks and legend drawn on the CPU with ImGui's pixel font at 13 px, whatever the interface's size), values and the view as GeoTIFF, Zeit results, embedding views (full resolution, `computeEmbeddingExport`); the options popup with its *Save to* path; background jobs, Exports window |
| `src/app_selftest.cpp` | `--selftest-ui`: the layers workflow, swipe, transect and the exports in a hidden window, checked by reading map pixels and the files written; the full-resolution cache checked against the files |
| `src/app_fullres.cpp` | Full-resolution cache in the Performance panel, its settings widgets, `--measure-cache` |
| `src/app_settings.cpp` | Settings window (theme, font size, Files panel, processing threads, overview memory, caches), kept in the layout .ini with the last export folder; the font applied between frames (`App::applyFont`); applies the threads to the open series and Zeit (the serve process is replaced once idle); its `--selftest-ui` checks (pools, contrast of every theme, Zeit restart) |
| `src/theme.*` | Interface themes (Dark, Light, Classic, Janus, Studio, Graphite): ImGui and ImPlot colours, status colours, contrast (WCAG) and data colours made legible on the charts |
| `src/embedding.*` | Embeddings: recognising a layer of them, the store (every year at reduced resolution, Int8 with a scale per dimension, read block by block), PCA (covariance with the SIMD kernels, subspace iteration), images of the principal components, similarity and change, the background engine (the newest request of each kind wins) |
| `src/embedding_simd.cpp` | SIMD kernels chosen at run time: AVX2 + FMA, SSE2, NEON or scalar (covariance, Int8 projections, dot products) |
| `src/app_embeddings.cpp` | Embeddings in the interface: drawing (an RGBA or float image per year), the Embeddings panel and its charts, the download for the visible area (a Zeit job, progress on the map), `--selftest-embeddings-ui` |
| `src/app_zeit.cpp` | Tools menu, tool windows, tasks, the Log window (a job's log or zeit.log, read as it grows), result layers, models on the chart |
| `src/zeit_client.*` | Bridge processes (JSON lines over pipes; Win32 or POSIX, thread limits in their environment), pixel calls, raster jobs (each with its own log: stderr of the process and the protocol messages), estimates |
| `src/results.*` | Result rasters loaded as map layers |
| `src/reproject.*` | Layers in another CRS or on a rotated grid: exact PROJ transformation (cursor, pins, ROI, tiles, view) and the warp grid the shaders sample through |
| `src/basemap.*` | Web basemap: XYZ tiles in EPSG:3857 read through GDAL's WMS driver (TMS service, curl, its disk cache) in a pool of its own, newest first, requests that left the screen dropped; an LRU of RGBA textures; drawn on the active layer's grid through a warp grid (`reproject.*`) |
| `src/app_basemap.cpp` | The basemap in the interface: Layers panel section, setting (layout .ini), drawn first in every map target, attribution on the map |
| `src/selftest.cpp` | `--selftest-zeit`: every applicable Zeit tool end to end (pixel + raster job and its log) without a window |
| `zeit_bridge/` | The Python bridge (protocol; raster jobs in full-width row bands, the next one read in a thread while the tool computes, outputs compressed on the job's threads), one `tool_*.py` per Zeit tool family calling Zeit's public API (a tool may fit a model on a sample of the window first, e.g. the SOM; write series outputs, one band per date, that Janus opens as layers, e.g. the NDFI; or run on result maps instead of the series, e.g. the agreement), `task_embeddings.py` (the download of embeddings) (`zeit_common.py`: series and chunks as dated xarray cubes, results back as arrays), the pinned runtime requirements |
| `tools/build_zeit_runtime.py` | Assembles the private Python runtime (Windows, Linux, macOS) |
| `cmake/package_linux.cmake` | Portable Linux package (bundled libraries, RPATH `$ORIGIN/lib`) |
| `cmake/package_macos.cmake` | macOS app and `.dmg` (bundled libraries, RPATH `@executable_path/../Frameworks`, ad hoc signature) |
| `.github/workflows/build.yml` | CI: Windows, Linux and macOS builds, self-tests, installers; releases on `v*` tags |
| `tools/make_selftest_data.py` | Synthetic inputs for `--selftest-ui` and `--selftest-zeit` |
| `src/platform.*` | OS-specific: arguments, open and save dialogs, data folder, opening folders, HDD detection (Windows, Linux, macOS) |
| `src/launcher.cpp` | `jn.com` console launcher |
