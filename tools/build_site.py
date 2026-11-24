#!/usr/bin/env python3
"""Regenerate the repeated parts of the static site in site/.

In every site/*.html, the text between `<!-- build:NAME -->` and
`<!-- /build:NAME -->` is rewritten (the markers stay). The blocks are:

  header, footer   partials in tools/site_partials/, with {{placeholders}}; the
                   menu marks Methods (methods.html) or Guides (guides.html and
                   every guide page) as the current page
  home-methods     index.html: four shortcut cards into the chooser of methods.html
                   (the count and names of the methods that match, by the same
                   rule as assets/methods.js) and the link to all of them
  home-guides      index.html: the tracks with their guide counts and the featured
                   guides ("featured": true in site/data/guides.json) with their
                   thumbnails
  cite-bibtex      index.html: the BibTeX entry, with the version and the year of
                   CITATION.cff
  method-catalog   methods.html: the chooser (chips, search, count) and the
                   catalog of every method in site/data/methods.json, grouped by
                   "group"; assets/methods.js only hides and shows its cards
  guide-tracks     guides.html: the guides by "track" of site/data/guides.json,
                   with facts computed from the guide pages
  python-tabs      a guide page, where its tool is fitted on the chart (also python-tabs-NAME:
                   a second block on the page, the key of site/data/python.json being
                   "<slug>-NAME"): the "In Janus" / "In Python" tabs, the steps and the
                   code from site/data/python.json (written by `cargo run -p janus-zeit
                   --example site_python`; this script never needs cargo). Put its
                   markers at column 0: the code must not be indented

Three kinds of page are generated whole (no hand-written text in them; their header and footer are
the same blocks): docs.html (the index) and docs-<name>.html, one per file of docs/ (install, usage,
architecture, building), converted by tools/md.py; and news.html, the releases of the repository's
tags vX.Y.Z (the message of the commit that carries "X.Y.Z: ...", the date of the tagged commit),
read with git into site/data/news.json. Without git or tags the existing news.json is kept, and
entries git does not have stay, so `--check` works in a shallow clone too.

Every <img> of a JPEG in site/assets/img/ that has a .webp next to it (tools/make_webp.py) is wrapped
in <picture><source type="image/webp" srcset="..."><img ...></picture> on every page.

It also writes site/data/search.json, the index of the Ctrl+K search (assets/search.js):
one entry per method, guide, guide section and subsection, docs page and section, release, and
section of the other pages.

Numbers inside a sentence are inline blocks, `<!-- n:NAME -->12<!-- /n:NAME -->`,
rewritten the same way: method-count, method-more (the methods beyond the three
named in the lede), guide-count, track-count, mcp-tools and news-link (the home page's
"What's new in X.Y.Z" link).

The guides' facts are computed from the pages and written back to
site/data/guides.json on every run: "minutes" (words of the page's <article>
over 200, rounded up), "figures" (its <figure> count), "learn" (the first
four entries of its contents that are not generic, such as "What next") and
"thumb" (the first .jpg of its figures, else its first image). The cards show
assets/img/thumbs/<slug>.jpg (tools/make_thumbs.py) when it exists, else that image.

Run it after editing a partial, a data file or a guide:

    python tools/build_site.py           # rewrite the pages
    python tools/build_site.py --check   # exit 1 and list the files that would change

Python 3 standard library only. Files are read and written as UTF-8 and keep
their own line endings (LF or CRLF).
"""

import argparse
import json
import posixpath
import re
import struct
import subprocess
import sys
from html import escape, unescape
from pathlib import Path
from textwrap import indent

import md  # tools/md.py, next to this script

ROOT = Path(__file__).resolve().parent.parent
SITE = ROOT / "site"
PARTIALS = Path(__file__).resolve().parent / "site_partials"

BLOCK = re.compile(
    r"^(?P<ind>[ \t]*)<!-- build:(?P<name>[\w-]+) -->.*?<!-- /build:(?P=name) -->",
    re.S | re.M,
)


INLINE = re.compile(r"<!-- n:(?P<name>[\w-]+) -->.*?<!-- /n:(?P=name) -->")


def fill(template, values):
    for key, value in values.items():
        template = template.replace("{{" + key + "}}", value)
    left = re.findall(r"\{\{\w+\}\}", template)
    if left:
        raise SystemExit(f"unfilled placeholders: {left}")
    return template


def load(name):
    with open(SITE / "data" / name, encoding="utf-8") as f:
        return json.load(f)


def page_values(page):
    """Placeholders of the header. The page marked as current in the menu:
    methods.html is Methods; guides.html and every guide page are Guides; demo.html is Demo;
    docs.html and the docs-*.html pages are Docs; news.html is News;
    index.html links to its own sections and has none."""
    slugs = {g["slug"] for g in load("guides.json")}
    current = ""
    if page == "methods.html":
        current = "methods"
    elif page == "guides.html" or page[: -len(".html")] in slugs:
        current = "guides"
    elif page == "demo.html":
        current = "demo"
    elif page == "docs.html" or page in DOC_PAGES:
        current = "docs"
    elif page == "news.html":
        current = "news"
    mark = ' aria-current="page"'
    values = {
        "current_methods": mark if current == "methods" else "",
        "current_guides": mark if current == "guides" else "",
        "current_demo": mark if current == "demo" else "",
        "current_docs": mark if current == "docs" else "",
        "current_news": mark if current == "news" else "",
    }
    if page == "index.html":
        values.update(base="", brand_href="#top", brand_label="Janus, back to top")
    else:
        values.update(base="./", brand_href="./", brand_label="Janus, home")
    return values


def partial(name, page):
    text = (PARTIALS / f"{name}.html").read_text(encoding="utf-8").replace("\r\n", "\n")
    return fill(text.rstrip("\n"), page_values(page))


# ---- guides: facts computed from the pages themselves

# Contents entries that say nothing about what the reader learns.
GENERIC_TOC = {
    "the idea", "the example", "the examples", "the data", "what next", "references",
    "opening the tool", "speed", "keys", "checked against the originals",
    "against rstoolbox", "against scikit-learn", "two tools", "four views",
}


def plain(html):
    html = re.sub(r"<(script|style)\b.*?</\1>", " ", html, flags=re.S)
    return unescape(re.sub(r"<[^>]+>", " ", html))


