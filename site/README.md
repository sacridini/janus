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
`phenology.html`, `snic.html`, `deep-learning.html`, `presto.html` and
`trend-test.html` walk
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
