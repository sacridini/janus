# Architecture

[README](../README.md) · [Install](install.md) · [Usage](usage.md) · [Building](building.md) · [Architecture](architecture.md)

```
 files (GDAL) ──► Overview (CPU, [t][y][x] float32) ──► GPU cube (wgpu: R32F texture array)
       │               │ disk cache (.januscube)                       │
       │               └─ instant approximate series                   ├─► statistics (compute shader, 1 pass)
       │                                                               └─► display shader (stretch, colormap, modes)
       ├──► FullResCache: every date at full resolution on the local disk (built with the overview on an HDD)
       │         └─ read first by the two below for the dates it has
       ├──► detail tiles of the visible area at full resolution ──► LRU on the GPU
       └──► exact pixel series / ROI (interactive pool, cancellable)

 XYZ tiles (GDAL WMS/TMS, curl) ──► Basemap: tiles of the view at the screen's zoom ──► LRU on the GPU,
                                    drawn first, through a warp grid (active layer's grid -> EPSG:3857)

 vector files (OGR) ──► VectorData (file coordinates) ──► placed on the active layer's grid (PROJ)
                                                          ├─► drawn over every map panel
                                                          ├─► a polygon as the ROI (pixel-centre mask)
                                                          └─► series extracted / interpretation labels

 a series or a window of it ──► Zeit's methods (zeit-core and janus-zeit, in the process, on every core)
                                ├─► a pixel: the model on the chart, its numbers in Statistics (< 1 ms)
                                └─► a raster run: result maps as layers, series as new layers
```

