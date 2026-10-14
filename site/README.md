# Janus website

Static site published to GitHub Pages by `.github/workflows/pages.yml`
(https://sacridini.github.io/janus/). No build step: edit `index.html`,
`assets/style.css` and `assets/main.js` directly.

- The download buttons point to the latest release page and, when the GitHub API
  answers, to the exact installer of the latest release (`assets/main.js`).
- Logos, colours and fonts come from `../branding/`.
- To preview locally: `python -m http.server -d site` and open http://localhost:8000.

## The validation guide

`validation.html` walks through the Validation tools with screenshots in
`assets/img/validation/`. They come from the program itself, on real data, so
they can be made again after the interface changes:

    python tools/make_tutorial_data.py data          # Sentinel-2, Global Forest Change, MapBiomas (~60 MB)
    janus --shots-validation data out                # PNGs of every step in out/, the report in out/work/report

The full-window shots with imagery (01, 05, 11) are converted to JPEG
(`gdal_translate -of JPEG -co QUALITY=90`); the others are copied as PNG.
`16-report-html.png` is `out/work/report/report.html` in a browser at 1000 px.
The numbers quoted in the page come from that run (`out/work/report/*.csv`).
