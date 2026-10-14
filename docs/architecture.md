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

 vector files (OGR) ──► VectorData (file coordinates) ──► placed on the active layer's grid (PROJ)
                                                          ├─► drawn by ImGui over every map panel
                                                          ├─► a polygon as the ROI (pixel-centre mask)
                                                          └─► series extracted / interpretation labels
```

| File | Role |
|---|---|
| `src/cube.*` | Layer, date and band discovery; spectra (wavelengths from the band metadata or EnMAP's METADATA.XML, the chart's pieces, RGB composites by wavelength); per-thread GDAL reader (nodata → NaN, scale/offset, normalized difference, QA mask) |
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
| `src/app_composite.cpp` | RGB composite of three bands per date (several bands per file): presets by band role or wavelength, each date read in the background at the overview's size (the dates shown first, an LRU of RGBA textures), the visible window again at screen resolution when zoomed in, a stretch kept for every date; drawn instead of the cube on the map, panels, swipe and exports; `--selftest-ui` stages 140–145 |
| `src/app_python.cpp` | *Copy Python*: code reading the cursor's, a pin's or the ROI's series with rasterio as Janus does (band, normalized difference, quality mask, polygon), its statistics and the Zeit calls the bridge recorded (with the multiband inputs: bands by role in reflectance × 10000, Fmask codes); `--selftest-ui` stages 150–154 run it |
| `src/app_swipe.cpp` | Swipe: the comparison drawn like a map panel into its own target, divider, View menu entries and keys of swipe and transect |
| `src/app_analysis.cpp` | Analysis panel: a series by day of the year (years overlaid, climatology, year × day heatmap), class shares over time and transitions (counted on the overview, a few dates per frame), scatter of two layers/dates (density, least squares) |
| `src/app_transect.cpp` | Space-time transect: sampling along the line (overview, then full resolution in the background), the Transect panel (image drawn by the map renderer) |
| `src/app_roi.cpp` | The ROI's shape: a rectangle or a polygon (`R`: clicked vertices or a freehand lasso; a vector polygon), its bounding box and outline; the ROI on another layer's pixels (`roiOnLayer`: scaled, or densified and reprojected), `--selftest-ui` stages 110–115 |
| `src/geom.*` | Polygons as rings (even-odd: holes and multipolygons without flags): bounds, point in polygon, area, the mask of pixel centres inside (scanlines), densifying, Douglas-Peucker |
| `src/vectors.*` | Vector files through OGR (GeoJSON, Shapefile, GeoPackage, KML, CSV of points...): features as rings in the file's coordinates with a point inside each and their fields as text; `VectorPlacer` (file CRS -> a raster's pixels and lon/lat, PROJ); tables written as CSV or through OGR with extra columns |
| `src/app_vectors.cpp` | Vector layers: loading in the background, placing on the active layer's grid (again when it changes), drawing over every map panel (cut to the screen, thinned to a vertex per point), picking, a polygon as the ROI, the Layers panel section, **Extract series** (points: the pixel; polygons: the mean inside; CSV, wide or long, a background job), `--selftest-ui` stages 120–125 |
| `src/app_labels.cpp` | Interpretation (TimeSync style): a vector layer's features one by one (map centred, a pin or the ROI on the chart), labels from a label set (keys 1-9), a date of change, confidence, more events and notes, saved as they are given (app data, by file and layer; a validation project's sample: its GeoPackage) and exported with the file's fields; a filter of the features to go through |
| `src/validation.*` | Validation (Olofsson et al. 2014, Stehman 2014), no interface: maps read by a rule (classes, intervals, events), strata in one pass at full resolution in bands of rows (other maps warped onto the first's grid, a buffer by dilation, a polygon limit by scanlines, areas per row on the sphere in a geographic CRS), the design (eq. 13, allocations as `zeit.sampling_design`), the seeded draw (Floyd's algorithm per stratum, the same on every platform), the estimators (the same numbers as `zeit.accuracy`), dates, kappa, the project in a GeoPackage, the reference of a point from several interpreters, the Python that reproduces the estimates |
| `src/validation_selftest.cpp` | `--selftest-validation`: the example of Olofsson et al. (2014), the designs against Zeit, strata against a pixel-by-pixel count, the sample, the project's file, and the estimates against `zeit.accuracy` run on a labelled project |
| `src/app_validation.cpp` | The Validation menu and panel (design, accuracy, maps, interpreters, report), the project's sample as a vector layer (colours by state, stratum or right/wrong; blind mode hides the maps assessed), labels written to the GeoPackage in the background, the maps' values at the points and areas read in the background, the HTML report, the Chips panel (every date around the feature, read ahead), the pixel's square on the map; `--selftest-ui` stages 170-179 |
| `src/app_animation.cpp` | Animation export: options (in the export popup), frames rendered one per UI frame once their date, detail tiles and basemap are on the GPU (the view frozen behind a modal progress popup), a short queue to the encoder thread (GIF, or ffmpeg fed raw RGBA on stdin), the ffmpeg setting, `--selftest-ui` stages 130–137 |
| `src/gif.*` | Animated GIF without a library: per-frame palette (exact ≤ 256 colours, else median cut of a 6-bit histogram, optional Floyd–Steinberg), LZW, loop extension; a small decoder for the self-test |
| `src/file_browser.*` | Lazily listed folder tree (rasters only by default); reveals (expands and scrolls to) the series opened; favourites; a folder's preview as a series (`describeSeriesFiles`) |
| `src/app_export.cpp` | Export: map as PNG (offscreen render at 1–4×; the overlay of a figure — label or an animation's date stamp, legend, pins, ROI and vector layers, scale bar, credit — drawn on the CPU with ImGui's pixel font at 13 px, whatever the interface's size, shared with the animation), values and the view as GeoTIFF, Zeit results, embedding views (full resolution, `computeEmbeddingExport`); the options popup with its *Save to* path; background jobs (`startExportJob`), Exports window |
| `src/app_selftest.cpp` | `--selftest-ui`: the layers workflow, swipe, transect, spectra (ENVI, an EnMAP product) and the exports in a hidden window, checked by reading map pixels and the files written; the full-resolution cache checked against the files |
| `src/app_fullres.cpp` | Full-resolution cache in the Performance panel, its settings widgets, `--measure-cache` |
| `src/app_settings.cpp` | Settings window (theme, font size, Files panel, processing threads, overview memory, caches), kept in the layout .ini with the last export folder; the font applied between frames (`App::applyFont`); applies the threads to the open series and Zeit (the serve process is replaced once idle); its `--selftest-ui` checks (pools, contrast of every theme, Zeit restart); the panels' and the whole window's opacity (every OS window, `glfwSetWindowOpacity`) |
| `src/theme.*` | Interface themes (Dark, Light, Classic, Janus, Studio, Graphite, Matrix): ImGui and ImPlot colours, status colours, contrast (WCAG) and data colours made legible on the charts; the panels' opacity (translucent backgrounds over the transparent framebuffer, a premultiplied clear colour, contrast judged on the opaque colours) |
| `src/embedding.*` | Embeddings, and a spectrum's bands as one vector per pixel: recognising a layer of them, the store (every year at reduced resolution, Int8 with a scale per dimension, read block by block; dimensions without data read as 0), PCA (covariance with the SIMD kernels, subspace iteration), images of the principal components, similarity and change, the background engine (the newest request of each kind wins) |
| `src/embedding_simd.cpp` | SIMD kernels chosen at run time: AVX2 + FMA, SSE2, NEON or scalar (covariance, Int8 projections, dot products) |
| `src/app_embeddings.cpp` | Embeddings in the interface (and a spectrum as one "year" of all its bands: principal components, spectral similarity): drawing (an RGBA or float image per year), the Embeddings panel and its charts, the download for the visible area (a Zeit job, progress on the map), `--selftest-embeddings-ui` |
| `src/app_zeit.cpp` | Tools menu, tool windows, tasks, the Log window (a job's log or zeit.log, read as it grows), result layers, models on the chart |
| `src/zeit_client.*` | Bridge processes (JSON lines over pipes; Win32 or POSIX, thread limits in their environment), pixel calls, raster jobs (each with its own log: stderr of the process and the protocol messages), estimates |
| `src/results.*` | Result rasters loaded as map layers |
| `src/reproject.*` | Layers in another CRS or on a rotated grid: exact PROJ transformation (cursor, pins, ROI, tiles, view) and the warp grid the shaders sample through |
| `src/basemap.*` | Web basemap: XYZ tiles in EPSG:3857 read through GDAL's WMS driver (TMS service, curl, its disk cache) in a pool of its own, newest first, requests that left the screen dropped; an LRU of RGBA textures; drawn on the active layer's grid through a warp grid (`reproject.*`) |
| `src/app_basemap.cpp` | The basemap in the interface: Layers panel section, setting (layout .ini), drawn first in every map target, attribution on the map |
| `src/selftest.cpp` | `--selftest-zeit`: every applicable Zeit tool end to end (pixel + raster job and its log) without a window |
| `zeit_bridge/` | The Python bridge (protocol; raster jobs in full-width row bands, the next one read in a thread while the tool computes, outputs compressed on the job's threads), one `tool_*.py` per Zeit tool family calling Zeit's public API (a tool may fit a model on a sample of the window first, e.g. the SOM; write series outputs, one band per date, that Janus opens as layers, e.g. the NDFI; or run on result maps instead of the series, e.g. the agreement), `task_embeddings.py` (the download of embeddings) (`zeit_common.py`: series and chunks as dated xarray cubes, results back as arrays), the pinned runtime requirements; every pixel run records the Zeit calls its tool makes (`run_recording`) and returns them as code for *Copy Python*; the BFAST family refits its models between the breaks for the chart |
| `tools/build_zeit_runtime.py` | Assembles the private Python runtime (Windows, Linux, macOS) |
| `cmake/package_linux.cmake` | Portable Linux package (bundled libraries, RPATH `$ORIGIN/lib`) |
| `cmake/package_macos.cmake` | macOS app and `.dmg` (bundled libraries, RPATH `@executable_path/../Frameworks`, ad hoc signature) |
| `.github/workflows/build.yml` | CI: Windows, Linux and macOS builds, self-tests, installers; releases on `v*` tags |
| `tools/make_selftest_data.py` | Synthetic inputs for `--selftest-ui` and `--selftest-zeit` |
| `src/platform.*` | OS-specific: arguments, open and save dialogs, data folder, opening folders, HDD detection, `ChildProcess` (a helper fed through stdin, e.g. ffmpeg, no console window) and `findOnPath` (Windows, Linux, macOS) |
| `src/launcher.cpp` | `jn.com` console launcher |
