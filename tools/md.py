#!/usr/bin/env python3
"""A small Markdown to HTML converter, Python 3 standard library only.

It covers what the files in docs/ use (and a little more): ATX headings with GitHub-style ids,
paragraphs, bullet and numbered lists (nested to any depth, with continuation lines, code blocks and
paragraphs inside an item), fenced code blocks, inline code, links, images, bold and italic, tables with
alignment, block quotes, horizontal rules and HTML blocks passed through as they are. Anything else is
plain text. It is not a CommonMark implementation: no reference links, no setext headings, no
indented code blocks, no inline HTML (the text is escaped; write a block of HTML on lines of its own).

    from md import convert
    html, headings = convert(text, resolve=lambda href: href)

`resolve` maps every link target (and image source) to the target to write; `headings` is a list of
(level, id, plain text) for the headings of the document, whose ids are those GitHub gives (lowercase,
punctuation dropped, spaces as hyphens, `-1`, `-2` for repeats), so `[text](#some-heading)` links written
for GitHub's rendering keep working.

    python tools/md.py docs/install.md   # prints the HTML
"""

import re
import sys
from html import escape

FENCE = re.compile(r"^\s*(`{3,}|~{3,})\s*([\w+-]*)[^`]*$")
HEADING = re.compile(r"^(#{1,6})[ \t]+(.*?)(?:[ \t]+#+)?[ \t]*$")
ITEM = re.compile(r"^(?P<ind> *)(?P<mark>[-*+]|\d{1,9}[.)])(?P<sp> +|$)")
RULE = re.compile(r"^ {0,3}([-*_])(?:[ \t]*\1){2,}[ \t]*$")
TABLE_SEP = re.compile(r"^\s*\|?\s*:?-+:?\s*(\|\s*:?-+:?\s*)*\|?\s*$")
HTML_BLOCK = re.compile(r"^ {0,3}<(?:/?[a-zA-Z][\w-]*(?:\s[^>]*)?/?>|!--)")


def slug(text, seen):
    """GitHub's heading id: lowercase, keep letters, digits, hyphens and underscores, spaces to hyphens."""
    s = re.sub(r"[^\w\- ]", "", text.strip().lower()).replace(" ", "-")
    n = seen.get(s, 0)
    seen[s] = n + 1
    return s if n == 0 else f"{s}-{n}"


def plain(text):
    """The text of an inline Markdown fragment, without its marks."""
    text = re.sub(r"!?\[([^\]]*)\]\([^)]*\)", r"\1", text)
    text = re.sub(r"`+([^`]*)`+", r"\1", text)
    text = re.sub(r"(\*\*|__|\*|(?<!\w)_|_(?!\w))", "", text)
    return text.strip()


