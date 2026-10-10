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
  Shift+drag a rectangle for an ROI (mean and p10–p90 per date, or a box plot
  per date: p25–p75 box, median, whiskers to p10 and p90).
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
  GeoTIFF (e.g. each LandTrendr output: year of detection, magnitude,
  duration...); the **embedding view** shown (principal components,
  similarity, change) as a Float32 GeoTIFF at full resolution. The export window shows where the file goes (*Save to*: type a path,
  or a name alone for the last export folder, or pick one with **Browse...**;
  it warns before replacing a file). Files are written in the background
  (File → Exports shows progress);
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
- **Basemap** (Layers panel → Basemap): satellite imagery or a map under the
  series, for context. Sources: **Esri World Imagery**, **Sentinel-2 cloudless
  2016** by EOX (CC BY 4.0), **OpenStreetMap**, or your own **XYZ URL**
  (`https://.../{z}/{x}/{y}.png?key=...`, e.g. Google Map Tiles API, MapTiler,
  Planet: with your key and under the provider's terms). Off by default:
  nothing is downloaded until you pick a source, and each session and each
  series opened start with it hidden (the source is remembered; adding a layer
  keeps it) until you tick it. Only the tiles in view, at the
  screen's zoom level, are read in the background (coarser ones fill in while
  they arrive) and kept in the cache folder; the map never waits for them. It
  is drawn under every layer (lower a layer's opacity to see the ground), on the
  map panels and the swipe (whose comparison can be the basemap alone), and in
  the exports, always with the source's attribution (bottom right). Tiles are
  in Web Mercator and placed on the active layer's grid like a layer in another
  CRS, so the active layer needs a CRS. Offline, the map shows "Basemap
  unavailable (offline)".
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
- **Analysis panel** (View → Analysis), three tabs:
  - **Seasonal** (series with several dates a year): the cursor, a pin or the
    ROI mean by day of the year, one line per year (colour = year, the current
    date's year on top) with the mean of all years by month (± 1 standard
    deviation), or a **year × day-of-year heatmap** in the layer's colours.
  - **Classes** (categorical layers): the **share of each class at every date**
    (stacked areas or lines) and the **transitions** between two dates (the
    largest ones, the share of pixels that changed, the full matrix); Copy CSV
    of both.
  - **Scatter**: two layers, or two dates of one, pixel by pixel in the ROI (or
    the whole image) as a density plot, with the 1:1 line, the least squares
    line, r, R², the mean difference and its RMS.

  Classes and scatter count the overview's pixels (what the map shows before
  zooming in), so they are immediate on any series.
- **Files panel**: a folder tree listing only rasters by default; double click
  opens a series, right click adds it as a layer or to **Favorites** (listed on
  top, kept between sessions). A folder's tooltip says what it would open as
  ("41 rasters, 1985 to 2025, yearly", from the file names). Whatever is opened
  (from the panel, File → Open, a drop or the command line), the tree goes to
  it. Dropping files with **Shift** held adds them as a layer.
- **Detachable panels**: drag any panel out of the main window, e.g. the map on
  a second monitor and the charts on the first.
- **Settings** (File → Settings, `Ctrl+,`): a dark, light, classic or Janus
  colour theme, the interface font size, and how many CPU threads Janus and Zeit may use (all cores but
  2 by default, so the computer stays responsive during long runs).
