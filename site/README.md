# Janus website

Static site published to GitHub Pages by `.github/workflows/pages.yml`
(https://sacridini.github.io/janus/). No build step: edit `index.html`,
`assets/style.css` and `assets/main.js` directly.

- The download buttons point to the latest release page and, when the GitHub API
  answers, to the exact installer of the latest release (`assets/main.js`).
- Logos, colours and fonts come from `../branding/`.
- To preview locally: `python -m http.server -d site` and open http://localhost:8000.

## The guides

`validation.html`, `landtrendr.html`, `ccdc.html`, `coded.html`, `bfast.html`,
`phenology.html`, `snic.html`, `deep-learning.html`, `presto.html`,
`trend-test.html`, `calculator.html`, `stac.html`, `series.html`,
`maps-to-numbers.html`, `workspace.html` and `visualize.html` walk
through a tool each, with screenshots in
`assets/img/<guide>/` (and a card each in the Guides section of `index.html`).
The screenshots come from the program itself: `janus --shots TOUR IN OUT`
drives the guide's steps in a window of 1680 x 1050 and saves the window and
some panels as PNGs (`crates/janus-app/src/shots.rs`; the validation's tour is
in `crates/janus-app/src/shots_validation.rs`, those of SNIC and deep learning in
`crates/janus-app/src/shots_ml.rs`), so they can be made again after
the interface changes.

| Guide | Data (`IN`) | Prepared with |
|---|---|---|
| validation | Sentinel-2, Global Forest Change, MapBiomas near Novo Progresso (~60 MB, public) | `python tools/make_tutorial_data.py data` |
| landtrendr | an annual NDVI stack of Rondônia, 1985–2024, as a VRT with 0 as no data | `python tools/make_guide_data.py annual STACK out.vrt` |
| ccdc, coded | Landsat 8/9 Collection 2 Level-2 of Rondônia, 2020–2026, a GeoTIFF per date (from a Google Earth Engine stack and its CSV of dates) | `python tools/make_guide_data.py landsat STACK DATES folder WEST SOUTH EAST NORTH` (-63.36 -10.37 -63.06 -10.08) |
| bfast, phenology | MODIS EVI every 16 days, 2001–2025, of western France (one GeoTIFF, a band per date) | used as it is |
| snic, ml, presto | the Landsat folder of ccdc; for ml and presto, labels from MapBiomas 2022 (collection 8, Amazon) in a folder `labels` next to it | `python tools/make_guide_data.py labels MAPBIOMAS.tif LANDSAT/ls_2020-02-04.tif labels` |
| trend-test | 41 annual Landsat medoid composites of Sergipe and Alagoas, 1985–2025 (`landsat_medoid_YYYY.tif`, EPSG:4326, 0 over the sea with no nodata declared) | `python tools/make_parts_guide.py data SERIES out` (VRTs with 0 as no data in `out/series`, the zones by distance to the sea in `out/zones.tif`) |

The data of the tool guides are the author's own stacks, not downloaded.

    janus --shots validation data out     # the report goes to out/work/report

The whole-window shots are converted to JPEG (`gdal_translate -of JPEG -co
QUALITY=90`); the panels are copied as PNG. For validation,
`16-report-html.png` is `out/work/report/report.html` in a browser at
1000 px, and the numbers quoted in the page come from that run
(`out/work/report/*.csv`); the other guides quote the Statistics table and the
legends of their screenshots.

The trend test's screenshots are taken with `--screenshot` rather than a tour
(the copy of `janus` run from `out`; the window 1600 x 950):

    janus out/series --interface modern --zeit ar_trend --pin 1426,1199 --view 1426,1150,1.5 --screenshot ar_pixel.png
    janus out/series --interface modern --zeit ar_trend --pin 1426,1199 --view 1426,1150,1.5 --detach-map --screenshot ar_pixel_d.png
    janus out/series --interface modern --zeit ar_trend --zeit-run --roi 1000,500,1700,1200 --view 1350,850,0.6 --screenshot ar_map.png
    janus out/series --interface modern --zeit ar_trend --roi 1000,500,1700,1200 --view 1350,850,0.6 --detach-map --screenshot ar_roi_d.png
    janus out/series --interface modern --roi 1000,500,1700,1200 --view 1350,850,0.6 --analysis trend,run --screenshot roi.png
    janus out/series --layer out/zones.tif --interface modern --analysis trend,run,classes --screenshot classes.png
    janus out/series --layer out/zones.tif --interface modern --detach-map --analysis trend,run,classes --screenshot classes_d.png

`02-ar-pixel.jpg`, `03-ar-map.jpg`, `04-test.jpg` and `08-classes.jpg` are
`ar_pixel.png`, `ar_map.png`, `roi.png` and `classes.png` as JPEG (quality 90);
the panels are crops (x0, y0, x1, y1) of the others: `02-ar-stats.png` of
`ar_pixel_d.png` (1172, 38, 1470, 495), `02-ar-tool.png` of `ar_pixel_d.png`
(15, 15, 457, 556), `03-roi-mean-stats.png` of `ar_roi_d.png` (1172, 38, 1470,
495), `04-test-panel.png` of `roi.png` (343, 655, 1165, 930) and
`08-classes-table.png` of `classes_d.png` (343, 70, 1165, 418). The figures
that explain the method (`01-problem.png`, `05-correlation.png`,
`06-partitions.png`, `07-calibration.png`) are made by
`python tools/make_parts_guide.py figures site/assets/img/trend-test`, which
runs Zeit's `ar_trend` and `parts` on simulated maps (about 3 minutes); the
numbers the page quotes from Zeit (the ROI's correlation plot, the partition
sizes) come from Zeit's documentation, `docs/scripts/make_parts_figures.py`
there.

The raster calculator's screenshots are taken with `--screenshot` too, on the
Sentinel-2 series of the STAC guide downloaded as GeoTIFFs (Planetary
Computer, Sentinel-2 L2A, 63.00 to 62.92 W, 10.00 to 9.92 S, 2019-06-01 to
2024-09-30, 5% of clouds at most: 107 dates, `s2ro`), with `--pin 776,270
--pin 128,548`:

    janus s2ro --interface modern --expr EVI --pin ... --screenshot index.png
    janus s2ro --interface modern --pin ... --calc $'# how many of the 107 dates had little vegetation\nndvi = (N - R) / (N + R)\ncount(ndvi < 0.4)' --calc-run --screenshot calculator.png
    janus s2ro --interface modern --calc $'# the moisture index of the dry seasons\nndmi = (N - swir) / (N + swir)\nmedian(ndmi)' --screenshot editor.png
    janus s2ro --interface modern --calc $'# the median NDVI of the five years, in three classes\nm = median(nd(N, R))\nresult = m\nm < 0.45 = 1\nm >= 0.45 && m < 0.7 = 2\nm >= 0.7 = 3' --calc-run --screenshot rules_w.png
    janus s2ro --layer RESULTS/calc/count_ndvi_*/result.vrt --interface modern --pin ... --screenshot count.png
    janus s2ro --layer RESULTS/calc/calc_*/result.vrt --interface modern --pin ... --screenshot rules.png

(`RESULTS` is `%LOCALAPPDATA%\Janus\results`.) `01-index.jpg`,
`02-calculator.jpg`, `04-count.jpg` and `05-rules.jpg` are `index.png`,
`calculator.png`, `count.png` and `rules.png` as JPEG (quality 90);
`03-editor.png` is the window of `editor.png` (17, 17, 995, 613).

`--window NAME,...` opens a tool's window once the layers are open, with its
defaults: regularize, speckle, warp, build, save, polygonize (the active
series), rasterize and zonal (the first vector layer), areas (Analysis >
Areas). `NAME-run` also runs it, and the screenshot waits for its result.
`--analysis areas|zonal` shows those tabs. Screenshots are RGBA: convert them
with `gdal_translate -of JPEG -b 1 -b 2 -b 3 -co QUALITY=90`, because without
`-b` GDAL writes a 4-band JPEG that is read as CMYK.

The workspace guide's screenshots are taken with `--screenshot` (window 1600 ×
950, `--threads 2`). The data is the trend test's medoid series, as VRTs with
0 as no data, in a folder named `215_067`:

    gdal_translate -of VRT -a_nodata 0 landsat_medoid_YYYY.tif 215_067/landsat_medoid_YYYY.vrt   (each year)
    janus 215_067 --interface modern --zeit ar_trend --zeit-run --roi 1000,500,1700,1200 --view 1350,850,0.6 --screenshot run.png
    janus 215_067 --interface modern --palette go --screenshot palette_go.png
    janus STAC --interface modern --palette burn --screenshot palette_burn.png
    janus 215_067 --interface modern --basemap esri --date 41 --blend multiply --view 6700,4250,1 --screenshot multiply.png
    janus 215_067 --interface modern --date 41 --minimap --view 6804,4252,1 --goto "10°17'S 36°35'W" --screenshot goto.png
    janus 215_067 --interface modern --date 28 --lens --view 6600,4250,1 --screenshot lens.png

