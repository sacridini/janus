"""
Janus brand identity: the transport symbol (rewind | forward).

  ◁ | ▷   Two outlined triangles on one vertical axis, the left one (the past)
           set higher, the right one (the future) set lower, the axis running
           past both of them: the same staggered heights and the same strokes
           that run beyond the frame as the A and the N of the name.

Construction (units of the wordmark, where the cap height is 0..100):
  - stroke 8.5, butt caps, miter joins (the same as the letters)
  - axis      x = 50, from y = -30 to y = 130
  - past      (50,-20) -> (2,30)  -> (50,80)    tip at the left
  - future    (50,20)  -> (98,70) -> (50,120)   tip at the right
  - one colour: ImGui blue #4296FA (pale #CEE3FF on dark, #1D4ED8 on light)

Writes:
  branding/janus-editavel.svg      master with layers (Inkscape, Illustrator, Figma, Affinity)
  branding/janus-editavel.pdf      the same master as a vector PDF
  branding/svg/*.svg               symbol, horizontal, vertical, name, icon (padrão / claro / escuro)
  branding/png/*.png               512 / 1024 / 2048 px, transparent
  branding/pdf/*.pdf               vector PDF of every SVG
  branding/icones/                 icon PNGs, janus.ico, janus.icns
  branding/favicon/                favicon PNG/SVG/ICO
  branding/png/janus-social-1280x640.png   GitHub social preview
  and copies into resources/ (program icon) and site/ (website)

PNGs and PDFs are drawn by headless Chrome or Edge (set CHROME to choose one).
"""
import os, re, shutil, subprocess, tempfile
from pathlib import Path
from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
BRANDING = ROOT / "branding"
SVG_DIR, PNG_DIR, PDF_DIR = BRANDING / "svg", BRANDING / "png", BRANDING / "pdf"
ICONES_DIR, FAVICON_DIR = BRANDING / "icones", BRANDING / "favicon"
RESOURCES, SITE = ROOT / "resources", ROOT / "site"
SITE_IMG = SITE / "assets" / "img"
for d in [SVG_DIR, PNG_DIR, PDF_DIR, ICONES_DIR, FAVICON_DIR, RESOURCES, SITE_IMG]:
    d.mkdir(parents=True, exist_ok=True)

# ------------------------------------------------------------------ colours
BLUE = "#4296FA"        # ImGui accent
PALE_BLUE = "#CEE3FF"   # 'claro': on dark or coloured backgrounds
DARK_BLUE = "#1D4ED8"   # 'escuro': on white
WINDOW = "#0F0F0F"      # ImGui window
TILE = "#111A2E"        # app icon tile
TILE_EDGE = "#2A4A7A"   # app icon edge (ImGui title bar)
FRAME = "#1C2A3C"       # ImPlot frame
VARIANTS = [("padrao", "", BLUE), ("claro", "-claro", PALE_BLUE), ("escuro", "-escuro", DARK_BLUE)]

SW = 8.5
AXIS = "M50,-30 L50,130"
PAST = "M50,-20 L2,30 L50,80"
FUTURE = "M50,20 L98,70 L50,120"

def stroke_attrs(color, sw=SW):
    return f'fill="none" stroke="{color}" stroke-width="{sw:g}" stroke-linecap="butt" stroke-linejoin="miter" stroke-miterlimit="12"'

def symbol_group(color, sw=SW):
    return (f'<g id="simbolo" {stroke_attrs(color, sw)}>'
            f'<path d="{PAST}"/><path d="{FUTURE}"/><path d="{AXIS}"/></g>')

def name_group(color):
    return f'''<g id="nome" {stroke_attrs(color)}>
    <path d="M43.75,-20 L43.75,76.0 A19.75,19.75 0 0 1 4.25,76.0 L4.25,68.0"/>
    <path transform="translate(72,0)" d="M-1.75,140 L27.0,-7.65 L55.75,140 M10.8,66.0 L43.2,66.0"/>
    <path transform="translate(150,0)" d="M4.25,130 L4.25,-1.7 L43.75,101.7 L43.75,-30"/>
    <path transform="translate(222,0)" d="M4.25,-20 L4.25,76.0 A19.75,19.75 0 0 0 43.75,76.0 L43.75,-20"/>
    <path transform="translate(294,0)" d="M43.75,29.0 L43.75,24.0 A19.75,19.75 0 0 0 4.25,24.0 C4.25,48.0 43.75,52.0 43.75,76.0 A19.75,19.75 0 0 1 4.25,76.0 L4.25,71.0"/>
  </g>'''