- **Zeit tools** ([Zeit](https://github.com/sacridini/zeit-cdts) change detection
  and time-series algorithms): **LandTrendr**, **Mann-Kendall** (with Sen's
  slope; Hamed-Rao, Yue-Wang and seasonal variants), **BFAST**, **BFAST Lite**,
  **BFAST Monitor**, **phenology** (season start/peak/end, length, amplitude),
  **CCDC** (multiband), **CODED** (forest degradation, multiband), the
  **NDFI** (spectral unmixing), **Tmask** (clouds the quality band missed),
  **TWDTW classification** (classes from pins), **SOM clustering** (typical
  trajectories), the **agreement** of several change maps and **smoothing**
  (Whittaker, Savitzky-Golay; chart only). Each is fitted live on the cursor, pins and ROI series
  (segments, trend lines, break dates, seasons or CCDC models drawn on the chart),
  or run on the whole image, the visible area or an ROI, with the results
  (year of detection, magnitude, slope, p-value, break dates...) shown as map
  layers; the NDFI and the series screened by Tmask open as new layers. Zeit
  runs in a bundled, invisible Python runtime — nothing to install.

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
                      F: B in another CRS, reprojected over it; the
                      basemap is checked with local tiles, no network)
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
| ROI | `Shift` + drag: rectangular ROI, mean and p10–p90 per date (or a box plot per date: the *ROI* list above the chart); with *All visible layers* on the chart, also on every visible layer (its own pixels under the rectangle, reprojected if needed) |
| Time | `←`/`→` previous/next date, `Space` play/pause, click or drag on the chart |
| Map mode, colormap, range | **Display** panel, for the active layer (range is automatic 2–98%, or drag it) |
| Change between dates | **Display** panel → *Difference (value - reference)*: pick the *Reference* (a date, or *Previous date (t-1)*); diverging colours centered at 0, follows the time bar |
| Largest drop | **Display** panel → *Largest drop: date* or *magnitude*: the largest decrease between consecutive valid observations (no-data dates skipped), dated at the lower one; the colour bar shows dates. Exact values for the cursor and pins in the **Statistics** panel |
| Classes (categorical data) | **Display** panel → *Categorical (classes)*: legend with colours, names and shares (click a colour to change it, untick a class to hide it); detection can be switched off or forced |
| Band, index, cloud mask | **Display** panel → Bands (one file per date with several bands): band A, optional normalized difference with B, quality band; **Apply** reopens the layer in place |
| Reset layout | **View → Reset layout** (`Ctrl+Shift+R`, Mac: `Command+Shift+R`): every panel docked where it was at the first start, the floating windows (Exports, Tasks, Log, Settings) back to their first size and place, map panels to the right of the main map; the panels open stay open |
| Performance panel | **View → Performance** (hidden by default): timings, threads in use, Zeit status, overview memory, full-resolution cache (progress, size, read times, **Build it now**) |
| Settings | **File → Settings...** (`Ctrl+,`, Mac: `Command+,`): interface theme (Dark, Light, Classic, Janus), font size, whether the Files panel follows what is opened, processing threads, overview memory, full-resolution cache (when it is built, its budget) and clearing the caches; kept between sessions (see [Settings](#settings)) |
| Several series | **Layers** panel or File → Add layer (`Ctrl+L`): show/hide, order, opacity, close; click a name to make it active. Layers in another CRS show "reprojected from EPSG:…" (hover for the grid size and its error) |
| Basemap | **Layers** panel → Basemap (below the layers): pick a source (None = nothing downloaded), tick to show/hide, opacity; *Custom XYZ URL*: the URL (`{z}`, `{x}`, `{y}`; `{-y}` for TMS rows; applied when you leave the field), the attribution to show, the finest zoom and the tile size. The source and its settings are kept between sessions, but it starts hidden, as does every series opened (tick to show); the map panels' and the swipe's layer list has *Basemap only* |
| Browse files | **Files** panel: double click opens, right click → Add as layer or Add to favorites; Ctrl+click selects several files; hover a folder for what it would open as. Opening a series expands the tree down to it (Settings → *Files panel follows what is opened*) |
| Add by dropping | drop files or a folder on the window with `Shift` held: a new layer (without Shift the drop replaces the series) |
| Seasonal views, classes over time, scatter | **View → Analysis** (see above) |
| Chart of several layers | Time series panel → *All visible layers* (one marker shape per layer) |
| Second monitor | drag a panel's tab out of the main window |
| Maps side by side | View → New map view (`Ctrl+T`): pick the layer and, if wanted, its own date and mode in the panel's bar; every panel follows the same pan/zoom; close it with its tab's **x** or `Ctrl+W` (the focused panel, else the last one opened) |
| Swipe | View → Swipe (`S`): drag the divider (white line with a handle); the bar above the map picks what is right of it: a layer and, if wanted, its own date and mode, or *Basemap only*; **Swipe off** or `S` again closes it |
| Space-time transect | `Ctrl` + drag a line on the map (Mac: `Command` + drag), or `T` / View → Draw transect, then drag; `Esc` cancels. **Transect** panel: distance from A (X) × dates (Y, oldest on top); hover a cell = distance, date, value, marked on the map; click = go to that date; Values / Anomaly (− each place's mean); Copy CSV (a row per date, a column per sample); Clear, or close the panel |
| Chart options | style, values/anomaly/z-score, trend (OLS/Sen), Y = map range |
| Map as a figure | File → Export map as PNG... (`Ctrl+E`): resolution (1×, 2×, 4×), background, date label, legend, pins and ROI; *Save to*: the file (a path, a name alone for the last export folder, or **Browse...**), then **Export** (or `Enter`) |
| Data for QGIS | File → Export values as GeoTIFF... (`Ctrl+Shift+E`; the active layer at the date: visible area or whole image) or Export rendered view as GeoTIFF... (`Ctrl+Shift+V`; RGBA as shown, georeferenced) |
| Save a tool result | right click it in the **Layers** panel → Save as GeoTIFF..., or File → Export Zeit result as GeoTIFF; `Ctrl+S` saves the result drawn on top. A copy of the result: Float32, full resolution, the series' grid and CRS, no data = NaN, named `<layer>_<tool>_<output>.tif` |
| Save every output of a run | right click a result → Save all outputs of this run... (`Ctrl+Shift+S` for the run of the result on top): all of them (e.g. LandTrendr's year of detection, magnitude, duration, rate, DSNR, values before and after) into a folder at once; the popup lists the files and warns before replacing one |
| Save embeddings | File → Export embeddings as GeoTIFF... (`Ctrl+Shift+M`; when a layer is drawn as embeddings): the view shown, visible area or whole layer, at full resolution; see [Embeddings](#embeddings) |
| Export progress | File → Exports (`Ctrl+J`; opens by itself with each export): progress, cancel, open the folder |
| Shortcuts on a Mac | `Command` instead of `Ctrl` (e.g. `Command+E`, `Command+Shift+S`) |
| Series as text | **Copy CSV** in the Time series and Transect panels |
| Zeit tools | **Tools** menu → tool window (parameters, chart fitting, raster runs); progress in **Tools → Tasks** |
| Zeit logs | **Tools → Log** (or **Log** next to a task): the log of each raster run, followed live (what ran, Python's output, progress, outputs, the end), or `zeit.log`; Copy, open the file or its folder |
| Tool results | listed under their layer in the **Layers** panel: show/hide, colormap, range, opacity |
| Embeddings | **View → Embeddings**: principal components as RGB (of every year, or *local* to the ROI), similarity to the cursor, a pin or the ROI, change between years; see [Embeddings](#embeddings) |
| Add embeddings of the visible area | **Layers** panel → Embeddings (above the basemap): AlphaEarth or TESSERA, years, cell size, visible area or ROI, then **Add embeddings**; progress on the map, the layer comes on top when it is ready |

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
| CODED (degradation) | one file per date with blue, green, red, NIR, SWIR1, SWIR2, ≥ 2 dates per year; a quality band is recommended | strata (forest, non-forest, degradation, deforestation, disturbance), first and last change dates, NDFI change, changes, mean NDFI of the training period |
| NDFI (spectral unmixing) | one file per date with blue, green, red, NIR, SWIR1, SWIR2 | the NDFI of every date as a **new layer** (and, if asked, the GV, NPV, soil and shade fractions), mean and lowest NDFI, observations used |
| Tmask (cloud screening) | one file per date with green and SWIR1, ≥ 2 dates per year | observations flagged and their share; the series shown without them as a **new layer** |
| TWDTW classification | dates | class map (one class per pattern, with a legend), distance, margin to the 2nd class |
| SOM clustering | any | cluster map (the typical series of the cluster under the cursor is drawn on the chart), distance to it |
| Agreement of change maps | maps of dates from other tools (any series) | consensus year, how many maps agree (and their share), maps with a change, spread of the years |
| Smoothing | any | chart only: the smoothed series (Whittaker or Savitzky-Golay) |

**TWDTW classes** come from the series itself: drop pins on places you know
(forest, crop, pasture...), open the tool and *Add a pattern from* each pin (or
the ROI mean), and name the classes. The distance is that of the R package
twdtw: a pattern may match any stretch of the series, and dates without a value
are left out of each pixel's series. By default time is measured between the
dates (*Time measured*), so patterns should cover the same period as the
series, and dates more than a year apart are never matched (*Max time apart*);
measured between days of the year, a pattern of one season matches that season
in any year.

**CODED** (Bullock et al. 2020) monitors forests with the NDFI of every
observation: a model of the NDFI over a training period (by default the first 3
years; *Monitoring start* moves it), then *Consecutive observations* more than
*Threshold* RMSEs below it are a change. After a change, the land is forest
again (degradation: selective logging, understory fire) when the new model's
mean NDFI is at least *Forest NDFI* (0.5), otherwise deforestation. On the
chart: vertical lines at the changes, and, when the layer shows a normalized
difference, the NDFI of each observation and the training mean. The **NDFI**
tool gives the index itself (Souza et al. 2005: near 1 in closed forest, below
0 on soil and pasture) as a series of the same dates, a new layer on which
LandTrendr, BFAST or the agreement can run.

**Tmask** (Zhu & Woodcock 2014) flags the observations whose green is too bright
(cloud) or whose SWIR is too dark (shadow) for a robust harmonic model of the
pixel's own series: haze and thin clouds a quality band misses. The flagged
dates are marked on the chart; a raster run writes the series shown without
them as a new layer. The model spans the whole series, so after a sudden
lasting change (a clearing) observations may be flagged too: look at such
pixels on the chart.

**SOM clustering** trains a Self-Organizing Map on a sample of the area (rows
spread over it, *Training sample* pixels) and gives every pixel the nearest
neuron: pixels with alike trajectories share a cluster, and neighbouring
clusters are alike. Each pixel needs a value on every date: gaps are filled
linearly in time (or the pixel is left out). Hover the cluster map: the chart
draws the typical series of the cluster under the cursor, in its colour.

The **agreement** runs on maps Janus already shows, not on the series: check two
or more results whose values are dates (LandTrendr's year of detection; CCDC,
BFAST or CODED dates), from any layer. A date counts by its calendar year, and
maps agree when their years are at most *Tolerance* apart; the consensus year is
the one most maps agree with. The outputs are on the grid of the first map
checked (the others are put on it), under its series.

A break's date (BFAST, BFAST Lite, BFAST Monitor) is that of the first
observation after it, and its magnitude the model after the break minus the
model before it on that date, as in Zeit's `extract_events`.

"Regular" means evenly spaced dates (monthly, 16-day...): a missing date must be
a no-data band, not a skipped one. BFAST and BFAST Lite are slow (~2–3 ms per
pixel on all cores): use the visible area or an ROI before a whole scene.

Multiband tools show a **Bands** section in their window: which band of each
date plays each role (guessed from the band names, e.g. `NIR`, `SWIR1`; unnamed
stacks of 6+ bands are taken as Blue, Green, Red, NIR, SWIR1, SWIR2). CCDC,
CODED, the NDFI and Tmask work on reflectance × 10000 internally; reflectance
0–1 and Landsat C2 Level-2 digital numbers are detected and converted
(parameter *Units*, decided once per run). CCDC's model is drawn in the units
of what the layer shows (a band or a normalized difference).

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
  the series (a fixed cost per block plus a cost per pixel, and the fit of
  tools that learn from a sample, such as the SOM; reading the data is not
  included).
- **Run on the raster**: whole image, visible area or ROI. The run happens in a
  separate process using the processing threads of [Settings](#settings)
  (every logical core but 2 by default), with progress and cancel in
  **Tools → Tasks**. Outputs are GeoTIFFs (default folder
  `%LOCALAPPDATA%\Janus\results`) loaded over the map and listed under their
  series in the Layers panel; cells without an event are transparent. Series
  outputs (the NDFI, a screened series) are one file with a band per date,
  added as a layer over the others (the active layer stays).
- **A log of every raster run**, `job.log` in the run's results folder (next to
  `job.json` and the outputs): the tool, its parameters, window and threads,
  then everything Python writes (warnings, prints, tracebacks), the progress
  (every 10% or 5 s), each output and how it ended. **Tools → Log** shows it
  while it grows (pick the run, or `zeit.log`, in the list), as does the
  **Log** button of each task.

How it works: Zeit runs in a separate Python process from a private runtime
inside the installation (`runtime\`: embeddable Python 3.12 + Zeit from PyPI +
numpy/scipy/rasterio/numba/dask/xarray/rioxarray, without PyTorch). It starts in the
background once a series is open (~1 s), so it never delays startup. The bridge
(`zeit_bridge/janus_zeit_bridge.py`) handles the protocol and reads/writes rasters
in chunks with progress (the next chunk is read while the tool computes the
current one, and the outputs are compressed on the processing threads: a
Mann-Kendall run takes about half the time it did in 0.25, a LandTrendr run
about 80%); each tool is a small module next to it
(`zeit_bridge/tool_*.py`) that describes its parameters and outputs and calls
Zeit's API for one series or one block of pixels. Adding a tool to Janus means
adding a module there — no C++ change.
Janus hands it the cube as a VRT (dates in order, nodata and scale applied).
Logs: each raster run's `job.log` (above); `%LOCALAPPDATA%\Janus\zeit.log`
for the process that fits the chart and estimates run times (its warnings and
errors), with a line per raster run pointing to its log.

## Embeddings

Foundation models summarise each 10 m pixel and year as a vector:
AlphaEarth Foundations (Google DeepMind; 64 dimensions, from
optical, radar, lidar and climate data) and [TESSERA](https://geotessera.org)
(128, from Sentinel-1 and -2 time series). The values have no physical unit;
what matters is how vectors compare: similar places have similar vectors.

**Opening.** A folder with one GeoTIFF per year and one band per dimension (what
Janus downloads, or `zeit.save_raster` per year), a single GeoTIFF of one year
with 16 or more bands, or `zeit.save_raster`'s file of every year (bands
`<date>_A00`...). Janus recognises them by Zeit's `ZEIT_EMBEDDING` tag or by
their band names (`A00`, `A01`...), and opens the **Embeddings** panel. Zeit's
change detection tools are disabled on them (an embedding has no seasonal
signal), as Zeit itself refuses them.

**Adding them over any series.** In the **Layers** panel, *Embeddings*: pick the
source, the years and the cell size (10 m native; coarser cells are the mean of
the 10 m vectors), the visible area or the ROI, and **Add embeddings**. The size
is estimated first (Int16, D values per pixel and year) and very large areas are
refused. Zeit downloads in the background (a bar on the map tells the year and
the time left; **Tools → Tasks** has its log) and writes one GeoTIFF per year
under `%LOCALAPPDATA%\Janus\results\embeddings`; the layer is added on top,
reprojected if the series is in another CRS, while the series you were studying
stays active. The first AlphaEarth download fetches its file index (78 MB, once
a month).

**Views** (Embeddings panel):

- *Principal components (RGB)*: three components (PC1-3 by default; any of the
  first six) stretched between the 2 and 98 % percentiles. One PCA for every
  year, so a colour means the same in each year and playing the series shows
  real change. *ROI (local)*: the PCA of the ROI's pixels only, applied to the
  whole map: the colours spread over the differences inside the ROI (e.g. the
  states of a forest that the whole scene's PCA spends on water vs. city). It
  is fitted again as you move the ROI (a few milliseconds).
- *Similarity to a reference*: the cosine similarity of every pixel to the
  vector under the cursor (live, as the mouse moves), a pin, or the ROI's mean
  vector, in the year shown or a fixed year. The panel charts the similarity of
  the cursor and the pins over the years ("did this pasture come to look like
  the forest?").
- *Change between years*: 1 − cosine similarity of each pixel's vectors in the
  year shown and the previous year (or a fixed year).
- *Band values*: one dimension as an ordinary band, with every mode of the
  Display panel.

The panel also plots the *latent profile*: the D values of the cursor's and the
pins' vectors. The status bar shows the components or the similarity under the
cursor.

**Saving a view** (File → Export embeddings as GeoTIFF...): the view shown, over
the visible area or the whole layer, as a Float32 GeoTIFF on the layer's own
grid and CRS (no data = NaN), computed again at full resolution from the
vectors in the files (not from the reduced copy on screen). *Principal
components*: one band per fitted component (six), the scores along each one
(not stretched), named with their share of the variance; *Similarity*: one band,
the cosine similarity to the reference shown; *Change*: one band, the cosine
distance between the two years. The model, year, reference and licence go in
the file's metadata.

**How it is computed.** Every year is kept in memory at reduced resolution as
Int8 with a scale per dimension (within the overview memory of Settings), its
range taken from a sample of every year (0.1 to 99.9 %, widened by 5 %): every
year fits it, not just one. The
PCA takes up to 65 536 vectors: the covariance in parallel with SIMD kernels
(AVX2 + FMA when the CPU has them, else SSE2; NEON on Apple Silicon), then the
leading eigenvectors by subspace iteration. Measured (2026-10-10, 18 threads,
AVX2): a PCA of 128 dimensions in ~10 ms (covariance 4 ms, 19 ms without SIMD),
an image of a year of 1 Mpx in ~4 ms; reading 8 years of TESSERA (622 × 453 px,
480 MB of GeoTIFFs) in ~4 s. `jn --selftest-embeddings` prints these timings on
any computer.

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
| Font size | Size of the interface text, 10 to 28 px (13 by default: ImGui's pixel font; other sizes use its scalable font). Widgets keep their widths; exported figures keep their text as at 13 px |
| Files panel follows what is opened | On by default: opening a series, a file or a folder (from anywhere) expands the Files panel down to it and scrolls there; its folder is listed again, so new files show up |
| Processing threads | How many CPU threads Janus and Zeit may use, shown as *N of M* logical cores; default: all but 2 (at least 1), so the computer stays responsive during long runs. Applies to the readers of the open series (they shrink or grow at once), the full-resolution cache, exports (GeoTIFF compression, PNG) and Zeit: raster jobs started from then on (as `OMP_NUM_THREADS`, `NUMBA_NUM_THREADS`, the BLAS limits and Zeit's `n_jobs`; jobs already running keep theirs) and the process that fits the chart and estimates run times (replaced in the background once idle) |
| Overview memory | Memory for the cube overview (1024 MB by default); **Apply** reopens the active layer. `--budget` overrides it for one run |
| Clear overview cache | Deletes the cached overviews except the active layer's |
| Full-resolution cache | When it is built (on demand, series on an HDD, every series), its budget, and clearing it (see [Performance](#performance)) |