`STAC` is the STAC guide's Sentinel-2 series read in place
(`RESULTS/stac/sentinel-2-l2a_2019-06-05_2024-09-11_*`). `01-run.jpg`,
`05-multiply.jpg`, `06-goto.jpg` and `07-lens.jpg` are `run.png`,
`multiply.png`, `goto.png` and `lens.png` as JPEG; `02-notice.png` is a crop of
`run.png` (1160, 810, 1600, 950), `03-palette.png` of `palette_go.png` and
`04-palette-indices.png` of `palette_burn.png` (480, 60, 1140, 395). The
provenance excerpt is the run's `provenance.json`; its `rerun.py`, run with
`n_jobs=2`, gave the same slope, p-value, t, standard error, intercept and rho
on all 490 000 pixels.

The maps-to-numbers guide's data is in `target/guide-maps` (`--threads 2`):
`landsat/` is the 41 medoids cut to 2000 × 2000 px
(`gdal_translate -of VRT -srcwin 4100 5200 2000 2000 -a_nodata 0`);
`mapbiomas/` is MapBiomas collection 4.1 (Atlantic Forest, 1985–2018) cut to a
box 200 px wider (`-srcwin 67815 20824 2400 2319 -a_nodata 0`), the class names
and colours set with GDAL's Python; `mapbiomas_grid/` is the years warped onto
the Landsat grid with the arguments of Reproject's *The grid of a layer*
(`gdalwarp -t_srs EPSG:4326 -te -37.3120643150748 -11.077844420705116
-36.773075144603084 -10.538855250233404 -ts 2000 2000 -r mode -of COG ...`; 0
pixels differ from the source), the names written back as `.aux.xml`;
`poly/forest_1985.gpkg` is the 1985 polygons with `value = 3 AND area_ha >=
100` (49 features, a `patch` column):

    janus landsat --layer mapbiomas --interface modern --window warp --screenshot warp.png
    janus landsat --layer mapbiomas_grid --interface modern --date 34 --pin 71,793 --pin 65,1038 --view 260,890,1.5 --screenshot grid.png
    janus landsat --layer mapbiomas_grid --interface modern --date 34 --window areas-run --analysis areas --detach-map --screenshot areas.png
    janus landsat --layer mapbiomas_grid --layer poly/mapbiomas_1985_polygons.gpkg --interface modern --date 1 --window polygonize --pin 71,793 --view 160,800,3 --screenshot poly.png
    janus mapbiomas_grid --layer landsat --layer poly/forest_1985.gpkg --interface modern --window rasterize-run --screenshot rasterize.png
    janus poly/forest_1985_mask_landsat.tif --layer landsat --interface modern --zeit mann_kendall --screenshot only.png
    janus landsat --layer mapbiomas_grid --layer poly/forest_1985.gpkg --interface modern --date 34 --window zonal-run --analysis zonal --screenshot zonal.png
    janus mapbiomas_grid --layer landsat --layer poly/forest_1985.gpkg --interface modern --window zonal-run --analysis zonal
    janus mapbiomas_grid --layer landsat --layer poly/forest_1985.gpkg --interface modern --date 34 --pin 71,793 --pin 65,1038 --view 200,880,1.5 --screenshot index.png

