# Using Janus

[README](../README.md) · [Install](install.md) · [Usage](usage.md) · [Building](building.md) · [Architecture](architecture.md)

## What it does

- **Opens** a folder of rasters (1 file = 1 date), a multiband file (1 band = 1
  date), a list of files or a wildcard pattern. Dates are parsed from file names
  or band descriptions (`YYYY-MM-DD`, `YYYYMMDD`, `YYYY_MM`, `YYYY`, `A2001001`, ...).
  A multiband file holding **several bands a date**, date after date, opens as
  such (each date of several bands, see below) when its bands are named
  `<date>_<band>` (`2020-01-15_red`, as Zeit's `save_raster` writes a time x band
  stack), or when a `<name>_dates.csv` next to it (a date a row, from the column
  named Date, as Zeit and cdts write it) has one date for every few bands: 875
  bands and 125 dates are 7 bands a date, named Band 1 to Band 7 without names
  in the file. With a date for every band it dates a file of one band a date.
  A file whose bands have wavelengths (hyperspectral, e.g. EnMAP) opens as a
  **spectrum** (see [Spectra](#spectra-hyperspectral)). A file whose bands each have
  a name that is not a date opens as a **stack of variables**: each band a map of
  its own (its own stretch and classes), picked by name over the map instead of
  played; no temporal statistics, trends or series chart, the value of every band
  under the cursor in Statistics (Display → *A stack of variables* switches it).
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
  used automatically). Multiband tools such as CCDC read every band. The map
  can show three bands of each date as an **RGB composite** (true colour,
  colour infrared...) while the chart keeps the band or index.
- **Map modes**, all computed on the GPU: value at date, anomaly (value − mean),
  **difference** (value − a reference date: a fixed date or the previous one),
  temporal mean, standard deviation, linear trend (OLS, per year), minimum,
  maximum, amplitude, trend R², **largest drop** (date and magnitude of the
  largest decrease between consecutive valid observations) and a multitemporal
  RGB of three dates. Changing date, colormap or stretch is instant; there is an
  animation player.
- **Pixel series**: approximate (from the overview) as soon as you hover, exact
  (full resolution) once the mouse rests. Click to drop pins and compare pixels;
  Shift+drag a rectangle for an ROI, or draw a **polygon** (`R`: click its
  vertices or drag a freehand outline; or Alt + click a polygon of a vector
  layer): mean and p10–p90 per date, a box plot per date (p25–p75 box,
  median, whiskers to p10 and p90), a **fan** (p10–p90 and p25–p75 bands, the
  median dashed) or the **density** of every pixel's value at each date (two
  groups of pixels, a half-cleared plot, show as two bands where the mean runs
  between them), of the pixels whose centres are inside.
- **Chart**: from the first date to the last; lines, markers, stems, stairs or
  **bars** (from the mean, or from 0 for anomalies, in the map's anomaly colours
  below and above it); raw values, anomaly, z-score, **seasonal anomaly** (each
  value minus the mean of its calendar month over every year: the seasonal
  cycle taken out, a drought stands out) or **CUSUM** (the running sum of the
  anomalies: a change of slope is a change of level); OLS or Sen trend line per
  series; Y axis locked to the map range if you want; **stripes** under the
  chart (a colour per date of the cursor's, the ROI's or the last pin's series,
  as the map colours it).
- **Statistics** per series: n, mean, median, std, CV, min/max (with date),
  amplitude, OLS slope + R², Sen's slope, largest drop (with date) and, in the
  difference mode, the difference shown on the map (the Mann-Kendall test, with
  autocorrelation corrections, is a Zeit tool).