The interface is [egui](https://github.com/emilk/egui) (with `egui_dock` for the panels and
`egui_plot` for the charts) on [wgpu](https://wgpu.rs): Direct3D 12, Vulkan or Metal. Long work
runs on thread pools; a GDAL dataset is never shared between threads (each thread has its readers).

## Crates

| Crate | Role |
|---|---|
| `janus-cube` | A series as a cube: layer, date and band discovery (folders, wildcards, multiband files, NetCDF/Zarr/HDF variables and levels, URLs and cloud buckets through GDAL's network file systems), spectra (wavelengths from the band metadata or EnMAP's METADATA.XML), band roles, per-thread readers (nodata → NaN, scale/offset, normalized difference, quality masks), the statistics of a pixel's series and an ROI's values, the series as a VRT |
| `janus-gdal` | The few GDAL C functions the `gdal` crate does not cover |
| `janus-cache` | The overview (built in parallel, following the focused date) and its disk cache; the full-resolution cache (lossless 64 × 64 blocks per date); the disk budget they share; the thread pool with priorities. The files are the same as the C++ versions' |
| `janus-session` | One open series: the pools (background and interactive), exact series, ROI, detail tiles, spinning-disk detection |
| `janus-gpu` | The map on the GPU: the cube, the per-pixel statistics (`stats.wgsl`), the display modes (`display.wgsl`), class tables, warp grids, render targets per map panel and their readback; the colormaps |
| `janus-geo` | Layers in another CRS or on a rotated grid (exact PROJ transformation and the warp grid the shader samples through), the web basemap (XYZ tiles through GDAL, an LRU of textures), plane geometry (polygons as even-odd rings, masks of pixel centres), vector files through OGR |
| `janus-ml` | Deep learning on Burn (the GPU through the same wgpu as the map, or the CPU): samples read from the cube at full resolution, gaps filled and scaled as sits scales them, sits' TempCNN and L-TAE, Presto and U-TAE with their authors' weights (read from their PyTorch files), their training (stratified split, AdamW, early stopping), the `.janusmodel` file, and a model applied to a window in row bands or, for U-TAE, overlapping windows (class map, probability, entropy) |
| `janus-zeit` | Zeit's tools in the process: their descriptors (`tools.json`), a pixel fit, a raster run over a window in row bands (outputs as GeoTIFFs, series outputs, the run's log), the run's time estimated on a sample, *Copy Python* (the Zeit calls a fit makes, as code); the methods are `zeit-core`'s, with the glue the Python bridge had (bands by role, NDFI, CODED, Tmask, the BFAST family's refits, SOM, TWDTW, the agreement of change maps, SNIC's segmentation by rows of tiles and its polygons) |
| `janus-embed` | Foundation-model embeddings (AlphaEarth, TESSERA): the store (every year at reduced resolution, Int8), SIMD kernels chosen at run time (AVX2 + FMA, SSE2, NEON), PCA by subspace iteration, similarity and change, exports at full resolution, the download of an area |
| `janus-validation` | Validation after Olofsson et al. (2014) and Stehman (2014), without an interface: maps read by a rule (classes, intervals, events), strata in one parallel pass at full resolution (other maps warped onto the first's grid, a buffer, a polygon limit, areas on the sphere in a geographic CRS), the design (eq. 13, allocations), the seeded draw, the estimators, dates, kappa, the project in a GeoPackage, the reference of a point from several interpreters, the Python that reproduces the estimates |
| `janus-app` | The program (`janus`, and `jn`, the command line) |

## The program (`crates/janus-app/src`)

| Module | Role |
|---|---|
| `main.rs` | Options, the bundled data (PROJ, GDAL, plugins, CA certificates), the window |
| `app.rs` | The state, the dock of panels (default layout, kept between sessions), panels in windows of their own, menus, shortcuts |
| `map.rs`, `views.rs` | The map (every layer drawn by the GPU into a target, marks, input), map panels and the swipe |
| `panels.rs`, `display.rs`, `layers.rs`, `files.rs`, `settings.rs`, `theme.rs` | Display, Layers, Files, Statistics, Performance and Settings; the seven themes and the fonts (ProggyClean at 13 px, ProggyForever at other sizes) |
| `series.rs`, `chart.rs`, `roi.rs`, `season.rs` | The series on the chart (cursor, pins, ROI, other layers'), the chart, the ROI's shape, the season and temporal composites |
| `classes.rs`, `composite.rs`, `analysis.rs`, `transect.rs` | Categorical series, RGB composites of three bands, the Analysis panel, the space-time transect |
| `vectors.rs`, `labels.rs` | Vector layers (placing, drawing, picking, extracting series) and interpretation (TimeSync style) |
| `zeit.rs`, `python.rs` | The Tools menu, a panel per tool made from its descriptor, fits on the chart, Tasks, Log, result layers; *Copy Python* |
| `embeddings.rs`, `emb_download.rs` | Embeddings in the interface and their download for the visible area |
| `validation.rs`, `validation_tabs.rs`, `chips.rs` | The Validation menu and panel, the project's sample as a vector layer, blind interpretation, Chips |
| `export_map.rs`, `canvas.rs`, `exports.rs`, `animation.rs`, `gif.rs` | Exports (PNG figures drawn on a CPU canvas, GeoTIFFs, results), the Exports window, animations (GIF without a library, MP4/WebM through ffmpeg) |
| `basemap.rs`, `open.rs`, `platform.rs` | The basemap in the interface, Open URL and multidimensional files, what depends on the system |
| `ui_test*.rs`, `selftest_zeit.rs`, `embeddings_test.rs`, `measure*.rs`, `tour.rs`, `golden.rs` | `--selftest-ui`, `--selftest-zeit`, `--selftest-embeddings-ui`, `--measure-cache`, `--measure-vectors`, `--profile-tour`, `--dump-golden` |
| `shots.rs`, `shots_validation.rs` | `--shots`: the website guides' screenshots |
| `bin/jn.rs` | `jn`: prints `--help` and `--version` to the terminal, else opens the interface and returns |

## Elsewhere

| Path | Role |
|---|---|
| `tools/package.py` | The packages: Windows installer (`installer/janus.iss`, Inno Setup), Linux `.tar.xz`, macOS `.dmg`; each with `LICENSE` and `THIRD_PARTY_NOTICES.txt` |
| `tools/notices.py` | `THIRD_PARTY_NOTICES.txt`: the crates in the program, the conda-forge libraries and data a package carries, the fonts, with their license texts |
| `tools/make_selftest_data.py` | Synthetic inputs for the self-tests |
| `tools/make_tutorial_data.py`, `tools/make_guide_data.py` | Real data for the website's guides and their screenshots |
| `tools/zeit_fixtures.py`, `tools/zeit_raster_fixtures.py` | The expected results of Zeit's Python, against which `janus-zeit`'s tests check the tools |
| `.github/workflows/build.yml` | CI: Windows, Linux and macOS builds, tests, self-tests, packages; releases on `v*` tags |
| `site/` | The website (GitHub Pages) |
| `RUST_MIGRATION.md` | How Janus moved from C++ to Rust (0.39 → 1.0), with the measures |