`01-warp.png` is a crop of `warp.png` (17, 18, 495, 439) and
`06-only-where.png` of `only.png` (15, 478, 467, 684); the others are the PNGs
as JPEG. The numbers come from the runs' `areas.csv`, `zonal_long.csv` and
logs.

The series guide's screenshots (`series.html`, and the STAC guide's
`04-*.jpg`) come from `--screenshot`, and where a file dialog or a combo box
has to be set, from a run driven through Windows UI Automation (egui's
accesskit) and captured with PrintWindow (`target/guide-series/gui.ps1`), with
`LOCALAPPDATA=target/guide-series/appdata` so the user's settings were not
touched, `--threads 2`, window 1600 × 950 and `--pin 195,837 --pin 179,761`
(the STAC guide's forest and pasture). `s2ro` is the STAC guide's series as
GeoTIFFs; `loose/` is 36 files cut from six of its 2024 dates, two tiles and
three bands named as in an L2A product
(`T20LMP_<datatake>_B04_10m.tif`, `..._B08_10m.tif`, `..._SCL_20m.tif`, the SCL
at 20 m with `-tr 20 20 -r mode`); the Sentinel-1 series is
`target/bench-s1/linear` (tools/bench_s1_vs_stackstac.py).

    janus s2ro --interface modern --pin ... --view 438,443,0.549 --window build        (Pick a folder: loose; 01-build)
    janus RESULTS/series/loose_* --interface modern --pin ... --date 6 --screenshot built.png
    janus s2ro --interface modern --pin ... --view 438,443,0.549 --window regularize   (Whittaker; 03-regularize; Run)
    janus s2ro --layer RESULTS/regularize/s2ro_regular_* --interface modern --pin ... --chart layers=1 --screenshot regular.png
    janus s2ro --interface modern --pin ... --roi 100,650,450,886 --view 438,443,0.549 --window save   (NetCDF, every band, ROI; 05-save)
    janus bench-s1/linear --interface modern --expr VHdB --pin ... --date 15 --window speckle --screenshot speckle.png
    janus bench-s1/linear --interface modern --expr VHdB --pin ... --date 15 --window speckle-run --swipe --view 330,700,1.6   (07-speckle-swipe = stac/04-speckle)
    janus bench-s1/linear --interface modern --expr VVdB --pin ... --date 15 --screenshot radar_vvdb.png   (stac/04-radar-db)

The timings quoted come from the runs' `job.log` on 2 threads; the benchmark
tables from `bench/series-io.json`, `bench/regularize-vs-zeit.json` and
`bench/s1-vs-stackstac.json` (Janus on 8 threads).

The visual analyses' screenshots (`visualize.html`, `assets/img/visualize/`)
come from `--screenshot` on 4 threads, window 1600 × 950: the Sentinel-2
series `s2ro` above with the NDVI and the series guide's pins, and the Landsat
series of the Maps to numbers guide (`target/guide-maps/landsat`) at 2018:

    janus s2ro --interface modern --threads 4 --expr NDVI --pin 195,837 --pin 179,761 --mode similarity,pin --screenshot similarity.png
    janus s2ro --interface modern --threads 4 --expr NDVI --pin 179,761 --mode similarity,pin --screenshot sim_values.png
    janus s2ro --interface modern --threads 4 --expr NDVI --pin 179,761 --mode similarity,pin,shape --screenshot sim_shape.png
    janus s2ro --interface modern --threads 4 --expr NDVI --pin 195,837 --pin 179,761 --date 60 --detach-map --analysis cycles --screenshot cycles.png
    janus s2ro --interface modern --threads 4 --expr NDVI --pin 195,837 --pin 179,761 --date 60 --detach-map --analysis bands --screenshot bands.png
    janus landsat --interface modern --threads 4 --pin 71,793 --pin 65,1038 --date 34 --analysis atlas,groups --screenshot atlas.png

With `--detach-map` the Analysis panel takes the main window; those
screenshots are cropped to the panel (340, 60, 1168, 925). The two similarity
maps to pin 2 are cropped to the map (724, 103, 1210, 590) and put side by
side (`01b-similarity-measures.jpg`). All are JPEG at
quality 88. The timings of the Speed section were measured on the 215/067
scene of the Trend test guide.