class Converter:
    def __init__(self, resolve=None, heading_hook=None):
        self.resolve = resolve or (lambda href: href)
        self.heading_hook = heading_hook
        self.headings = []
        self.seen = {}

    # ---- inline

    def inline(self, text):
        stash = []

        def keep(html):
            stash.append(html)
            return f"\x00{len(stash) - 1}\x00"

        def code(m):
            body = re.sub(r"\s*\n\s*", " ", m.group(2))
            body = body.strip(" ") if body.strip() else body
            return keep(f"<code>{escape(body, quote=False)}</code>")

        text = re.sub(r"(?<!`)(`+)(?!`)(.+?)(?<!`)\1(?!`)", code, text, flags=re.S)
        text = re.sub(r"\\([\\`*_{}\[\]()#+\-.!|<>])", lambda m: keep(escape(m.group(1), quote=False)), text)
        # autolinks <https://...>
        text = re.sub(
            r"<(https?://[^>\s]+)>",
            lambda m: keep(f'<a href="{escape(self.resolve(m.group(1)))}">{escape(m.group(1), quote=False)}</a>'),
            text,
        )
        text = escape(text, quote=False)

        def image(m):
            return keep(
                f'<img src="{escape(self.resolve(m.group(2)))}" alt="{escape(plain(m.group(1)))}" loading="lazy">'
            )

        text = re.sub(r"!\[([^\]]*)\]\(\s*([^)\s]+)(?:\s+&quot;[^)]*&quot;)?\s*\)", image, text)

        def link(m):
            href = escape(self.resolve(m.group(2).replace("&amp;", "&")))
            return keep(f'<a href="{href}">{self.emphasis(m.group(1))}</a>')

        text = re.sub(r"\[((?:[^\[\]]|\[[^\]]*\])+)\]\(\s*([^)\s]+)(?:\s+(?:&quot;|')[^)]*(?:&quot;|'))?\s*\)", link, text)
        text = self.emphasis(text)
        text = re.sub(r" {2,}\n", "<br>\n", text)
        # restore the stash (it can nest: a link holds code spans)
        for _ in range(3):
            text = re.sub(r"\x00(\d+)\x00", lambda m: stash[int(m.group(1))], text)
        return text

    @staticmethod
    def emphasis(text):
        text = re.sub(r"\*\*(?=\S)(.+?)(?<=\S)\*\*", r"<strong>\1</strong>", text, flags=re.S)
        text = re.sub(r"(?<![\w])__(?=\S)(.+?)(?<=\S)__(?![\w])", r"<strong>\1</strong>", text, flags=re.S)
        text = re.sub(r"(?<![\w*])\*(?=[^\s*])(.+?)(?<=[^\s*])\*(?![\w*])", r"<em>\1</em>", text, flags=re.S)
        text = re.sub(r"(?<![\w_])_(?=[^\s_])(.+?)(?<=[^\s_])_(?![\w_])", r"<em>\1</em>", text, flags=re.S)
        return text

    # ---- blocks

    def blocks(self, lines, top=False):
        out = []
        i, n = 0, len(lines)
        while i < n:
            line = lines[i]
            if not line.strip():
                i += 1
                continue
            m = FENCE.match(line)
            if m:
                i = self.fenced(lines, i, m, out)
                continue
            m = HEADING.match(line)
            if m and not line.startswith(" "):
                out.append(self.heading(len(m.group(1)), m.group(2)))
                i += 1
                continue
            if RULE.match(line) and not ITEM.match(line):
                out.append("<hr>")
                i += 1
                continue
            if line.lstrip().startswith("|") and i + 1 < n and TABLE_SEP.match(lines[i + 1]) and "-" in lines[i + 1]:
                i = self.table(lines, i, out)
                continue
            if line.startswith(">"):
                j = i
                quote = []
                while j < n and lines[j].startswith(">"):
                    quote.append(re.sub(r"^> ?", "", lines[j]))
                    j += 1
                out.append("<blockquote>\n" + self.blocks(quote) + "\n</blockquote>")
                i = j
                continue
            if ITEM.match(line):
                i = self.list(lines, i, out)
                continue
            if HTML_BLOCK.match(line):
                j = i
                while j < n and lines[j].strip():
                    j += 1
                out.append("\n".join(lines[i:j]))
                i = j
                continue
            i = self.paragraph(lines, i, out)
        return "\n".join(out)

    def heading(self, level, text):
        hid = slug(plain(text), self.seen)
        self.headings.append((level, hid, plain(text)))
        html = self.inline(text)
        if self.heading_hook:
            return self.heading_hook(level, hid, html)
        return f'<h{level} id="{hid}">{html}</h{level}>'

    def fenced(self, lines, i, m, out):
        fence, lang = m.group(1), m.group(2)
        indent = len(lines[i]) - len(lines[i].lstrip())
        j = i + 1
        body = []
        while j < len(lines) and not re.match(r"^\s*" + re.escape(fence[0]) + "{" + str(len(fence)) + r",}\s*$", lines[j]):
            body.append(lines[j][indent:] if lines[j][:indent].strip() == "" else lines[j].lstrip())
            j += 1
        cls = f' class="language-{escape(lang)}"' if lang else ""
        out.append(f'<pre data-lang="{escape(lang)}"><code{cls}>{escape(chr(10).join(body), quote=False)}</code></pre>')
        return j + 1

    @staticmethod
    def cells(row):
        row = row.strip()
        if row.startswith("|"):
            row = row[1:]
        if row.endswith("|") and not row.endswith("\\|"):
            row = row[:-1]
        out, cur, tick = [], "", False
        i = 0
        while i < len(row):
            c = row[i]
            if c == "\\" and i + 1 < len(row) and row[i + 1] == "|":
                cur += "|"
                i += 2
                continue
            if c == "`":
                tick = not tick
            if c == "|" and not tick:
                out.append(cur.strip())
                cur = ""
            else:
                cur += c
            i += 1
        out.append(cur.strip())
        return out

    def table(self, lines, i, out):
        head = self.cells(lines[i])
        aligns = []
        for c in self.cells(lines[i + 1]):
            left, right = c.startswith(":"), c.endswith(":")
            aligns.append("center" if left and right else "right" if right else "left" if left else "")
        j = i + 2
        rows = []
        while j < len(lines) and lines[j].strip() and lines[j].lstrip().startswith("|"):
            rows.append(self.cells(lines[j]))
            j += 1

        def cell(tag, text, k):
            a = aligns[k] if k < len(aligns) and aligns[k] else ""
            style = f' style="text-align:{a}"' if a else ""
            return f"<{tag}{style}>{self.inline(text)}</{tag}>"

        html = ["<table>", "<thead><tr>" + "".join(cell("th", c, k) for k, c in enumerate(head)) + "</tr></thead>", "<tbody>"]
        for r in rows:
            r = (r + [""] * len(head))[: len(head)]
            html.append("<tr>" + "".join(cell("td", c, k) for k, c in enumerate(r)) + "</tr>")
        html += ["</tbody>", "</table>"]
        out.append("\n".join(html))
        return j

    def paragraph(self, lines, i, out):
        j = i
        buf = []
        while j < len(lines):
            line = lines[j]
            if not line.strip() or FENCE.match(line) or (HEADING.match(line) and not line.startswith(" ")):
                break
            if j > i and (line.startswith(">") or (ITEM.match(line) and ITEM.match(line).group("sp"))):
                break
            if j > i and line.lstrip().startswith("|") and j + 1 < len(lines) and TABLE_SEP.match(lines[j + 1]):
                break
            if j > i and RULE.match(line):
                break
            buf.append(line.strip() if j > i else line.lstrip())
            j += 1
        out.append("<p>" + self.inline("\n".join(buf)) + "</p>")
        return j

    def list(self, lines, i, out):
        first = ITEM.match(lines[i])
        base = len(first.group("ind"))
        ordered = first.group("mark")[0].isdigit()
        items = []
        n = len(lines)
        j = i
        while j < n:
            m = ITEM.match(lines[j])
            if not m or len(m.group("ind")) != base or m.group("mark")[0].isdigit() != ordered:
                break
            width = len(m.group("ind")) + len(m.group("mark")) + max(1, len(m.group("sp")) if m.group("sp") else 1)
            width = min(width, len(m.group("ind")) + len(m.group("mark")) + 4)
            body = [lines[j][m.end():]] if m.group("sp") else [""]
            j += 1
            while j < n:
                line = lines[j]
                if not line.strip():
                    # a blank line stays in the item if what follows is indented under it
                    k = j
                    while k < n and not lines[k].strip():
                        k += 1
                    if k < n and len(lines[k]) - len(lines[k].lstrip()) >= width:
                        body += [""] * (k - j)
                        j = k
                        continue
                    break
                ind = len(line) - len(line.lstrip())
                nm = ITEM.match(line)
                if ind >= width:
                    body.append(line[width:])
                elif nm and ind <= base:
                    break
                elif nm and ind < width:
                    break
                elif HEADING.match(line) or FENCE.match(line) or line.startswith(">") or line.startswith("|"):
                    break
                else:
                    body.append(line.strip())  # a lazy continuation of the item's paragraph
                j += 1
            items.append(body)
            # skip the blank lines between two items of one list
            k = j
            while k < n and not lines[k].strip():
                k += 1
            nm = ITEM.match(lines[k]) if k < n else None
            if k > j and nm and len(nm.group("ind")) == base and nm.group("mark")[0].isdigit() == ordered:
                j = k
        tag = "ol" if ordered else "ul"
        start = ""
        if ordered:
            num = int(re.match(r"\d+", first.group("mark")).group(0))
            start = f' start="{num}"' if num != 1 else ""
        html = [f"<{tag}{start}>"]
        for body in items:
            inner = self.blocks(body)
            single = re.fullmatch(r"<p>(.*)</p>", inner, re.S)
            if single and "<p>" not in single.group(1):
                inner = single.group(1)
            html.append(f"<li>{inner}</li>")
        html.append(f"</{tag}>")
        out.append("\n".join(html))
        return j


def convert(text, resolve=None, heading_hook=None):
    """(html, headings) of a Markdown text; see the module's docstring."""
    c = Converter(resolve, heading_hook)
    html = c.blocks(text.replace("\r\n", "\n").replace("\t", "    ").split("\n"), top=True)
    return html, c.headings


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    print(convert(open(sys.argv[1], encoding="utf-8").read())[0])
