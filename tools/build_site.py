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

It also writes site/data/search.json, the index of the Ctrl+K search (assets/search.js):
one entry per method, guide, guide section and subsection, and section of the other pages.

Numbers inside a sentence are inline blocks, `<!-- n:NAME -->12<!-- /n:NAME -->`,
rewritten the same way: method-count, method-more (the methods beyond the three
named in the lede), guide-count and track-count.

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
import re
import struct
import sys
from html import unescape
from pathlib import Path

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
    index.html links to its own sections and has none."""
    slugs = {g["slug"] for g in load("guides.json")}
    current = ""
    if page == "methods.html":
        current = "methods"
    elif page == "guides.html" or page[: -len(".html")] in slugs:
        current = "guides"
    elif page == "demo.html":
        current = "demo"
    mark = ' aria-current="page"'
    values = {
        "current_methods": mark if current == "methods" else "",
        "current_guides": mark if current == "guides" else "",
        "current_demo": mark if current == "demo" else "",
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
        article = re.search(r"<article\b.*?</article>", html, re.S)
        out += page_sections(article.group(0) if article else html, f"{g['slug']}.html", g["title"], True)
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
    "mcp-tools": lambda: str(
        len(set(re.findall(r'\btool\(\s*"(\w+)",', (ROOT / "crates/janus-app/src/mcp.rs").read_text(encoding="utf-8"))))
    ),
}


def rebuild(page, text):
    eol = "\r\n" if "\r\n" in text else "\n"

    def one(m):
        name = m.group("name")
        if name not in RENDERERS:
            raise SystemExit(f"{page}: unknown block build:{name}")
        ind = m.group("ind")
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
    texts = {}
    for path in sorted(SITE.glob("*.html")):
        with open(path, encoding="utf-8", newline="") as f:
            old = f.read()
        new = rebuild(path.name, old)
        texts[path.name] = new.replace("\r\n", "\n")
        if new != old:
            changed.append(path.name)
            if not args.check:
                with open(path, "w", encoding="utf-8", newline="") as f:
                    f.write(new)

    if search_json_update(texts, write=not args.check):
        changed.append("data/search.json")

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