def svg_doc(viewbox, body, w=None, h=None):
    _, _, vw, vh = viewbox.split()
    w = w or vw; h = h or vh
    return (f'<?xml version="1.0" encoding="UTF-8"?>\n'
            f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="{viewbox}" width="{w}" height="{h}"><title>Janus</title>\n'
            f'{body}\n</svg>\n')

# The symbol occupies x -4..104, y -30..130 (stroke included); centre (50, 50).
SYMBOL_VB = "-34 -34 168 168"
# Horizontal: the symbol 44 units to the left of the J, as wide a gap as between letters.
HORIZ_VB, HORIZ_SYM = "-160 -40 520 190", "translate(-148,0)"
# Vertical: the symbol centred over the name.
VERT_VB, VERT_SYM = "-20 -250 386 400", "translate(123,-210)"
NAME_VB = "-8 -36 366 184"

def icon_svg(rx=44, sw=SW, scale=0.72, edge=True):
    t = 100 - 50 * scale
    border = (f'<rect x="1" y="1" width="198" height="198" rx="{max(rx-1,0)}" fill="none" stroke="{TILE_EDGE}" stroke-width="2"/>'
              if edge else "")
    body = (f'<rect width="200" height="200" rx="{rx}" fill="{TILE}"/>{border}'
            f'<g transform="translate({t:g},{t:g}) scale({scale:g})">{symbol_group(BLUE, sw)}</g>')
    return svg_doc("0 0 200 200", body)

def master_svg():
    return f'''<?xml version="1.0" encoding="UTF-8"?>
<!--
  JANUS · arquivo mestre editável
  ◁ | ▷  retroceder e avançar, desencontrados na altura como as letras do nome.

  Camadas (Inkscape: camadas de verdade; Illustrator, Figma e Affinity: grupos com nome):
    guias            linhas de construção (escondida): topo 0, base 100, hastes -30 e 130, folga de 44
    simbolo          eixo, passado (◁, mais alto) e futuro (▷, mais baixo)
    nome             o wordmark JANUS

  Regras: traço 8,5 · pontas retas (butt) · cantos em quina (miter) · uma cor só.
  Azul ImGui {BLUE} · claro {PALE_BLUE} (sobre escuro) · escuro {DARK_BLUE} (sobre branco).
  Gere todas as versões de novo com: python tools/generate_brand.py
-->
<svg xmlns="http://www.w3.org/2000/svg" xmlns:inkscape="http://www.inkscape.org/namespaces/inkscape"
     viewBox="{HORIZ_VB}" width="1040" height="380">
  <title>Janus</title>
  <g id="guias" inkscape:groupmode="layer" inkscape:label="guias" style="display:none" fill="none" stroke="#F29A38" stroke-width="0.6" stroke-dasharray="4 3">
    <path d="M-160,0 L360,0 M-160,100 L360,100"/>
    <path d="M-160,-30 L360,-30 M-160,130 L360,130"/>
    <path d="M-44,-40 L-44,150 M0,-40 L0,150"/>
  </g>
  <g id="camada-simbolo" inkscape:groupmode="layer" inkscape:label="simbolo" transform="{HORIZ_SYM}">
    <g id="simbolo" {stroke_attrs(BLUE)}>
      <path id="passado" d="{PAST}"/>
      <path id="futuro" d="{FUTURE}"/>
      <path id="eixo" d="{AXIS}"/>
    </g>
  </g>
  <g id="camada-nome" inkscape:groupmode="layer" inkscape:label="nome">
    {name_group(BLUE)}
  </g>
</svg>
'''

# ------------------------------------------------------------------ SVGs
print("SVG...")
(BRANDING / "janus-editavel.svg").write_text(master_svg(), encoding="utf-8")
for var, suf, col in VARIANTS:
    (SVG_DIR / f"janus-simbolo{suf}.svg").write_text(svg_doc(SYMBOL_VB, symbol_group(col)), encoding="utf-8")
    (SVG_DIR / f"janus-horizontal{suf}.svg").write_text(
        svg_doc(HORIZ_VB, f'<g transform="{HORIZ_SYM}">{symbol_group(col)}</g>\n{name_group(col)}'), encoding="utf-8")
    (SVG_DIR / f"janus-vertical{suf}.svg").write_text(
        svg_doc(VERT_VB, f'<g transform="{VERT_SYM}">{symbol_group(col)}</g>\n{name_group(col)}'), encoding="utf-8")
    (SVG_DIR / f"janus-nome{suf}.svg").write_text(svg_doc(NAME_VB, name_group(col)), encoding="utf-8")
(SVG_DIR / "janus-icone.svg").write_text(icon_svg(rx=44), encoding="utf-8")
(SVG_DIR / "janus-icone-quadrado.svg").write_text(icon_svg(rx=0, edge=False), encoding="utf-8")

