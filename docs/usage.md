# Using Janus

[README](../README.md) · [Install](install.md) · [Usage](usage.md) · [Building](building.md) · [Architecture](architecture.md)

## What it does

- **Opens** a folder of rasters (1 file = 1 date), a multiband file (1 band = 1
  date), a list of files or a wildcard pattern. Dates are parsed from file names
  or band descriptions (`YYYY-MM-DD`, `YYYYMMDD`, `YYYY_MM`, `YYYY`, `A2001001`, ...).
- **Categorical series** (land cover, masks, classifications): detected when
  the file has a colour table, category names or an attribute table, or when a
  date has only a few whole values. Each class gets the file's colour (or one of
  a qualitative palette) and name, editable in a legend; the chart shows the
  class sequence as steps with class names on the axis, and the statistics show
  the class at the date, the majority class, the number of changes and the last
  change (e.g. *2006: Forest → Pasture*).
- **Several bands per date** (e.g. one Landsat surface-reflectance file per
  date): show any band or a **normalized difference** of two (NDVI, NDMI, NBR...),
  and hide cloudy observations with a **quality band** (Fmask codes, Landsat
  Collection 2 `QA_PIXEL` bits or a 0/1 mask; a band named Fmask or QA_PIXEL is
  used automatically). Multiband tools such as CCDC read every band.
- **Map modes**, all computed on the GPU: value at date, anomaly (value − mean),
  **difference** (value − a reference date: a fixed date or the previous one),
  temporal mean, standard deviation, linear trend (OLS, per year), minimum,
  maximum, amplitude, trend R², **largest drop** (date and magnitude of the
  largest decrease between consecutive valid observations) and a multitemporal
  RGB of three dates. Changing date, colormap or stretch is instant; there is an
  animation player.
- **Pixel series**: approximate (from the overview) as soon as you hover, exact
  (full resolution) once the mouse rests. Click to drop pins and compare pixels;
  Shift+drag a rectangle for an ROI (mean and p10–p90 per date).
- **Chart**: lines, markers, stems or stairs; raw values, anomaly or z-score;
  OLS or Sen trend line per series; Y axis locked to the map range if you want.
- **Statistics** per series: n, mean, median, std, CV, min/max (with date),
  amplitude, OLS slope + R², Sen's slope, largest drop (with date) and, in the
  difference mode, the difference shown on the map (the Mann-Kendall test, with
  autocorrelation corrections, is a Zeit tool).
- **Full resolution on zoom**: past the overview resolution, tiles of the visible
  area are read in the background and cached on the GPU (value and difference
  modes; the difference reads the tiles of both dates).
