# Using Janus

[README](../README.md) · [Install](install.md) · [Usage](usage.md) · [Building](building.md) · [Architecture](architecture.md)

## What it does

- **Opens** a folder of rasters (1 file = 1 date), a multiband file (1 band = 1
  date), a list of files or a wildcard pattern. Dates are parsed from file names
  or band descriptions (`YYYY-MM-DD`, `YYYYMMDD`, `YYYY_MM`, `YYYY`, `A2001001`, ...).
  A file whose bands have wavelengths (hyperspectral, e.g. EnMAP) opens as a
  **spectrum** (see [Spectra](#spectra-hyperspectral)).
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
  layer): mean and p10–p90 per date, or a box plot per date (p25–p75 box,
  median, whiskers to p10 and p90), of the pixels whose centres are inside.
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
  similarity, change) as a Float32 GeoTIFF at full resolution; a range of
  dates as an **animation** (GIF, or MP4/WebM through ffmpeg) with the date
  stamp, legend and a scale bar (see [Animation](#animation-gif-mp4-webm)). The export window shows where the file goes (*Save to*: type a path,
  or a name alone for the last export folder, or pick one with **Browse...**;
  it warns before replacing a file). Files are written in the background
  (File → Exports shows progress);
  **Copy CSV** of the cursor, pins and ROI series; **Copy Python**: code that
  reproduces a series, its statistics and the Zeit tool on the chart.
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
  CSV of points, in any CRS, drawn over every layer and map panel. A polygon
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
- **Swipe** (View → Swipe, `C`): a divider you drag across the map; left of it
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
- **Settings** (File → Settings, `Ctrl+,`): a colour theme (dark, light,
  classic, Janus, Studio, Graphite or Matrix), translucent panels or a
  translucent window (the desktop shows through), the interface font size, and how many CPU threads Janus and Zeit may use (all cores but
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
  https://.../cog.tif cloud-optimized GeoTIFFs on the web, read in place (only
  s3://, gs://, az:// the blocks shown are downloaded); several URLs = a series
  clim.nc, data.zarr  NetCDF / Zarr / HDF: a variable's time steps (CF dates);
                      with several variables the window asks which one, or
                      name it: NETCDF:"clim.nc":t2m, ZARR:"data.zarr":/t2m
  plots.gpkg ...      vector files (GeoJSON, Shapefile, GeoPackage, KML, CSV
                      of points) open as vector layers over the series

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
  --describe IN       print how IN opens (dates, size, CRS, variables of a
                      NetCDF / Zarr, the series of the centre pixel) and exit
  --selftest-zeit IN  run the Zeit tools end to end on IN without a window
  --measure-validation IN  time the strata of the map IN (a buffer of 1 px), its
                      areas, a sample of 1000 points and the estimators
  --selftest-validation  check the sampling design, strata, sample and accuracy
                      estimators (Olofsson et al. 2014) and, with the runtime,
                      the estimates against zeit.accuracy
  --shots TOUR IN OUT  a website guide's screenshots (PNGs in OUT), in a hidden
                      window: validation (IN: tools/make_tutorial_data.py's
                      data), landtrendr, ccdc, coded, bfast, phenology (IN: the
                      series)
  --selftest-ui A B [C [D [E [F [G]]]]]  drive the layers workflow in a hidden window
                      (A, then B as a layer; C: several bands per date;
                      D, E: categorical, with and without a colour table;
                      F: B in another CRS, reprojected over it; G: spectra,
                      ENVI and an EnMAP product; then a polygonal ROI,
                      vector layers, the interpretation, an animation and a
                      validation project on D; the basemap is checked with
                      local tiles, no network)
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
`devenv.com`).

## Using the viewer

| Action | How |
|---|---|
| Remote rasters (cloud) | **File → Open URL...** (`Ctrl+U`; *Add layer: URL...* adds one): one URL per line, several make a series. `https://`, `s3://`, `gs://`, `az://`: cloud-optimized GeoTIFFs (AWS Earth Search, Planetary Computer with a signed URL, USGS, ESA...) are read in place through GDAL's network file systems: only the header, the overview level the map needs and the blocks looked at (the cursor's pixel, the detail tiles) are downloaded. Public S3 and GCS buckets need no account; private ones use the usual credentials (`AWS_ACCESS_KEY_ID`, `~/.aws`, `GOOGLE_APPLICATION_CREDENTIALS`, `AZURE_STORAGE_CONNECTION_STRING`...). Dates come from the file names, or from the folder each file sits in (Sentinel-2 COGs: `.../S2B_23KPQ_20230811_0_L2A/B04.tif`). The overview is cached on disk like any other |
| NetCDF, Zarr, HDF | open the file (or the `.zarr` folder): with several variables a list asks which one (name, shape, long name and units); the time axis is decoded from CF units (`days since 1970-01-01`, hours, months...) and calendars (`noleap`, `360_day`, `all_leap`...: a model's 30 February becomes the 28th/29th); hourly data are labelled with the hour. **Display** panel → *Variable*: another variable of the file, *level*: another value of the other dimensions (pressure level, depth, member); both reopen the layer in place. A lon/lat grid without a grid mapping is taken as WGS 84 |
| Pan / zoom | drag / mouse wheel (zooming out stops at the image extent), `F` fits the map; trackpad: two fingers pan, pinch zooms |
| Pixel series | hover (exact once the mouse rests) |
| Compare pixels | click to drop a pin |
| Remove a pin | right click it (Mac: Control + click, or a two-finger click on the trackpad), `Delete` (last one) or the **x** in the statistics table |
| ROI | `Shift` + drag: rectangular ROI, mean and p10–p90 per date (or a box plot per date: the *ROI* list above the chart); with *All visible layers* on the chart, also on every visible layer (its own pixels under the rectangle, reprojected if needed) |
| Polygonal ROI | `R` (or View → Draw ROI polygon), then click the vertices and close it on the first one, with a double click or `Enter` (`Backspace` undoes a vertex, `Esc` cancels); or, after `R`, drag a freehand outline (lasso), closed when you let go. The statistics, the local PCA of embeddings, the similarity to the ROI and the scatter use the pixels whose centres are inside; a Zeit run on the ROI covers its bounding box. **Alt + click** a polygon of a vector layer (holes included) to make it the ROI; View → Clear ROI |
| Vector layers | File → Add vector layer..., the **Layers** panel (*Add vector layer...*), a drop or the command line: GeoJSON, Shapefile, GeoPackage (each of its layers), KML, FlatGeobuf, GML, GPX or a CSV of points; listed on top of the **Layers** panel: show/hide, colour, remove; right click → Zoom to it, Extract series, Interpret. Hovering a point shows its attributes (lines and polygons: hold `Alt`); see [Vector layers](#vector-layers) |
| Series at points or polygons | a vector layer → **Extract series...**: the active layer or every visible one, a row per feature (a column per date) or a row per feature and date; CSV |
| Interpretation (labelling) | a vector layer → **Interpret...** (the **Interpretation** panel): `N` / `Shift+N` (or Page Down / Page Up) next / previous feature, `1`–`9` its label, `0` clears it, `D` the date of the change (the date on the map), `Shift+D` none; notes; *Export table...* |
| Animation | File → Export animation (`Ctrl+Shift+A`): dates (from, to, step), frame rate, resolution, GIF / MP4 / WebM, date stamp, legend, scale bar, pins, ROI and vector layers; see [Animation](#animation-gif-mp4-webm) |
| Time | `←`/`→` previous/next date, `P` play/pause, click or drag on the chart |
| Map mode, colormap, range | **Display** panel, for the active layer (range is automatic 2–98%, or drag it) |
| Change between dates | **Display** panel → *Difference (value - reference)*: pick the *Reference* (a date, or *Previous date (t-1)*); diverging colours centered at 0, follows the time bar |
| Temporal composite | **Display** panel → *Temporal composite*: median, mean, maximum, minimum or number of observations of each pixel over a window: the shown date's year (the arrows and playback then go year by year), some days around the date (a moving window) or the whole series; with the season, only its dates. Computed in the map shader over the overview's dates each time the map is drawn (the median of up to 64 values: every k-th one beyond) |
| Season | **Display** panel → Season → *Only part of the year*: a window of days of the year (presets by months: DJF, JJA, Apr–Sep...; a start after the end wraps over the new year and belongs to the year it ends in). The dates outside it are left out of the temporal statistics (mean, trend, std...), the composites, playback and the arrow keys; the chart shades the season. Applies to every layer with dates |
| Largest drop | **Display** panel → *Largest drop: date* or *magnitude*: the largest decrease between consecutive valid observations (no-data dates skipped), dated at the lower one; the colour bar shows dates. Exact values for the cursor and pins in the **Statistics** panel |
| Classes (categorical data) | **Display** panel → *Categorical (classes)*: legend with colours, names and shares (click a colour to change it, untick a class to hide it); detection can be switched off or forced |
| Band, index, cloud mask | **Display** panel → Bands (one file per date with several bands): band A, optional normalized difference with B, quality band; **Apply** reopens the layer in place |
| RGB of three bands | **Display** panel → RGB composite (one file per date with several bands): *Three bands of each date as RGB*, a preset (true colour, colour infrared, SWIR, agriculture, land/water: bands found by their names or wavelengths) or any three bands, the map's date or a fixed one, the stretch of each channel (2–98% of the first image, kept for every date; *Stretch to this image*). The chart, the cursor and the tools keep the series' band or index; zoomed in, the visible area is read again at full resolution; map panels, the swipe, PNGs and animations show it too |
| Python of a series | Time series panel → **Copy Python** → the cursor, a pin or the ROI: code (numpy, rasterio) that reads the same series from the files (band, normalized difference, quality mask; the ROI's pixels inside it), prints its statistics and, with a Zeit tool on the chart, makes the same Zeit calls with the same parameters (as the tool made them: recorded, not rewritten) |
| Reset layout | **View → Reset layout** (`Ctrl+Shift+R`, Mac: `Command+Shift+R`): every panel docked where it was at the first start, the floating windows (Exports, Tasks, Log, Settings) back to their first size and place, map panels to the right of the main map; the panels open stay open |
| Performance panel | **View → Performance** (hidden by default): timings, threads in use, Zeit status, overview memory, full-resolution cache (progress, size, read times, **Build it now**) |
| Settings | **File → Settings...** (`Ctrl+,`, Mac: `Command+,`): interface theme (Dark, Light, Classic, Janus, Studio, Graphite), font size, whether the Files panel follows what is opened, processing threads, overview memory, full-resolution cache (when it is built, its budget) and clearing the caches; kept between sessions (see [Settings](#settings)) |
| Several series | **Layers** panel or File → Add layer (`Ctrl+L`): show/hide (`Space`: the active layer), order, opacity, close; click a name to make it active. Layers in another CRS show "reprojected from EPSG:…" (hover for the grid size and its error) |
| Basemap | **Layers** panel → Basemap (below the layers): pick a source (None = nothing downloaded), tick to show/hide, opacity; `S` shows or hides satellite imagery (Esri World Imagery, or the imagery source picked); *Custom XYZ URL*: the URL (`{z}`, `{x}`, `{y}`; `{-y}` for TMS rows; applied when you leave the field), the attribution to show, the finest zoom and the tile size. The source and its settings are kept between sessions, but it starts hidden, as does every series opened (tick to show); the map panels' and the swipe's layer list has *Basemap only* |
| Browse files | **Files** panel: double click opens, right click → Add as layer or Add to favorites; Ctrl+click selects several files; hover a folder for what it would open as. Opening a series expands the tree down to it (Settings → *Files panel follows what is opened*) |
| Add by dropping | drop files or a folder on the window with `Shift` held: a new layer (without Shift the drop replaces the series) |
| Seasonal views, classes over time, scatter | **View → Analysis** (see above) |
| Chart of several layers | Time series panel → *All visible layers* (one marker shape per layer) |
| Second monitor | drag a panel's tab out of the main window |
| Maps side by side | View → New map view (`Ctrl+T`): pick the layer and, if wanted, its own date and mode in the panel's bar; every panel follows the same pan/zoom; close it with its tab's **x** or `Ctrl+W` (the focused panel, else the last one opened) |
| Swipe | View → Swipe (`C`): drag the divider (white line with a handle); the bar above the map picks what is right of it: a layer and, if wanted, its own date and mode, or *Basemap only*; **Swipe off** or `C` again closes it |
| Space-time transect | `Ctrl` + drag a line on the map (Mac: `Command` + drag), or `T` / View → Draw transect, then drag; a **polyline** (a river, a road, a valley): `T`, then click its vertices, a double click or `Enter` ends it (`Backspace` undoes a vertex); **Alt + click** a line of a vector layer: the transect along it; `Esc` cancels. **Transect** panel: distance from A along the line (X; dashed verticals at the bends; no data where a polyline leaves the layer) × dates (Y, oldest on top); hover a cell = distance, date, value, marked on the map; click = go to that date; Values / Anomaly (− each place's mean); Copy CSV (a row per date, a column per sample); Clear, or close the panel |
| Chart options | style, values/anomaly/z-score, trend (OLS/Sen), Y = map range |
| Map as a figure | File → Export map as PNG... (`Ctrl+E`): resolution (1×, 2×, 4×), background, date label, legend, scale bar, pins, ROI and vector layers; *Save to*: the file (a path, a name alone for the last export folder, or **Browse...**), then **Export** (or `Enter`) |
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
that Janus exposes (step-by-step guides with screenshots on the website:
[LandTrendr](https://sacridini.github.io/janus/landtrendr.html),
[CCDC](https://sacridini.github.io/janus/ccdc.html),
[CODED and the NDFI](https://sacridini.github.io/janus/coded.html),
[BFAST](https://sacridini.github.io/janus/bfast.html),
[Phenology](https://sacridini.github.io/janus/phenology.html)):

| Tool | Series | Maps |
|---|---|---|
| LandTrendr | annual (one date per year) | year of detection, magnitude, duration, pre/post value, rate, DSNR of the chosen loss/gain event |
| Mann-Kendall | any (seasonal variant: several dates per year) | significant Sen's slope, slope, p-value, Z, tau, trend class, intercept |
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
- **Slow files**: a compressed GeoTIFF with every band in each block
  (pixel-interleaved, `gdal_translate`'s default) is read band by band about 70
  times slower; the series description says so. `gdal_translate -co
  INTERLEAVE=BAND` makes it fast.
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

## Vector layers

Field plots, samples of a map's accuracy assessment, forest inventory plots,
parcels or burned areas usually come as vector files. File → **Add vector
layer...** (or the **Layers** panel, a drop, the Files panel, the command line)
reads them through OGR, which comes with GDAL: GeoJSON, Shapefile, GeoPackage
(each of its layers becomes a vector layer), KML/KMZ, FlatGeobuf, GML, GPX and
CSV tables of points (columns named `x`/`lon`/`longitude`/`easting` and
`y`/`lat`/`latitude`/`northing`; without a CRS, coordinates within ±180/±90 are
read as longitude and latitude).

- **On the map**: drawn over every raster layer, on every map panel and the
  swipe, in any CRS (placed on the active layer's grid through PROJ, again
  whenever the active layer changes to another grid). Points are dots; lines
  and polygons (with their holes) are outlines, thinned to about one vertex
  per screen point. Each layer has a colour, can be hidden or removed; right
  click its name for *Zoom to it*.
- **Attributes**: hovering a point shows its fields; for lines and polygons
  hold `Alt`. The status bar names the feature under the cursor.
- **A polygon as the ROI**: `Alt` + click it. Its holes are left out; the
  statistics, box plots, embeddings' local PCA and the scatter use the pixels
  whose centres are inside, on every layer.
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
  - **Chips** (Validation → Chips, or *Chips* in the panel): the feature at
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

The **Validation** menu turns a map — a Zeit result (LandTrendr's year of
detection, CCDC's breaks, a TWDTW or SOM class map...), a categorical layer at
a date (MapBiomas), or any raster file — into a stratified random sample,
interpreted point by point, and estimates of the map's accuracy and of the
**error-adjusted area** of each class, with confidence intervals, following
the good practices of Olofsson et al. (2014). A step-by-step guide with
screenshots, from the strata to the report, is on the website:
[Validating a map, step by step](https://sacridini.github.io/janus/validation.html).

**Design** (Validation → New sampling design; the **Validation** panel):

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
design, the maps assessed, the translations). Validation → Open project...
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
and the references) and a CSV of every table. *Copy Python* gives the
`zeit.accuracy` code that computes the same estimates from the project's
files.

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
| Theme | **Dark** (the default), **Light**, **Classic** (ImGui's original colours), **Janus** (the program's colours: blue on navy panels, orange accents), **Studio** (mid grey with dark text and dark chart displays) **Graphite** (dark grey with a yellow accent) or **Matrix** (phosphor green on black). Status text and chart series are adjusted to stay legible on each; the map, its colour bars and exported figures keep their colours |
| Panels' background | How opaque the panels' backgrounds are, 10 to 100% (100% by default): below 100% the desktop (or whatever is behind the window) shows through the panels, the menus and the map's empty area, while text, chart lines, controls and the map's data stay opaque; in every window, the detached panels' included. Contrast is kept on the theme's own colours |
| Whole window | The opacity of the whole window, 20 to 100% (the system's window transparency: text and map included), and of every detached panel |
| Font size | Size of the interface text, 10 to 28 px (13 by default: ImGui's pixel font; other sizes use its scalable font). Widgets keep their widths; exported figures keep their text as at 13 px |
| Files panel follows what is opened | On by default: opening a series, a file or a folder (from anywhere) expands the Files panel down to it and scrolls there; its folder is listed again, so new files show up |
| Processing threads | How many CPU threads Janus and Zeit may use, shown as *N of M* logical cores; default: all but 2 (at least 1), so the computer stays responsive during long runs. Applies to the readers of the open series (they shrink or grow at once), the full-resolution cache, exports (GeoTIFF compression, PNG) and Zeit: raster jobs started from then on (as `OMP_NUM_THREADS`, `NUMBA_NUM_THREADS`, the BLAS limits and Zeit's `n_jobs`; jobs already running keep theirs) and the process that fits the chart and estimates run times (replaced in the background once idle) |
| Overview memory | Memory for the cube overview (1024 MB by default); **Apply** reopens the active layer. `--budget` overrides it for one run |
| Clear overview cache | Deletes the cached overviews except the active layer's |
| Full-resolution cache | When it is built (on demand, series on an HDD, every series), its budget, and clearing it (see [Performance](#performance)) |
| Interpreter | The name your labels of a validation project are kept under (by default your login), see [Validation](#validation-accuracy-and-area-olofsson-et-al-2014) |
| Video export (ffmpeg) | The `ffmpeg` executable used for MP4 and WebM animations (**Browse...**); empty: one next to Janus, else the one on PATH (see [Animation](#animation-gif-mp4-webm)) |