# 48 px: the drawing with a heavier stroke. 16, 24 and 32 px are drawn pixel by pixel below.
SMALL = {48: dict(rx=10, sw=12, scale=0.74, edge=True)}
tmp_small = {}
for s, kw in SMALL.items():
    p = FAVICON_DIR / f"favicon-{s}.svg"
    p.write_text(icon_svg(**kw), encoding="utf-8")
    tmp_small[s] = p
# The browser-tab favicon: the symbol alone, a little heavier, no tile.
fav = svg_doc(SYMBOL_VB, symbol_group(BLUE, 12))
(FAVICON_DIR / "favicon.svg").write_text(fav, encoding="utf-8")

# ------------------------------------------------------------------ browser
def find_browser():
    for p in [os.environ.get("CHROME", ""),
              r"C:\Program Files\Google\Chrome\Application\chrome.exe",
              r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
              "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
              shutil.which("google-chrome") or "", shutil.which("chromium") or ""]:
        if p and os.path.exists(p):
            return p
    raise SystemExit("Chrome or Edge not found: set CHROME to its path.")
BROWSER = find_browser()

def chrome(args, url):
    subprocess.run([BROWSER, "--headless=new", "--disable-gpu", "--hide-scrollbars", "--no-sandbox",
                    "--allow-file-access-from-files", *args, url], check=True, capture_output=True, timeout=120)

def svg_size(path):
    m = re.search(r'viewBox="([-\d.]+) ([-\d.]+) ([\d.]+) ([\d.]+)"', Path(path).read_text(encoding="utf-8")[:2000])
    return float(m.group(3)), float(m.group(4))

def screenshot_html(html, out, w, h):
    with tempfile.TemporaryDirectory() as tmp:
        page = Path(tmp) / "page.html"
        page.write_text(html, encoding="utf-8")
        shot = Path(tmp) / "shot.png"
        # a window larger than the image, cropped after: headless Chrome keeps part of the
        # window for itself and will not draw one smaller than ~100 px
        chrome(["--force-device-scale-factor=1", "--default-background-color=00000000",
                f"--window-size={max(w, 200)},{h + 400}", f"--screenshot={shot}"], page.as_uri())
        Image.open(shot).convert("RGBA").crop((0, 0, w, h)).save(out)

def rasterize(svg, out, w, h=None):
    vw, vh = svg_size(svg)
    h = h or round(w * vh / vw)
    screenshot_html(f'<!doctype html><body style="margin:0;background:transparent">'
                    f'<img src="{Path(svg).resolve().as_uri()}" width="{w}" height="{h}" style="display:block">', out, w, h)

def to_pdf(svg, out):
    vw, vh = svg_size(svg)
    inline = re.sub(r'^<\?xml[^>]*>\s*', '', Path(svg).read_text(encoding="utf-8"))
    inline = re.sub(r'<!--.*?-->\s*', '', inline, flags=re.S)
    inline = re.sub(r'width="[\d.]+" height="[\d.]+"', f'width="{vw}pt" height="{vh}pt"', inline, count=1)
    html = (f'<!doctype html><style>@page{{size:{vw}pt {vh}pt;margin:0}}html,body{{margin:0}}'
            f'svg{{display:block}}</style>{inline}')
    with tempfile.TemporaryDirectory() as tmp:
        page = Path(tmp) / "page.html"
        page.write_text(html, encoding="utf-8")
        chrome(["--no-pdf-header-footer", f"--print-to-pdf={Path(out).resolve()}"], page.as_uri())

# ------------------------------------------------------------------ PNG, PDF
print("PNG and PDF...")
for svg in sorted(SVG_DIR.glob("*.svg")):
    for size in (512, 1024, 2048):
        rasterize(svg, PNG_DIR / f"{svg.stem}-{size}.png", size)
    to_pdf(svg, PDF_DIR / f"{svg.stem}.pdf")
to_pdf(BRANDING / "janus-editavel.svg", BRANDING / "janus-editavel.pdf")

for size in (64, 128, 180, 192, 256, 512, 1024):
    rasterize(SVG_DIR / "janus-icone.svg", ICONES_DIR / f"icone-{size}.png", size, size)
    rasterize(SVG_DIR / "janus-icone-quadrado.svg", ICONES_DIR / f"icone-quadrado-{size}.png", size, size)
for s, p in tmp_small.items():
    rasterize(p, FAVICON_DIR / f"favicon-{s}.png", s, s)

