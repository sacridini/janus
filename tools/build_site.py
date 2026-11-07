#!/usr/bin/env python3
"""Regenerate the repeated parts of the static site in site/.

In every site/*.html, the text between `<!-- build:NAME -->` and
`<!-- /build:NAME -->` is rewritten (the markers stay). The blocks are:

  header, footer   partials in tools/site_partials/, with {{placeholders}}
  guide-cards      the Guides cards of index.html, from site/data/guides.json
  method-list      the methods list of index.html, from site/data/methods.json

Run it after editing a partial or a data file:

    python tools/build_site.py           # rewrite the pages
    python tools/build_site.py --check   # exit 1 and list the pages that would change

Python 3 standard library only. Files are read and written as UTF-8 and keep
their own line endings (LF or CRLF).
"""

import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SITE = ROOT / "site"
PARTIALS = Path(__file__).resolve().parent / "site_partials"

BLOCK = re.compile(
    r"^(?P<ind>[ \t]*)<!-- build:(?P<name>[\w-]+) -->.*?<!-- /build:(?P=name) -->",
    re.S | re.M,
)


def fill(template, values):
    for key, value in values.items():
        template = template.replace("{{" + key + "}}", value)
    left = re.findall(r"\{\{\w+\}\}", template)
    if left:
        raise SystemExit(f"unfilled placeholders: {left}")
    return template


def page_values(page):
    """Placeholders of the header: index.html links to its own sections."""
    if page == "index.html":
        return {
            "base": "",
            "brand_href": "#top",
            "brand_label": "Janus, back to top",
            "current_guides": "",
        }
    return {
        "base": "./",
        "brand_href": "./",
        "brand_label": "Janus, home",
        "current_guides": ' aria-current="page"',
    }


def partial(name, page):
    text = (PARTIALS / f"{name}.html").read_text(encoding="utf-8").replace("\r\n", "\n")
    return fill(text.rstrip("\n"), page_values(page))


def load(name):
    with open(SITE / "data" / name, encoding="utf-8") as f:
        return json.load(f)


def guide_cards(page):
    return "\n".join(
        f'<li><a href="{g["slug"]}.html"><span>{g["label"]}</span>'
        f'<h3>{g["title"]}</h3><p>{g["summary"]}</p></a></li>'
        for g in load("guides.json")
    )


def method_list(page):
    return "\n".join(
        f'<li><h3>{m["name"]}</h3><p>{m["summary"]}</p><span>{m["requires"]}</span></li>'
        for m in load("methods.json")
    )


RENDERERS = {
    "header": lambda page: partial("header", page),
    "footer": lambda page: partial("footer", page),
    "guide-cards": guide_cards,
    "method-list": method_list,
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

    return BLOCK.sub(one, text)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--check", action="store_true", help="write nothing; exit 1 if a page would change")
    args = ap.parse_args()

    changed = []
    for path in sorted(SITE.glob("*.html")):
        with open(path, encoding="utf-8", newline="") as f:
            old = f.read()
        new = rebuild(path.name, old)
        if new != old:
            changed.append(path.name)
            if not args.check:
                with open(path, "w", encoding="utf-8", newline="") as f:
                    f.write(new)

    if args.check:
        if changed:
            print("out of date (run python tools/build_site.py):")
            for name in changed:
                print("  site/" + name)
            return 1
        print("site/ is up to date")
        return 0
    print(f"rewrote {len(changed)} page(s)" + "".join(f"\n  site/{n}" for n in changed))
    return 0


if __name__ == "__main__":
    sys.exit(main())