- **Full-resolution cache**: a lossless copy of the whole series in the cache
  folder (an SSD), built in the background; exact series, pins, ROI and detail
  tiles are then read from it, about 1 ms per series even when the files sit on
  a spinning HDD. On by default for series on an HDD (see [Performance](#performance)).
- **Fast**: about 0.15 s from launch to the first frame; the app sleeps when
  nothing changes (a frame costs ~0.3 ms of CPU). The cube overview is cached on
  disk, so reopening a series takes a fraction of a second.
- **Export**: the map as a **PNG** figure (as shown, rendered at 1×, 2× or 4×
  the screen resolution, with the date, a colour bar or class legend, pins and
  the ROI if wanted, on the map's, a white or a transparent background); the
  active layer's **values** at the date as a Float32 **GeoTIFF** (visible area
  or whole image, full resolution, same grid and CRS, no data = NaN) and the
  **rendered view** as an RGBA GeoTIFF, both ready for QGIS; **Zeit results** as
  GeoTIFF. Files are written in the background (File → Exports shows progress);
  **Copy CSV** of the cursor, pins and ROI series.
- **Layers**: open several series at once (e.g. NDVI and NBR of the same area,
  or neighboring scenes). Layers are placed by their georeferencing, can be
  shown/hidden (including the first one), reordered and faded; the chart shows
  the series of the active layer or of every visible layer at the cursor/pins.
- **Layers in any CRS**: a layer in another projection (e.g. UTM 22S next to UTM
  23S, or geographic EPSG:4326 over UTM) or on a rotated grid is reprojected
  onto the active layer's grid on the GPU (nearest neighbour, values unchanged,
  error < 0.02 px). Cursor, pins and ROI use the exact PROJ transformation. The
  ROI applies to every visible layer.
- **Map panels side by side** (View → New map view): each panel shows one layer,
  optionally at its own date and in its own display mode (e.g. NDVI next to NBR,
  or 2005 next to 2020 of the same series). All panels show the same area: pan
  or zoom in any of them moves every one; the cursor is mirrored as a cross, and
  pins and the ROI appear in all of them. Panels dock anywhere or go to another
  monitor; they share the loaded data (no extra memory).
- **Swipe** (View → Swipe, `S`): a divider you drag across the map; left of it
  the map as it is, right of it another layer, or the same one at another date
  or in another display mode (picked in a bar above the map).
- **Space-time transect** (Hovmöller): `Ctrl` + drag a line on the map (or `T`,
  then drag); the **Transect** panel shows distance along the line against the
  dates, in the layer's colours (class colours for categorical data), first
  from the overview and then at full resolution. Hover a cell for its distance,
  date and value (marked on the map), click to go to that date; values or
  anomalies; Copy CSV.
- **Files panel**: a folder tree listing only rasters by default; double click
  opens a series, right click adds it as a layer.
- **Detachable panels**: drag any panel out of the main window, e.g. the map on
  a second monitor and the charts on the first.
- **Settings** (File → Settings, `Ctrl+,`): a dark, light, classic or Janus
  colour theme, and how many CPU threads Janus and Zeit may use (all cores but
  2 by default, so the computer stays responsive during long runs).
- **Zeit tools** ([Zeit](https://github.com/sacridini/zeit-cdts) change detection
  and time-series algorithms): **LandTrendr**, **Mann-Kendall** (with Sen's
  slope; Hamed-Rao, Yue-Wang and seasonal variants), **BFAST**, **BFAST Lite**,
  **BFAST Monitor**, **phenology** (season start/peak/end, length, amplitude),
  **CCDC** (multiband), **TWDTW classification** (classes from pins) and
  **smoothing** (Whittaker, Savitzky-Golay; chart only). Each is fitted live on the cursor, pins and ROI series
  (segments, trend lines, break dates, seasons or CCDC models drawn on the chart),
  or run on the whole image, the visible area or an ROI, with the results
  (year of detection, magnitude, slope, p-value, break dates...) shown as map
  layers. Zeit runs in a bundled, invisible Python runtime — nothing to install.

## Command line

```
jn [options] [input ...]

Inputs:
  folder\             every raster in the folder, 1 file = 1 date
  series.tif          1 multiband file, 1 band = 1 date
  a.tif b.tif ...     several files, 1 file = 1 date
  "ndvi_*.tif"        wildcard pattern (* and ?)

Options:
  --band N            band used when each file is one date (default: 1)
  --budget MB         memory for the cube overview, this run only
                      (default: Settings, 1024)
  --threads N         background reader threads, this run only (default: the
                      processing threads in Settings, at most 12; HDD = 1);
                      with --selftest-zeit, Zeit's threads
  -h, --help          show this help
  --version           show the version

Developer options:
  --zeit-python EXE   Python with Zeit to use instead of the bundled runtime
                      (or set JANUS_ZEIT_PYTHON)
  --zeit-bridge PY    bridge script to use (or set JANUS_ZEIT_BRIDGE)
  --measure-startup   print startup timings and exit after the first frame
  --measure-cache on|off IN  time exact series and ROI from the files, build the
                      overview (and the full-resolution cache if on), time the
                      reads again from the cache, in a hidden window
  --selftest-zeit IN  run the Zeit tools end to end on IN without a window
  --selftest-ui A B [C [D [E [F]]]]  drive the layers workflow in a hidden window
                      (A, then B as a layer; C: several bands per date;
                      D, E: categorical, with and without a colour table;
                      F: B in another CRS, reprojected over it)
```

Examples:

```
jn D:\data\ndvi_annual\
jn landsat_stack.tif
jn "S2_*_NDVI.tif" --band 1
```

`jn` opens the window and returns the prompt immediately (on Windows `jn.com` is
a small console launcher next to `janus.exe`, the same trick Visual Studio uses with
`devenv.com`).

## Using the viewer

| Action | How |
|---|---|
| Pan / zoom | drag / mouse wheel (zooming out stops at the image extent), `H` fits the map; trackpad: two fingers pan, pinch zooms |
| Pixel series | hover (exact once the mouse rests) |
| Compare pixels | click to drop a pin |
| Remove a pin | right click it (Mac: Control + click, or a two-finger click on the trackpad), `Delete` (last one) or the **x** in the statistics table |
| ROI | `Shift` + drag: rectangular ROI, mean and p10–p90 per date; with *All visible layers* on the chart, also on every visible layer (its own pixels under the rectangle, reprojected if needed) |
| Time | `←`/`→` previous/next date, `Space` play/pause, click or drag on the chart |
| Map mode, colormap, range | **Display** panel, for the active layer (range is automatic 2–98%, or drag it) |
| Change between dates | **Display** panel → *Difference (value - reference)*: pick the *Reference* (a date, or *Previous date (t-1)*); diverging colours centered at 0, follows the time bar |
| Largest drop | **Display** panel → *Largest drop: date* or *magnitude*: the largest decrease between consecutive valid observations (no-data dates skipped), dated at the lower one; the colour bar shows dates. Exact values for the cursor and pins in the **Statistics** panel |
| Classes (categorical data) | **Display** panel → *Categorical (classes)*: legend with colours, names and shares (click a colour to change it, untick a class to hide it); detection can be switched off or forced |
| Band, index, cloud mask | **Display** panel → Bands (one file per date with several bands): band A, optional normalized difference with B, quality band; **Apply** reopens the layer in place |
| Performance panel | **View → Performance** (hidden by default): timings, threads in use, Zeit status, overview memory, full-resolution cache (progress, size, read times, **Build it now**) |
| Settings | **File → Settings...** (`Ctrl+,`, Mac: `Command+,`): interface theme (Dark, Light, Classic, Janus), processing threads, overview memory, full-resolution cache (when it is built, its budget) and clearing the caches; kept between sessions (see [Settings](#settings)) |
| Several series | **Layers** panel or File → Add layer (`Ctrl+L`): show/hide, order, opacity, close; click a name to make it active. Layers in another CRS show "reprojected from EPSG:…" (hover for the grid size and its error) |
| Browse files | **Files** panel: double click opens, right click → Add as layer; Ctrl+click selects several files |
| Chart of several layers | Time series panel → *All visible layers* (one marker shape per layer) |
| Second monitor | drag a panel's tab out of the main window |
| Maps side by side | View → New map view (`Ctrl+T`): pick the layer and, if wanted, its own date and mode in the panel's bar; every panel follows the same pan/zoom; close it with its tab's **x** or `Ctrl+W` (the focused panel, else the last one opened) |
| Swipe | View → Swipe (`S`): drag the divider (white line with a handle); the bar above the map picks what is right of it: a layer and, if wanted, its own date and mode; **Swipe off** or `S` again closes it |
| Space-time transect | `Ctrl` + drag a line on the map (Mac: `Command` + drag), or `T` / View → Draw transect, then drag; `Esc` cancels. **Transect** panel: distance from A (X) × dates (Y, oldest on top); hover a cell = distance, date, value, marked on the map; click = go to that date; Values / Anomaly (− each place's mean); Copy CSV (a row per date, a column per sample); Clear, or close the panel |
| Chart options | style, values/anomaly/z-score, trend (OLS/Sen), Y = map range |
| Map as a figure | File → Export map as PNG...: resolution (1×, 2×, 4×), background, date label, legend, pins and ROI |
| Data for QGIS | File → Export values as GeoTIFF... (the active layer at the date: visible area or whole image) or Export rendered view as GeoTIFF... (RGBA as shown, georeferenced) |
| Save a tool result | right click it in the **Layers** panel → Save as GeoTIFF..., or File → Export Zeit result as GeoTIFF |
| Export progress | File → Exports (opens by itself with each export): progress, cancel, open the folder |
| Series as text | **Copy CSV** in the Time series and Transect panels |
| Zeit tools | **Tools** menu → tool window (parameters, chart fitting, raster runs); progress in **Tools → Tasks** |
| Tool results | listed under their layer in the **Layers** panel: show/hide, colormap, range, opacity |

Clicking a legend entry hides/shows that series together with its trend line.

## Zeit tools

The **Tools** menu lists the algorithms of [Zeit](https://github.com/sacridini/zeit-cdts)
that Janus exposes:

| Tool | Series | Maps |
|---|---|---|
| LandTrendr | annual (one date per year) | year of detection, magnitude, duration, pre/post value, rate, DSNR of the chosen loss/gain event |
| Mann-Kendall | any (seasonal variant: several dates per year) | significant Sen's slope, slope, p-value, Z, tau, trend class, intercept |
| BFAST | regular, ≥ 2 dates per year | trend/season break counts, date and magnitude of the largest trend break |
| BFAST Lite | regular, ≥ 2 dates per year | break count, date and magnitude of the largest break, first break date |
| BFAST Monitor | regular, ≥ 2 dates per year | first break date in the monitoring period, magnitude, break yes/no |
| Phenology | ≥ 6 dates per year | start, peak and end of season (day of year), length, peak value, amplitude, fit R², seasons — for a typical year (median), the latest season or a chosen year |
| CCDC | one file per date with blue, green, red, NIR, SWIR1, SWIR2 [+ thermal]; a quality band is recommended | break count, largest break date and magnitude, NDVI change, first/last break, segments |
| TWDTW classification | dates | class map (one class per pattern, with a legend), distance, margin to the 2nd class |
| Smoothing | any | chart only: the smoothed series (Whittaker or Savitzky-Golay) |

**TWDTW classes** come from the series itself: drop pins on places you know
(forest, crop, pasture...), open the tool and *Add a pattern from* each pin (or
the ROI mean), and name the classes. Patterns are matched on real dates, so they
should cover the same period as the series.

"Regular" means evenly spaced dates (monthly, 16-day...): a missing date must be
a no-data band, not a skipped one. BFAST and BFAST Lite are slow (~2–3 ms per
pixel on all cores): use the visible area or an ROI before a whole scene.

Multiband tools show a **Bands** section in their window: which band of each
date plays each role (guessed from the band names, e.g. `NIR`, `SWIR1`; unnamed
stacks of 6+ bands are taken as Blue, Green, Red, NIR, SWIR1, SWIR2). CCDC
works on reflectance × 10000 internally; reflectance 0–1 and Landsat C2 Level-2
digital numbers are detected and converted (parameter *Units*). Its model is
drawn in the units of what the layer shows (a band or a normalized difference).

Each tool window has:

- **Parameters**, generated from the tool description sent by Zeit (hover a
  field for help). Tools that do not fit the open series are disabled with the
  reason (e.g. LandTrendr needs an annual series).
- **On the chart**: fits the model to the cursor, pin and ROI series and draws it
  over them in the same color (LandTrendr segments and vertices, Sen's line,
  vertical lines at break dates); the model's numbers are added to the
  Statistics table. With *All visible layers* on the chart, single-band tools
  are also fitted to the other layers' cursor and pin series.
- **Estimated run time** for the chosen scope, measured by Zeit on samples of
  the series (a fixed cost per block plus a cost per pixel; reading the data is
  not included).
- **Run on the raster**: whole image, visible area or ROI. The run happens in a
  separate process using the processing threads of [Settings](#settings)
  (every logical core but 2 by default), with progress and cancel in
  **Tools → Tasks**. Outputs are GeoTIFFs (default folder
  `%LOCALAPPDATA%\Janus\results`) loaded over the map and listed under their
  series in the Layers panel; cells without an event are transparent.

How it works: Zeit runs in a separate Python process from a private runtime
inside the installation (`runtime\`: embeddable Python 3.12 + Zeit from PyPI +
numpy/scipy/rasterio/numba/dask/xarray, without PyTorch). It starts in the
background once a series is open (~1 s), so it never delays startup. The bridge
(`zeit_bridge/janus_zeit_bridge.py`) handles the protocol and reads/writes rasters
in chunks with progress; each tool is a small module next to it
(`zeit_bridge/tool_*.py`) that describes its parameters and outputs and calls
Zeit's API for one series or one block of pixels. Adding a tool to Janus means
adding a module there — no C++ change.
Janus hands it the cube as a VRT (dates in order, nodata and scale applied).
Logs: `%LOCALAPPDATA%\Janus\zeit.log`.

## Performance

- The first open of a cube builds a reduced overview of every date (in parallel
  on SSDs) and caches it in `%LOCALAPPDATA%\Janus\cache` (macOS: `~/Library/Caches/Janus`); later opens read that one
  file. Each date is saved as soon as it is built, so closing Janus in the
  middle of a build loses nothing: the next open reads those dates from the cache
  and builds only the missing ones. The **Performance** panel shows startup,
  build and read timings.
- Spinning HDDs are detected and read with a single sequential reader (two
  readers were measured to be 4× slower). While the overview builds on an HDD,
  pins, ROI and detail tiles wait, so the head is not pulled away.
- On an HDD the first open is bound by how fast the disk can sweep the files:
  GeoTIFFs stored as 1-row strips must be read almost entirely, even for a
  reduced overview. Cloud-optimized GeoTIFFs (tiled, with internal overviews) or
  an SSD make the first open much faster.
- **Full-resolution cache** (`.janusfull` files next to the overview cache):
  every date in 64×64 blocks, each compressed losslessly (float bits as
  integers, difference with the previous pixel, byte planes, zstd), so a series
  reads one small block per date. Values are bit-for-bit those of the files
  (after nodata, scale, normalized difference and quality mask). Measured on
  Landsat NDVI composites on a 5400 rpm HDD (41 dates of 7441×7317 Float32, LZW,
  1-row strips, 8.5 GB per tile):

  | | from the files (HDD) | from the cache |
  |---|---|---|
  | exact series (41 dates) | 250–500 ms cold, ~5 ms once read | 0.7–1.0 ms |
  | ROI 256×256 (41 dates) | ~2 s cold, ~1.3 s once read | ~21 ms |

  Size: 6.3–6.7 GB per tile (1.37–1.41× smaller than raw floats; zstd alone
  gives 1.17×). On an HDD the first open builds it in the same read as the
  overview: the overview pass reads each date at full resolution, one thread
  streams the file ahead while four decode and compress, and the overview is
  taken from those rows (48.7 s for 38 dates, against 52 s for 41 dates of the
  overview alone). When the overview already came from its cache, the cache is
  built in a pass of its own (54 s per tile, bound by the disk). Each date is saved as it is
  built and used right away; a stopped build resumes.
- The cache is on by default for series on an HDD; **File → Settings**
  turns it on for every series or leaves it to **Build it now** (per series),
  and sets its budget (64 GB by default: the least recently opened series leave
  first). With the files on an SSD it matters less.
- **Threads**: on an SSD the background readers (overview, full-resolution
  cache) use the processing threads of [Settings](#settings), at most 12, and
  exact series, ROI, tiles and the transect up to 4. On an HDD both are one
  reader whatever the setting (a disk decision); the full-resolution cache
  built with the overview decodes on half the processing threads (1 to 4).
  The Performance panel shows the counts in use.

## Settings

**File → Settings...** (`Ctrl+,`; Mac: `Command+,`). Everything applies at
once and is kept in the layout file, next to the other data of Janus
(`%LOCALAPPDATA%\Janus\layout-0.7.ini` on Windows).

| Setting | What it does |
|---|---|
| Theme | **Dark** (the default), **Light**, **Classic** (ImGui's original colours) or **Janus** (the program's colours: blue on navy panels, orange accents). Status text and chart series are adjusted to stay legible on each; the map, its colour bars and exported figures keep their colours |
| Processing threads | How many CPU threads Janus and Zeit may use, shown as *N of M* logical cores; default: all but 2 (at least 1), so the computer stays responsive during long runs. Applies to the readers of the open series (they shrink or grow at once), the full-resolution cache, exports (GeoTIFF compression, PNG) and Zeit: raster jobs started from then on (as `OMP_NUM_THREADS`, `NUMBA_NUM_THREADS`, the BLAS limits and Zeit's `n_jobs`; jobs already running keep theirs) and the process that fits the chart and estimates run times (replaced in the background once idle) |
| Overview memory | Memory for the cube overview (1024 MB by default); **Apply** reopens the active layer. `--budget` overrides it for one run |
| Clear overview cache | Deletes the cached overviews except the active layer's |
| Full-resolution cache | When it is built (on demand, series on an HDD, every series), its budget, and clearing it (see [Performance](#performance)) |
