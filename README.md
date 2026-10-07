# tsv — raster time series viewer

**tsv** is a fast desktop viewer for raster time series and data cubes (one date
per file, or one date per band). It keeps the whole cube on the GPU, computes
per-pixel temporal statistics in a shader, and reads the exact series of any
pixel in the background while you hover the map. It is meant to *explore* time
series quickly, not to be a full GIS.

Built with Dear ImGui (docking) + ImPlot + OpenGL 3.3 + GDAL, in C++17.

![tsv](docs/screenshot.jpg)

## Features

- **Opens** a folder of rasters (1 file = 1 date), a multiband file (1 band = 1
  date), a list of files or a wildcard pattern. Dates are parsed from file names
  or band descriptions (`YYYY-MM-DD`, `YYYYMMDD`, `YYYY_MM`, `YYYY`, `A2001001`, ...).
- **Map modes**, all computed on the GPU: value at date, anomaly (value − mean),
  temporal mean, standard deviation, linear trend (OLS, per year), minimum,
  maximum, amplitude, trend R² and a multitemporal RGB of three dates.
  Changing date, colormap or stretch is instant; there is an animation player.
- **Pixel series**: approximate (from the overview) as soon as you hover, exact
  (full resolution) once the mouse rests. Click to drop pins and compare pixels;
  Shift+drag a rectangle for an ROI (mean and p10–p90 per date).
- **Chart**: lines, markers, stems or stairs; raw values, anomaly or z-score;
  OLS or Sen trend line per series; Y axis locked to the map range if you want.
- **Statistics** per series: n, mean, median, std, CV, min/max (with date),
  amplitude, OLS slope + R², Sen's slope, Mann-Kendall Z and p-value.
- **Full resolution on zoom**: past the overview resolution, tiles of the visible
  area are read in the background and cached on the GPU.
- **Fast**: about 0.15 s from launch to the first frame; the app sleeps when
  nothing changes (a frame costs ~0.3 ms of CPU). The cube overview is cached on
  disk, so reopening a series takes a fraction of a second.
- **Copy CSV** of the cursor, pins and ROI series.

## Installation (Windows)

Run `tsv-<version>-setup.exe`. It installs per user (no administrator prompt),
bundles GDAL (no Python or conda needed), adds a Start menu entry and, optionally:

- adds `tsv` to `PATH`, so you can call it from any terminal;
- adds **Open in tsv** to the right-click menu of folders and `.tif` files;
- creates a desktop shortcut.

Installers are built from this repository (see [Building](#building-from-source)).

## Command line

```
tsv [options] [input ...]

Inputs:
  folder\             every raster in the folder, 1 file = 1 date
  series.tif          1 multiband file, 1 band = 1 date
  a.tif b.tif ...     several files, 1 file = 1 date
  "ndvi_*.tif"        wildcard pattern (* and ?)

Options:
  --band N            band used when each file is one date (default: 1)
  --budget MB         GPU memory for the cube overview (default: 1024)
  --threads N         background reader threads (default: auto; HDD = 1)
  -h, --help          show this help
  --version           show the version
```

Examples:

```
tsv D:\data\ndvi_annual\
tsv landsat_stack.tif
tsv "S2_*_NDVI.tif" --band 1
```

`tsv` opens the window and returns the prompt immediately (`tsv.com` is a small
console launcher next to `tsv.exe`, the same trick Visual Studio uses with
`devenv.com`).

## Using the viewer

| Action | How |
|---|---|
| Pan / zoom | drag / mouse wheel, `Home` fits the map |
| Pixel series | hover (exact once the mouse rests) |
| Compare pixels | click to drop a pin |
| Remove a pin | right click it, `Delete` (last one) or the **x** in the statistics table |
| ROI | `Shift` + drag (mean and p10–p90 per date) |
| Time | `←`/`→` previous/next date, `Space` play/pause, click or drag on the chart |
| Map mode, colormap, range | **Layer** panel (range is automatic 2–98%, or drag it) |
| Chart options | style, values/anomaly/z-score, trend (OLS/Sen), Y = map range |
| Export | **Copy CSV** in the Time series panel |

Clicking a legend entry hides/shows that series together with its trend line.

### Performance notes

- The first open of a cube builds a reduced overview of every date (in parallel
  on SSDs) and caches it in `%LOCALAPPDATA%\tsv\cache`; later opens read that one
  file. The **Performance** panel shows startup, build and read timings.
- Spinning HDDs are detected and read with a single sequential reader (two
  readers were measured to be 4× slower). While the overview builds on an HDD,
  pins, ROI and detail tiles wait, so the head is not pulled away.
- On an HDD the first open is bound by how fast the disk can sweep the files:
  GeoTIFFs stored as 1-row strips must be read almost entirely, even for a
  reduced overview. Cloud-optimized GeoTIFFs (tiled, with internal overviews) or
  an SSD make the first open much faster.

## Building from source

Requirements: Windows, Visual Studio 2022 (C++), CMake ≥ 3.21 and a GDAL
installation with CMake config files, e.g. a conda-forge environment
(`conda create -n geo -c conda-forge gdal`). GLFW, Dear ImGui and ImPlot are
downloaded by CMake (`FetchContent`).

```powershell
cmake -S . -B build -G "Visual Studio 17 2022" -A x64 -DCMAKE_PREFIX_PATH="<conda-env>\Library"
cmake --build build --config Release
.\build\Release\tsv.exe
```

The post-build step (`cmake/deploy_runtime.cmake`) copies `gdal.dll` and all of
its dependencies, plus `proj.db` and `share/gdal`, next to the executable, so the
build runs on double click and never picks up another GDAL from `PATH`.

Installer (requires [Inno Setup 6](https://jrsoftware.org/isinfo.php)):

```powershell
cmake --build build --config Release --target installer   # -> dist\tsv-<version>-setup.exe
```

Developer option: `tsv --measure-startup [input]` prints the startup timings and exits.

## Architecture

```
 files (GDAL) ──► Overview (CPU, [t][y][x] float32) ──► GpuCube (R32F texture array)
       │               │ disk cache (%LOCALAPPDATA%\tsv\cache)        │
       │               └─ instant approximate series                  ├─► statistics shader (1 pass)
       │                                                              └─► display shader (stretch, colormap, modes)
       ├──► TileManager: full-resolution tiles of the visible area ──► LRU cache on the GPU
       └──► exact pixel series / ROI (interactive pool, cancellable)
```

| File | Role |
|---|---|
| `src/cube.*` | Layer and date discovery; per-thread GDAL reader (nodata → NaN, scale/offset) |
| `src/overview.*` | Reduced cube built in parallel, following the focused date; disk cache |
| `src/gpu.*` | Shaders: per-pixel temporal statistics and display; map framebuffer |
| `src/tiles.*` | Detail tiles per zoom level; drops requests that left the screen |
| `src/session.*` | One open series: thread pools, exact series, ROI |
| `src/stats.*` | OLS, Sen's slope, Mann-Kendall, percentiles |
| `src/app.*` | User interface (ImGui/ImPlot) |
| `src/platform.*` | Windows: UTF-8 arguments, dialogs, app data folder, HDD detection |
| `src/launcher.cpp` | `tsv.com` console launcher |

## Roadmap

Planned work and the reasoning behind design decisions live in
[IDEIAS.md](IDEIAS.md) (in Portuguese). Next up: running the
[Zeit](https://github.com/sacridini/zeit-cdts) change-detection and time-series
algorithms (LandTrendr, CCDC, BFAST, phenology, Mann-Kendall) directly from tsv,
through a bundled, invisible Python runtime.
