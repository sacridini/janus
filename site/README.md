# Janus website

Static site published to GitHub Pages by `.github/workflows/pages.yml`
(https://sacridini.github.io/janus/). No build step: edit `index.html`,
`assets/style.css` and `assets/main.js` directly.

- The download buttons point to the latest release page and, when the GitHub API
  answers, to the exact installer of the latest release (`assets/main.js`).
- Logos, colours and fonts come from `../branding/`.
- To preview locally: `python -m http.server -d site` and open http://localhost:8000.

## The guides

`validation.html`, `landtrendr.html`, `ccdc.html`, `coded.html`, `bfast.html`
and `phenology.html` walk through a tool each, with screenshots in
`assets/img/<guide>/` (and a card each in the Guides section of `index.html`).
The screenshots come from the program itself: `janus --shots TOUR IN OUT`
drives the guide's steps in a window of 1680 x 1050 and saves the window and
some panels as PNGs (`crates/janus-app/src/shots.rs`; the validation's tour is
in `crates/janus-app/src/shots_validation.rs`), so they can be made again after
the interface changes.

| Guide | Data (`IN`) | Prepared with |
|---|---|---|
| validation | Sentinel-2, Global Forest Change, MapBiomas near Novo Progresso (~60 MB, public) | `python tools/make_tutorial_data.py data` |
| landtrendr | an annual NDVI stack of Rondônia, 1985–2024, as a VRT with 0 as no data | `python tools/make_guide_data.py annual STACK out.vrt` |
| ccdc, coded | Landsat 8/9 Collection 2 Level-2 of Rondônia, 2020–2026, a GeoTIFF per date (from a Google Earth Engine stack and its CSV of dates) | `python tools/make_guide_data.py landsat STACK DATES folder WEST SOUTH EAST NORTH` (-63.36 -10.37 -63.06 -10.08) |
| bfast, phenology | MODIS EVI every 16 days, 2001–2025, of western France (one GeoTIFF, a band per date) | used as it is |

The data of the tool guides are the author's own stacks, not downloaded.

    janus --shots validation data out     # the report goes to out/work/report

The whole-window shots are converted to JPEG (`gdal_translate -of JPEG -co
QUALITY=90`); the panels are copied as PNG. For validation,
`16-report-html.png` is `out/work/report/report.html` in a browser at
1000 px, and the numbers quoted in the page come from that run
(`out/work/report/*.csv`); the other guides quote the Statistics table and the
legends of their screenshots.