def pixel_icon(n, ax, a0, a1, top, d, corner):
    """1-px lines on the pixel grid, as icons were drawn in the 90s: axis at column ax
    from row a0 to a1; the past's top vertex at row top, the future's bottom vertex at
    the mirror row; d is how far the tips reach from the axis."""
    from PIL import ImageColor
    im = Image.new("RGBA", (n, n), TILE)
    px = im.load()
    BLUE = ImageColor.getcolor(globals()["BLUE"], "RGBA")
    for y in range(a0, a1 + 1):
        px[ax, y] = BLUE
    ftop = (a0 + a1) - (top + 2 * d)          # the future, turned 180 degrees about the centre
    for i in range(d + 1):
        px[ax - i, top + i] = px[ax - i, top + 2 * d - i] = BLUE
        px[ax + i, ftop + i] = px[ax + i, ftop + 2 * d - i] = BLUE
    clear = (0, 0, 0, 0)
    for cx, cy in ((0, 0), (n - 1, 0), (0, n - 1), (n - 1, n - 1)):
        for k in range(corner):
            px[cx, abs(cy - k)] = px[abs(cx - k), cy] = clear
    return im
pixel_icon(16, 7, 1, 14, 2, 4, 1).save(FAVICON_DIR / "favicon-16.png")
pixel_icon(24, 11, 2, 21, 3, 6, 2).save(FAVICON_DIR / "favicon-24.png")
pixel_icon(16, 7, 1, 14, 2, 4, 1).resize((32, 32), Image.NEAREST).save(FAVICON_DIR / "favicon-32.png")

# ------------------------------------------------------------------ ICO, ICNS
print("ICO and ICNS...")
layers = [Image.open(FAVICON_DIR / f"favicon-{s}.png") for s in (16, 24, 32, 48)]
layers += [Image.open(ICONES_DIR / f"icone-{s}.png") for s in (64, 128, 256)]
# Pillow writes each size from the image of that exact size when it is given
layers[-1].save(ICONES_DIR / "janus.ico", format="ICO",
                sizes=[im.size for im in layers], append_images=layers[:-1])
shutil.copy(ICONES_DIR / "janus.ico", FAVICON_DIR / "favicon.ico")
try:
    Image.open(ICONES_DIR / "icone-1024.png").save(ICONES_DIR / "janus.icns", format="ICNS")
except Exception as e:
    print(f"ICNS: {e}")

# ------------------------------------------------------------------ social cards
print("Social cards...")
def card(out, w, h, lockup_w):
    tag_y = h * 0.73
    html = f'''<!doctype html>
<link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=IBM+Plex+Mono&display=block">
<body style="margin:0;width:{w}px;height:{h}px;background:{WINDOW};position:relative;overflow:hidden">
<img src="{(SVG_DIR / 'janus-horizontal.svg').resolve().as_uri()}" style="position:absolute;width:{lockup_w}px;left:{(w-lockup_w)/2}px;top:{h*0.47 - lockup_w*190/520/2 - h*0.04}px">
<div style="position:absolute;left:0;right:0;top:{tag_y - 14}px;text-align:center;font:24px 'IBM Plex Mono',Consolas,'DejaVu Sans Mono',monospace;letter-spacing:.18em;color:{BLUE}">RASTER TIME SERIES ANALYSIS</div>
<div style="position:absolute;left:0;right:0;bottom:0;height:10px;background:{FRAME}"></div>'''
    screenshot_html(html, out, w, h)
card(SITE_IMG / "og.png", 1200, 630, 820)
card(PNG_DIR / "janus-social-1280x640.png", 1280, 640, 840)

# ------------------------------------------------------------------ sync
print("resources/ and site/...")
shutil.copy(ICONES_DIR / "janus.ico", RESOURCES / "janus.ico")
if (ICONES_DIR / "janus.icns").exists():
    shutil.copy(ICONES_DIR / "janus.icns", RESOURCES / "janus.icns")
shutil.copy(ICONES_DIR / "icone-256.png", RESOURCES / "janus.png")
for f in ("janus-simbolo.svg", "janus-nome.svg", "janus-icone.svg"):
    shutil.copy(SVG_DIR / f, SITE_IMG / f)
shutil.copy(ICONES_DIR / "icone-192.png", SITE_IMG / "icon-192.png")
shutil.copy(ICONES_DIR / "icone-512.png", SITE_IMG / "icon-512.png")
shutil.copy(ICONES_DIR / "icone-180.png", SITE / "apple-touch-icon.png")
shutil.copy(FAVICON_DIR / "favicon.ico", SITE / "favicon.ico")
shutil.copy(FAVICON_DIR / "favicon.svg", SITE / "favicon.svg")
print("Done.")