- **Pixel values**: when the active layer is not a series shown (hidden, a single
  image, a stack of bands), or a run's map is shown over it (a class map just
  made over its series: the view follows the layer on top; a tool's preview
  aside), the chart's panel becomes *Pixel values*: a row per
  layer the map shows (the maps of the runs, single images, the other series at
  their dates), top of the drawing order first, with its value under the cursor
  and at each pin (a class by its name, a year as a year; `*` while a series'
  value is still from its overview), *Copy* as tab-separated text. A row clicked
  (here, or in Layers) is described: its histogram beside the table in the map's
  colours, the cursor and the pins marked on it; in **Statistics**, its pixels
  with a value and their area (hectares, on a projected CRS), minimum, maximum,
  mean, standard deviation and percentiles, and, for whole numbers (classes,
  years), its pixels, percentage and hectares by value (LandTrendr's year of
  detection: the area of each year's loss). Show the series again: the chart
  comes back.
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
  similarity, change) as a Float32 GeoTIFF at full resolution; a range of
  dates as an **animation** (GIF, or MP4/WebM through ffmpeg) with the date
  stamp, legend and a scale bar (see [Animation](#animation-gif-mp4-webm)). The export window shows where the file goes (*Save to*: type a path,
  or a name alone for the last export folder, or pick one with **Browse...**;
  it warns before replacing a file). Files are written in the background
  (File → Exports shows progress);
  **Copy CSV** of the cursor, pins and ROI series; **Python...**: code that
  reproduces a pin's or the ROI's series, its statistics and the Zeit tool on
  the chart, shown in the [Python history](#python-history)'s editor.
- **Layers**: open several series at once (e.g. NDVI and NBR of the same area,
  or neighboring scenes). Layers are placed by their georeferencing, can be
  shown/hidden (including the first one), reordered and faded; the chart shows
  the series of the active layer or of every visible layer at the cursor/pins.
- **Layers in any CRS**: a layer in another projection (e.g. UTM 22S next to UTM
  23S, or geographic EPSG:4326 over UTM) or on a rotated grid is reprojected
  onto the active layer's grid on the GPU (nearest neighbour, values unchanged,
  error < 0.02 px). Cursor, pins and ROI use the exact PROJ transformation. The
  ROI applies to every visible layer.
- **Vector layers** (File → Add vector layer, or drop them): field plots,
  samples, parcels... from GeoJSON, Shapefile, GeoPackage, KML, FlatGeobuf or a
  CSV of points, in any CRS, in one drawing order with the series (as in a
  GIS: a vector layer can go under a raster one). A polygon
  becomes the ROI with Alt + click; **Extract series** writes the series of
  every feature (a point's pixel, a polygon's mean) of the active or every
  visible layer to a CSV table. See [Vector layers](#vector-layers).
- **Interpretation** (TimeSync style): go through a vector layer's features
  one by one (`N` / `Shift+N`), the map centred on each and its series on the
  chart, and label them with keys `1`–`9`, a date of change (`D`) and notes;
  saved as you go and exported as GeoPackage, GeoJSON, Shapefile or CSV.
- **Basemap** (Layers panel → Basemap): satellite imagery or a map under the
  series, for context. Sources: **Esri World Imagery**, **Sentinel-2 cloudless
  2016** by EOX (CC BY 4.0), **OpenStreetMap**, or your own **XYZ URL**
  (`https://.../{z}/{x}/{y}.png?key=...`, e.g. Google Map Tiles API, MapTiler,
  Planet: with your key and under the provider's terms). Off by default:
  nothing is downloaded until you pick a source, and each session and each
  series opened start with it hidden (the source is remembered; adding a layer
  keeps it) until you show it (its eye in the Layers panel). Only the tiles in view, at the
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
- **Swipe** (View → Swipe, `C`): a divider you drag across the map; left of it
  the map as it is, right of it another layer, or the same one at another date
  or in another display mode (picked in a bar above the map).
- **Space-time transect** (Hovmöller): `Ctrl` + drag a line on the map (or `T`,
  then drag); the **Transect** panel shows distance along the line against the
  dates, in the layer's colours (class colours for categorical data), first
  from the overview and then at full resolution. Hover a cell for its distance,
  date and value (marked on the map), click to go to that date; values or
  anomalies; Copy CSV.
- **Analysis panel** (View → Analysis), its tabs:
  - **Dates**: the visible area at **every date side by side** (or the dates of
    the shown date's month, every year), in the layer's colours (Value at date:
    its colormap and range, or its classes), each with its date and the share
    of it that is valid; *valid at least* keeps only the clean dates, *size*
    the small maps' width. A click goes to the date. Clouds, gaps and a change
    are seen at once. Made from the overview, again as the map moves.
  - **Seasonal** (series with several dates a year): the cursor, a pin or the
    ROI mean by day of the year, one line per year (colour = year, the current
    date's year on top) with the mean of all years by month (± 1 standard
    deviation), or a **year × day-of-year heatmap** in the layer's colours;
    the year can start in any month (*Year from*: July, a southern summer's
    crop from October to March seen whole; years named 2019/20).
  - **Classes** (categorical layers): the **share of each class at every date**
    (stacked areas or lines) and the **transitions** between two dates (the
    largest ones, the share of pixels that changed, the full matrix); Copy CSV
    of both. *Flows*: a **Sankey diagram** between two to six dates across
    the panel's width, or *Open in a window* for one as large as you make it
    (a bar per class as tall as its pixels, a few points at least; a ribbon
    per change shaded from the class it left to the one it joined, the pixels
    that kept their class fainter; the outer columns named with their shares;
    hover for the rest), counted a pair of consecutive dates at a time.
    *Other < 0.5%* groups the classes under that share at every date (0.5 to
    5%, or none) as one grey class, its tooltip naming them. *Changes only*
    leaves out the pixels that kept their class: only what changed, at a scale
    of its own (a class that changed nowhere disappears). The wheel zooms on
    the pointer (up to 40x), a drag moves the diagram, a double click (or
    *Fit*) shows it whole; the names follow the part of each bar in view.
  - **Scatter**: two layers, or two dates of one, pixel by pixel in the ROI (or
    the whole image) as a density plot, with the 1:1 line, the least squares
    line, r, R², the mean difference and its RMS; the **trajectory** of the
    cursor, a pin or the ROI mean between the two layers date by date
    (coloured by time, arrows the way it goes: a season's loop, a disturbance
    and its recovery).
  - **Trend test**: whether the trend of a region (the ROI, the visible area or
    the whole image), or of each class of a categorical layer, differs from 0,
    and whether the classes' trends differ, by **PARTS** (Ives et al. 2021,
    the R package remotePARTS): each pixel's AR(1) trend, the spatial
    correlation of the pixels fitted to their residuals, and a GLS of the
    trends in random partitions of the pixels, combined into tests that
    account for the correlation between partitions. The table gives each
    mean trend with its standard error, t and p, the chi-square test of the
    classes, and, for comparison, how many pixels' own tests are significant
    (which is no regional test: neighbours are correlated, and thousands of
    tests run at once). Runs on a thread (seconds to a minute), reading the
    region at full resolution (one pixel in k × k of a large one); Copy CSV.
    A guide with screenshots, the method step by step and how to read the
    results: [Trend test](https://sacridini.github.io/janus/trend-test.html).
  - **Areas**: the **area of each class at every date** of the active series, or
    of each value of a run's map (a map of change years counted **by year**:
    *Area by value...* in its menu), on the ROI or the whole layer, counted at
    **full resolution** in a task (*Count*; also *Areas...* in Classes). Each
    pixel has its true area: the geotransform's in an equal-area projection,
    else its geodesic area on the CRS's ellipsoid (longitudes and latitudes by
    latitude; UTM and other projections by their scale, the note saying how far
    the nominal pixel area is from it, e.g. +0.05%). Bars per date (the classes
    stacked) or per value in **ha, km² or %**, a table of a date (pixels, ha,
    km², %), Copy / Save CSV, and `areas.csv` written in the task's folder. The
    mapped area: Validation gives the adjusted one with its interval.
  - **Zonal**: the last *Zonal statistics over time* run (a vector layer's
    menu) drawn for one feature: the mean and median over the dates with the
    bands from the 10th to the 90th and the 25th to the 75th percentile, or the
    area of each class stacked by date (of each year, a map of change dates).
    The feature picked in the attribute table is followed.

  Classes and scatter count the overview's pixels (what the map shows before
  zooming in), so they are immediate on any series.
- **Files panel**: a folder tree listing only rasters by default; double click
  opens a series as a new layer, right click adds it as a layer or to **Favorites** (listed on
  top, kept between sessions). A folder's tooltip says what it would open as
  ("41 rasters, 1985 to 2025, yearly", from the file names). Whatever is opened
  (from the panel, File → Open, a drop or the command line), the tree goes to
  it. Whatever is opened joins the map as a new layer, as in a GIS (*File → Close
  all* starts again); dropping several files with **Shift** held makes each one a
  layer of its own (*File → Open files as separate layers...* too), without it
  they make one series. In **Layers**, a series unfolded lists its dates: one
  dragged out of it becomes a layer of its own and the series is opened again
  without it; the series' menu *Split into layers* makes every date a layer of
  its own, in a group named after the series, to show several at once. Either
  way the new layers take the series' colours (its ranges and class names too).
- **Detachable panels**: drag any panel out of the main window, e.g. the map on
  a second monitor and the charts on the first.
- **Settings** (File → Settings, `Ctrl+,`): a colour theme (dark, light,
  classic, Janus, Studio, Graphite or Matrix), translucent panels or a
  translucent window (the desktop shows through), the interface font size, and how many CPU threads Janus and Zeit may use (all cores but
  2 by default, so the computer stays responsive during long runs).
- **Zeit tools** ([Zeit](https://github.com/sacridini/zeit-cdts) change detection
  and time-series algorithms): **LandTrendr**, **Mann-Kendall** (with Sen's
  slope; Hamed-Rao, Yue-Wang and seasonal variants), the **AR(1) trend**
  (remotePARTS's REML fit: a t-test of the trend that accounts for the
  series' autocorrelation, what the Trend test tests over a region), **emerging hot spots**
  (the Getis-Ord Gi* of every date and the Mann-Kendall trend of each pixel's Gi*: ESRI's patterns, new,
  consecutive, intensifying, persistent, diminishing, sporadic, oscillating or historical hot or cold
  spot; the Gi* equals R sfdep's), **BFAST**, **BFAST Lite**,
  **BFAST Monitor**, **phenology** (season start/peak/end, length, amplitude),
  **CCDC** (multiband), **CODED** (forest degradation, multiband), the
  **NDFI** (spectral unmixing), **Tmask** (clouds the quality band missed),
  **TWDTW classification** (classes from pins), **SOM** and **k-means clustering** (typical
  trajectories, named by the samples in them), **SNIC segmentation** (segments of alike trajectories, with
  their polygons), the **agreement** of several change maps and **smoothing**
  (Whittaker, Savitzky-Golay; chart only). Each is fitted live on the cursor, pins and ROI series
  (segments, trend lines, break dates, seasons or CCDC models drawn on the chart),
  or run on the whole image, the visible area or an ROI, with the results
  (year of detection, magnitude, slope, p-value, break dates...) shown as map
  layers; the NDFI and the series screened by Tmask open as new layers. Zeit's
  methods are part of Janus — nothing to install, no Python.

## Command line

```
jn [options] [input ...]

Inputs:
  folder\             every raster in the folder, 1 file = 1 date
  series.tif          1 multiband file, 1 band = 1 date
  a.tif b.tif ...     several files, 1 file = 1 date
  "ndvi_*.tif"        wildcard pattern (* and ?)
  https://.../cog.tif cloud-optimized GeoTIFFs on the web, read in place (only
  s3://, gs://, az:// the blocks shown are downloaded); several URLs = a series
  clim.nc, data.zarr  NetCDF / Zarr / HDF: a variable's time steps (CF dates);
                      with several variables the window asks which one, or
                      name it: NETCDF:"clim.nc":t2m, ZARR:"data.zarr":/t2m
  plots.gpkg ...      vector files (GeoJSON, Shapefile, GeoPackage, KML, CSV
                      of points) open as vector layers over the series
  work.janus          a project: its layers, styles, view and the rest, as saved

Options:
  --band N            band used when each file is one date (default: 1)
  --budget MB         memory for the cube overview, this run only
                      (default: Settings, 1024)
  --threads N         processing threads, this run only (default: Settings);
                      with --selftest-zeit, Zeit's threads
  -h, --help          show this help
  --version           show the version

Developer options (all of them in jn --help):
  --describe IN       print how IN opens (dates, size, CRS, variables of a
                      NetCDF / Zarr, the series of the centre pixel) and exit
  --profile-tour IN   a fixed tour of IN (overview, cursor series, dates, modes,
                      zoom and pan at full resolution, an ROI), timed stage by stage
  --measure-cache on|off IN  time exact series and ROI from the files, build the
                      overview (and the full-resolution cache if on), time the
                      reads again from the cache
  --selftest-zeit IN  run the Zeit tools end to end on IN without a window
  --selftest-validation  check the sampling design, strata, sample and accuracy
                      estimators (Olofsson et al. 2014)
  --selftest-ui A B [C [D [E [F [G]]]]]  drive the interface frame by frame on
                      the series of tools/make_selftest_data.py
  --shots TOUR IN OUT  a website guide's screenshots (PNGs in OUT): validation,
                      landtrendr, ccdc, coded, bfast, phenology
```

Examples:

```
jn D:\data\ndvi_annual\
jn landsat_stack.tif
jn "S2_*_NDVI.tif" --band 1
jn https://sentinel-cogs.s3.us-west-2.amazonaws.com/sentinel-s2-l2a-cogs/36/Q/WD/2020/7/S2A_36QWD_20200701_0_L2A/B04.tif
jn era5_t2m.nc
jn NETCDF:"cmip6.nc":tas --band 3     (the 3rd level of a variable with levels)
```

`jn` opens the window and returns the prompt immediately (on Windows `jn.com` is
a small console launcher next to `janus.exe`, the same trick Visual Studio uses with
`devenv.com`; the one on `PATH` is in the install folder's `bin`, alone, so that
Janus's DLLs stay out of other programs' way).

## MCP server

`janus mcp` (or `jn mcp`) makes Janus a server of the **Model Context Protocol**
on its standard input and output: an assistant (Claude Desktop, Claude Code,
any MCP client) works with your series through Janus's own engines, without
the window and without Python. Every result is a file (cloud-optimized
GeoTIFFs, VRTs, CSVs, `.janusmodel`), in the folder the call names or under
Janus's results folder (`results/mcp/<tool>_<time>`); its paths are in the
answer. Long calls report their progress and stop when the client cancels them.
`--threads N` caps the processing threads (default: Settings').

| Tool | What it does |
|------|--------------|
| `describe_series` | a series' size, CRS, pixel size, extent (also in longitude and latitude), bands, dates, what is shown |
| `list_indices` | the spectral indices by name, with their formulas |
| `pixel_series` | the values of places through time (pixels, coordinates or longitude and latitude; other bands too) |
| `region_series` | each date's statistics of an area (valid share, mean, std, min, p10, median, p90, max); sampled when large |
| `check_expression` | a raster calculator text planned over named layers: the result (dates or an image), grid, layers read, or the error's line and column |
| `calculate` | the text computed: a COG per date (or VRTs computed when read) |
| `list_tools`, `describe_tool` | Zeit's tools, their parameters and outputs |
| `run_tool` | a Zeit tool on a series or an area of it (BFAST, CCDC, LandTrendr, trends, phenology...); TWDTW from samples (each class's mean series its pattern); the SOM's and k-means' clusters named by the samples' classes; the tools on maps (agreement of change maps) on `maps` |
| `regularize` | a fixed step (16 days, month, year): median, medoid or clearest date, gaps filled |
| `speckle` | a radar series through Quegan's multitemporal filter or Lee's |
| `harmonize` | Sentinel-2 taken to Landsat 8's OLI by HLS's bandpass adjustment (Landsat 7 and 4-5 by Roy et al. 2016 if asked); with `join`, a second series on the same grid joined in time |
| `warp` | a raster reprojected, resampled, clipped (gdalwarp) |
| `zonal` | each polygon's statistics, or areas of each class or change year, of maps; a CSV |
| `areas` | the area of each class or change year of maps (each pixel's own area); a CSV |
| `polygonize` | a map of classes or dates into polygons (a GeoPackage), small patches merged, simplified |
| `rasterize` | a vector layer on a raster's or a series' grid: 1 inside, each feature's number or an attribute |
| `train_classifier` | a Random Forest, TempCNN, L-TAE, Presto (a year's dates, fine-tuned) or U-TAE (patches of the area, on PASTIS's weights or none) on samples (places with a class, or a vector file and its class field), validated (by place for the forest) |
| `classify` | a `.janusmodel` applied to a series or an area: class map, probability, area of applicability |
| `stac_catalogs` | the STAC catalogues Janus knows (Earth Search, Planetary Computer, ...) and their collections |
| `stac_search` | a collection's items over an area and dates (and cloud cover): the dates found, the clouds of each, the bands to choose |
| `stac_series` | the search as a series: VRTs read in place, downloaded (a cloud-optimized GeoTIFF a date) or composited (best available pixel, greenest, medoid or median, the whole range or a year a period); clouds masked by the quality band |
| `describe_vector` | a vector file's layers: features, geometry, CRS, extent (also in longitude and latitude), fields, the first features |
| `vector_series` | each feature's series (a point's pixel, a polygon's mean) of a series; a CSV of feature, id, date, value and pixels |
| `open_in_janus` | files shown in Janus, places as pins: as layers of the window already open (it comes to the front; it watches an inbox beside Janus's data), else in a new window |

A series is a path (a folder, a file, a wildcard) or
`{"paths": [...], "band": 4, "expression": "NDVI", "qa_band": 7}`; a place is
`{"col", "row"}`, `{"x", "y"}` (the series' CRS) or `{"lon", "lat"}`; an area a
`window` in pixels, a `bbox` in the CRS or a `bbox_lonlat`.

Claude Code:

```
claude mcp add janus -- "C:\Program Files\Janus\janus.exe" mcp --threads 8
```

Claude Desktop (`claude_desktop_config.json`):

```json
{ "mcpServers": { "janus": { "command": "C:\\Program Files\\Janus\\janus.exe", "args": ["mcp", "--threads", "8"] } } }
```

## Using the window

| Action | How |
|---|---|
| Remote rasters (cloud) | **File → Open URL...** (`Ctrl+U`, a new layer): one URL per line, several make a series. `https://`, `s3://`, `gs://`, `az://`: cloud-optimized GeoTIFFs (AWS Earth Search, Planetary Computer with a signed URL, USGS, ESA...) are read in place through GDAL's network file systems: only the header, the overview level the map needs and the blocks looked at (the cursor's pixel, the detail tiles) are downloaded. Public S3 and GCS buckets need no account; private ones use the usual credentials (`AWS_ACCESS_KEY_ID`, `~/.aws`, `GOOGLE_APPLICATION_CREDENTIALS`, `AZURE_STORAGE_CONNECTION_STRING`...). Dates come from the file names, or from the folder each file sits in (Sentinel-2 COGs: `.../S2B_23KPQ_20230811_0_L2A/B04.tif`). The overview is cached on disk like any other |
| Build a series | **File → Build series...**: loose files (picked, or a folder's) grouped by the **date in their names** (as when opening); a date's files of one band **stacked as its bands** (their names from what tells the files apart: `S2_20210105_T1_B04` and `..._B08` give B04 and B08), its **tiles** (files of other extents) **mosaicked**; files of several bands each keep their bands. Every date on one grid (the files' extent at their finest pixel; one CRS: files of another are left out, reproject them first). The plan is shown before anything is written: the dates, each band's files and tiles, the files left out and why. Then a folder of **VRTs** (`gdalbuildvrt`'s own code: at once, nothing copied) or of **COGs**, `<name>_<YYYYMMDD>`, opened as a series (a task; its provenance kept) |
| Save a series | **File → Save series as...**: the active series, the quantity shown (a band, an index or an expression, its quality mask applied) or, for NetCDF and Zarr, every band (a variable each), over the whole layer, the visible area or the ROI, as a **multiband COG** (a band per date, its date in the band's name: it opens back as a series), a **NetCDF** (CF: time in days since 1970, x / y or lon / lat, the CRS as a grid mapping) or a **Zarr** with a time axis; opened back when saved (a task). Reprojected or clipped by polygons: Tools → Data preparation → Reproject, resample, clip first |
| STAC catalogs | **File → Search STAC catalog...**: Earth Search (Sentinel-2 Collection 1 L2A), Planetary Computer (Sentinel-2 L2A, Landsat Collection 2 L2, HLS S30 and L30, Sentinel-1 RTC), Brazil Data Cube (16-day cubes of Sentinel-2, Landsat, CBERS-4 WFI) or another STAC API by its URL (*List all* gives its collections). The visible area, the ROI or a box in degrees, two dates and the maximum cloud cover; *Search* lists the dates found with their scenes and clouds (*Clearest* ticks those under 10%), then the bands (with their wavelengths; SCL and QA_PIXEL as the quality band) and the cell size. **Open as a layer** writes one VRT per date (the mosaic of the day's scenes, the clearest on top, on one grid: the CRS of most scenes, the finest band's cells; scenes of another UTM zone warped onto it) under `results/stac` and opens them, read in place; **Download as GeoTIFFs** writes the same series as cloud-optimized GeoTIFFs (Tools → Tasks). Planetary Computer's files are read with tokens Janus asks for and renews, also when the folder is opened again later. **Sentinel-1 RTC** (radar, VV and VH as linear gamma0 corrected for the terrain, 10 m; no clouds to filter, no quality band): the search lists the satellite's **passes** (ascending or descending, relative orbit) and keeps the one with the most scenes (*Pass*; mixing passes makes false steps), and the series opens in **dB** (the VVdB index, `10 * log10(VV)`; VHdB and VHVVdB, the ratio, and the radar indices of the library in the Display panel); Tools → Data preparation → Speckle filter smooths it. **Composite**: one image of the dates ticked (or *one a year*) instead of the series, written as cloud-optimized GeoTIFFs and opened as a layer (a task): the **best available pixel** (each pixel's clear observation of the highest score, every band from that date: its day of the year near a *target day* (`exp(-0.5 (days / ±)²)`, the middle of the dates unless typed as MM-DD), its distance to the nearest cloud or shadow of its date (0 at the first distance or nearer, 1 at the second or farther: 30 to 1500 m by default), its sensor (Landsat 7 after its scan line corrector failed in 2003: 0.5) and its scene's cloud cover, weighted 0.3, 0.5, 0.1 and 0.1; Griffiths et al. 2013, White et al. 2014; zeit's `build_annual_composites(method="bap")`), the **greenest** (the highest NDVI, Earth Engine's `qualityMosaic`), the **medoid** or the **median**. The best available pixel reads the quality band of every date first, in blocks of 1024 pixels with the margin of the cloud distance around them, chooses the winners, then reads the bands of the dates that win in each block only, each over the box of its winners there (and a pixel's next best date where a winner has a band without a value): most of a series is never downloaded (the job's log says how many pixels were read of those every date's bands would need); the greenest reads the red, NIR and quality bands of every date; the medoid and the median every band. *The date, score and clear observations of each pixel* adds a GeoTIFF per composite in its `info` folder (year, day of the year, score, clear dates). `--stac-composite bap|greenest|medoid|median[;yearly]` with `--stac`. HLS's Fmask is a quality rule of its own (*HLS Fmask bits*: cloud, adjacent, shadow and snow masked). *Footprints as a vector layer*: the scenes' outlines with their id, date and cloud cover. See the [STAC guide](https://sacridini.github.io/janus/stac.html) |
| NetCDF, Zarr, HDF | open the file (or the `.zarr` folder): with several variables a list asks which one (name, shape, long name and units); the time axis is decoded from CF units (`days since 1970-01-01`, hours, months...) and calendars (`noleap`, `360_day`, `all_leap`...: a model's 30 February becomes the 28th/29th); hourly data are labelled with the hour. **Display** panel → *Variable*: another variable of the file, *level*: another value of the other dimensions (pressure level, depth, member); both reopen the layer in place. A lon/lat grid without a grid mapping is taken as WGS 84 |
| Pan / zoom | drag / mouse wheel (zooming out goes as far as the CRS's area of use and every layer, the basemap under all of it; zooming in until the finest layer in view, active or not, shows its pixels as large as the active one's may be, each drawn from its full-resolution tiles, not only its overview), `F` fits the active layer; trackpad: two fingers pan, pinch zooms |
| Pixel series | hover (exact once the mouse rests) |
| Command palette | `Ctrl+K` (or `Ctrl+Shift+P`; View → Command palette): type what you want (`bfast`, `evi`, `swipe`, `export png`, a layer's name): the menus' commands, every tool, the display modes, the spectral indices the active layer has the bands for (shown on it), the open layers (make active, zoom to), the panels; each with its category and keys, the ones used last first; arrows and `Enter`, or a click; one that cannot run now is faint, the reason on hover. `Esc` or a click outside closes it |
| Go to a coordinate | `Ctrl+G` (View → Go to coordinate...): latitude and longitude in decimal degrees (`-23.55, -46.63`; a decimal comma works when no point is used: `-23,55 -46,63`), or in degrees, minutes and seconds with or without the hemisphere (`23°33'S 46°38'W`, `23 33 S 46 38 W`, `O` for west too); UTM as zone + `N`/`S` (the hemisphere) or a latitude band (`23S 333000 7394000`, `23 K 333000 7394000`); `EPSG:code x y`; two numbers past the degrees' range are `x y` in the layer's CRS; `px column row` a pixel. The window shows where it lands (the pixel, or that it is off the layer); `Enter` or *Go* centres the map there (zoomed in to a screen point a pixel at least) with a mark that fades, and *Drop a pin there* adds a pin. `--goto PLACE` on the command line |
| Saved views | View → **Saved views**: a name and *Save view* keep the view (its centre in longitude and latitude and its width in metres, so any layer in any CRS goes back to the same area; a layer without a CRS: its pixels), the date shown and the layers shown; a click on one goes back to it (the date by its label, the layers shown by their names when any is open), `x` deletes it. Kept in the settings; also in the palette (*Saved view: name*) |
| Minimap | `M` (View → Minimap): the active layer whole in the map's bottom right corner, the view as a rectangle on it; a click or a drag on it moves the map there. Drawn again only when what it shows changes. `--minimap` |
| Lens | `L` (View → Lens, or *Lens* on the swipe's bar): the swipe's comparison inside a circle that follows the cursor instead of right of a divider; `Ctrl` + wheel steps the date the lens shows (up: later; it takes its own date, as *Own date* on the bar) instead of zooming, the wheel alone zooms the map, `Alt` + wheel changes its size. `L` again turns it off (a swipe that was on before it, `C`, comes back as the divider). `--lens` |
| Status bar | at the foot of the window: under the cursor, the pixel (column, row), its coordinates in the layer's CRS and in longitude and latitude (WGS 84), and what the map shows there; at the right, the zoom, the map's scale on the screen (1:N at 96 points per inch) and the ground a screen pixel covers, the active layer's CRS, and the tasks under way (Zeit and deep-learning runs, downloads, exports) with the progress of the oldest: a click opens Tasks |
| Downloads | every download runs in the background, the interface usable meanwhile: STAC series and composites, embeddings, the networks' weights (their megabytes said as they come). Each is a task in **Tools → Tasks** with its bar, its log and *Cancel*, and its bar on the map (top right); at most *Downloads at once* transfer at the same time (Settings, 2 by default), the others **wait in line** in the order they were asked for (*waiting* in Tasks, which says how many are before it; *Move first* makes one the next to start; the status bar counts those waiting). Two tasks needing the same weights or the AlphaEarth index download them once |
| Notices | a task that ends after a few seconds or more (or fails) leaves a notice at the bottom right: *Show in Tasks*, *Open folder*, *Log*; it goes after a few seconds (a failure stays longer) or with its **×**. With Janus in the background, the system is asked for your attention (the taskbar's icon flashes; on a Mac, the Dock's bounces) |
| Compare pixels | click to drop a pin |
| Close layers | the **Layers** panel: click a row to select it (a series also becomes active), `Ctrl` + click adds or takes a row, `Shift` + click selects the rows between; `Delete` or `Ctrl+D` (Mac: `Command+D`) closes the selected layers, series and vector layers alike (a row's menu: *Close the N selected*). The keys belong to the list until you click elsewhere |
| Remove a pin | right click it (Mac: Control + click, or a two-finger click on the trackpad), `Delete` (last one) or the **x** in the statistics table |
| ROI | `Shift` + drag: rectangular ROI, mean and p10–p90 per date (or a box plot per date: the *ROI* list above the chart); with *All visible layers* on the chart, also on every visible layer (its own pixels under the rectangle, reprojected if needed) |
| Polygonal ROI | `R` (or View → Draw ROI polygon), then click the vertices and close it on the first one, with a double click or `Enter` (`Backspace` undoes a vertex, `Esc` cancels); or, after `R`, drag a freehand outline (lasso), closed when you let go. The statistics, the local PCA of embeddings, the similarity to the ROI and the scatter use the pixels whose centres are inside; a Zeit run on the ROI covers its bounding box. **Alt + click** a polygon of a vector layer (holes included) to make it the ROI; View → Clear ROI |
| Edit ROI vertices | `Shift+R` (or View → Edit ROI vertices, the command palette) with an ROI, drawn or taken from a polygon of a vector layer (its file is not changed): a square at each vertex, a dot in the middle of each edge. Drag a vertex to move it; drag an edge's middle to add a vertex there; right click a vertex, or `Delete` / `Backspace` over it, to remove it (a ring keeps three). Each change is the ROI at once (its statistics read again) and a step of **Undo** (`Ctrl+Z`); `Shift+R`, `Esc` or `Enter` end the editing. A rectangle edited becomes a polygon of four vertices. Not a GIS editor: one ROI, its vertices only |
| Save ROI as vector | View → **Save ROI as vector...** (or the command palette): the ROI (a rectangle or a polygon, holes kept) as a GeoPackage (`.gpkg`) or GeoJSON (`.geojson`), by the name's extension, in the active layer's CRS (a GeoJSON in WGS 84 when the CRS has no EPSG code, as RFC 7946 asks), a feature with `name`, `layer` and `pixels`; *Open it as a vector layer* adds it to the map. **Alt + click** it later makes the same ROI again, the same pixels inside. A project (File → Save project) keeps the ROI too |
| Vector layers | File → Add vector layer..., the **Layers** panel (*Add vector layer...*), a drop or the command line: GeoJSON, Shapefile, GeoPackage (each of its layers), KML, FlatGeobuf, GML, GPX or a CSV of points; listed in the **Layers** panel with the series (a new one on top): show/hide, colour, labels (a column's values on the map), move, remove; right click → Zoom to it, Attribute table, Extract series, Interpret. Hovering a point shows its attributes (lines and polygons: hold `Alt`); see [Vector layers](#vector-layers) |
| Series at points or polygons | a vector layer → **Extract series...**: the active layer or every visible one, a row per feature (a column per date) or a row per feature and date; CSV |
| Interpretation (labelling) | a vector layer → **Interpret...** (the **Interpretation** panel): `N` / `Shift+N` (or Page Down / Page Up) next / previous feature, `1`–`9` its label, `0` clears it, `D` the date of the change (the date on the map), `Shift+D` none; notes; *Export table...* |
| Animation | File → Export animation (`Ctrl+Shift+A`): dates (from, to, step), frame rate, resolution, GIF / MP4 / WebM, date stamp, legend, scale bar, pins, ROI and vector layers; see [Animation](#animation-gif-mp4-webm) |
| Time | `←`/`→` previous/next date, `P` play/pause, click or drag on the chart; the time bar above the map (play, the date slider, the speed) shows with a series of two dates or more, not with a single image or vector layers alone. Under the slider, each date's **availability**: a bar as tall as the share of the visible area that is valid on that date (no cloud, no gap; of the overview), red under half; the pointer over it gives the share, a click goes to the date |
| Map mode, colormap, range | **Display** panel, for the active layer (range is automatic 2–98%, or typed, or dragged on the histogram) |
| Histogram | **Display** panel, under the range: the values shown (0.5–99.5% of them) filled in the colours the map gives them, dimmed outside the range, the colormap strip under it; drag a triangle (or its line) to move an end of the range, between them to move both, double click for the automatic range again; the pointer over it shows a bar's values and share. **Click a bar** to highlight its pixels on the map, the others dimmed to grey (click it again: none); **Shift + click** adds bars, **Shift + drag** a range of values; its share of the pixels under the histogram, *Clear* to end it. With *Largest drop: date*, a bar is a date: what dropped then. `--mode NAME,highlight=LO:HI` from the command line |
| Change between dates | **Display** panel → *Difference (value - reference)*: pick the *Reference* (a date, or *Previous date (t-1)*); diverging colours centered at 0, follows the time bar |
| Temporal composite | **Display** panel → *Temporal composite*: median, mean, maximum, minimum or number of observations of each pixel over a window: the shown date's year (the arrows and playback then go year by year), some days around the date (a moving window) or the whole series; with the season, only its dates. Computed in the map shader over the overview's dates each time the map is drawn (the median of up to 64 values: every k-th one beyond) |
| When it greens | **Display** panel → *Seasonal peak (when it greens)* (dated series, 6 dates or more round the year): each pixel's annual harmonic (`v = m + a cos θ + b sin θ`, θ the date's time of year) fitted by least squares to its dates with a value (in the season), as a colour: the **hue** the time of year it peaks (a wheel of the months on the map and in the panel, the cursor's peak marked), the **saturation** how much it rises and falls (the stretch: the peak-to-peak amplitude, 0 to its 98th percentile), the **brightness** its mean (*Brightness by the mean*, from a quarter; off: all bright). A September soybean and an April second crop in different hues, an evergreen forest or water grey; dates all in a few months give no fit (not drawn). The status bar gives the cursor's peak date, amplitude and mean; the PNG export a legend of the months. Computed in the map shader over the overview's dates |
| Season | **Display** panel → Season → *Only part of the year*: a window of days of the year (presets by months: DJF, JJA, Apr–Sep...; a start after the end wraps over the new year and belongs to the year it ends in). The dates outside it are left out of the temporal statistics (mean, trend, std...), the composites, playback and the arrow keys; the chart shades the season. Applies to every layer with dates |
| Largest drop | **Display** panel → *Largest drop: date* or *magnitude*: the largest decrease between consecutive valid observations (no-data dates skipped), dated at the lower one; the colour bar shows dates. Exact values for the cursor and pins in the **Statistics** panel |
| Classes (categorical data) | **Display** panel → *Categorical (classes)*: legend with colours, names and shares (click a colour to change it, untick a class to hide it); detection can be switched off or forced. **Classes over time...** (under the legend, and on the chart's toolbar) opens View → Analysis → Classes: *Shares* (each class's share at every date, its legend beside the chart: a click on a swatch hides or shows the class; one date, or a stack's band: a bar per class), *Transitions* (the matrix of two dates as a heatmap, the largest changes beside it) and *Flows* (Sankey). On the chart, a categorical layer's series has styles of its own: *Steps + markers* (the markers in the classes' colours), *Steps*, *Markers* or *Class blocks* (a block per date in its class's colour); every class is named on the Y axis. Values, trend and *Y = map range* do not apply to class codes and are disabled |
| Band, index, cloud mask | **Display** panel → Bands (one file per date with several bands): band A, optional normalized difference with B, quality band; **Apply** reopens the layer in place |
| Spectral index, expression | **Display** panel → *Index or expression*: an index of the [Awesome Spectral Indices](https://github.com/awesome-spectral-indices/awesome-spectral-indices) (275, searched by name or domain; only those the series has the bands for: roles `N`, `R`, `RE1`, `S1`... placed by wavelength or band name, constants and wavelengths in place), or an expression typed over the bands (`2.5 * (N - R) / (N + 6*R - 7.5*B + 1)`, `nd(nir, swir1)`, `B08 > 0.3 = 1`); **Apply** reopens the layer showing it: map, charts, ROI, Zeit's tools, exports and Copy Python follow it. `--expr EVI` (or a text) opens the inputs so. *Open in calculator* takes it to the raster calculator |
| Raster calculator | **Tools → Raster calculator...**: an expression over the open layers, each by a short name in the *Layers* list (`s2.nir`, `dem`, `"any layer".band4`); bare band names are the active layer's. A series with an image uses the image at every date; two series are matched by date (the same day, the nearest within N days, the same month or year: a date without a match is no data); a layer on another grid is resampled, in another CRS reprojected (*Options*: whose dates, whose grid, the resampling, the quality masks). Operators `+ - * / % ^ < <= > >= == != and or not`, `name = expression` lines, rules `condition = value` (the result starts from the condition's first operand, or `result = ...`; `x == nan` tests no data), functions (`where`, `coalesce`, `clamp`, `nd`, `bit`, `qa`, math...), the date's `year`, `month`, `doy`... One argument of `max`, `min`, `mean`, `sum`, `median`, `std` (the population's, numpy's), `count`, `percentile` reduces a series over time to an image (no data skipped; `ndvi - mean(ndvi)` is each date's anomaly); `at(ndvi, 2003)` is one of its dates (a year, `200307` a month, `20030715` a day; the *Layers* list gives each date's), `at(ndvi, 2020) - at(ndvi, 1990)` two of them. Over time, in the series' real dates: `argmax(ndvi)`/`argmin` the date (decimal year) of the largest/smallest value, `first(ndvi < 0.3 and year >= 2020)`/`last(...)` of the first/last date a condition held, `longest(ndvi < 0.3)` the longest it held in days, `persist(ndvi < 0.3, 3)` 1 where the last 3 observations held; slices of the calendar `median(ndvi[2019-06 .. 2019-09])`, `mean(ndvi[11 .. 2])` (those months of every year); periods `max(ndvi) by year`, `mean(ndvi) by month` (a series of the years or months); windows `mean(ndvi, 90d)` (the dates of the 90 days up to each, `w` weeks, `y` years); lags `ndvi[-1]`, `lag(ndvi, 2)` (the pixel's previous observations with a value), `lag(ndvi, 1y)` (the nearest date a year before); `anomaly(ndvi, base=2015..2019, by=month)` (each date's z-score against its month in the base years; `by=doy, window=15d`). **Live** shows the result on the map as it is typed (the overview's size, a layer marked preview; a reduction drawn as its dates are read, "max so far: 23 of 107 dates"; a series at the date shown); **Chart** gives its values at the cursor or the first pin under the editor (the result's series, or an image's value with the values its reductions reduce; a part of the text selected drawn too). A reduction read by several dates (`ndvi - mean(ndvi)`, an anomaly's climatology, `(s - min(s)) / (max(s) - min(s))`) is computed once, on every thread, into an image beside the result, by a virtual layer too (each date computed it again); a result's overview reads only the rows it shows, as a series' own does, also through a lag's or a window's VRTs (41 dates of 7318 × 7329, 8 threads: `s - mean(s)` 7 s + 5 s, `s * 2` 2 s, `mean(s, 3y)` 6 s, `s - s[-1]` 23 s). Janus reads the result's VRTs itself: their files straight, each file's block by one thread, on the processing threads (eight at most), the values GDAL's VRT gives bit for bit (GDAL reads what it does not cover: a file on the network, another kind of VRT); on the guide's 107 dates of Sentinel-2, 3 threads, `count(nd(N, R) > 0.5)` is on the map in 0.8 s (2.6 s through GDAL's VRT), a pin's 107 values in 0.1 s (1.1 s), its median written as a COG in 1.1 s (3.7 s). A band of one file with overviews, zoomed out, shows the expression (GDAL's VRT showed the file's overview: `x * 2` as `x`). While a result's dates are computed, a bar on the map and one in the calculator say so (`Computing "calc": 12 of 41 dates`). The editor colours the text, completes names (`Tab`/`Enter`, `Ctrl+Space`), explains the name under the pointer and underlines an error as it is typed; `Ctrl+Enter` or **Run** adds the result as a layer: VRTs computed as the map reads them, or cloud-optimized GeoTIFFs (a task). History and favourites kept; *Examples*; the index library inserts formulas. `--calc TEXT [--calc-run]` |
| Regularize series | **Tools → Data preparation → Regularize series...**: the active series at a fixed step (8 or 16 days, monthly, yearly, any number of days; periods from the first date's day, month or year, as pandas' resample, the empty ones too): each period's **median** (each band's), **medoid** (the observation nearest the bands' medians, so its bands come from one date; zeit.regularize_time_series) or **clearest date** (the value of the period's date with the most clear pixels that has one); the band, index or expression shown with its quality mask, or every band (masked). Empty periods left empty, or filled **linearly** in time or from a **Whittaker** curve (λ; the values kept, the ends not extended) when the values around are at most N days apart. On the whole layer or the ROI, in blocks of rows on the processing threads; a COG per regular date (`<name>_<YYYYMMDD>.tif`) in a folder that opens as a new series when done (a task in Tools → Tasks; its provenance kept) |
| Speckle filter | **Tools → Data preparation → Speckle filter...**: a radar series (Sentinel-1's VV and VH) through **Quegan's multitemporal filter** (each date's window mean times the mean, over the dates with a value, of each date's value over its own window mean; Quegan & Yu 2001: the speckle averaged through time, each date's changes kept) or **Lee's** (each date alone: the window's mean plus the pixel's departure from it, weighted by how much more the window varies than the speckle of L looks does; Lee 1980; Sentinel-1 IW GRDH 4.4 looks); a window of 3 to 15 pixels, cut by the series' edges, counting the pixels with a value only. Every band of every date on the linear values (not dB), the whole layer or the ROI, in blocks of rows on the processing threads; a COG per date in a folder that opens as a new series shown as the one filtered (its dB) |
| Harmonize sensors | **Tools → Data preparation → Harmonize sensors...** (zeit's `bandpass_adjust`): a series that mixes Landsat and Sentinel-2 has a step wherever the sensor changes, which CCDC or BFAST read as a break. The Sentinel-2 dates go through the **bandpass adjustment of HLS** (Claverie et al. 2018; HLS v2.0 User Guide, table 5): a line per band and unit (2A, 2B, 2C; unknown taken as 2A), `OLI = slope × MSI + intercept`, for the coastal, blue, green, red, narrow NIR (B8A) and SWIR bands; Landsat 8 and 9 stay as they are (OLI is the reference, as in HLS). Landsat 7 and 4-5 stay as they are too unless asked: then Roy et al. 2016's lines (reduced major axis or least squares), fitted on pre-collection data. Each date's sensor comes from its scene (the STAC item a search's VRT records, else the file's name: `S2B_MSIL2A_...`, `LC08_L2SP_...`) or is chosen for all; HLS products are refused (already adjusted). Sentinel-2's broad NIR (B08) has no line and is left as it is (search with B8A to adjust it). The values are reflectance or reflectance × 10000, as found (the intercepts scaled). **Join with** another open series on the same grid (a Sentinel-2 search and a Landsat one, the second reprojected onto the first's grid): both adjusted, their dates in one series, the bands they share named by role (Blue, Green, Red, NIR (narrow), SWIR 1, SWIR 2), in the units of the first, each masked by its own quality band. Only HLS's bandpass step, not its atmospheric correction or BRDF normalization. The whole layer or the ROI; a COG per date in a folder that opens as a new series; `--window harmonize[-run]` |
| Polygonize | **Polygonize...** in the menu of a run's map or of a series (its date shown) in the Layers panel: the map's patches (pixels of one value, 4 or 8 neighbours) as polygons in a GeoPackage that opens as a vector layer, each with its `value`, `class` (the class names), `date` (a map of years), `area_ha` and `perimeter_m` (geodesic, on the CRS's ellipsoid), `pixels` and the mean of another map of the same grid (`mean_<map>`, a loss's magnitude). A **minimum area** first merges the smaller patches into the neighbour they share the longest border with (the minimum mapping unit of Post-process), a patch with none is left out; the outlines can be **simplified** (a tolerance in metres, their topology kept). A task in Tools → Tasks; its provenance kept |
| Rasterize | **Rasterize on the series' grid...** in a vector layer's menu: its features burned on the active series' grid (its CRS, origin, pixel size and size; the features reprojected to it) through GDAL's own rasterizer (the same pixels as `gdal_rasterize`): **1 inside, 0 outside** (a mask), **the feature's number** (1, 2... in reading order) or **an attribute** (a number as it is; a text as **classes**, numbered by name, their names and colours on the layer); the pixels whose **centre** is inside or **every pixel touched**; a later feature over an earlier one. A task whose GeoTIFF opens as a layer, ready as a Zeit tool's mask (*Only where ... is not 0*, under its scope); its provenance kept |
| Zonal statistics over time | **Zonal statistics over time...** in a vector layer's menu: each feature is a zone (its pixels those whose centre is inside, or every pixel touched; a pixel in several features counts in each), through every date of the active series or a run's map, on every feature or within the ROI: for **values** each date's count, mean, standard deviation, least, percentiles 10, 25, 50, 75, 90 and largest; for **classes** each class's pixels and area (each pixel's true area, as in Areas) at every date and the **transitions** between two dates; for a **map of change dates** the area by year. The zones named by an attribute or their FID. A task writing `zonal_long.csv` (a row per feature and date, and class), `zonal_wide.csv` (a row per feature, a column per date: the statistic chosen, or each class's ha), `transitions.csv`, and a GeoPackage of the features with those columns added, opened as a layer; Analysis → Zonal draws a feature's series; its provenance kept |
| Reproject, resample, clip | **Tools → Data preparation → Reproject, resample, clip...**: the active series' files (or a run's map) through gdalwarp's own code (the same result as the `gdalwarp` command, whose arguments the window and the task's log show): to **a CRS** (EPSG:4326, ESRI:102033, a PROJ string, a WKT), **the CRS of a layer** or **the grid of a layer** (its CRS, origin, pixel size and extent: the maps aligned pixel to pixel), a pixel size (optionally aligned, `-tap`), resampling near, bilinear, cubic, average, **mode** (the default for classes), min, max or med; the whole layer, the visible area, the ROI or **the polygons of a vector layer** (the pixels outside them without a value, cropped to them); no data, data type; **COG**, GeoTIFF or **virtual** (warped VRTs: nothing written, ready at once). File by file on the processing threads, into a folder that opens as a new series (a task; its provenance kept) |
| RGB of three bands | **Display** panel → RGB composite (one file per date with several bands): *Three bands of each date as RGB*, a preset (true colour, colour infrared, SWIR, agriculture, land/water: bands found by their names or wavelengths) or any three bands, the map's date or a fixed one, the stretch of each channel (2–98% of the first image, kept for every date; *Stretch to this image*). The chart, the cursor and the tools keep the series' band or index; zoomed in, the visible area is read again at full resolution; map panels, the swipe, PNGs and animations show it too |
| Python of a series | Time series panel → **Python...** → a pin or the ROI: code (numpy, rasterio) that reads the same series from the files (band, normalized difference, quality mask; the ROI's pixels inside it), prints its statistics and, with a Zeit tool on the chart, makes the same Zeit calls with the same parameters (as the tool made them: recorded, not rewritten); shown in the Python history's editor, to read, change, copy or save |
| Python history | **Tools → Python history...** (or the palette): every finished run as Python, kept between sessions (`history.jsonl` in Janus' settings folder, the last 5000). See [Python history](#python-history) |
| Reset layout | **View → Reset layout** (`Ctrl+Shift+R`, Mac: `Command+Shift+R`): every panel docked where it was at the first start, the floating windows (Exports, Tasks, Log, Settings) back to their first size and place, map panels to the right of the main map; the panels open stay open |
| Performance panel | **View → Performance** (hidden by default, and not kept open from one session to the next): timings, threads in use, Zeit status, overview memory, full-resolution cache (progress, size, read times, **Build it now**), disk cache (what the caches take, their budget, the disk's free space) |
| Settings | **File → Settings...** (`Ctrl+,`, Mac: `Command+,`): the interface (Modern or Classic) and its theme, font size, whether the Files panel follows what is opened, processing threads, overview memory, the disk cache budget (automatic or set by hand), full-resolution cache (when it is built) and clearing the caches; kept between sessions (see [Settings](#settings)) |
| Several series | **Layers** panel (*+ Add*) or File → Open (`Ctrl+O`): each opens as a new layer; several images that are not dates of one series: *File → Open files as separate layers...* or a drop with `Shift`. The panel is a table of contents: the active layer's opacity on top, then one list, top of the list drawn on top, the basemap its last row (under every layer); *Add embeddings* folds under it: series, the maps of the runs and vector layers in one order, as in a GIS (a new series or map goes over the other rasters but under the vector layers, a new vector layer on top; a vector layer can then be moved under a raster one, or a raster over it); a row a layer (the eye shows or hides it, a picture of it in its colormap or geometry, its name over its dates or kind, the date shown at its right, a **⋯** menu while the pointer is over it); click a series to make it active (`Space` shows or hides it), double click to zoom to it, drag any row onto another to reorder. **Right click** (or **...**): make active, zoom to it, opacity, move (to the top, up, down, to the bottom), **Export** (map as PNG, values or rendered view as GeoTIFF, animation), close; a vector layer's: colour, attribute table, interpret, extract series, zoom to it, **Export as** GeoPackage, GeoJSON, Shapefile or CSV, remove. The arrow unfolds a series: its dates (a stack's bands, unfolded from the start) a row each, the one shown with its eye open (a click or its eye shows another; its menu exports that date alone or opens it as a layer of its own). The maps a run makes (LandTrendr's year of detection, magnitude...) are layers of their own, not rows of the series: each its eye, its place in the order, its group; hiding or closing the series leaves them on the map. A map's arrow unfolds its colormap and range, or its classes' legend; a click selects it (the **Display** panel then shows its colours, and Pixel values its histogram); its menu: opacity, *Show only this map*, move, group, **Save as GeoTIFF**, save every output of its run, remove (Delete removes the ones selected; the files stay on disk). With five or more, a filter by name keeps the list short |
| Blend, styles, groups | **Layers** panel: the active layer's **blend mode** beside its opacity (Normal, Multiply darkens by the layers under it, Screen lightens, Add sums, Darken and Lighten keep the darker or lighter of each channel), also under *Blend* in a series' menu (`--blend MODE` on the command line). A row's menu → **Style**: *Copy style* (its mode, each mode's colormap and the ranges set by hand, the stretch per date, the RGB bands, opacity, blend, its classes' names, colours and visibility; a vector layer's colour), *Paste style* on the layers selected (or the row), the **saved styles** (kept in the settings, given with a click, `x` deletes one) and *Save style* under a name. **Groups**: select rows (`Ctrl`/`Shift` + click) → *Group the N selected*: they go next to each other in the drawing order under a row of their own, whose eye shows or hides them all, whose arrow folds them, whose click selects them (`Delete` closes them) and whose menu renames it, moves it to the top or the bottom, ungroups it or closes its layers; a row dropped on a member (or on the group's row) joins it, *Out of its group* takes one out. The palette has *Blend: ...*, *Copy style of the active layer*, *Paste style on the selected layers* and *Group the selected layers* |
| Basemap | **Layers** panel → Basemap (the list's last row): its eye shows or hides it; click the row (or its arrow) to pick a source (None = nothing downloaded); opacity in its **⋯** menu; `S` shows or hides satellite imagery (Esri World Imagery, or the imagery source picked); *Custom XYZ URL*: the URL (`{z}`, `{x}`, `{y}`; `{-y}` for TMS rows; applied when you leave the field), the attribution to show, the finest zoom and the tile size. The source and its settings are kept between sessions, but it starts hidden, as does every series opened (its eye shows it); the map panels' and the swipe's layer list has *Basemap only* |
| Web layers | **File → Add web layer...** (also *+ Add* in the Layers panel and the palette): a server's URL or a preset (TerraBrasilis' DETER and PRODES, IBGE, an OGC API demo); the service found from the URL (OGC API - Features, WFS, WMS tried in turn) or chosen (WMS, WMTS, WFS, OGC API - Features); *Connect* lists its layers (title, name, the abstract on hover; a filter by name, title or abstract). **A map** (WMS, WMTS): *Add map layer* puts it on top of the drawing order, drawn as the basemap (Web Mercator tiles around the view, a GetMap per tile through GDAL's WMS driver; WMTS by a Google-compatible tile matrix set), with its style, format, *Transparent*, opacity and, for a layer with a time dimension, the server's default, a time typed, *the series' date* (the last time listed not after the date shown) or *up to the series' date* (from the first time to it); its credit on the map and in the exports; its row's menu: opacity, time, reload its tiles, move, remove. **Features** (WFS, OGC API): the visible area, the ROI, the active layer or the whole layer (the box sent to the server), *At most* (50000 by default), a date field with two dates and an attribute filter (OGR SQL; both sent to the server when it takes them); *Fetch features* is a task of Tools → Tasks (progress, Cancel): paged into a GeoPackage in the cache's `web` folder (the same request within 12 h read from it; files older than 7 days removed), opened as a vector layer named after the server's layer (*(first N)* and a notice when the cap was reached); every vector tool works on it, *Export as...* keeps it. Errors (no connection, the server's error, a bad URL) are said in the window; nothing waits on the interface. Projects keep the maps and the fetched layers' requests (fetched again when the GeoPackage is gone). `--web-layer URL,LAYER[,SERVICE][,KEY=VALUE...]` adds one once the series is open (keys `time=DATE|series|upto`, `field=`, `from=`, `to=`, `max=`, `opacity=`, `area=0..3`, `dialog=1`, `filter=`). Compared with geopandas, OWSLib and requests: `tools/bench_ogc_vs_python.py`, `bench/ogc-vs-python.json` |
| Official alerts | **Validation & uncertainty → Official alerts...** (the Validation panel's *Alerts* tab): a run's map of change dates (LandTrendr, CCDC, BFAST, CODED...) against official alerts, on the map's grid, over the area both cover and a window of dates: **DETER** (INPE; TerraBrasilis' WFS, Amazon or Cerrado, fetched for the map's area in a task and opened as a vector layer), any **vector layer** of dated polygons (a date field, a class field), or **alert rasters**: UMD's **GLAD-L** and **GLAD-S2** (read from UMD's public GeoTIFFs for the map's area, no key, or from files), Wageningen's **RADD** and GFW's **integrated alerts** (their GeoTIFFs need a GFW API key: open them from disk, or *Of the map's area* reads GFW's public map tiles, zoom 14, and decodes their colours). **Compare** (a task of Tools → Tasks) gives the area of change in both, only in the map, only in the alerts; the alerts the map detects (by area, by count, by their own area) and the map's change the alerts confirm (by area, by patches), Dice; the lag (the map's date minus the alert's: median, quantiles, the share within 30, 90 and 365 days, a histogram); by class (DETER's classname) or confidence; the **agreement map** (both, only the map, only the alerts) and, for rasters, the alerts' dates join the layers; *Export CSV...* writes the summary, the classes, every alert, the lags and the agreement map. See [Official alerts](#official-alerts-against-a-map-of-change). `--alerts SOURCE[,KEY=VALUE...]` compares once a map of change is there |
| Browse files | **Files** panel: double click opens, right click → Add as layer or Add to favorites; Ctrl+click selects several files; hover a folder for what it would open as. Opening a series expands the tree down to it (Settings → *Files panel follows what is opened*) |
| Add by dropping | drop files or a folder on the window: a new layer (several files make one series of their dates; with `Shift` held, each file a layer of its own) |
| Seasonal views, classes over time, scatter | **View → Analysis** (see above) |
| Chart of several layers | Time series panel → *All visible layers* (one marker shape per layer) |
| Second monitor | drag a panel's tab out of the main window |
| Maps side by side | View → New map view (`Ctrl+T`): pick the layer and, if wanted, its own date and mode in the panel's bar; every panel follows the same pan/zoom; close it with its tab's **x** or `Ctrl+W` (the focused panel, else the last one opened) |
| Swipe | View → Swipe (`C`): drag the divider (white line with a handle); the bar above the map picks what is right of it: a layer and, if wanted, its own date and mode, or *Basemap only*; **Swipe off** or `C` again closes it |
| Space-time transect | `Ctrl` + drag a line on the map (Mac: `Command` + drag), or `T` / View → Draw transect, then drag; a **polyline** (a river, a road, a valley): `T`, then click its vertices, a double click or `Enter` ends it (`Backspace` undoes a vertex); **Alt + click** a line of a vector layer: the transect along it; `Esc` cancels. **Transect** panel: distance from A along the line (X; dashed verticals at the bends; no data where a polyline leaves the layer) × dates (Y, oldest on top); hover a cell = distance, date, value, marked on the map; click = go to that date; Values / Anomaly (− each place's mean); Copy CSV (a row per date, a column per sample); Clear, or close the panel |
| Profiles of every line | **Profiles of every line...** in a line layer's menu (Layers panel): every line (each part of a multi-line) sampled about a pixel apart along its length, as the transect samples it, through every date of the active series or those from one to another; the values the chart shows. A task (Tools → Tasks: progress, cancel; the processing threads) writing `profiles.csv`, a row per sample and date (`fid`, the fields chosen, `part`, `sample`, `distance_m` (metres; `distance_map_units` in a CRS without an EPSG code), `x`, `y`, `col`, `row`, `date`, `value`; samples off the series left out), and optionally `profiles_wide.csv` (a row per sample, a column per date); its provenance kept |
| Chart options | style (lines ... bars; a categorical layer: steps, markers, class blocks), values/anomaly/z-score/seasonal anomaly/CUSUM, trend (OLS/Sen), Y = map range, stripes, the ROI's spread (p10–p90, box plot, fan, density) |
| Map as a figure | File → Export map as PNG... (`Ctrl+E`): resolution (1×, 2×, 4×), background, date label, legend, scale bar, pins, ROI and vector layers; *Save to*: the file (a path, a name alone for the last export folder, or **Browse...**), then **Export** (or `Enter`) |
| Data for QGIS | File → Export values as GeoTIFF... (`Ctrl+Shift+E`; the active layer at the date: visible area or whole image) or Export rendered view as GeoTIFF... (`Ctrl+Shift+V`; RGBA as shown, georeferenced) |
| Projects | **File → New project / Open project... / Save project** (`Ctrl+S`) **/ Save project as... / Recent projects**: a `.janus` file (JSON) keeps the series (their files, band, index or expression, quality mask, date, style, opacity, blend, visibility), the vector layers (colour, visibility), the runs' maps (their GeoTIFFs, colours, range), the drawing order, the groups, the active layer, the pins, the ROI (box or polygon), the transect, the view, the swipe, the map panels, the basemap, the web layers (the maps, and the fetched layers' requests), the saved views and the season. Paths are kept relative to the project's file (and absolute, used when the relative one is not found), so a folder with the project and its data can be moved or shared; a file not found leaves its layer out with a notice, the rest opens. The window's title gives the project's name, `*` when it has changes not saved; New and Open ask what to do with them (Save, Discard, Cancel); closing Janus does not ask, the session (changes included) is kept. Opening a `.janus` file (File → Open, a drop on the window or `jn work.janus`) opens the project. The session is kept every minute and at exit (`last-session.janus` beside Janus' other data) and, with *Settings → Reopen the last session at start* turned on (off by default), reopened at the next start |
| Undo | `Ctrl+Z` undoes the last change to the layers, `Ctrl+Shift+Z` or `Ctrl+Y` redoes it; **Edit → Undo / Redo** says what (*Undo Opacity of NDVI*, *Undo Close Landsat*). Undone: closing layers (a series, a vector layer, a run's map: opened again in their place, with their style and date), opening them, styles, opacity and blend, colours, the drawing order, groups, visibility, names, pins, the ROI and the transect. A slider or a layer dragged is one step, whatever the frames in between; panning and zooming, the date shown, the panels and the active layer are not steps. The last 100 steps are kept; opening a project or New project starts again. A text field keeps its own `Ctrl+Z` |
| Save a tool result | right click it in the **Layers** panel → Save as GeoTIFF..., or File → Export Zeit result as GeoTIFF. A copy of the result: Float32, full resolution, the series' grid and CRS, no data = NaN, named `<layer>_<tool>_<output>.tif` |
| Save every output of a run | right click a result → Save all outputs of this run... (`Ctrl+Shift+S` for the run of the result on top): all of them (e.g. LandTrendr's year of detection, magnitude, duration, rate, DSNR, values before and after) into a folder at once; the popup lists the files and warns before replacing one |
| Save embeddings | File → Export embeddings as GeoTIFF... (`Ctrl+Shift+M`; when a layer is drawn as embeddings): the view shown, visible area or whole layer, at full resolution; see [Embeddings](#embeddings) |
| Export progress | File → Exports (`Ctrl+J`; opens by itself with each export): progress, cancel, open the folder |
| Shortcuts on a Mac | `Command` instead of `Ctrl` (e.g. `Command+E`, `Command+Shift+S`) |
| Series as text | **Copy CSV** in the Time series and Transect panels |
| Zeit tools | **Tools** menu, grouped by the question asked in the order of a workflow (*Data preparation*: Tmask, Smoothing, Regularize, Speckle filter, Harmonize sensors, Reproject; *Spectral analysis*: NDFI, spectral unmixing; *Temporal dynamics*: phenology, Mann-Kendall, AR(1) trend; *Change detection*; *Spatial pattern analysis*: emerging hot spots, SNIC, texture; *Classification & pattern recognition*: TWDTW, SOM, k-means, post-processing, the Classification window, field delineation; then the raster calculator and Tasks) → tool window (parameters, chart fitting, raster runs); progress in **Tools → Tasks** |
| Zeit logs | **Tools → Log** (or **Log** next to a task): the log of each raster run, followed live (what ran, its progress, outputs, the end or the error); Copy, open the file or its folder |
| Tool results | layers of their own in the **Layers** panel (over the rasters): show/hide, order, groups, colormap, range, opacity, a class map's class names (edited in its legend); their values at the cursor and pins, histogram and statistics in **Pixel values** (in the chart's place while a map is shown over its series) |
| Embeddings | **View → Embeddings**: principal components as RGB (of every year, or *local* to the ROI), similarity to the cursor, a pin or the ROI, change between years; see [Embeddings](#embeddings) |
| Add embeddings of the visible area | **Layers** panel → Embeddings (above the basemap): AlphaEarth or TESSERA, years, cell size, visible area or ROI, then **Add embeddings**; progress on the map, the layer comes on top when it is ready |

Clicking a legend entry hides/shows that series together with its trend line. Hovering the chart shows the
date under the pointer: a dashed line, each series' value marked on it, and a card with the values (the ROI's
p10 and p90 under its mean; the class's name for a categorical layer). The main series (the cursor's, else the
ROI's mean, else the last pin's) has an area fading under it; trend lines are dashed; with Lines + markers the
markers are left out when the dates are closer than 7 points.

## Zeit tools

The **Tools** menu lists the algorithms of [Zeit](https://github.com/sacridini/zeit-cdts)
that Janus exposes (step-by-step guides with screenshots on the website:
[LandTrendr](https://sacridini.github.io/janus/landtrendr.html),
[CCDC](https://sacridini.github.io/janus/ccdc.html),
[CODED and the NDFI](https://sacridini.github.io/janus/coded.html),
[BFAST](https://sacridini.github.io/janus/bfast.html),
[Phenology](https://sacridini.github.io/janus/phenology.html),
[the AR(1) trend and the Trend test](https://sacridini.github.io/janus/trend-test.html)):

| Tool | Series | Maps |
|---|---|---|
| LandTrendr | annual (one date per year) | year of detection, magnitude, duration, pre/post value, rate, DSNR of the chosen loss/gain event |
| Mann-Kendall | any (seasonal variant: several dates per year) | significant Sen's slope, slope, p-value, Z, tau, trend class, intercept |
| Emerging hot spots | any, ≥ 3 dates (raster runs) | the pattern (no pattern, 8 kinds of hot spot, 8 of cold spot, named and coloured), the Gi* of every date as a **new layer** (hidden: show it, or hover it for a pixel's Gi* series), the trend of the Gi* (z), the dates a hot / cold spot |
| AR(1) trend | any (harmonics: several dates per year) | significant slope, slope (per year or per time step), p-value, t, its standard error, AR(1) rho, intercept, residual SD |
| BFAST | regular, ≥ 2 dates per year | trend/season break counts, date and magnitude of the largest trend break |
| BFAST Lite | regular, ≥ 2 dates per year | break count, date and magnitude of the largest break, first break date |
| BFAST Monitor | regular, ≥ 2 dates per year | first break date in the monitoring period, magnitude, break yes/no |
| Phenology | ≥ 6 dates per year | start, peak and end of season (day of year), length, peak value, amplitude, fit R², seasons — for a typical year (median), the latest season or a chosen year |
| CCDC | one file per date with blue, green, red, NIR, SWIR1, SWIR2 [+ thermal]; a quality band is recommended | break count, largest break date and magnitude, NDVI change, first/last break, segments |
| CCDC synthetic image | as CCDC | the image CCDC's models predict for a date (*Date*, a decimal year; 0 = the date on the map): every band in the series' units and the quantity shown (e.g. NDVI), without clouds or gaps |
| CODED (degradation) | one file per date with blue, green, red, NIR, SWIR1, SWIR2, ≥ 2 dates per year; a quality band is recommended | strata (forest, non-forest, degradation, deforestation, disturbance), first and last change dates, NDFI change, changes, mean NDFI of the training period |
| NDFI (spectral unmixing) | one file per date with blue, green, red, NIR, SWIR1, SWIR2 | the NDFI of every date as a **new layer** (and, if asked, the GV, NPV, soil and shade fractions), mean and lowest NDFI, observations used |
| Tmask (cloud screening) | one file per date with green and SWIR1, ≥ 2 dates per year | observations flagged and their share; the series shown without them as a **new layer** |
| TWDTW classification | dates | class map (one class per pattern, with a legend), distance, margin to the 2nd class |
| Post-processing (classes) | a categorical layer (one map or a map per date) | the classes cleaned as a **new layer** with the layer's names and colours: each pixel's short excursions removed (A B A becomes A A A), then each date's map through a majority filter and a minimum mapping unit that merges the small patches into the neighbouring class; how many dates of each pixel changed |
| SOM clustering | any | cluster map (the typical series of the cluster under the cursor is drawn on the chart; with samples, each cluster named by them), distance to it |
| k-means clustering | any | cluster map, ordered by the centres' mean value (the centre of the cluster under the cursor is drawn on the chart; with samples, each cluster named by them), distance to it |
| SNIC segmentation | any | segments (a colour each), each segment's mean series as a **new layer**, the segments' **polygons** (GeoPackage, opened as a vector layer) with their size, centre and mean of every value compared |
| Texture (GLCM) | any (raster runs only) | a map per feature of a temporal summary of the shown series in a moving window: the GLCM's contrast, dissimilarity, homogeneity, ASM, energy, correlation, entropy, mean and variance, and the window's mean, standard deviation and Sobel gradient |
| Agreement of change maps | maps of dates from other tools (any series) | consensus year, how many maps agree (and their share), maps with a change, spread of the years |
| Smoothing | any | chart only: the smoothed series (Whittaker or Savitzky-Golay) |

**TWDTW classes** come from the series itself: drop pins on places you know
(forest, crop, pasture...) and give them a class in **Tools > Classification & pattern recognition**
(the window opens on TWDTW): each class's mean series of its samples is its
pattern, and the method is estimated on the samples by place. The distance is that of the R package
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

**k-means clustering** (Janus' own, as scikit-learn's `KMeans`: k-means++
starts, Lloyd's iterations, the best of *Starts* runs) fits its centres on
the same sample of the area as the SOM, its gaps filled the same way, and
gives every pixel the nearest centre; the clusters are ordered by their
centres' mean value, low to high. Against scikit-learn 1.9 on the same
series (8 kinds of NDVI-like trajectory, 60 dates, 8 clusters, 10 starts):
the same partition (adjusted Rand index 1.0 on 20 000 pixels, 0.99996 on
200 000) and inertia (within 4e-8), 1.2 times as fast on one thread and 1.5
times on four (`tools/bench_kmeans_vs_sklearn.py`,
`bench/kmeans-vs-sklearn.json`).

Run from the Classification window with samples (its *Samples* tab), the SOM
and k-means **name their clusters by the samples in them**: each cluster
takes the class of most of its samples, each place (a polygon, a pin)
counting once however many pixels it has, as in *Forest (cluster 3, 21%)*;
*Forest or Pasture (cluster 2, 9%)* when no class has two thirds of them;
*Cluster 4, 6%, no samples* when none falls in it. The samples are read as
the run reads the area, so a sample is in the cluster its pixel has on the
map.

**SNIC segmentation** (Achanta & Süsstrunk 2017, as `zeit.snic` and R's
`sits_segment`) divides the area into segments of neighbouring pixels whose
whole series are alike: every date of the shown band or index (or, with
*Values compared: Every band*, every band of every date) is one value compared.
Segments grow from seeds every *Seed spacing* pixels on one of R snic's grids;
*Compactness* trades regular shapes for following the data, and is in the
values' units (0.5 for reflectance or an index in 0-1, thousands for
reflectance x 10000). The area is segmented in tiles (*Tile size*) on every
core, and segments never cross a tile's edge. Gaps are filled in time as for the
SOM. The run writes the labels (`snic_labels.tif`, Int32, -1 outside every
segment), each segment's mean series (`snic_mean.tif`, opened as a layer: the
series as objects) and the polygons (`snic_polygons.gpkg`, layer `segments`:
`supercells`, `x`, `y`, `n_pixels` and a column per value compared, named
`date` or `date_band`), opened as a vector layer for interpretation by object.

**Texture (GLCM)** (as `zeit.texture`) maps the texture of the shown series:
first a summary of its dates (*Summary of the dates*: median, mean, minimum,
maximum, standard deviation or amplitude, the 95th minus the 5th percentile),
then, in a *Window* of 7 pixels about each pixel, the grey-level
co-occurrence matrix's properties (Haralick et al. 1973) as scikit-image's
`graycomatrix(symmetric=True, normed=True)` and `graycoprops` give them,
averaged over four directions at the *Pair distance*, the values quantized to
*Grey levels* over the summary's 2nd to 98th percentile; and three of spfeas's
window statistics (the mean, the standard deviation and the mean Sobel
gradient). Fields, pastures, orchards, plantations and forests differ in
texture where their spectra are alike: the maps are predictors for the
Random Forest (*Beside the series*) and show the grain of a landscape. The
windows need their neighbours, so it runs on the raster only. Zeit's texture
equals scikit-image's to 1e-12, 78 times faster on one thread.

The **agreement** runs on maps Janus already shows, not on the series: check two
or more results whose values are dates (LandTrendr's year of detection; CCDC,
BFAST or CODED dates), from any layer. A date counts by its calendar year, and
maps agree when their years are at most *Tolerance* apart; the consensus year is
the one most maps agree with. The outputs are on the grid of the first map
checked (the others are put on it), as layers of their own.

A break's date (BFAST, BFAST Lite, BFAST Monitor) is that of the first
observation after it, and its magnitude the model after the break minus the
model before it on that date, as in Zeit's `extract_events`.

On the chart, the BFAST family draws its **fitted model** with the breaks:
BFAST, the piecewise linear trend (a segment between trend breaks) plus the
harmonic season (its own between season breaks), and the trend alone, lighter;
BFAST Lite, trend + season refitted between its breaks; BFAST Monitor, the
history's model and, lighter, where it goes over the monitoring period (what
the observations are compared with). Zeit returns the breaks, not the models:
Janus fits them again by least squares, on Zeit's regular time axis.

The **CCDC synthetic image** follows the map: with *Date* 0 its chart marker
(the predicted value, on the CCDC segments) moves with the date shown, and a
raster run predicts the map's date (named in the task's title).

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
- **On the chart**: on as soon as the tool is opened (closing its window takes
  it off): fits the model to the cursor, pin and ROI series and draws it over
  them (LandTrendr segments and vertices, Sen's line, vertical lines at break
  dates); the model's numbers are added to the Statistics table. Several tools
  can be fitted at once (e.g. LandTrendr, Mann-Kendall and the AR(1) trend, or
  phenology and smoothing): each in its own colour and line (solid, dashed,
  dotted), named in the chart's legend (a click there hides that tool's
  models; hiding a series hides its models), the same line shown next to the
  checkbox and over the tool's rows in the Statistics table; changing one
  tool's parameters fits only that tool again. On a series that does not suit
  the tool (e.g. LandTrendr on a monthly series) the checkbox is off and
  disabled, with the reason; it fits again on a series that does. With *All
  visible layers* on the chart, single-band tools are also fitted to the other
  layers' cursor and pin series. **Python...** gives the calls of every tool
  fitted on the series.
- **Estimated run time** for the chosen scope, measured by Zeit on samples of
  the series (a fixed cost per block plus a cost per pixel, and the fit of
  tools that learn from a sample, such as the SOM; reading the data is not
  included).
- **Run on the raster**: whole image, visible area or ROI; **Only** everywhere
  or *where X is not 0*, X a layer on the series' grid (a vector layer
  rasterized, a class map): its pixels 0 or without a value are left out and
  have no value in the outputs. The run happens in
  the background on the processing threads of [Settings](#settings)
  (every logical core but 2 by default), with progress and cancel in
  **Tools → Tasks**. Outputs are GeoTIFFs (default folder
  `%LOCALAPPDATA%\Janus\results`) loaded over the map as layers of their own
  in the Layers panel (the first output shown, the others hidden); cells
  without an event are transparent. Series
  outputs (the NDFI, a screened series) are one file with a band per date,
  added as a layer over the others (the active layer stays).
- **Preview** (under Run): the tool on the ROI, or without one on at most
  256 x 256 px at the centre of the view, in a folder of the work folder; its
  maps open as layers marked *preview* (the badge in the Layers panel), its
  window dashed on the map (*Preview...* while it runs), and the next preview
  replaces them (their folder deleted). **Auto-preview** previews again a moment
  after a parameter changes, when a preview is estimated to take 3 s or less;
  the window warns when the parameters changed since the preview shown. Closing
  the tool's window removes its preview; **Run** then runs on the whole scope
  as before.
- **A log of every raster run**, `job.log` in the run's results folder (next to
  `job.json` and the outputs): the tool, its parameters, window and threads,
  the progress (every 10% or 5 s), each output, and how it ended (with the
  error, if any). **Tools → Log** shows it while it grows (pick the run in the
  list), as does the **Log** button of each task.
- **Provenance of every result**, `provenance.json` in the run's folder (Zeit
  tools, deep learning models applied, the raster calculator): the tool, the
  versions of Janus and Zeit (zeit-core's revision), when it ran and for how
  long, how it ended, the series it read (its files, the band, index or
  expression shown, the quality mask), the scope and the window in pixels, the
  parameters (those not at the tool's default named), what else it took (the
  model's file and post-processing, the calculator's text and layers) and the
  files it wrote. A result's menu in the Layers panel (a run's map, or a series
  a run or the calculator wrote) gives **Provenance...** (all of it in a window,
  the changed parameters in the accent colour), **Run again with these
  parameters...** (the tool's window with them and the same scope; on the same
  series, the scope *The run's window* is the very window it ran on, even if
  the ROI has moved since), **Python of the run...** (the run's code in the
  Python history's editor: a Zeit tool's `rerun.py` reads the series over the
  run's window from its files, makes the same Zeit calls with the same
  parameters and writes every map they return as a GeoTIFF, numpy, rasterio,
  xarray and zeit-cdts; Reproject, resample, clip, Rasterize, Polygonize,
  Zonal statistics, Regularize, Build series and the calculator's simple
  expressions have a `rerun.py` too, see [Python history](#python-history))
  and **Open folder**.

### Python history

**Tools → Python history...** lists every processing run that finished, newest
first, as Python: a Zeit tool's run, the calculator's results, Reproject,
resample, clip, Rasterize, Polygonize, Zonal statistics, Regularize and the
other tools that keep a provenance. Opening, viewing and closing layers do not
count, nor runs cancelled or failed. The history is kept between sessions
(`history.jsonl` in Janus' settings folder, the last 5000 runs), and an entry
stays when its run's folder is deleted.

Each entry's code starts with what ran (Janus' version, the tool, when, the
series and scope, the run's folder and files). Then it has one of two kinds of code:

- **A script** (`rerun.py`, also written beside the run): a Zeit tool's run
  reads the series over the run's window and makes the same Zeit calls with
  the same parameters. Reproject, resample, clip calls `gdal.Warp` with the
  same gdalwarp options, file by file. Rasterize burns the same features with
  the same values on the same grid through `gdal.RasterizeLayer`. Polygonize
  traces the same polygons with `rasterio.features.shapes` (the same
  simplification, the minimum area through Zeit's sieve) and measures their
  geodesic areas and perimeters with pyproj, into a GeoPackage of the same
  columns. Zonal statistics burns each feature alone and writes the same long
  and wide tables (and the transitions), pixel areas as Janus measures them.
  Regularize reads the series as Janus does and makes the same composites
  (median, medoid; clearest date approximately) and fills (linear, Whittaker)
  in numpy. Build series calls `gdal.BuildVRT` (and `gdal.Translate` for COGs)
  with the same grid. The calculator's expressions without reductions over
  time or stages, on layers of the same grid, read the inputs as Janus' VRTs
  read them and compute the expression in numpy, date by date. Each script
  writes its files in a `python` folder of the run, and the UI self-test checks
  that they equal Janus' own (rasters value for value, GeoPackages feature for
  feature, tables cell for cell; the calculator within 1e-5, as Janus computes
  in float32).
- **The run as data** (*as data* in the list), for the tools that have no
  script yet: the parameters, inputs, window and outputs as Python values,
  with the equivalent command when there is one.

The editor beside the list colours the code and can be edited: **Copy**,
**Save .py...**, **Save notebook...** (a Jupyter notebook), **Revert** (back to
the run's code), **Open folder**, **Provenance** and **Remove** (the entry
only). **All as .py...** and **All as notebook...** write the entries shown
(the filter on top searches tools, folders and code) as one script with a
`# %%` cell per run, or one notebook; **Clear history...** empties it. The
chart's and the validation's **Python...** buttons show their code in the
same editor, without adding it to the history.

How it works: Zeit's methods are compiled into Janus: zeit-core (the Rust core
of [Zeit](https://github.com/sacridini/zeit-cdts)) with what Zeit's Python adds
around it (bands by role, the NDFI's unmixing, the BFAST family's refits...),
giving the same numbers as Zeit's Python API (the tests compare them tool by
tool). A fit of the chart's series takes under a millisecond, so models follow
the cursor; a raster run reads the series in bands of rows (from the
full-resolution cache when it has the dates) and computes them on every
processing thread. Nothing starts at startup and no Python runs.

## Classification (Random Forest, TWDTW, embeddings, SOM, k-means, networks)

**Tools > Classification & pattern recognition > Classification** trains a model on samples you
already have in Janus and classifies every pixel by its whole series, without
Python: a **Random Forest** (Breiman 2001; the first in the list: quick, on the
CPU, good with few samples), **TempCNN** (Pelletier et al. 2019: convolutions
over time) or **L-TAE** (Garnot & Landrieu 2020: attention over the dates), the
networks as sits builds them (`sits_tempcnn`, `sits_lighttae`) and with sits'
defaults, trained and applied on the GPU (the same wgpu as the map; the CPU
without one).

- **Samples** from the *interpretation's labels*, a *validation project's*
  references (the adjudicated label, else the interpreters' when they agree),
  a *vector layer's* class column (points, or polygons: their centre, each
  pixel inside up to a number, or the mean of their pixels; SNIC's segments
  are such polygons), or *pins* given a class in the window (the source the
  window opens on). The classes keep the label set's order.
- **Pins by class** (source *Pins*): a list of classes (type a name, *Add*;
  its colour, its count of pins; *x* removes it), the class **new pins** get
  (a click on the map drops a pin of it: drop a class's pins one after the
  other), the pins without a class given one at once; *Coloured by class*
  gives each pin its class's colour, the one the class map will show. A new
  class takes the first colour no other class has, from a palette of
  colours each as unlike those before it as can be (Tableau 10's, then
  Glasbey's farthest in CIELAB); a click on a class's colour picks another.
  The project and undo keep each pin's class and each class's colour.
- **One window for every classifier**, the method chosen in it (tab
  *Method*), on the same samples and their quality check:
  - *learned from the samples*: the Random Forest and the networks below;
  - *matched to each class's series*: **TWDTW** (Zeit's): each class's
    **mean series of its samples** is its pattern (no pin to pick per class:
    every sample counts), with its settings (time weight, time measured, max
    distance); *Classify the cursor, pins and ROI on the chart* gives each
    series its class as it is fitted; *Apply* runs it on the raster (whole
    image, visible area or ROI, its preview);
  - *on a layer of embeddings*: the Embeddings panel's **logistic regression
    or nearest centroid**, fitted on the samples' vectors in the year shown;
    *Apply* makes the samples the layer's classes and shows its Classes view;
  - *without samples*: the **Self-Organizing Map** (Zeit's) and **k-means**,
    run from *Apply*; with samples, each cluster is named by the class of most
    of the samples in it (*Names of the clusters* in the Method tab, the last
    run's clusters below it).
  Tools > Classification & pattern recognition > TWDTW, SOM and k-means open this window on them. TWDTW and
  the embeddings' classifier are **estimated on the samples** as the forest
  is, cross-validated by place (5 folds; at most 3000 samples classified):
  the estimated accuracy, kappa, the confusion matrix (with the samples left
  unclassified) and each class's accuracy, the forest's check beside it, so
  the methods are compared on the same samples (s2ro, 54 pins of three
  classes: TWDTW 91 %, the forest 96 %).
- The window has three tabs: **Samples** (where they come from and their
  quality), **Method** (the method, the values, its settings, its training or
  estimate) and **Apply**. It
  opens as wide as half the screen and lays the Samples tab in two columns
  when there is room, in one when narrow; every list scrolls in a box of its
  own, so the window never grows past the screen.
- **The samples' quality, as they are gathered**: whenever the samples change
  (a pin dropped or moved to a class, a polygon labelled), after a short pause
  and in the background, Janus reads the new places' series (each pixel once)
  and grows quick forests cross-validated **by place** (each sample classified
  by forests that never saw its place, 5 folds, 60 trees each). It shows:
  - a summary: the samples, the classes, the **estimated accuracy** (with a
    small curve of it as the samples grew), the warnings, and the verdict on
    the **last pin dropped** ("looks like Pasture, 92 %", or "looks more like
    Soy: check it");
  - the **classes**: samples and places, their balance, each class's accuracy
    and the class it is most often taken for (a click shows its samples);
  - **what to look at**, the worst first: samples that look like another class
    (mislabelled, or a mixed place: their pins ringed in red on the map),
    samples of different classes on the same pixel, classes of few samples or
    few places, unbalanced classes, classes taken for one another, samples
    left out for too few dates with a value (a click shows those samples),
    and **bunched samples**: with 10 places or more, the forests are
    cross-validated again on kNNDM folds (as far from the training as the map
    is from the samples); an accuracy 5 points below the place-by-place one or
    more ("somewhat bunched"; 10 points: "bunched") says that the samples
    cover the area unevenly and the map far from them will do worse: samples
    from the places left out help. The summary shows it as the accuracy
    *far from the samples*;
  - each class's **typical series**: its mean and p10-p90 band on each date
    (of the value chosen), where the classes part and where they overlap;
  - the **samples' table**: each pin or place with its class (a pin's changed
    there), how well it **fits its class** (its class's probability, or the
    class it looks like), its share of dates with a value, *>* to go to it and
    *x* to remove a pin; filtered (all, those that look like another class, on
    a shared pixel, without a class, a class) and sorted (in order, least
    fitting first, by class). Only the rows seen are drawn: thousands of
    samples scroll smoothly.
  The estimate guides the gathering; the training's validation is the
  measure. `--window classification` opens it on the pins; `--pin X,Y,CLASS`
  drops a pin of a class.
- **Values read on each date**: the series shown (the band or normalized
  difference, the quality band applied) and, with several bands per date, any
  of the source bands (screened by the quality band too). Gaps are filled
  linearly in time; pixels with values on fewer than *Min dates with a value*
  are left out. Each value is scaled by its 2-98 % quantiles of the samples
  (sits' normalization).
- **Training** (a task in Tools > Tasks, cancellable): a validation share split
  off each class (seeded), AdamW, the learning rate decayed by steps, the
  classes optionally weighed, early stopping on the validation loss
  (*Patience*); the model of the best validation loss is kept. With few samples
  the batch is made smaller, so that an epoch has at least 8 batches (with
  fewer, the batch normalizations' statistics are too rough and the model
  classifies everything alike). The window draws the loss and accuracy curves
  and, at the end, the validation samples' confusion matrix with the user's and
  producer's accuracies.
- **Random Forest**: *Trees* (500), the *values tried at each split* (auto:
  the square root of their number, a date's values each a value), the
  *samples in a leaf at least* (1) and the *depth at most* (no limit), as
  R's randomForest and ranger and scikit-learn's defaults: each tree grown on a
  bootstrap sample, each split the best by the Gini impurity among the values
  drawn, a pixel's probabilities the mean of its leaves' class shares over the
  trees. The validation share is held out by **whole places** (all of a
  polygon's pixels on one side), so the accuracy is that of places the forest
  never saw; the **out-of-bag** accuracy (each sample by the trees that did
  not see it) is drawn beside it as the trees grow. At the end, the
  **importance of each date** (the mean decrease of the impurity, summing to
  1; by value too, with several): the dates that tell the classes apart. With
  *Importance: by permutation too*, also the out-of-bag accuracy lost when a
  value is shuffled (each tree's out-of-bag samples classified again, ranger's
  `importance = "permutation"`, unscaled), which does not favour values of many
  levels; the trees take about twice as long. The
  trees grow on the CPU's threads, each from its own seed: the same forest
  whatever the threads. Against scikit-learn and R's randomForest on the same
  samples (`tools/bench_forest.py`, 500 trees each),
  the same test accuracy within a point (the mean of 3 seeds: 0.904, 0.907
  and 0.903 on sits' MODIS NDVI samples; 0.865, 0.875 and 0.875 on its
  Landsat-8 of Rondonia, where one of the 32 test samples is 3 points; 0.827,
  0.826 and 0.825 on 20,000 made-up samples of 64 values); grown 3.5 times
  faster than scikit-learn and 2.5 times faster than randomForest on one thread
  (19 s, 66 s and 47 s on the 20,000 samples), 5.4 s on four threads
  (scikit-learn 16.7 s); the test samples classified as fast as they are on
  one thread, 1.4 times faster on four. Against ranger 0.18 too: the same
  accuracy within a point, grown 2.8 times faster on one thread (56 s) and 2.3
  on four (14 s); with the importance by permutation, 39 s on one thread
  against ranger's 93 s and randomForest's 126 s, the same ten most important
  values as randomForest's and nine of ranger's (Spearman 0.90 and 0.85 over
  the 64, the uninformative ones near 0 in any order; 0.95 to 0.99 on sits'
  samples).
- **Validation** (the settings of the forest and the networks): how the places
  are held out, so that the accuracy is not that of places next to those the
  model learned from (blockCV, Valavi et al. 2019; CAST's kNNDM, Linnenbrink et
  al. 2024; as Zeit's `spatial_folds`). Each place (a polygon, a pin) is a
  point, its pixels' middle, in metres.
  - *kNNDM* (the default, *recommended*): see below. If its folds cannot be
    made (too few places, no coordinates), the training says so and holds out
    whole places at random instead.
  - *Whole places at random*: the validation share of each class's places.
  - *Spatial blocks*: a grid of hexagons or squares, the blocks dealt into
    *Folds* at random (the most even of 100 draws, each fold near a k-th of
    each class's places), in rows, or as a checkerboard (squares, 2 folds).
    Their size is given, or found from the values read: the median range of
    the spatial autocorrelation of up to 12 dates' values, semivariograms of
    2000 pixels drawn at random, each the spherical, exponential or Gaussian
    model that fits best (blockCV's `cv_spatial_autocor`, zeit-core's
    variogram).
  - *Buffer around the places tested*: the validation share of the places,
    and the places within the distance of them out of the training too
    (blockCV's `cv_buffer`).
  - *Clusters (k-means)*: the places' mean values, standardised, in k
    clusters (blockCV's `cv_cluster`): a validation of conditions the model
    has not seen; or, *measured on the map*, their positions (a fold's places
    together).
  - *kNNDM*: the places grouped by Ward's hierarchical clustering (k-means
    beyond 4000 places, or chosen) into q clusters, merged into k folds of
    about equal size, for q from k to the number of places; kept, the folds
    whose distances from each place to the nearest place of another fold are
    distributed most like those from the map (a grid of about 1000 cells) to
    the nearest place. The window draws the three distributions, as CAST's
    plot of `knndm`. *Measured between the values* (CAST's and Zeit's
    `space="feature"`): the distances between the places' series (each date's
    bands, standardised by the places'), and from the series of about 1000
    pixels of the area to the most alike place: a validation of values the
    model has not seen, wherever they are.

  With folds, the **forest is cross-validated**: a forest for each fold,
  grown on the others, predicts its samples (the confusion matrix is of every
  sample), and the forest kept is grown on all of them. A **network** is
  **cross-validated** too (*Cross-validate*, on by default; k+1 trainings): a
  network for each fold, trained on the others and stopping early on places
  of its own training, predicts its fold, and the confusion matrix is of
  every sample; the network kept validates, and stops early, on one fold (the
  one whose classes the others keep best), its accuracy there shown beside.
  Off, the network is only trained on that split. The area of applicability takes the folds as CAST does (each
  sample's dissimilarity to the samples of the other folds), and its error
  profile the cross-validation's predictions. *Show the folds on the map*
  makes them now and opens them as a vector layer coloured by fold (the
  blocks and the places; written as a GeoPackage in the results folder's
  `folds`); a training writes its folds beside the model
  (`<model>.folds.gpkg`). Spatial folds choose a model and calibrate its
  area of applicability; a map's accuracy is still estimated from a
  probability sample (Validation & uncertainty; Wadoux et al. 2021). Against
  Zeit's `spatial_folds` on the same points (`crates/janus-ml/tests/folds_zeit.rs`):
  the same blocks in rows and as a checkerboard, the same buffers, Ward's
  clusters as scipy's, k-means as tight as scikit-learn's, and kNNDM's folds
  as near the map's distances (W 130.4 against 129.7, from different random
  draws) in 0.03 s against 0.07 s (k-means: 1.0 s against 3.2 s), for 1000
  places.
- **What this means** (the Model tab, after a training, or a model opened):
  the validation read in plain words, the most pressing first: how good the
  accuracy is (good from 85 %, fair from 70 %) and how it was measured (as
  far from the training as the map, on places at random, or on pixels of the
  places learned from); with fewer than 100 validation samples, the range
  the accuracy could be in (95 %); each class found less than 70 % of the
  time and the class it is taken for, with what to do (samples of both, from
  more places, where they differ; or join them); a network that learned its
  samples by heart (15 points more on them than on the others); a forest
  whose out-of-bag accuracy is far above its spatial one (neighbouring
  samples alike); kNNDM suggested when the validation was not spatial; and
  that the map's own accuracy comes from a probability sample.
- **Beside the series** (the forest's settings): rasters on the series' grid
  read as further predictors, one value each a sample after its series'
  values: the maps of runs on the series (**Texture (GLCM)**'s above all, see
  below) and single rasters open of its size (a DEM, a distance map). A
  raster may cover part of the series (a run's ROI) but must have its pixel
  size and be aligned with it; pixels without a value in one are left out. A
  polygon's mean takes their mean too. The model keeps their files' paths and
  reads them again when it is applied; their importance is listed after the
  dates'.
- **The model** is kept in a `.janusmodel` file (in the results folder's
  `models`): its weights and what applying it needs (the architecture, the
  values and dates, the scaling, the classes and their colours, the samples and
  the training's history). *Open a model...* applies one to another series of
  the same number of dates and the same values (bands by name); the window says
  what does not fit.
- **Apply** to the whole image, the visible area or the ROI: the class map
  (Byte, with the classes' colours and names in the file), the probability of
  the class given and the uncertainty (the entropy over that of equal
  probabilities: 0 sure, 1 undecided) open as result layers, ready for the
  validation (the uncertainty as a stratum, too). Before applying, the class
  map can be **post-processed** as Zeit does it: *Smooth the probabilities*
  (sits' Bayesian smoothing, `sits_smooth`: each class's probability pulled
  towards its neighbours', the more where they agree; window, smoothness and
  neighbours as sits') and a *Minimum mapping unit* (patches of fewer pixels
  merged into the neighbouring class of the longest border); the uncertainty
  stays the model's. For a map you already have, Tools > Classification & pattern recognition >
  Post-processing (classes).
- **Area of applicability** (Meyer & Pebesma 2021, as Zeit's `aoa` and R's
  CAST): where the model can be trusted. A Random Forest, TempCNN or L-TAE
  model keeps its samples' series as it reads them (gaps filled, scaled; up to about
  a million values, as many of each class as their share) and the threshold of
  their dissimilarity: each sample's distance to the nearest sample of another
  place (another polygon or pin), over the mean distance between samples, and
  the threshold the upper whisker of those (75 % quantile + 1.5 IQR). Applied
  with *Area of applicability* ticked, it also gives the **dissimilarity
  index** of each pixel (its distance to the nearest sample over that mean)
  and the **AOA** (Byte: 1 inside, 0 outside, 255 not classified): outside it
  the class is an extrapolation the validation says nothing of. The tick's
  tooltip gives the model's samples and threshold; older models, Presto's and
  U-TAE's keep no samples. A forest's values are weighed by their importance
  (as CAST weighs a model's variables): a date that does not tell the classes
  apart does not make a pixel unlike the samples.
  - **Error profile** (CAST's `errorProfiles`, Zeit's `error_profile`): the
    validation samples (whole places held out from the training) sorted by
    their DI to the samples of the other places, the accuracy in moving
    windows of 5 of them, and a curve that only falls fitted to those (a
    monotone P-spline). The model keeps it; the Apply tab draws it, and an
    applied model gives the **expected accuracy** of each pixel inside the
    AOA (NaN outside), a continuous map of its uncertainty.
  - **Local point density** (ticked under *Area of applicability*; slower):
    how many training samples are as near a pixel as the threshold, i.e.
    whether the model learned from many samples like it or from one.
  - **Mahalanobis distance** (Classification window, Settings: *Area of
    applicability by the Mahalanobis distance*; CAST's `method = "MD"`): the
    samples whitened by their covariance, so values that move together
    count once. It needs values of full rank: a series' filled gaps are
    combinations of the dates around them, and then the Euclidean distance is
    kept (the task's log says why).
  - **Validation & uncertainty > Area of applicability (AOA)...** opens the
    Apply tab with *Only the area of applicability* ticked: the DI, the AOA,
    the LPD and the expected accuracy, mapped without classifying (the
    network is not run).

**Presto** (Tseng et al. 2023) is pretrained by its authors on monthly series
of Sentinel-1, Sentinel-2, ERA5 and SRTM worldwide. Janus runs its encoder with
their weights (3 MB, downloaded once from their repository and checked against
their SHA-256; without network, put `default_model.pt` in a folder and set
`JANUS_WEIGHTS` to it): the series' bands are matched to Presto's inputs by
name (blue, green, red, rededge1-3, nir, nir08 or B8A, swir1, swir2, VV, VH,
elevation, slope, or the B02... names), their units found from the values
(reflectance 0-1 or x 10000; dB or linear backscatter), and a year of monthly
composites made (each month the median of its observations; *The series' own
dates* gives it up to 24 dates as they are), with each pixel's latitude and
longitude. Two uses:

- *Make the embeddings*: Presto's encoding of every pixel of the scope (128
  values) for the year, written as an embedding layer that opens in the
  Embeddings panel like TESSERA's and AlphaEarth's (principal components,
  similarity, change).
- *Train*: a classifier on the encodings (the encoder kept as pretrained, a
  linear head learning from your samples; Adam 1e-3, 50 epochs, as
  `zeit.ai.train`), applied as the others. Few samples suffice where the
  encodings separate the classes.

Janus's encodings are those of the authors' code to float32's precision (a few
millionths; `tools/presto_reference.py`, `crates/janus-ml/tests/presto.rs`).

**U-TAE** (Garnot & Landrieu 2021) segments whole patches (32, 64 or 128 px a
side, every date): a U-Net encodes each date, a temporal attention fuses them at
the bottleneck and weighs the dates of every scale, so a pixel's class comes
from its neighbourhood too (the edges of fields). It learns from dense labels:
every pixel of the polygons of the sources above (or of the pins and points:
one pixel each), or *A class map* (every pixel of a classification already on
the map, e.g. a TempCNN's corrected), optionally only *inside the ROI*. Each
band is standardized (its mean and deviation); a date without observation is
left out of the attention. With the ten Sentinel-2 bands (B2, B3, B4, B5, B6,
B7, B8, B8A, B11, B12, by name) it can *start from the authors' PASTIS
weights* (20 crop types in France; 13 MB taken once from their Zenodo record,
CC-BY-4.0), its last layer made for your classes. The image (or the visible
area, or the ROI) is classified in windows overlapping by a quarter, each pixel
the mean of the windows' probabilities. With the authors' weights, Janus's
U-TAE gives the scores of their code (zeit.ai.utae, a port of it layer for
layer) to float32's precision (`tools/utae_reference.py`,
`crates/janus-ml/tests/utae.rs`).

On sits' own samples (`samples_modis_ndvi`, `samples_l8_rondonia_2bands`), the
same training and test samples give Janus the test accuracy of sits (within a
point or two, over three seeds; `tools/ml_sits_reference.R` and
`crates/janus-ml/tests/sits.rs`).

## Field delineation (parcels)

**Tools > Classification & pattern recognition > Field delineation (parcels)** draws the fields of a
Sentinel-2 series (blue, green, red and NIR, L2A digital numbers) as parcels,
with the networks of Fields of The World (Kerner et al. 2025) and WRI's Trazo
(Grupp et al. 2026): a U-Net (EfficientNet-B3) that labels each pixel of two
composites of a season, window A about the green-up and B about the harvest,
as background, field interior or field boundary. Ported to Burn from their
published weights (CC-BY-4.0, downloaded once into Janus's folder), on the GPU
or the CPU, no Python.

- **Model**: Trazo3 (South America, best in Mato Grosso), Trazo SA (the soy
  ecoregions), Trazo2, Trazo1, FTW B3 (the world), or a `.janusfields` file
  trained in Janus (*Open...*). The Trazo networks read the composites as they
  are (as they were trained: upsampled, their pixel IoU in Mato Grosso falls
  from 77 to 55 %), FTW's upsampled twice, as `ftw inference run` does.
- **Season**: found in the series (the area's NDVI date by date; each window
  *± days* about its start and end, shown by *Find the season*) or two windows
  chosen; each window's median composite, the quality band applied.
- **Parcels**: *watershed* (each field's sure interior, *Seeds* %, floods over
  the boundary's probability, so neighbouring fields touch) or *interiors
  (FTW)* (each set of connected interior pixels, as `ftw-tools polygonize`);
  a *Minimum area* (smaller ones merged into a neighbour) and a *Simplify*
  tolerance. *Tiles* of 128-512 px overlapping, each cut back to its middle.
- **Outputs**: the class map, the interior and boundary probabilities, the
  parcels' map and their polygons (`fields_parcels.gpkg`: `area_ha`,
  `perimeter_m`, `pixels`, `mean_interior`), opened as a vector layer.
- **Train on your fields**: a vector layer of fields drawn (or opened) in an
  area and an ROI around that area, where every field is drawn (in the ROI,
  pixels in no polygon are background and the polygons' edges boundary;
  outside it nothing is assumed). The chosen model is fine-tuned on chips of
  the ROI (its batch normalization frozen) for a number of epochs and saved in
  `field_models/NAME.janusfields`, chosen as the model at once.

On the Trazo dataset's test chips, scored as `ftw model test` scores them,
Trazo3 in Janus gives WRI's published figures (pixel IoU of the interior
79.1 % in Mato Grosso, 75.3 % on the soy set, 68.1 % in Chiquitania; WRI:
79.2, 75.0 and 68.8; `crates/janus-ml/examples/fields_eval.rs`), and its
scores are those of PyTorch's segmentation_models_pytorch within 1e-3
(`tools/fieldnet_reference.py`, `crates/janus-ml/tests/fieldnet.rs`). The
watershed finds more whole fields than the interiors (object recall 36, 49
and 44 % against 25, 35 and 36 %). The guide: `site/fields.html`.

## Spectra (hyperspectral)

A file whose bands carry their wavelength opens as a **spectrum** instead of a
series in time: the chart's axis is the wavelength (nm), the time bar moves
through the bands and each layer is labelled by its wavelength (*650.2 nm*).
It works for any such file; EnMAP is the first case tested.

- **Wavelengths** are read from the band metadata (ENVI's `wavelength` and
  `wavelength units`, nm or µm), from GDAL's `CENTRAL_WAVELENGTH_UM`, or from
  the `METADATA.XML` of an EnMAP product (its GeoTIFF delivery has none of its
  own). Bands are sorted by wavelength.
- **EnMAP**: open the product folder, the folder its zip extracts to (the
  product in a subfolder) or its `…-SPECTRAL_IMAGE` file: the spectral image
  is opened alone, not the quality layers beside it. Example
  scenes (L2A, no registration):
  [enmap.org/data_tools/exampledata](https://enmap.org/data_tools/exampledata);
  the whole archive is free with a DLR account.
- **On the map**: a spectrum opens in **true colour** (bands nearest to 640,
  550 and 470 nm) in the *RGB composite* mode; the composite list above the
  R, G, B sliders also has colour infrared, SWIR and agriculture. *Value* mode
  shows one band, the time bar choosing which (it starts at 860 nm).
- **On the chart**: the spectrum of the cursor, the pins and the ROI (mean and
  p10–p90 per band). Bands without data (e.g. the water absorption bands, all
  no-data in EnMAP L2A) leave a gap, and so does a stretch of the spectrum
  without bands (EnMAP L2A has none from ~1760 to ~1940 nm). Other spectra open
  as layers are drawn on the same axis and follow the band shown.
- **CSV** (copy the chart): a `wavelength_nm` column instead of `date`.
- **Index map** (Display panel → *Index map*): NDVI, NDRE (red edge), NDWI,
  NDMI, NBR, NDSI or PRI from the bands nearest to their wavelengths, added as
  a layer of its own above the spectrum (dated by the scene's name, e.g. an
  EnMAP product's acquisition date).
- **Principal components and similarity** (Display panel → *Principal
  components, similarity*, or the Embeddings panel): each pixel's spectrum is
  one vector, as embeddings are. *Principal components (RGB)*: the first three
  of the whole scene, or of an ROI (*ROI (local)*), as red, green and blue.
  *Similarity to a reference*: the cosine of the angle between each spectrum
  and the cursor's, a pin's or the ROI mean (1 = the same shape, brightness
  does not count; the spectral angle of SAM is its arc cosine). Bands without
  data anywhere are left out; the view can be exported as a GeoTIFF at full
  resolution (Ctrl+Shift+M).
- **A series of scenes** (one file per date, e.g. a folder holding several
  EnMAP products): dates from the names, each band named with its wavelength,
  and the *Index by wavelength* list in the Bands section. It opens in NDVI.
- **Every band in each block**: a compressed GeoTIFF with every band in each
  block (pixel-interleaved, `gdal_translate`'s default; a stack exported from
  Earth Engine) decompresses all its bands for any one of them. Janus reads it
  block by block, every date of a block at once, its blocks decompressed by
  Janus itself when DEFLATE or ZSTD without a predictor: the overview, an ROI
  and the exact series each decompress a block once (875 Float64 bands of
  1690 x 1675, a CCDC stack of 3.3 GB, 125 dates of 7 bands: the overview in
  5 s rather than over 15 minutes). Zoomed in, the first date shown at a
  place reads every date there at once (each block decompressed once) and
  keeps them in memory (an eighth of it at most, the places seen longest ago
  let go first): paging or playing through the dates there reads nothing
  more.
- The Zeit tools work along time and are disabled on a spectrum.

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
refused. Janus downloads them itself, no Python needed, in the background (a bar on the map tells the year and
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

## Vector layers

Field plots, samples of a map's accuracy assessment, forest inventory plots,
parcels or burned areas usually come as vector files. File → **Add vector
layer...** (or the **Layers** panel, a drop, the Files panel, the command line)
reads them through OGR, which comes with GDAL: GeoJSON, Shapefile, GeoPackage
(each of its layers becomes a vector layer), KML/KMZ, FlatGeobuf, GML, GPX and
CSV tables of points (columns named `x`/`lon`/`longitude`/`easting` and
`y`/`lat`/`latitude`/`northing`; without a CRS, coordinates within ±180/±90 are
read as longitude and latitude).

- **On the map**: in the drawing order of the **Layers** panel, with the
  series (a new vector layer on top; drag it under a series, or right click →
  *Move*, and the series covers it), on every map panel and the swipe, in any
  CRS (placed on the active layer's grid through PROJ, in
  parallel and in the background, again whenever the active layer changes to
  another grid: the map goes on meanwhile, the layer marked *placing...* in the
  **Layers** panel). Points
  are dots; lines and polygons (with their holes) are outlines. Each layer has
  a colour, can be hidden or removed; right click its name for *Zoom to it*.
- **Without a raster**: vector files opened alone make a map of their own (an
  empty canvas in their CRS over their extent, with the basemap under them);
  it goes when a series is opened. The **Time series** and **Statistics**
  panels are left out until then: they come with the first raster series
  (where they were, if nothing else was moved) and go with the last one.
- **Large layers**: the GPU draws them, as a GIS does. Once placed, a layer
  is uploaded once (in the background, a moment for millions of vertices) as
  levels of detail (each ring simplified with Douglas–Peucker, the tolerance
  doubling from 1/32 of a pixel) in pieces along a Morton curve; each frame
  draws the coarsest level within half a screen pixel of the exact outline,
  and only the pieces in view. The 5572 municipalities of Brazil (IBGE 2021,
  16.9 million vertices) draw in about 0.3 ms a frame, the whole country or
  zoomed in to the street, against 45 ms (and an incomplete map) on the CPU;
  read in 0.5 s, placed in 0.05 s (0.4 s reprojected to UTM), built for the
  GPU in 0.4–1.2 s, 230–380 MB of GPU memory. A level is kept when it has at
  most 60% of the points of the finer one kept (counted on a sample of the
  pieces first, so only those are made); a ring smaller than a level's
  tolerance is a dot in it. A file is read whole up to 100 million vertices
  (about 140 bytes each once drawn). A state's CAR, made up
  (`tools/bench_vector_draw.py`: 1.2 million polygons, 34 million vertices,
  `bench/vector-draw.json`), is read in 1.6 s, placed in 0.4 s, built for the
  GPU in 1.0–1.3 s (730 MB on the GPU, 4.9 GB of memory in all) and drawn
  whole 3–4 s after it was opened; a frame takes 4–9 ms with the whole layer
  in view, 1 ms 8 times closer, 0.4 ms zoomed in, and 97% of its outlines (as
  GDAL rasterizes them) are lit zoomed out, where the CPU drew 56% of a file
  cut at 20 million vertices. Until a layer is uploaded the CPU draws it
  (300 000 vertices a frame), and always the feature under the cursor, the
  one interpreted and those picked in the attribute table. The feature under
  the cursor (and `Alt` + click) is found through a grid of the features'
  boxes: 38 µs among those 1.2 million polygons (0.7 ms before).
- **Labels** (the layer's menu → *Labels*): the values of a column (a
  field's name, a municipality, a CAR id...) written on the map, their size,
  colour and halo (an outline that keeps them legible over any image). A
  polygon's label at its point inside (the one Alt + click and the
  interpretation use), a line's at the middle of its length, a point's to its
  right. A polygon or a line is labelled only when its box on screen is at
  least twice the label's height; no two labels overlap: the larger features
  first (a polygon's box, a line's length), a label that would cover one
  already placed is left out, as are those of a layer under another's; at
  most 1500 a view. The column is read on a thread (OGR skipping the
  geometries and the other fields), the label points made once per placement
  in a grid of cells, each cell's features from the largest, so a view of a
  million polygons takes only those large enough. Kept in the project, copied
  with *Copy style* (a saved style too), drawn in the map's PNG and the
  animation (*Pins, ROI and vector layers*). `--labels FIELD[,SIZE]` labels
  every vector layer (screenshots).
- **Attributes**: a file opens with its geometries alone (OGR skips the
  fields: 200 000 polygons of 16 fields read in 0.23 s from a GeoPackage
  instead of 1.07 s, 0.68 s instead of 1.85 s from a Shapefile); the fields
  are read when asked. Hovering a point shows its fields (read for it alone);
  for lines and polygons hold `Alt`. The status bar names the feature under
  the cursor.
- **Attribute table** (right click → *Attribute table*, or View → Attribute
  table): the layer's rows in a panel next to the chart (below the map without
  a series), read from the file in the background the first time; a filter
  (a text in any field), a click on a column's header sorts by it (numbers as
  numbers; again: descending, then as read), a click on a row highlights its
  feature on the map, a double click zooms to it.
- **A polygon as the ROI**: `Alt` + click it. Its holes are left out; the
  statistics, box plots, embeddings' local PCA and the scatter use the pixels
  whose centres are inside, on every layer.
- **The ROI's vertices edited** (`Shift+R`, View → *Edit ROI vertices*): the
  ROI's polygon (drawn, or taken from a layer with `Alt` + click: the file is
  not changed) adjusted before it is used. Drag a vertex; drag the dot in the
  middle of an edge to add a vertex there; right click a vertex, or `Delete`
  over it, to remove it. Every change is a step of Undo (`Ctrl+Z`).
- **The ROI saved** (View → *Save ROI as vector...*): a GeoPackage or GeoJSON
  of one polygon in the active layer's CRS (fields `name`, `layer`, `pixels`),
  opened as a vector layer if asked; `Alt` + click on it gives back the same
  ROI, pixel for pixel. Read back by geopandas and burned by
  `rasterio.features.rasterize` on the grid, an outline of 600 vertices with a
  hole gives the same 5 146 615 pixels (`tools/bench_profiles_vs_rasterio.py`,
  `bench/profiles-vs-rasterio.json`).
- **Profiles of every line** (right click a line layer → *Profiles of every
  line...*): each line, or each part of a multi-line, sampled about a pixel
  apart as the transect samples it (n = its length in pixels, rounded up,
  plus one, evenly spaced; distances in metres), through every date or a range
  of them, the values the chart shows. A task (Tools → Tasks, with progress and
  cancel, on the processing threads) that reads only the windows of up to
  128 × 128 pixels around the samples, each once a date, and writes
  `profiles.csv` (a row per sample and date: `fid`, the fields chosen, `part`,
  `sample`, `distance_m`, `x`, `y`, `col`, `row`, `date`, `value`) and, if
  asked, `profiles_wide.csv` (a row per sample, a column per date). Against
  shapely (`line_interpolate_point`) and rasterio on 200 lines over 4096 ×
  4096 pixels and 12 dates (2 822 100 rows): the same pixels and the same
  values, float32 for float32, distances within 5 µm; 0.72 s with 3 threads
  against 3.6 s reading each date whole with numpy and 71 s with
  `src.sample`; the CSV written in 0.77 s against 9.9 s by pandas.
- **Extract series** (right click, or the *Extract series...* button under
  the layer): the series of every feature at full resolution, as the chart
  shows them (band or normalized difference, quality mask applied), for the
  active layer or every visible layer, written as CSV in the background
  (Exports window):
  - points: the value of the pixel under each; polygons: the mean of the
    pixels whose centres are inside, with their count (`pixels`); lines are
    left out; a feature off a layer has empty cells for it;
  - *a row per feature, a column per date* (`fid`, the file's fields, `lon`,
    `lat`, then the dates; with several layers the columns are `<layer>
    <date>`), or *a row per feature and date* (`layer`, `date`, `value`),
    easier to read in R or pandas.
- **Interpretation** (right click → *Interpret...*, or *Interpret...* under
  the layer; the **Interpretation** panel, next to Statistics): the features
  one by one, as TimeSync does with Landsat. The map goes to each one (zoomed
  in to at least the *zoom on each* setting; a polygon whole) and its series is
  on the chart: a point gets a pin, a polygon becomes the ROI.
  - `N` / `Shift+N` (or Page Down / Page Up, or the arrows in the panel): next
    / previous feature; *Skip labelled* goes to those without a label only;
    *Next after a label* moves on by itself.
  - `1`–`9`: a label of the label set (Stable forest, Deforestation,
    Degradation, Regeneration, Agriculture, Pasture, Water, Cloud / no data,
    Other by default; edit it under *Label set*); `0` clears it.
  - `D`: the date of the change is the date on the map (go to it with `←`/`→`
    or a click on the chart); `Shift+D` clears it. *Notes* for anything else.
  - `Shift+1` / `Shift+2` / `Shift+3`: the label's *confidence* (low, medium,
    high). `E` adds another *event* at the map's date (the label chosen under
    *Other events*, else the feature's label): a point degraded, then cleared,
    keeps both.
  - **Chips** (Validation & uncertainty → Chips, or *Chips* in the panel): the feature at
    every date of the active layer, a small window around it in the map's
    colours (or its RGB composite), the feature's pixel outlined; *one per
    year* keeps the date nearest the map's day of the year. Click a chip to
    show its date on the map, double click to make it the date of the change.
    The next features' chips are read ahead.
  - The labels are saved a second after each change (in Janus' data folder,
    by file and layer): starting the interpretation of the same file again
    resumes it at the first feature without a label.
  - *Export table...*: the file's features with their fields and three more,
    `label`, `change_date` and `notes`, as GeoPackage, GeoJSON or Shapefile
    (with the geometries) or CSV (with `lon`, `lat`).

## Validation: accuracy and area (Olofsson et al. 2014)

The **Validation & uncertainty** menu turns a map — a Zeit result (LandTrendr's year of
detection, CCDC's breaks, a TWDTW or SOM class map...), a categorical layer at
a date (MapBiomas), or any raster file — into a stratified random sample,
interpreted point by point, and estimates of the map's accuracy and of the
**error-adjusted area** of each class, with confidence intervals, following
the good practices of Olofsson et al. (2014). A step-by-step guide with
screenshots, from the strata to the report, is on the website:
[Validating a map, step by step](https://sacridini.github.io/janus/validation.html).

**Design** (Validation & uncertainty → New sampling design; the **Validation** panel):

- *Maps to stratify*: one map, whose classes are the strata, or several, whose
  agreement is (a stratum for each number of maps that call a pixel change:
  *change in 2 of 3 maps*...). Each map is read as **classes** (its values),
  **intervals** of its values (edges, e.g. `-1, -0.1, 1` for a magnitude) or
  **events** (a date of change: > 0 is change, no data and 0 are not; with
  periods, e.g. `2000, 2010, 2021`, a stratum per period). Tick the classes
  (or intervals) that are change.
- *Buffer*: the pixels without change within N pixels of a change become a
  stratum of their own: omission errors (change the map missed) hide there,
  and a few points in it make the area of change much more precise.
- *Limit*: every pixel, the pixels inside the ROI, or inside the polygons of a
  vector layer.
- **Compute the strata**: one pass over the map(s) at full resolution in the
  background (another map is read on the first one's grid); the strata raster
  opens as a layer, with each stratum's pixels, area (hectares; on the sphere
  for a geographic CRS) and weight.
- *Sample size*: from the standard error wanted for the overall accuracy
  (eq. 13, with the user's accuracy expected of each stratum), or given;
  *allocation* proportional, equal, Neyman or by hand, with a minimum per
  stratum (rare change needs 50–100 points). The panel shows the errors the
  design should give: the standard error of the overall and of each user's
  accuracy, and the interval of the area of change.
- **Draw the sample and create the project...**: a GeoPackage (the project)
  and its strata raster beside it. The same strata, sizes and seed draw the
  same points. Later, *Add points* draws more in a stratum (still a
  stratified random sample).

**The project** (a GeoPackage: open it in QGIS, keep it with the paper):
`samples` (the points at their pixels' centres, with the reference label of
each), `labels` (every interpreter's labels), `strata` and `project` (the
design, the maps assessed, the translations). Validation & uncertainty → Open project...
resumes the work.

**Interpretation**: the sample opens in the Interpretation panel (see
[Vector layers](#vector-layers)), the map going to each point, its pixel
outlined. **Blind** (on by default): the maps assessed and the strata are not
drawn and the points are coloured only by their state (yours, others',
disagreement, adjudicated), so the reference is not swayed by the maps.
Labels describe the ground, not the map: the class, the date of the change,
the confidence, other events, notes. They are written to the GeoPackage a
second after each change. *Mine to do* lists the points you have not
labelled.

**Accuracy** (updated with every label):

- *Reference labels → the map's classes*: how each label reads for this map
  (guessed from the names; for a map of events, loss and disturbance labels
  are change); "can't tell" and clouds are left out. A point is change when
  any of its events is; with periods, by the date of the first.
- The overall accuracy, the **error matrix** in points or in proportions of
  area (a click on a cell lists its points in the Interpretation panel: the
  errors one by one), each class's user's and producer's accuracy, and its
  mapped and **adjusted area ± the interval** (a chart of both).
- *As the labels came*: the overall accuracy or a class's area as points were
  labelled, with its interval: label until it is narrow enough.
- Maps of events: the agreement of the **dates** (within ± k years, the
  histogram of the differences) and the **area of change by year** with its
  interval, from the reference's dates.
- *Areas by the ... estimator*: **direct** (the sample alone, Olofsson et al.
  2014; Stehman 2014 when the strata are not the map's classes),
  **difference** (model-assisted: the mapped area plus the estimated net
  error, the share where the reference has the class minus where the map has
  it) or **ratio** (the mapped area times the estimated ratio of the two;
  needs points the map calls the class) (Stehman 2013; McRoberts et al. 2018).
  When the strata are the map's classes the three agree; when they are not
  (another map assessed with the sample, transitions, a buffer stratum) the
  model-assisted ones give narrower intervals where the map is good. The
  accuracies do not change with the estimator. The choice is kept in the
  project and used by the report and its Python.
- **Transitions between two dates**: two of the project's maps of classes
  (e.g. two dates of a categorical series): each pair of classes (*Forest ->
  Pasture*) is a class, its mapped area counted pixel by pixel inside the
  strata (in the background) and its reference read from each point's label
  and its dated events: the class at a date is the label, or the last event
  on or before it with one of the maps' classes (the dates are guessed from
  the maps' names, and can be typed). The table gives each transition's
  mapped and adjusted area ± its interval, user's and producer's accuracy
  (*only changes* hides the persistences); *Python...* gives the
  `zeit.transitions` code. The strata need not be the transitions: the
  points are weighed by the project's strata (Stehman 2014).

**Maps**: one sample assesses many maps. Add a Zeit result, a layer at a date
or a file (*Assess another map...*): each is read at the points and its areas
over the strata, and its labels translated; the estimators weigh the points by
the project's strata (Stehman 2014), so maps other than the one stratified are
assessed without bias. The table compares their overall accuracy, the user's
and producer's accuracy of change and the area of change.

**Interpreters**: your name is in Settings (by default your login). Each
interpreter can label a copy of the project (*Save a copy for an
interpreter...*); *Merge labels from a copy...* brings their labels back (by
point, the newer kept). The panel shows each pair's agreement and Cohen's
kappa; *Adjudicate...* goes through the disagreements, and the label given
settles each (the others' labels shown).

**Report**: *Export the report...* writes `report.html` (design, maps,
matrices, accuracies, areas with figures, interpreters, a methods paragraph
and the references; the transitions between two dates, when chosen, with
`transitions.csv`) and a CSV of every table. *Python...* gives the
`zeit.accuracy` (and `zeit.transitions`) code that computes the same estimates,
by the same estimator, from the project's files.

## Official alerts against a map of change

**Validation & uncertainty → Official alerts...** (the Validation panel's
*Alerts* tab) compares a run's map of change dates (a Zeit result of this
session that is a date: LandTrendr's year of detection, CCDC's, BFAST's or
SCCD's break dates, CODED's change dates...; lengths of time, such as
LandTrendr's duration, are left out) with the alerts the agencies publish. It is
not an accuracy assessment (the alerts are another map, with errors of their
own; see the sampling design above for that); it tells where and when the two
maps agree, and how late or early one is against the other.

**Sources**

| Source | What it is | How Janus reads it |
|---|---|---|
| **DETER** (INPE) | Polygons of deforestation and degradation seen in daily WFI/AWiFS images, dated (`view_date`) and classed (`classname`: DESMATAMENTO_CR, DEGRADACAO, CICATRIZ_DE_QUEIMADA, MINERACAO...) | TerraBrasilis' WFS (`https://terrabrasilis.dpi.inpe.br/geoserver/ows`): `deter-amz:deter_amz` (Amazon) or `deter-cerrado-nb:deter_cerrado` (Cerrado), the map's box and window fetched in a task (Tools → Tasks) into a GeoPackage of the cache and opened as a vector layer (as *Add web layer...* does) |
| Any vector layer | Dated polygons (a date field, a class field optional) | An open vector layer or a file |
| **GLAD-L** (UMD; Hansen et al. 2016) | Landsat, 30 m (0.00025°), a pair of files per year and 10° tile: `alertYY` (2 probable, 3 confirmed) and `alertDateYY` (the day of the year, 1 = 1 January) | UMD's public bucket, no key: `storage.googleapis.com/earthenginepartners-hansen/GLADalert/C2/current/` (2021 to now), read over the network (`/vsicurl/`) for the map's box only; or files |
| **GLAD-S2** (UMD's GLAD laboratory) | Sentinel-2, 10 m (0.0001°), Amazon basin: `alert` (1 loss in the most recent observation only, 2 low, 3 medium, 4 high confidence) and `alertDate` (days since 2018-12-31) | The same bucket's `S2alert/alert/` and `S2alert/alertDate/`, no key; or files |
| **RADD** (Wageningen; Reiche et al. 2021) | Sentinel-1, 10 m, the tropics: as GFW encodes it, one band `confidence * 10000 + days since 2014-12-31` (2 low, 3 high) | GFW's GeoTIFFs need a GFW API key: open them from disk. *Of the map's area* reads GFW's public map tiles instead (`tiles.globalforestwatch.org/wur_radd_alerts/latest/default/14/x/y.png`, zoom 14, about 10 m at the equator; at most 400 tiles) and decodes their colours: the day is `R * 255 + G`, the confidence `B / 100 - 1` (gfw-data-api's `date_conf_rgb_calc`). The export of Google Earth Engine (Alert 2/3, Date `yyDOY`) is not read |
| **Integrated alerts** (GFW) | GLAD-L, GLAD-S2 and RADD in one band: `confidence * 10000 + days since 2014-12-31` (2 low, 3 high, 4 highest: two or more systems) | As RADD (`gfw_integrated_alerts`; the confidence of a tile's pixel in its alpha, `A >> 2`: 1 low, 2 high, 4 highest) |

GLAD-L's files of the years the window covers are paired by name
(`alert21_*`, `alertDate21_*`); several tiles or years keep each pixel's
earliest alert. The hover text of each product says where its files are.

**Rules** (the same in `tools/bench_alerts_vs_python.py`):

- The grid is the map's. A polygon covers the pixels whose centres it holds
  (GDAL's rasterization, as `rasterio.features.rasterize`); where polygons
  overlap, the earliest alert holds the pixel. A raster of alerts is warped
  onto the grid by the nearest pixel, each pixel's centre projected exactly
  (`gdalwarp -et 0`: GDAL's default interpolation within 0.125 pixel depends
  on how the warp is read, and took the other neighbour at a few edges).
  Pixels outside the alerts' cover (a raster's tiles) are left out of
  everything.
- A map pixel changed when its date (decimal years; Janus's: the year plus
  the days since 1 January over the year's length, to the nearest day) is in
  the window; an alert pixel when its alert's date is. The window is *From*
  and *To* (by default all dates).
- **Areas**: of change in both, only in the map, only in the alerts (ha, a
  geographic CRS's pixels by their latitude); the share of the alerts' area
  the map detects, of the map's change the alerts confirm, Dice's coefficient.
- **Counts**: an alert (a polygon, or a raster's patch of 8-connected alert
  pixels: its date the earliest, its confidence the highest) is detected when
  one of its pixels changed in the map; a patch of the map's change
  (8-connected) is confirmed when one of its pixels is an alert's. Polygons too
  small to hold a pixel's centre are counted apart.
- **Lag**: the map's date minus the alert's, in days, at every pixel in both
  (positive: the map later). Median, mean, quantiles (5, 25, 75, 95%; numpy's
  linear), the share within 30, 90 and 365 days either way, and a histogram of
  bins of *Bin* days (30 by default). A map of whole years (LandTrendr's) is
  said: its lags count from 1 January.
- **By class**: DETER's classname, or the raster's confidence: alerts,
  detected, their area, the median lag.

**Layers and files**: the **agreement map** (1 both, 2 only the map, 3 only
the alerts; the *Agreement* colours) joins the Layers panel, and for rasters
the alerts' dates (decimal years, as the map) too; the run's folder is
`RESULTS/alerts/NAME_TIME`. *Export CSV...* writes `NAME_summary.csv`
(metric,value), `NAME_classes.csv`, `NAME_alerts.csv` (every alert: its id,
date, class, pixels and hectares, those the map changed, detected, its median
lag, a polygon's own area), `NAME_lags.csv` (the
histogram) and `NAME_agreement.tif`.

**Command line**: `--alerts SOURCE[,KEY=VALUE...]` compares once a map of
change dates is there (a run of this session: `--zeit NAME --zeit-run`; a
result opened with `--layer` is a layer, not a run's map): SOURCE is `deter-amz`, `deter-cerrado`, `wfs`/`oapif`
(`url=`, `layer=`, `date=`, `class=`, `name=`), `glad-l`, `glad-s2`, `radd` or
`integrated` (`files=CONF;DATE;...` or `download=1` for the map's area); keys
`map=` (part of the map's name), `from=`, `to=` (YYYY-MM-DD), `tab=0` (the
panel left closed). `--screenshot` waits for the comparison and its layers.

Compared with geopandas, rasterio, numpy and scipy (the same rules written
with them): `tools/bench_alerts_vs_python.py` (`conda run -n geo`), results in
`bench/alerts-vs-python.json`:

| Case | Grid | Alerts (detected) | Both / only map / only alerts (ha) | Median lag | Janus | Python | Same numbers |
|---|---|---|---|---|---|---|---|
| Synthetic polygons (20 000, two classes) | 3000 × 3000, 30 m | 18 093 (8 300) | 99 458 / 76 475 / 348 575 | 200 d | 0.54 s | 1.84 s | yes |
| Synthetic GLAD-S2 (10 m over a third) | 3000 × 3000, 30 m | 2 242 (661) | 3 978 / 14 620 / 15 106 | 23 d | 0.30 s | 1.27 s | yes |
| DETER (Amazon), WFS | 831 × 667, 20 m | 91 (85) | 783 / 908 / 7 212 | 272 d | 0.03 s | 0.11 s | yes |
| GLAD-S2, UMD's files | 831 × 667, 20 m | 3 024 (284) | 654 / 1 038 / 4 870 | 145 d | 0.21 s | 0.28 s | yes |
| GLAD-L 2021-2023, UMD's files | 831 × 667, 20 m | 518 (143) | 333 / 1 359 / 1 053 | 116 d | 0.57 s | 0.66 s | yes |
| RADD, GFW's tiles | 831 × 667, 20 m | 2 049 (312) | 431 / 1 261 / 1 916 | −202 d | 0.14 s | 0.20 s | yes |

The real cases are the validation guide's place (16 × 13 km south-west of Novo
Progresso, Pará; `tools/make_tutorial_data.py`): LandTrendr's year of
detection on the NBR of the eight dry-season Sentinel-2 composites
(2017-2024) against the alerts of 2019-2023. Times are the comparison alone,
the median of 3 (the alerts already fetched: DETER's 98 features took 5.6 s,
RADD's 48 tiles 41 s); the rasters' times are mostly GDAL's warp, the same on
both sides. GFW's RADD tiles decoded by Janus and by Python from the same PNGs
give the same values. The lags count from 1 January of LandTrendr's whole
years, so their sign says little here; with a dated map (CCDC, BFAST) they
are the delay in days.

## Animation (GIF, MP4, WebM)

File → **Export animation** (`Ctrl+Shift+A`) writes the map over a range of
dates as a timelapse, drawn exactly as the map shows it (layers, colours,
detail tiles, basemap, results on top):

- **Dates**: from / to (a range that ends before it starts plays backwards; on
  a spectrum, bands), a step (every n-th date) and the frame rate (the map's
  speed by default).
- **Resolution**: 1× or 2× the map's size on screen (rendered at it, not enlarged).
- **Format**: GIF (no extra software; up to 256 colours per frame, optional
  dithering), MP4 (H.264) or WebM (VP9) through [ffmpeg](https://ffmpeg.org).
  Janus uses the ffmpeg set in File → Settings → Video export, else one next to
  the program, else the one on PATH (`winget install ffmpeg`, `apt install
  ffmpeg`, `brew install ffmpeg`). Without it, MP4 and WebM are disabled.
- **Overlays**: a large date stamp (top left, with the layer and display mode),
  the legend, a **scale bar** (ground distance at the centre of the view; needs
  a CRS; also available for PNG figures), pins, the ROI and the vector layers;
  the basemap's credit whenever it is shown.

While the frames are drawn a progress window keeps the view as it is (Cancel
stops it); each frame waits for its date, its detail tiles and the basemap's.
The date shown comes back at the end. Encoding continues in the Exports window
(`Ctrl+J`); a cancelled or failed export leaves no file behind. If ffmpeg fails,
the error shows its last lines (the full log is `ffmpeg.log` in Janus' data
folder).

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
- On an SSD the first date of a new overview is read by every background
  reader together, rows split among them, and shown as soon as it is read; the
  other dates follow, several at once. 41 Landsat dates (7318×7329 Float32, LZW,
  1-row strips): the first image after 0.15 s on an NVMe SSD and 0.36 s on a
  SATA SSD (2.3 s when each reader took a date of its own), the whole overview
  in 3.6 s and 14 s.
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
  taken from those rows; once a date is read the disk goes on to the next one
  while its last rows are decoded. 41 dates of 7318×7329 (7.7 GB) on a 4 TB
  HDD: both caches in 51–52 s, against 47 s to read the files alone. When the overview already came from its cache, the cache is
  built in a pass of its own (54 s per tile, bound by the disk). Each date is saved as it is
  built and used right away; a stopped build resumes.
- The cache is on by default for series on an HDD; **File → Settings**
  turns it on for every series or leaves it to **Build it now** (per series).
  With the files on an SSD it matters less.
- **Disk cache budget**: the overview and full-resolution caches share one
  budget, taken from the disk that holds the cache folder. Automatic (the
  default): a tenth of the disk, 8 to 256 GB (a 256 GB SSD: ~25 GB; a 4 TB
  disk: 256 GB); **File → Settings** sets one by hand instead. Either way a
  tenth of the disk (10 to 100 GB) is left free: as the disk fills up the
  budget shrinks and the least recently opened series leave the cache first;
  the open series' caches are never removed. Checked at startup, before each
  cache file is written, when a series is closed and every minute. A
  full-resolution build that does not fit is not started (the panel says why:
  the budget, or the disk's free space); one running when the disk fills up
  stops (**Resume** once there is room); an overview that does not fit is
  built in memory without being saved. The Performance panel shows what the
  caches take, the budget and the disk's free space. Basemap tiles keep their
  own limit (512 MB, `cache\basemap`).
- **Threads**: on an SSD the background readers (overview, full-resolution
  cache) use the processing threads of [Settings](#settings), at most 12, and
  exact series, ROI, tiles and the transect up to 4. On an HDD both are one
  reader whatever the setting (a disk decision); the full-resolution cache
  built with the overview decodes on half the processing threads (1 to 4).
  The Performance panel shows the counts in use.

## Updates

**Help → Check for updates...** asks GitHub for the latest release and shows the
version you use (also under **Help → About Janus**, with GDAL's), the new one and
its notes. On Windows, when Janus was installed with its installer,
**Download and install** downloads the new installer and **Install now** closes
Janus, installs the new version in the same folder (for you or for all users, as
before) and opens it again. Elsewhere, or from a build, **Download** opens the
new package in the browser. Janus checks once when it starts (Settings: *Check
for updates when Janus starts*); when there is a newer version, a notice says so
and the right of the menu bar shows *Janus X.Y.Z is out (you are using ...)* and
**Update**, which in the installed program downloads it at once (then **Install
now**).

## Settings

**File → Settings...** (`Ctrl+,`; Mac: `Command+,`). Everything applies at
once and is kept in the layout file, next to the other data of Janus
(`%LOCALAPPDATA%\Janus\layout-0.7.ini` on Windows).

| Setting | What it does |
|---|---|
| Interface | **Modern** (the default): the look of the image editors (Photoshop, Lightroom): the panels as rounded cards on a darker ground, their tabs on a darker bar, rounded controls, the selection in blue, soft shadows under windows and menus, and a proportional font (Inter); its themes are Photoshop's four greys, **Darkest** (the default), **Dark**, **Medium** and **Light**, picked from their swatches. **Classic**: Dear ImGui's look, as the C++ version draws it (its pixel font and the seven themes below). Everything else (the panels, their places, the menus, the shortcuts) is the same in both; `--interface modern|classic[,THEME]` chooses for one run |
| Theme (classic) | **Dark** (the default), **Light**, **Classic** (ImGui's original colours), **Janus** (the program's colours: blue on navy panels, orange accents), **Studio** (mid grey with dark text and dark chart displays) **Graphite** (dark grey with a yellow accent) or **Matrix** (phosphor green on black). Status text and chart series are adjusted to stay legible on each; the map, its colour bars and exported figures keep their colours |
| Panels' background | How opaque the panels' backgrounds are, 10 to 100% (100% by default): below 100% the desktop (or whatever is behind the window) shows through the panels, the menus and the map's empty area, while text, chart lines, controls and the map's data stay opaque; in every window, the detached panels' included. Contrast is kept on the theme's own colours |
| Whole window | The opacity of the whole window, 20 to 100% (the system's window transparency: text and map included), and of every detached panel |
| Font size | Size of the interface text, 10 to 28 px (13 by default: ImGui's pixel font in the classic interface, other sizes its scalable font; Inter in the modern one). Widths grow with the font (the fields, lists, buttons and sliders, in proportion to 13 px), texts that would not fit are cut short with an ellipsis (the whole on hover); exported figures keep their text as at 13 px |
| Files panel follows what is opened | On by default: opening a series, a file or a folder (from anywhere) expands the Files panel down to it and scrolls there; its folder is listed again, so new files show up |
| Reopen the last session at start | Off by default. On: Janus opens with the layers, styles and view it had when it was closed (kept every minute and at exit, see *Projects* in [Using the window](#using-the-window)); a session with changes not saved in its project is marked changed (`*`) |
| Check for updates when Janus starts | On by default: one request to GitHub when Janus opens; when there is a newer version, a notice and, on the right of the menu bar, the version and **Update** (downloads and installs it; **Help → Check for updates...** shows its progress) |
| Processing threads | How many CPU threads Janus and Zeit may use, shown as *N of M* logical cores; default: all but 2 (at least 1), so the computer stays responsive during long runs. Applies to the readers of the open series (they shrink or grow at once), the full-resolution cache, exports (GeoTIFF compression, PNG) and Zeit: raster jobs started from then on (as `OMP_NUM_THREADS`, `NUMBA_NUM_THREADS`, the BLAS limits and Zeit's `n_jobs`; jobs already running keep theirs) and the process that fits the chart and estimates run times (replaced in the background once idle) |
| Downloads at once | How many downloads (STAC series and composites, embeddings) transfer at the same time, 1 to 8 (default 2); the others wait their turn in Tools → Tasks |
| Overview memory | Memory for the overviews (1024 MB by default), shared by every layer open by their dates: 41 maps of one date each opened as layers of their own take what one series of their 41 dates takes. A single date has no statistics over time (nothing reserved for them). **Apply** reopens the active layer. `--budget` overrides it for one run |
| Disk cache budget | **Automatic** (the default: a tenth of the disk holding the cache folder, 8 to 256 GB) or a size set by hand, 8 to 2048 GB; shows the size now, why it is smaller when the disk is filling up, and what the overviews and full-resolution caches take (see [Performance](#performance)). From an older version, a full-resolution cache budget changed by hand is kept; the old default (64 GB) becomes automatic |
| Clear overview cache | Deletes the cached overviews except the open series' |
| Full-resolution cache | When it is built (on demand, series on an HDD, every series), and clearing it (see [Performance](#performance)) |
| Interpreter | The name your labels of a validation project are kept under (by default your login), see [Validation](#validation-accuracy-and-area-olofsson-et-al-2014) |
| Video export (ffmpeg) | The `ffmpeg` executable used for MP4 and WebM animations (**Browse...**); empty: one next to Janus, else the one on PATH (see [Animation](#animation-gif-mp4-webm)) |