def guide_facts(slug):
    text = (SITE / f"{slug}.html").read_text(encoding="utf-8")
    article = re.search(r"<article\b.*?</article>", text, re.S)
    if not article:
        raise SystemExit(f"{slug}.html: no <article>")
    # The collapsed "How these screenshots were made" block is reference, not reading time.
    reading = re.sub(r'<details class="howmade">.*?</details>', " ", article.group(0), flags=re.S)
    reading = PYTABS.sub(" ", reading)  # the steps and the code of the tabs are not reading time either
    words = len(re.findall(r"\S+", plain(reading)))
    figures = len(re.findall(r"<figure\b", article.group(0)))
    toc = re.search(r'<nav class="toc".*?</nav>', text, re.S)
    entries = [
        re.sub(r"\s+", " ", plain(e)).strip()
        for e in re.findall(r'<li><a href="#[^"]*">(.*?)</a>', toc.group(0) if toc else "", re.S)
    ]
    learn = [e for e in entries if e.lower() not in GENERIC_TOC][:4]
    images = []
    for fig in re.findall(r'<figure class="fig[^"]*".*?</figure>', article.group(0), re.S):
        m = re.search(r'<img[^>]*?\ssrc="([^"]+)"', fig)
        if m:
            images.append(m.group(1))
    thumb = next((i for i in images if i.lower().endswith(".jpg")), images[0] if images else None)
    return {"minutes": -(-words // 200), "figures": figures, "learn": learn, "thumb": thumb}


def image_size(rel):
    """(width, height) of a PNG or JPEG of site/, read from its header."""
    data = (SITE / rel).read_bytes()
    if data[:4] == b"\x89PNG":
        return struct.unpack(">II", data[16:24])
    i = 2
    while i + 9 < len(data):
        if data[i] != 0xFF:
            i += 1
            continue
        marker = data[i + 1]
        if 0xC0 <= marker <= 0xCF and marker not in (0xC4, 0xC8, 0xCC):
            h, w = struct.unpack(">HH", data[i + 5 : i + 9])
            return w, h
        i += 2 + struct.unpack(">H", data[i + 2 : i + 4])[0]
    raise SystemExit(f"{rel}: not a PNG or JPEG")


def thumb_img(g):
    """The card's image: the small copy in assets/img/thumbs/ if tools/make_thumbs.py made it."""
    if not g.get("thumb"):
        return ""
    small = f"assets/img/thumbs/{g['slug']}.jpg"
    src = small if (SITE / small).exists() else g["thumb"]
    w, h = image_size(src)
    return f'<img class="g-thumb" src="{src}" alt="{g["title"]}" width="{w}" height="{h}" loading="lazy">'


def guides_data():
    """guides.json with the computed fields filled (minutes, figures, learn)."""
    guides = load("guides.json")
    for g in guides:
        g.update(guide_facts(g["slug"]))
    return guides


def guides_json_update(write):
    """True if guides.json lacks the computed fields; rewrites it if `write`."""
    path = SITE / "data" / "guides.json"
    new = json.dumps(guides_data(), indent=2, ensure_ascii=False) + "\n"
    with open(path, encoding="utf-8", newline="") as f:
        old = f.read()
    if old.replace("\r\n", "\n") == new:
        return False
    if write:
        with open(path, "w", encoding="utf-8", newline="") as f:
            f.write(new.replace("\n", "\r\n") if "\r\n" in old else new)
    return True


TRACKS = [
    ("data", "Get the data", "Find imagery, build a series from it and save it in the formats other tools read."),
    ("explore", "Explore a series", "Look at a series before running anything on it: indices, spectra, cycles, embeddings and the workspace itself."),
    ("change", "Find change and trends", "Run a method on a pixel, then on the whole image, and read its maps."),
    ("classify", "Classify and segment", "Turn a series into classes, fields and segments."),
    ("validate", "Validate and report", "Check a map against a sample, and turn maps into hectares."),
]


def guide_tracks(page):
    guides = guides_data()
    out = ['<div class="wrap">', '  <nav class="g-tracks" aria-label="Tracks">', "    <p>Tracks</p>", "    <ul>"]
    for key, title, _ in TRACKS:
        n = sum(g["track"] == key for g in guides)
        out.append(f'      <li><a href="#{key}">{title}<span>{n}</span></a></li>')
    out += ["    </ul>", "  </nav>", "</div>"]
    for i, (key, title, blurb) in enumerate(TRACKS):
        out.append('<div class="pixrow" aria-hidden="true"></div>')
        out += [
            f'<section class="g-track" id="{key}">',
            '  <div class="wrap">',
            '    <div class="sec-head">',
            f'      <p class="eyebrow">Track {i + 1:02d}</p>',
            f"      <h2>{title}</h2>",
            f"      <p>{blurb}</p>",
            "    </div>",
            '    <ul class="guide-list g-list">',
        ]
        for g in (g for g in guides if g["track"] == key):
            learn = "".join(f"<li>{e}</li>" for e in g["learn"])
            out.append(
                f'      <li><a href="{g["slug"]}.html">{thumb_img(g)}<span>{g["label"]}</span>'
                f'<h3>{g["title"]}</h3><p>{g["summary"]}</p>'
                f'<small class="g-facts">{g["minutes"]} min read · {g["figures"]} figures</small>'
                + (f'<ul class="g-learn" aria-label="You will">{learn}</ul>' if learn else "")
                + "</a></li>"
            )
        out += ["    </ul>", "  </div>", "</section>"]
    return "\n".join(out)


# ---- methods

GROUPS = [
    ("change", "Abrupt change and disturbance", "When and where the land changed suddenly: a clearing, a fire, a harvest, a thinning."),
    ("trend", "Trends and recovery", "Slow change over the years, and how fast a place grew back."),
    ("seasons", "Seasons and phenology", "When the growing season starts, peaks and ends, year by year."),
    ("cover", "Cover and spectra", "What a pixel is made of, from its spectrum, and the clouds that hide it."),
    ("classify", "Classes, clusters and segments", "Maps of classes, from your samples or from the series alone, and the cleaning of them."),
    ("spatial", "Spatial patterns", "Where values cluster, where maps agree and how rough the land is."),
    ("prepare", "Prepare the series", "Smooth, fill, regularize and summarize a series before another method reads it."),
]

DATA_CHIPS = [
    ("annual", "Annual series"),
    ("subannual", "Several dates a year"),
    ("everydate", "Every acquisition, multiband"),
    ("classes", "A class map"),
]
FIND_CHIPS = [
    ("abrupt", "Abrupt change"),
    ("degradation", "Degradation"),
    ("trend", "Trends"),
    ("recovery", "Recovery"),
    ("seasons", "Seasons"),
    ("classes", "Classes"),
    ("clusters", "Clusters"),
    ("spatial", "Spatial patterns"),
    ("prepare", "Prepare the series"),
]
def chip_row(row, label, chips):
    out = [
        f'<div class="m-row" role="group" aria-labelledby="m-l-{row}">',
        f'  <p class="m-label" id="m-l-{row}">{label}</p>',
        '  <div class="m-chips">',
    ]
    out += [
        f'    <button type="button" class="chip" data-row="{row}" data-value="{value}" aria-pressed="false">{text}</button>'
        for value, text in chips
    ]
    return out + ["  </div>", "</div>"]


def method_catalog(page):
    methods = load("methods.json")
    out = ['<div class="wrap">', '  <div class="m-chooser">']
    out += ["    " + line for line in chip_row("data", "My data", DATA_CHIPS)]
    out += ["    " + line for line in chip_row("find", "I want to find", FIND_CHIPS)]
    out += [
        '    <div class="m-bar">',
        '      <div class="m-search"><label class="m-label" for="m-q">Search</label>'
        '<input id="m-q" type="search" placeholder="Name or word" autocomplete="off" spellcheck="false"></div>',
        f'      <p class="m-count" id="m-count" aria-live="polite">{len(methods)} of {len(methods)} methods</p>',
        '      <button type="button" class="chip m-clear" id="m-clear" hidden>Clear</button>',
        "    </div>",
        "  </div>",
        '  <p class="m-empty" id="m-empty" hidden>No method matches. Clear a chip, or the search.</p>',
    ]
    for key, title, blurb in GROUPS:
        group = [m for m in methods if m["group"] == key]
        out += [
            f'  <section class="m-group" id="{key}" data-group>',
            '    <div class="m-head">',
            f"      <h2>{title}</h2>",
            f"      <p>{blurb}</p>",
            "    </div>",
            '    <ul class="m-cards">',
        ]
        for m in group:
            text = f'{m["name"]} {m["summary"]} {m["requires"]}'.lower().replace('"', "")
            guide = f'<a class="m-guide" href="{m["guide"]}.html">Guide &rarr;</a>' if m["guide"] else ""
            out.append(
                f'      <li class="m-card" id="{m["id"]}" data-cadence="{" ".join(m["cadence"])}" '
                f'data-questions="{" ".join(m["questions"])}" data-text="{text}">'
                f'<h3>{m["name"]}</h3><p>{m["summary"]}</p>'
                f'<span class="m-req">{m["requires"]}</span>{guide}</li>'
            )
        out += ["    </ul>", "  </section>"]
    out.append("</div>")
    return "\n".join(out)


def method_matches(m, data, find):
    """The rule of assets/methods.js: a data chip matches a method whose cadence has it or `any`."""
    if data and data not in m["cadence"] and "any" not in m["cadence"]:
        return False
    return not find or find in m["questions"]


# The shortcut cards of the home page: (the question, the "My data" chip, the "I want to find" chip).
HOME_QUESTIONS = [
    ("Clearings in an annual Landsat series", "annual", "abrupt"),
    ("Degradation in every Landsat date", "everydate", "degradation"),
    ("Growing seasons from MODIS", "subannual", "seasons"),
    ("A land-cover map from the whole series", "", "classes"),
]


def home_methods(page):
    methods = load("methods.json")
    out = ['<ul class="mt-cards">']
    for question, data, find in HOME_QUESTIONS:
        hit = [m for m in methods if method_matches(m, data, find)]
        names = ", ".join(m["name"] for m in hit[:3])
        rest = len(hit) - 3
        names += f" and {rest} more" if rest > 0 else ""
        query = "&amp;".join(p for p in (f"data={data}" if data else "", f"find={find}") if p)
        out.append(
            f'  <li><a href="methods.html#{query}"><h3>{question}</h3>'
            f'<p class="mt-n"><b>{len(hit)}</b> methods</p><p class="mt-names">{names}</p>'
            f'<span class="mt-go">Open the chooser</span></a></li>'
        )
    out += ["</ul>", f'<p class="mt-all"><a class="btn btn-ghost" href="methods.html">See all {len(methods)} methods</a></p>']
    return "\n".join(out)


def home_guides(page):
    guides = guides_data()
    out = ['<nav class="g-tracks" aria-label="Tracks">', "  <p>Tracks</p>", "  <ul>"]
    for key, title, _ in TRACKS:
        n = sum(g["track"] == key for g in guides)
        out.append(f'    <li><a href="guides.html#{key}">{title}<span>{n}</span></a></li>')
    out += ["  </ul>", "</nav>", '<ul class="guide-list g-feat">']
    for g in (g for g in guides if g.get("featured")):
        out.append(
            f'  <li><a href="{g["slug"]}.html">{thumb_img(g)}<span>{g["label"]}</span>'
            f'<h3>{g["title"]}</h3><p>{g["summary"]}</p>'
            f'<small class="g-facts">{g["minutes"]} min read · {g["figures"]} figures</small></a></li>'
        )
    out += ["</ul>", f'<p class="mt-all"><a class="btn btn-ghost" href="guides.html">See all {len(guides)} guides</a></p>']
    return "\n".join(out)


def citation():
    """The version and the date of the latest release, from CITATION.cff (checked against Cargo.toml)."""
    text = (ROOT / "CITATION.cff").read_text(encoding="utf-8")
    version = re.search(r'^version:\s*"?([^"\s]+)', text, re.M).group(1)
    released = re.search(r'^date-released:\s*"?([\d-]+)', text, re.M).group(1)
    cargo = re.search(r'^version\s*=\s*"([^"]+)"', (ROOT / "Cargo.toml").read_text(encoding="utf-8"), re.M)
    if cargo and cargo.group(1) != version:
        print(f"warning: CITATION.cff says version {version}, Cargo.toml {cargo.group(1)}", file=sys.stderr)
    return version, released[:4]


def cite_bibtex(page):
    version, year = citation()
    entry = (
        "@software{lacerda_janus,\n"
        "  author  = {Lacerda, Eduardo},\n"
        "  title   = {Janus: raster time series analysis},\n"
        f"  version = {{{version}}},\n"
        f"  year    = {{{year}}},\n"
        "  publisher = {Zenodo},\n"
        "  doi     = {10.5281/zenodo.24515398},\n"
        "  url     = {https://sacridini.github.io/janus/},\n"
        "  note    = {Source code at https://github.com/sacridini/janus}\n"
        "}"
    )
    # Newlines as &#10;: the block is indented line by line, which would shift the lines of a <pre>.
    text = entry.replace("&", "&amp;").replace("<", "&lt;").replace('"', "&quot;").replace("\n", "&#10;")
    return (
        '<div class="code-wrap cite-code">\n'
        '<div class="code-bar">BibTeX</div>\n'
        f'<div class="cite-body"><pre class="code"><code>{text}</code></pre>'
        f'<button class="copy" data-copy="{text}" aria-label="Copy the BibTeX entry">Copy</button></div>\n'
        "</div>"
    )


# ---- search index (assets/search.js)

SEARCH_TOKEN = re.compile(
    r"<section\b([^>]*)>|<h([23])\b([^>]*)>(.*?)</h\2>|<p\b([^>]*)>(.*?)</p>",
    re.S,
)
SEARCH_SKIP = {"references", "what next"}


def inline_text(html):
    """Plain text of a fragment: comments and inline tags dropped, other tags as spaces."""
    html = re.sub(r"<!--.*?-->", "", html, flags=re.S)
    html = re.sub(r'<span class="n">.*?</span>', "", html, flags=re.S)
    html = re.sub(r"</?(a|b|i|em|strong|code|span|small|sup|sub|kbd|mark)\b[^>]*>", "", html)
    return re.sub(r"\s+", " ", plain(html)).strip()


def snippet(text, limit=160):
    """The text cut to `limit` characters, at the end of a sentence if one is near, else at a word."""
    if len(text) <= limit:
        return text
    cut = text[:limit]
    end = max(cut.rfind(". "), cut.rfind("? "))
    if end >= 60:
        return cut[: end + 1]
    return cut[: limit - 1].rsplit(" ", 1)[0].rstrip(",;:.-") + "…"


def page_sections(html, page, context, with_h3, eyebrows=False):
    """Entries for the h2 (and h3) of a page that have an anchor: their own id, else the id of the
    section around them. The text is the first paragraph after the heading."""
    out, cur, eyebrow, pending, h2 = [], None, "", None, ""
    for m in SEARCH_TOKEN.finditer(html):
        if m.group(0).startswith("<section"):
            sid = re.search(r'\bid="([^"]+)"', m.group(1))
            cur, eyebrow, pending = (sid.group(1) if sid else None), "", None
        elif m.group(2):
            level, attrs, title = m.group(2), m.group(3), inline_text(m.group(4))
            own = re.search(r'\bid="([^"]+)"', attrs)
            anchor = own.group(1) if own else cur
            if level == "2":
                h2 = title
                pending = None
            if anchor and title and title.lower() not in SEARCH_SKIP and (level == "2" or with_h3):
                where = context if level == "2" else f"{context} · {h2}"
                if eyebrows and eyebrow and level == "2":
                    where = f"{context} · {eyebrow}"
                pending = {"k": "s", "t": title, "u": f"{page}#{anchor}", "c": where, "x": ""}
                out.append(pending)
        else:
            attrs, text = m.group(5), inline_text(m.group(6))
            if re.search(r'class="[^"]*\b(eyebrow|story-n|mt-n|crumbs)\b', attrs):
                if "eyebrow" in attrs:
                    eyebrow = text
                continue
            if pending is not None and not pending["x"] and len(text) >= 20:
                pending["x"] = snippet(text)
    return out


def page_lede(html):
    m = re.search(r'<p class="lede">(.*?)</p>', html, re.S)
    return snippet(inline_text(m.group(1))) if m else ""


def search_index(texts):
    """The entries of site/data/search.json from the pages' text (page name -> html). Keys: k kind
    (m method, g guide, s section or page), t title, u link, c context, x text."""
    methods, guides = load("methods.json"), load("guides.json")
    group_title = {key: title for key, title, _ in GROUPS}
    track_title = {key: title for key, title, _ in TRACKS}
    out = []
    for m in methods:
        out.append({"k": "m", "t": m["name"], "u": f"methods.html#{m['id']}", "c": group_title[m["group"]], "x": inline_text(m["summary"])})
    for g in guides:
        out.append({"k": "g", "t": g["title"], "u": f"{g['slug']}.html", "c": track_title[g["track"]], "x": inline_text(g["summary"])})
    out += page_sections(texts["index.html"], "index.html", "Home", False, eyebrows=True)
    for page, name in (("methods.html", "Methods"), ("guides.html", "Guides"), ("demo.html", "Demo")):
        out.append({"k": "s", "t": name, "u": page, "c": "Page", "x": page_lede(texts[page])})
        out += page_sections(texts[page], page, name, False)
    for g in guides:
        html = texts[f"{g['slug']}.html"]
        article = re.search(r"<article\b.*?</article>", PYTABS.sub("", html), re.S)
        out += page_sections(article.group(0) if article else html, f"{g['slug']}.html", g["title"], True)
    out.append({"k": "s", "t": "Docs", "u": "docs.html", "c": "Page", "x": page_lede(texts["docs.html"])})
    for name, label, description in DOCS:
        page = f"docs-{name}.html"
        article = re.search(r"<article\b.*?</article>", texts[page], re.S)
        title = re.search(r"<h1>(.*?)</h1>", texts[page], re.S).group(1)
        out.append({"k": "s", "t": unescape(title), "u": page, "c": "Docs", "x": snippet(description)})
        out += page_sections(article.group(0), page, f"Docs · {label}", True)
    out.append({"k": "s", "t": "News", "u": "news.html", "c": "Page", "x": page_lede(texts["news.html"])})
    for r in news():
        out.append({"k": "s", "t": f"Janus {r['version']}", "u": f"news.html#{r['tag']}", "c": f"News · {r['date']}", "x": snippet(re.sub(r"`", "", notes_text(r)))})
    return out


def search_json_update(texts, write):
    """True if data/search.json differs from the index of `texts`; rewrites it if `write`."""
    path = SITE / "data" / "search.json"
    new = json.dumps(search_index(texts), ensure_ascii=False, separators=(",", ":")) + "\n"
    old = path.read_text(encoding="utf-8").replace("\r\n", "\n") if path.exists() else None
    if old == new:
        return False
    if write:
        with open(path, "w", encoding="utf-8", newline="") as f:
            f.write(new)
    return True


# ---- pages generated whole: docs.html, docs-<name>.html (from docs/*.md) and news.html (from git)

REPO = "https://github.com/sacridini/janus"
SITE_URL = "https://sacridini.github.io/janus/"

# The files of docs/ that become pages: (file name without .md, menu label, one-line description).
DOCS = [
    ("install", "Install", "The installers and packages for Windows, Linux and macOS: what each one puts on your computer, and how to open Janus from a terminal."),
    ("usage", "Usage", "Everything the program does, from opening a series to validating a map: the window, the Zeit tools, classification, vector layers, the command line and the MCP server."),
    ("architecture", "Architecture", "How the pieces fit: from the files to the GPU cube, the series and Zeit's methods, and what each crate and module of the program does."),
    ("building", "Building", "Build Janus from source, run its tests and self-tests, make the packages, and where its data lives."),
]
DOC_NAMES = {name for name, _, _ in DOCS}
DOC_PAGES = {f"docs-{name}.html" for name in DOC_NAMES}
GENERATED = {"docs.html", "news.html"} | DOC_PAGES

# The row of links at the top of every file in docs/, which the generated pages replace with their own.
DOC_NAV_LINE = re.compile(r"^\[README\]\(\.\./README\.md\)(?: · \[[^\]]+\]\([^)]+\))+[ \t]*\n?", re.M)

HEAD = """<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<title>{title} · Janus</title>
<meta name="description" content="{description}">
<meta name="theme-color" content="#0F0F0F">
<link rel="icon" href="favicon.svg?v=4" type="image/svg+xml">
<link rel="icon" href="favicon.ico?v=4" sizes="any">
<link rel="apple-touch-icon" href="apple-touch-icon.png?v=4">
<meta property="og:type" content="{kind}">
<meta property="og:title" content="{title} · Janus">
<meta property="og:description" content="{description}">
<meta property="og:image" content="{site_url}assets/img/og.png">
<meta name="twitter:card" content="summary_large_image">
<link rel="preload" href="assets/fonts/IBMPlexSans-Regular-Latin1.woff2" as="font" type="font/woff2" crossorigin>
<link rel="preload" href="assets/fonts/IBMPlexMono-Regular-Latin1.woff2" as="font" type="font/woff2" crossorigin>
<link rel="stylesheet" href="assets/style.css">
</head>
<body>

<a class="skip" href="#main">Skip to content</a>

<!-- build:header -->
<!-- /build:header -->

<main id="main">
{body}
</main>

<!-- build:footer -->
<!-- /build:footer -->

<script src="assets/main.js" defer></script>
</body>
</html>
"""


def generated_page(page, title, description, body, kind="website"):
    """The text of a generated page (LF line endings), with its header and footer filled."""
    text = HEAD.format(
        title=escape(title), description=escape(description, quote=True), kind=kind, site_url=SITE_URL, body=body.strip("\n")
    )
    text = rebuild(page, text).replace("<!doctype html>\n", "<!doctype html>\n<!-- Generated by tools/build_site.py: do not edit by hand. -->\n", 1)
    return text


def hero(crumbs, eyebrow, title, lede):
    return (
        '<section class="guide-hero">\n  <div class="wrap">\n'
        f'    <p class="crumbs">{crumbs}</p>\n    <p class="eyebrow">{eyebrow}</p>\n    <h1>{title}</h1>\n'
        f'    <p class="lede">{lede}</p>\n  </div>\n</section>'
    )


# -- docs

def doc_resolver(name):
    """Where a link of docs/<name>.md goes on the site: other docs to their pages, pages of the site by their
    relative name, other files of the repository to GitHub, anything else as it is."""
    def resolve(href):
        if href.startswith("#"):
            return href
        m = re.fullmatch(re.escape(SITE_URL) + r"([\w-]+\.html)(#.*)?", href)
        if m and ((SITE / m.group(1)).exists() or m.group(1) in GENERATED):
            return m.group(1) + (m.group(2) or "")
        if re.match(r"[a-zA-Z][a-zA-Z0-9+.-]*:", href):
            return href
        path, _, frag = href.partition("#")
        target = posixpath.normpath(posixpath.join("docs", path)) if path else f"docs/{name}.md"
        if target.startswith("docs/") and target.endswith(".md") and target[len("docs/") : -len(".md")] in DOC_NAMES:
            return f"docs-{target[len('docs/') : -len('.md')]}.html" + (f"#{frag}" if frag else "")
        kind = "tree" if (ROOT / target).is_dir() else "blob"
        return f"{REPO}/{kind}/main/{target}" + (f"#{frag}" if frag else "")

    return resolve


def doc_heading(level, hid, html):
    return f'<h{level} id="{hid}">{html}<a class="anchor" href="#{hid}" aria-label="Link to this section"></a></h{level}>'


def doc_toc(headings):
    out = ['<nav class="toc" aria-label="On this page">', "  <p>On this page</p>", "  <ol>"]
    inner = False
    items = [(level, hid, text) for level, hid, text in headings if level in (2, 3)]
    for k, (level, hid, text) in enumerate(items):
        link = f'<a href="#{hid}">{escape(text, quote=False)}</a>'
        nxt = items[k + 1][0] if k + 1 < len(items) else 2
        if level == 2:
            out.append(f"    <li>{link}" + ("" if nxt == 3 else "</li>"))
        else:
            if not inner:
                out.append("      <ol>")
                inner = True
            out.append(f"        <li>{link}</li>")
            if nxt == 2:
                out += ["      </ol>", "    </li>"]
                inner = False
    out += ["  </ol>", "</nav>"]
    return "\n".join(out)


def doc_html(html):
    """The classes of the guide pages on what md.py writes: tables in a scrolling box, code in a framed block."""
    html = html.replace("<table>", '<div class="tbl-wrap"><table class="tbl">').replace("</table>", "</table></div>")

    def code(m):
        lang = m.group(1)
        bar = f'<div class="code-bar">{lang}</div>' if lang else ""
        return f'<div class="code-wrap doc-code">{bar}<pre class="code"><code{m.group(2)}>{m.group(3)}</code></pre></div>'

    return re.sub(r'<pre data-lang="([^"]*)"><code([^>]*)>(.*?)</code></pre>', code, html, flags=re.S)


def doc_source(name):
    text = (ROOT / "docs" / f"{name}.md").read_text(encoding="utf-8").replace("\r\n", "\n")
    title = re.search(r"^# (.+)$", text, re.M).group(1).strip()
    body = re.sub(r"^# .+\n", "", DOC_NAV_LINE.sub("", text, count=1), count=1, flags=re.M)
    html, headings = md.convert(body, resolve=doc_resolver(name), heading_hook=doc_heading)
    return title, html, headings


def article_minutes(html):
    return -(-len(re.findall(r"\S+", plain(html))) // 200)


def render_doc(page):
    name = page[len("docs-") : -len(".html")]
    label, description = next((lab, desc) for n, lab, desc in DOCS if n == name)
    title, html, headings = doc_source(name)
    parts = [p for p in re.split(r"(?=<h2 id=)", doc_html(html)) if p.strip()]
    nav = " ".join(
        f'<a href="docs-{n}.html"' + (' aria-current="page"' if n == name else "") + f">{lab}</a>" for n, lab, _ in DOCS
    ) + f' <a href="{REPO}/blob/main/README.md">README</a>'
    source = (
        f'<p class="doc-src">This page is made from <a href="{REPO}/blob/main/docs/{name}.md"><code>docs/{name}.md</code></a>'
        "; to change it, change that file.</p>"
    )
    parts[-1] = parts[-1].rstrip() + "\n" + source
    # not indented: the text of a <pre> must stay as it is
    sections = "\n\n".join(f"    <section>\n{p.strip()}\n    </section>" for p in parts)
    body = (
        hero(f'<a href="./">Janus</a> / <a href="docs.html">Docs</a> / {label}', "Docs", escape(title, quote=False), escape(description, quote=False))
        + '\n\n<div class="wrap guide">\n\n'
        + indent(doc_toc(headings), "  ")
        + '\n\n  <article class="article doc">\n    <nav class="doc-nav" aria-label="The docs">'
        + nav
        + "</nav>\n\n"
        + sections
        + "\n  </article>\n\n</div>"
    )
    return generated_page(page, title, description, body, kind="article")


def render_docs_index():
    cards = []
    for name, label, description in DOCS:
        title, html, headings = doc_source(name)
        sections = sum(1 for level, _, _ in headings if level == 2)
        cards.append(
            f'      <li><a href="docs-{name}.html"><span>{label}</span><h3>{escape(title, quote=False)}</h3>'
            f'<p>{escape(description, quote=False)}</p>'
            f'<small class="g-facts">{article_minutes(html)} min read · {sections} sections</small></a></li>'
        )
    n_guides, n_methods = len(load("guides.json")), len(load("methods.json"))
    more = [
        ("guides.html", "Guides", "Step by step", f"{n_guides} guides, one tool each, with the screenshots made by the program.", None),
        ("methods.html", "Methods", "Find the right one", f"The {n_methods} methods built in, with a chooser by your data and the question you ask.", None),
        ("news.html", "News", "Release by release", "What each version of Janus added, in the words of the release commit.", None),
    ]
    cards2 = [
        f'      <li><a href="{href}"><span>{label}</span><h3>{h3}</h3><p>{text}</p></a></li>' for href, label, h3, text, _ in more
    ]
    body = (
        hero('<a href="./">Janus</a> / Docs', "Docs", "Documentation.",
             "What Janus does and how to use it, how it is built, and how to build it yourself. The guides teach one tool at a time; these pages are the reference.")
        + '\n\n<div class="docs-index">\n  <div class="wrap">\n    <ul class="guide-list d-two">\n'
        + "\n".join(cards)
        + '\n    </ul>\n  </div>\n</div>\n\n<div class="pixrow" aria-hidden="true"></div>\n\n'
        + '<section class="docs-more">\n  <div class="wrap">\n    <div class="sec-head">\n      <p class="eyebrow">Also on this site</p>\n'
        + "      <h2>Learn it by doing, or look a method up.</h2>\n    </div>\n    <ul class=\"guide-list\">\n"
        + "\n".join(cards2)
        + "\n    </ul>\n  </div>\n</section>"
    )
    return generated_page(
        "docs.html", "Docs", "The documentation of Janus: installing it, using it, how it is built and how to build it, with the guides and the methods one click away.", body
    )


# -- news

def git_text(*args):
    """The output of a git command in the repository, or None if git (or the repository) is not there."""
    try:
        done = subprocess.run(["git", *args], cwd=ROOT, capture_output=True, text=True, encoding="utf-8", errors="replace")
    except OSError:
        return None
    return done.stdout if done.returncode == 0 else None


def version_key(version):
    return tuple(int(x) for x in re.findall(r"\d+", version))


TRAILER = re.compile(r"^(Co-Authored-By|Signed-off-by):", re.I)


def top_level_split(text):
    """The text cut at every "; " that is not inside brackets or double quotes."""
    parts, depth, quote, start = [], 0, False, 0
    for i, c in enumerate(text):
        if c == '"':
            quote = not quote
        elif quote:
            continue
        elif c in "([{":
            depth += 1
        elif c in ")]}":
            depth = max(0, depth - 1)
        elif c == ";" and depth == 0 and text[i + 1 : i + 2] == " ":
            parts.append(text[start:i].strip())
            start = i + 2
    parts.append(text[start:].strip())
    return [p for p in parts if p]


def parse_notes(version, body):
    """The release commit's message as blocks: {"p": text} or {"ul": [items]}. The "X.Y.Z: " that opens it goes;
    the first paragraph is cut into a list at its top-level "; " when that gives three items or more; a "- " list of
    the message is a list; nothing else of the text changes (git trailers apart)."""
    lines = [l.rstrip() for l in body.strip().splitlines() if not TRAILER.match(l)]
    paragraphs, cur = [], []
    for line in lines:
        if line.strip():
            cur.append(line)
        elif cur:
            paragraphs.append(cur)
            cur = []
    if cur:
        paragraphs.append(cur)
    blocks = []
    for k, para in enumerate(paragraphs):
        text = re.sub(r"\s+", " ", " ".join(l.strip() for l in para))
        if k == 0:
            text = re.sub("^" + re.escape(version) + r": ", "", text)
            items = top_level_split(text)
            blocks.append({"ul": items} if len(items) >= 3 else {"p": text})
            continue
        plain_lines, items = [], []
        for line in para:
            if line.startswith("- "):
                items.append(line[2:].strip())
            elif items and line.startswith(" "):
                items[-1] += " " + line.strip()
            else:
                plain_lines.append(line.strip())
        if plain_lines:
            blocks.append({"p": " ".join(plain_lines)})
        if items:
            blocks.append({"ul": items})
    return blocks


def read_releases():
    """The releases of the repository's tags vX.Y.Z, from git, or None without git or tags. The notes are the message of
    the commit that carries "X.Y.Z: ..." (the tagged commit, else the nearest before it); the date is the tagged commit's."""
    out = git_text("tag", "--list", "v*")
    tags = [t for t in (out or "").split() if re.fullmatch(r"v\d+\.\d+\.\d+", t)]
    if not tags:
        return None
    releases = []
    for tag in tags:
        version = tag[1:]
        raw = git_text("log", "-1", "--format=%ad%x1f%B", "--date=short", tag)
        if raw is None:
            return None
        date, _, body = raw.partition("\x1f")
        if not body.lstrip().startswith(version + ":"):
            found = git_text("log", tag, "-E", f"--grep=^{re.escape(version)}: ", "--format=%x1e%B") or ""
            for message in found.split("\x1e"):
                if message.lstrip().startswith(version + ":"):
                    body = message
                    break
        releases.append({"tag": tag, "version": version, "date": date.strip(), "blocks": parse_notes(version, body)})
    return releases


_NEWS = None


def news():
    """The releases, newest first: data/news.json updated from git's tags (the entries git does not have stay)."""
    global _NEWS
    if _NEWS is None:
        path = SITE / "data" / "news.json"
        old = json.loads(path.read_text(encoding="utf-8")) if path.exists() else []
        merged = {r["tag"]: r for r in old}
        merged.update({r["tag"]: r for r in (read_releases() or [])})
        _NEWS = sorted(merged.values(), key=lambda r: version_key(r["version"]), reverse=True)
    return _NEWS


def news_json_update(write):
    """True if data/news.json differs from news(); rewrites it if `write`."""
    path = SITE / "data" / "news.json"
    new = json.dumps(news(), indent=2, ensure_ascii=False) + "\n"
    old = path.read_text(encoding="utf-8") if path.exists() else None
    if old is not None and old.replace("\r\n", "\n") == new:
        return False
    if write:
        with open(path, "w", encoding="utf-8", newline="") as f:
            f.write(new.replace("\n", "\r\n") if old and "\r\n" in old else new)
    return True


def rich(text):
    """A message's text as HTML: escaped, with `code` spans."""
    return re.sub(r"`([^`]+)`", lambda m: f"<code>{m.group(1)}</code>", escape(text, quote=False))


def notes_text(release):
    return " ".join(b["p"] if "p" in b else " ".join(b["ul"]) for b in release["blocks"])


def notes_html(release, indent_by):
    out = []
    for b in release["blocks"]:
        if "p" in b:
            out.append(f"<p>{rich(b['p'])}</p>")
        else:
            out.append("<ul>\n" + "\n".join(f"  <li>{rich(i)}</li>" for i in b["ul"]) + "\n</ul>")
    out.append(f'<p class="rel-link"><a href="{REPO}/releases/tag/{release["tag"]}">Release page on GitHub</a></p>')
    return indent("\n".join(out), indent_by)


NEWS_FULL = 15  # the latest releases shown in full; the older ones are collapsed


def render_news():
    releases = news()
    full, older = releases[:NEWS_FULL], releases[NEWS_FULL:]
    sections = []
    for k, r in enumerate(full):
        latest = ' <span class="rel-new">Latest</span>' if k == 0 else ""
        sections.append(
            f'    <section id="{r["tag"]}">\n      <h2><a href="#{r["tag"]}">{r["version"]}</a>'
            f'<time datetime="{r["date"]}">{r["date"]}</time>{latest}</h2>\n'
            + notes_html(r, "      ") + "\n    </section>"
        )
    if older:
        rows = []
        for r in older:
            first = r["blocks"][0]["p"] if r["blocks"] and "p" in r["blocks"][0] else (r["blocks"][0]["ul"][0] if r["blocks"] else "")
            rows.append(
                f'      <details class="rel-old" id="{r["tag"]}"><summary><span class="rv">{r["version"]}</span>'
                f'<time datetime="{r["date"]}">{r["date"]}</time><span class="rs">{escape(snippet(first, 110), quote=False)}</span></summary>\n'
                f'        <div class="rel-body">\n{notes_html(r, "          ")}\n        </div>\n      </details>'
            )
        sections.append(
            '    <section id="older">\n      <h2>Older releases</h2>\n' + "\n".join(rows) + "\n    </section>"
        )
    latest = releases[0]["version"] if releases else ""
    body = (
        hero('<a href="./">Janus</a> / News', "News", "What&rsquo;s new.",
             f"What each version of Janus added, newest first, in the words of the commit that made the release. "
             f"The latest is {latest}; the installers are on each release&rsquo;s page on GitHub.")
        + '\n\n<div class="wrap news-wrap">\n  <div class="article news">\n'
        + "\n".join(sections)
        + "\n  </div>\n</div>"
    )
    return generated_page("news.html", "What's new", "What each version of Janus added, release by release, from the first to the latest.", body)


def news_link():
    """The home page's one-line link to the latest release's notes."""
    rel = news()
    return f'<a href="news.html">What&rsquo;s new in {rel[0]["version"]}</a>' if rel else '<a href="news.html">What&rsquo;s new</a>'


# -- WebP twins of the screenshots (tools/make_webp.py)

IMG_TAG = re.compile(r"<picture>.*?</picture>|<img\b[^>]*>", re.S)


def picturize(text):
    """Every <img> of a JPEG in assets/img/ that has a .webp next to it goes in <picture><source type="image/webp"
    srcset="...webp"><img ...></picture>; one without a twin (any more) is left a bare <img>. Safe to run again."""

    def wrap(img):
        src = re.search(r'\ssrc="(assets/img/[^"?#]+\.jpg)"', img)
        if not src:
            return img
        webp = src.group(1)[: -len(".jpg")] + ".webp"
        if not (SITE / webp).exists():
            return img
        return f'<picture><source type="image/webp" srcset="{webp}">{img}</picture>'

    def one(m):
        s = m.group(0)
        if s.startswith("<picture>"):
            img = re.search(r"<img\b[^>]*>", s)
            return wrap(img.group(0)) if img else s
        return wrap(s)

    return IMG_TAG.sub(one, text)


# ---- python-tabs: "In Janus" / "In Python" (the code is what Janus's Copy Python gives, site/data/python.json)

PYTABS = re.compile(r"<!-- build:python-tabs[\w-]* -->.*?<!-- /build:python-tabs[\w-]* -->", re.S)

PY_KEYWORDS = (
    "and|as|assert|class|def|elif|else|except|for|from|if|import|in|is|lambda|not|or|return|try|while|with"
    "|None|True|False"
)
PY_TOKEN = re.compile(
    r"(?P<c>#[^\n]*)"
    r'|(?P<s>"""(?:.|\n)*?"""|"(?:\\.|[^"\\\n])*"|' + r"'(?:\\.|[^'\\\n])*')"
    r"|(?P<k>\b(?:" + PY_KEYWORDS + r")\b)"
    r"|(?P<n>\b\d+(?:\.\d*)?(?:[eE][+-]?\d+)?\b)"
)


def highlight_python(code):
    """The code as HTML with keywords (k), strings (s), comments (c) and numbers (n) in <span>s."""
    out, pos = [], 0
    for m in PY_TOKEN.finditer(code):
        out.append(escape(code[pos : m.start()], quote=False))
        out.append(f'<span class="{m.lastgroup}">{escape(m.group(0), quote=False)}</span>')
        pos = m.end()
    out.append(escape(code[pos:], quote=False))
    return "".join(out)


def python_files(write):
    """site/assets/python/<key>.py: each full script of python.json, as it is. The names of those that are not as
    they should be (written if `write`)."""
    folder = SITE / "assets" / "python"
    changed = []
    for key, case in load("python.json").items():
        path = folder / f"{key}.py"
        data = case["code"].encode("utf-8")
        if not path.exists() or path.read_bytes() != data:
            changed.append(f"assets/python/{key}.py")
            if write:
                folder.mkdir(parents=True, exist_ok=True)
                path.write_bytes(data)
    return changed


def python_tabs(page, name):
    """The tabs of a guide: the steps in Janus and the code of Copy Python. Both panels are shown
    without JavaScript (the tab list is hidden until assets/main.js shows it)."""
    slug = page[: -len(".html")]
    key = slug if name == "python-tabs" else f"{slug}-{name[len('python-tabs-'):]}"
    data = load("python.json")
    if key not in data:
        raise SystemExit(f"{page}: no entry {key!r} in site/data/python.json")
    case = data[key]
    steps = "\n".join(f"        <li>{step}</li>" for step in case["steps"])
    code = case["code"].replace("\r\n", "\n").rstrip("\n") + "\n"
    long = " long" if code.count("\n") > 60 else ""
    tool = escape(case["tool"], quote=False)
    return "\n".join([
        '<div class="pytabs" data-tabs>',
        f'  <div class="term-tabs pytabs-bar" role="tablist" aria-label="{tool}: in Janus or in Python" hidden>',
        f'    <button type="button" role="tab" id="pt-{key}-janus" aria-controls="pp-{key}-janus" aria-selected="true" data-tab="janus">In Janus</button>',
        f'    <button type="button" role="tab" id="pt-{key}-python" aria-controls="pp-{key}-python" aria-selected="false" data-tab="python" tabindex="-1">In Python</button>',
        '  </div>',
        '  <div class="pytabs-body">',
        f'    <div role="tabpanel" id="pp-{key}-janus" aria-labelledby="pt-{key}-janus" class="pytabs-panel">',
        '      <p class="pytabs-h">In Janus</p>',
        '      <ol>',
        steps,
        '      </ol>',
        '    </div>',
        f'    <div role="tabpanel" id="pp-{key}-python" aria-labelledby="pt-{key}-python" class="pytabs-panel">',
        '      <p class="pytabs-h">In Python</p>',
        f'      <div class="pycode"><pre class="code py{long}" tabindex="0"><code>{highlight_python(code)}</code></pre>'
        f'<div class="pycode-btns"><button class="copy" type="button" data-copy="" data-src="assets/python/{key}.py" aria-label="Copy the whole Python script">Copy</button>'
        f'<a class="copy" href="assets/python/{key}.py" download="{key}.py">Download .py</a></div></div>',
        '      <p class="pytabs-note">The code Janus&rsquo;s Copy Python button gives for this pixel and these parameters; '
        'it runs with Zeit: the header of the script says which Zeit it needs (<code>pip install zeit-cdts</code>, version 3.2 or later; 3.2.1 for CCDC, CODED and S-CCD). Copy and Download give the same script, with the file&rsquo;s name as it is here (put the script beside your data).</p>',
        '    </div>',
        '  </div>',
        '</div>',
    ])


RENDERERS = {
    "header": lambda page: partial("header", page),
    "footer": lambda page: partial("footer", page),
    "home-methods": home_methods,
    "home-guides": home_guides,
    "cite-bibtex": cite_bibtex,
    "method-catalog": method_catalog,
    "guide-tracks": guide_tracks,
}

INLINE_VALUES = {
    "method-count": lambda: str(len(load("methods.json"))),
    "method-more": lambda: str(len(load("methods.json")) - 3),  # LandTrendr, CCDC and BFAST are named
    "guide-count": lambda: str(len(load("guides.json"))),
    "track-count": lambda: str(len(TRACKS)),
    # the tools jn mcp lists: the calls of the catalogue in crates/janus-app/src/mcp.rs
    "news-link": news_link,
    "mcp-tools": lambda: str(
        len(set(re.findall(r'\btool\(\s*"(\w+)",', (ROOT / "crates/janus-app/src/mcp.rs").read_text(encoding="utf-8"))))
    ),
}


def rebuild(page, text):
    eol = "\r\n" if "\r\n" in text else "\n"

    def one(m):
        name = m.group("name")
        ind = m.group("ind")
        if name == "python-tabs" or name.startswith("python-tabs-"):
            body = python_tabs(page, name).split("\n")
        elif name not in RENDERERS:
            raise SystemExit(f"{page}: unknown block build:{name}")
        else:
            body = RENDERERS[name](page).split("\n")
        lines = [f"{ind}<!-- build:{name} -->"]
        lines += [ind + line if line else line for line in body]
        lines.append(f"{ind}<!-- /build:{name} -->")
        return eol.join(lines)

    def number(m):
        name = m.group("name")
        if name not in INLINE_VALUES:
            raise SystemExit(f"{page}: unknown inline block n:{name}")
        return f"<!-- n:{name} -->{INLINE_VALUES[name]()}<!-- /n:{name} -->"

    return INLINE.sub(number, BLOCK.sub(one, text))


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--check", action="store_true", help="write nothing; exit 1 if a file would change")
    args = ap.parse_args()

    changed = []
    # The guides' facts first: the guide-tracks block reads the pages, not the file.
    if guides_json_update(write=not args.check):
        changed.append("data/guides.json")
    if news_json_update(write=not args.check):
        changed.append("data/news.json")
    texts = {}
    # The pages generated whole, in the line endings of their file (those of index.html when new).
    with open(SITE / "index.html", encoding="utf-8", newline="") as f:
        default_eol = "\r\n" if "\r\n" in f.read() else "\n"
    generated = {"docs.html": render_docs_index, "news.html": render_news}
    generated.update({page: (lambda page=page: render_doc(page)) for page in DOC_PAGES})
    for name, render in sorted(generated.items()):
        path = SITE / name
        old = None
        if path.exists():
            with open(path, encoding="utf-8", newline="") as f:
                old = f.read()
        eol = "\r\n" if old and "\r\n" in old else default_eol
        text = picturize(render())
        texts[name] = text
        if old is None or old.replace("\r\n", "\n") != text:
            changed.append(name)
            if not args.check:
                with open(path, "w", encoding="utf-8", newline="") as f:
                    f.write(text.replace("\n", eol))
    for path in sorted(SITE.glob("*.html")):
        if path.name in generated:
            continue
        with open(path, encoding="utf-8", newline="") as f:
            old = f.read()
        new = picturize(rebuild(path.name, old))
        texts[path.name] = new.replace("\r\n", "\n")
        if new != old:
            changed.append(path.name)
            if not args.check:
                with open(path, "w", encoding="utf-8", newline="") as f:
                    f.write(new)

    if search_json_update(texts, write=not args.check):
        changed.append("data/search.json")
    changed += python_files(write=not args.check)

    if args.check:
        if changed:
            print("out of date (run python tools/build_site.py):")
            for name in changed:
                print("  site/" + name)
            return 1
        print("site/ is up to date")
        return 0
    print(f"rewrote {len(changed)} file(s)" + "".join(f"\n  site/{n}" for n in changed))
    return 0


if __name__ == "__main__":
    sys.exit(main())
