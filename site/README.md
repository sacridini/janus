# Janus website

Static site published to GitHub Pages by `.github/workflows/pages.yml`
(https://sacridini.github.io/janus/). No build step: edit `index.html`,
`assets/style.css` and `assets/main.js` directly.

- The download buttons point to the latest release page and, when the GitHub API
  answers, to the exact installer of the latest release (`assets/main.js`).
- Logos, colours and fonts come from `../branding/`.
- To preview locally: `python -m http.server -d site` and open http://localhost:8000.
