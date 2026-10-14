"""
Generator for Janus Brand Identity (The Bitemporal Eye / "Look back. Look ahead.")
Generates:
  - Master editable SVG (with layers, comments, easy styling)
  - All brand SVG variants (symbol, horizontal, vertical, icon) in standard, claro, escuro
  - High-res PNGs across all scales (16 to 2048), drawn by headless Chrome or Edge
    (PyMuPDF, the fallback, draws gradients black and ignores clip paths)
  - Multi-resolution Windows .ico and macOS .icns
  - Site assets and resources sync
"""
import os, sys, shutil, subprocess, tempfile
from pathlib import Path
from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
BRANDING = ROOT / "branding"
SVG_DIR = BRANDING / "svg"
PNG_DIR = BRANDING / "png"
ICONES_DIR = BRANDING / "icones"
FAVICON_DIR = BRANDING / "favicon"
RESOURCES = ROOT / "resources"
SITE = ROOT / "site"
SITE_IMG = SITE / "assets" / "img"

for d in [SVG_DIR, PNG_DIR, ICONES_DIR, FAVICON_DIR, RESOURCES, SITE_IMG]:
    d.mkdir(parents=True, exist_ok=True)

# -------------------------------------------------------------
# 1. SVG Definitions for the Eye Symbol
# -------------------------------------------------------------
# Colors
BLUE = "#4296FA"
BLUE_HI = "#6AADFF"
BLUE_DARK = "#2F6FE0"  # end of the past's gradient: still visible on black
NAVY = "#1D3B6B"       # inside of the iris (past): a deep blue that reads on black and on the icon's navy
NAVY_DEEP = "#144A63"  # inside of the iris (future)
CYAN = "#00F2FE"
ORANGE = "#F29A38"
ORANGE_GLOW = "#FF6D00"
WHITE = "#FFFFFF"

# Pale colors for 'claro'
PALE_BLUE = "#CEE3FF"
PALE_CYAN = "#A5F3FC"
PALE_ORANGE = "#FDBA74"

# Dark colors for 'escuro' (on light backgrounds)
DARK_BLUE = "#1D4ED8"
DARK_NAVY = "#0F172A"
DARK_CYAN = "#0284C7"

def make_master_editable_svg():
    """
    Returns a human-editable, clean SVG with full layer grouping and documentation.
    Can be opened directly in Illustrator, Figma, Inkscape, or any code editor.
    """
    return f'''<?xml version="1.0" encoding="UTF-8"?>
<!-- 
  ============================================================
  JANUS - MASTER EDITABLE BRAND SYMBOL (THE BITEMPORAL EYE)
  "Look back. Look ahead."
  ============================================================
  Layers:
    #fundo           - Fundo transparente ou de contraste
    #contornos       - Asas externas do olho (passado à esquerda, futuro à direita)
    #iris-passado    - Estratos da íris do passado (camadas históricas)
    #iris-futuro     - Estratos da íris do futuro (camadas de mudança/anomalia)
    #cursor-data     - Sensor focal central (Marcador de data laranja #F29A38)
    #meridianos      - Eixo temporal vertical e setas direcionais
-->
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 200" width="200" height="200">
  <title>Janus — Símbolo Master Editável</title>
  
  <defs>
    <!-- Gradiente da asa do passado (olhar para trás) -->
    <linearGradient id="grad-passado" x1="100%" y1="0%" x2="0%" y2="100%">
      <stop offset="0%" stop-color="{BLUE}"/>
      <stop offset="100%" stop-color="{BLUE_DARK}"/>
    </linearGradient>

    <!-- Gradiente da asa do futuro (olhar para frente) -->
    <linearGradient id="grad-futuro" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="{BLUE_HI}"/>
      <stop offset="100%" stop-color="{CYAN}"/>
    </linearGradient>

    <!-- Gradiente do sensor central (data presente) -->
    <radialGradient id="grad-sensor" cx="40%" cy="40%" r="60%">
      <stop offset="0%" stop-color="{WHITE}"/>
      <stop offset="40%" stop-color="{ORANGE}"/>
      <stop offset="100%" stop-color="{ORANGE_GLOW}"/>
    </radialGradient>

    <!-- Brilho sutil no cursor de data -->
    <filter id="glow-data" x="-30%" y="-30%" width="160%" height="160%">
      <feGaussianBlur stdDeviation="3" result="blur"/>
      <feComposite in="SourceGraphic" in2="blur" operator="over"/>
    </filter>
  </defs>

  <g id="janus-eye-master">
    <!-- ================= CAMADA: CONTORNOS EXTERNOS ================= -->
    <g id="contornos">
      <!-- Asa Esquerda (Passado / Look Back) -->
      <path id="asa-esquerda" 
            d="M 100, 48 C 65, 48 35, 75 22, 100 C 35, 125 65, 152 100, 152"
            fill="none" stroke="url(#grad-passado)" stroke-width="7" stroke-linecap="round"/>

      <!-- Asa Direita (Futuro / Look Ahead) -->
      <path id="asa-direita" 
            d="M 100, 48 C 135, 48 165, 75 178, 100 C 165, 125 135, 152 100, 152"
            fill="none" stroke="url(#grad-futuro)" stroke-width="7" stroke-linecap="round"/>
    </g>

    <!-- ================= CAMADA: ÍRIS E ESTRATOS ================= -->
    <g id="iris">
      <!-- Íris Esquerda (Passado) -->
      <g id="iris-passado">
        <path d="M 100, 64 A 36,36 0 0,0 100, 136 Z" fill="url(#grad-passado)"/>
        <path d="M 100, 76 A 24,24 0 0,0 100, 124 Z" fill="{NAVY}"/>
      </g>

      <!-- Íris Direita (Futuro) -->
      <g id="iris-futuro">
        <path d="M 100, 64 A 36,36 0 0,1 100, 136 Z" fill="url(#grad-futuro)"/>
        <path d="M 100, 76 A 24,24 0 0,1 100, 124 Z" fill="{NAVY_DEEP}"/>
      </g>
    </g>

    <!-- ================= CAMADA: SETAS E EIXOS ================= -->
    <g id="meridianos">
      <!-- Marcadores verticais do meridiano do tempo -->
      <line id="eixo-topo" x1="100" y1="32" x2="100" y2="46" stroke="{ORANGE}" stroke-width="5" stroke-linecap="round"/>
      <line id="eixo-base" x1="100" y1="154" x2="100" y2="168" stroke="{ORANGE}" stroke-width="5" stroke-linecap="round"/>

      <!-- Setas direcionais no horizonte -->
      <path id="seta-passado" d="M 16, 100 L 31, 90 L 31, 110 Z" fill="{BLUE}"/>
      <path id="seta-futuro"  d="M 184, 100 L 169, 90 L 169, 110 Z" fill="{CYAN}"/>
    </g>

    <!-- ================= CAMADA: SENSOR CENTRAL ("NOW") ================= -->
    <g id="cursor-data" filter="url(#glow-data)">
      <circle id="sensor-anel-externo" cx="100" cy="100" r="10" fill="url(#grad-sensor)"/>
      <circle id="sensor-ponto-focal" cx="100" cy="100" r="4" fill="{WHITE}"/>
    </g>
  </g>
</svg>
'''

def make_symbol_svg(variant="padrao", uid="sym"):
    """Generates standard, claro or escuro SVG symbol."""
    c_blue = BLUE if variant != "escuro" else DARK_BLUE
    c_blue_hi = BLUE_HI if variant != "escuro" else BLUE
    c_dark = BLUE_DARK if variant != "escuro" else DARK_NAVY
    c_cyan = CYAN if variant != "escuro" else DARK_CYAN
    c_orange = ORANGE
    c_white = WHITE
    c_iris_inner_l = NAVY if variant != "escuro" else "#0F172A"
    c_iris_inner_r = NAVY_DEEP if variant != "escuro" else "#1E293B"

    if variant == "claro":
        c_blue = PALE_BLUE
        c_blue_hi = WHITE
        c_cyan = PALE_CYAN
        c_orange = PALE_ORANGE

    return f'''<g id="simbolo">
  <defs>
    <linearGradient id="{uid}-pass" x1="100%" y1="0%" x2="0%" y2="100%">
      <stop offset="0%" stop-color="{c_blue}"/>
      <stop offset="100%" stop-color="{c_dark}"/>
    </linearGradient>
    <linearGradient id="{uid}-fut" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="{c_blue_hi}"/>
      <stop offset="100%" stop-color="{c_cyan}"/>
    </linearGradient>
  </defs>
  <!-- Contornos do Olho -->
  <path d="M 100, 48 C 65, 48 35, 75 22, 100 C 35, 125 65, 152 100, 152" fill="none" stroke="url(#{uid}-pass)" stroke-width="7" stroke-linecap="round"/>
  <path d="M 100, 48 C 135, 48 165, 75 178, 100 C 165, 125 135, 152 100, 152" fill="none" stroke="url(#{uid}-fut)" stroke-width="7" stroke-linecap="round"/>
  <!-- Íris Esquerda (Passado) -->
  <path d="M 100, 64 A 36,36 0 0,0 100, 136 Z" fill="url(#{uid}-pass)"/>
  <path d="M 100, 76 A 24,24 0 0,0 100, 124 Z" fill="{c_iris_inner_l}"/>
  <!-- Íris Direita (Futuro) -->
  <path d="M 100, 64 A 36,36 0 0,1 100, 136 Z" fill="url(#{uid}-fut)"/>
  <path d="M 100, 76 A 24,24 0 0,1 100, 124 Z" fill="{c_iris_inner_r}"/>
  <!-- Meridianos e Setas -->
  <line x1="100" y1="32" x2="100" y2="46" stroke="{c_orange}" stroke-width="5" stroke-linecap="round"/>
  <line x1="100" y1="154" x2="100" y2="168" stroke="{c_orange}" stroke-width="5" stroke-linecap="round"/>
  <path d="M 16, 100 L 31, 90 L 31, 110 Z" fill="{c_blue}"/>
  <path d="M 184, 100 L 169, 90 L 169, 110 Z" fill="{c_cyan}"/>
  <!-- Sensor Central -->
  <circle cx="100" cy="100" r="10" fill="{c_orange}"/>
  <circle cx="100" cy="100" r="4" fill="{c_white}"/>
</g>'''

def make_wordmark_group(color=BLUE, cid="wm"):
    """Wordmark 'JANUS' using the geometric monospace font of the project."""
    return f'''<g id="nome">
  <g fill="none" stroke="{color}" stroke-width="8.5" stroke-linejoin="miter" stroke-miterlimit="12">
    <path transform="translate(0,0)" d="M43.75,-20 L43.75,76.0 A19.75,19.75 0 0 1 4.25,76.0 L4.25,68.0"/>
    <path transform="translate(72,0)" d="M-1.75,140 L27.0,-7.65 L55.75,140 M10.8,66.0 L43.2,66.0"/>
    <path transform="translate(150,0)" d="M4.25,130 L4.25,-1.7 L43.75,101.7 L43.75,-30"/>
    <path transform="translate(222,0)" d="M4.25,-20 L4.25,76.0 A19.75,19.75 0 0 0 43.75,76.0 L43.75,-20"/>
    <path transform="translate(294,0)" d="M43.75,29.0 L43.75,24.0 A19.75,19.75 0 0 0 4.25,24.0 C4.25,48.0 43.75,52.0 43.75,76.0 A19.75,19.75 0 0 1 4.25,76.0 L4.25,71.0"/>
  </g>
</g>'''

def make_horizontal_svg(variant="padrao", uid="h"):
    c_text = BLUE if variant == "padrao" else (PALE_BLUE if variant == "claro" else DARK_BLUE)
    sym = make_symbol_svg(variant, f"{uid}-s")
    wm = make_wordmark_group(c_text, f"{uid}-w")
    # viewBox: -10 0 620 200 (Symbol 200x200 on left, Wordmark on right)
    return f'''<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 600 200" width="600" height="200">
  <title>Janus</title>
  <g transform="translate(0, 0)">
    {sym}
  </g>
  <g transform="translate(220, 52) scale(1.0)">
    {wm}
  </g>
</svg>'''

def make_vertical_svg(variant="padrao", uid="v"):
    c_text = BLUE if variant == "padrao" else (PALE_BLUE if variant == "claro" else DARK_BLUE)
    sym = make_symbol_svg(variant, f"{uid}-s")
    wm = make_wordmark_group(c_text, f"{uid}-w")
    # viewBox: 0 0 360 320 (Symbol centered at top, wordmark centered below)
    return f'''<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 360 400" width="360" height="400">
  <title>Janus</title>
  <g transform="translate(80, 10)">
    {sym}
  </g>
  <g transform="translate(4, 248) scale(0.98)">
    {wm}
  </g>
</svg>'''

def make_app_icon_svg(rx=44, uid="ico"):
    """Modern squircle / square app icon for desktop, dock, and installer."""
    sym = make_symbol_svg("padrao", f"{uid}-s")
    return f'''<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 200" width="200" height="200">
  <title>Janus</title>
  <defs>
    <linearGradient id="{uid}-bg" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="#162447"/>
      <stop offset="100%" stop-color="#0A1120"/>
    </linearGradient>
  </defs>
  <!-- Background Tile -->
  <rect x="0" y="0" width="200" height="200" rx="{rx}" fill="url(#{uid}-bg)"/>
  <rect x="1" y="1" width="198" height="198" rx="{rx-1}" fill="none" stroke="#2563EB" stroke-width="1.5" stroke-opacity="0.4"/>
  <!-- Centered Symbol (Scale 0.85) -->
  <g transform="translate(15, 15) scale(0.85)">
    {sym}
  </g>
</svg>'''

# -------------------------------------------------------------
# 2. Generate and write all master SVG files
# -------------------------------------------------------------
print("Writing Master SVGs...")
# 1. Master editable SVG
(BRANDING / "janus-olho-editavel.svg").write_text(make_master_editable_svg(), encoding="utf-8")

# 2. Symbol variants
for var, suf in [("padrao", ""), ("claro", "-claro"), ("escuro", "-escuro")]:
    content = f'''<?xml version="1.0" encoding="UTF-8"?>\n<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 200" width="200" height="200"><title>Janus</title>{make_symbol_svg(var, f"s{suf}")}</svg>\n'''
    (SVG_DIR / f"janus-simbolo{suf}.svg").write_text(content, encoding="utf-8")

# 3. Horizontal lockups
for var, suf in [("padrao", ""), ("claro", "-claro"), ("escuro", "-escuro")]:
    (SVG_DIR / f"janus-horizontal{suf}.svg").write_text(make_horizontal_svg(var, f"h{suf}"), encoding="utf-8")

# 4. Vertical lockups
for var, suf in [("padrao", ""), ("claro", "-claro"), ("escuro", "-escuro")]:
    (SVG_DIR / f"janus-vertical{suf}.svg").write_text(make_vertical_svg(var, f"v{suf}"), encoding="utf-8")

# 5. Icon variants
(SVG_DIR / "janus-icone.svg").write_text(make_app_icon_svg(rx=44, uid="ico-sq"), encoding="utf-8")
(SVG_DIR / "janus-icone-quadrado.svg").write_text(make_app_icon_svg(rx=0, uid="ico-flat"), encoding="utf-8")

# 6. Wordmark standalone
for var, suf in [("padrao", ""), ("claro", "-claro"), ("escuro", "-escuro")]:
    c_text = BLUE if var == "padrao" else (PALE_BLUE if var == "claro" else DARK_BLUE)
    wm = f'''<?xml version="1.0" encoding="UTF-8"?>\n<svg xmlns="http://www.w3.org/2000/svg" viewBox="-8 -36 366 184" width="366" height="184"><title>Janus</title>{make_wordmark_group(c_text, f"wm{suf}")}</svg>\n'''
    (SVG_DIR / f"janus-nome{suf}.svg").write_text(wm, encoding="utf-8")

print("All SVGs successfully written to branding/svg/!")

# -------------------------------------------------------------
# 3. Rasterization to PNG with PyMuPDF
# -------------------------------------------------------------
print("Rasterizing PNGs via PyMuPDF...")

def find_browser():
    for p in [os.environ.get("CHROME", ""),
              r"C:\Program Files\Google\Chrome\Application\chrome.exe",
              r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
              "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
              shutil.which("google-chrome") or "", shutil.which("chromium") or ""]:
        if p and os.path.exists(p):
            return p
    return None

BROWSER = find_browser()
if not BROWSER:
    print("No Chrome or Edge found (set CHROME): PyMuPDF draws the gradients black.")

def svg_size(svg_path):
    import re
    head = Path(svg_path).read_text(encoding="utf-8")[:600]
    m = re.search(r'viewBox="([-\d.]+) ([-\d.]+) ([\d.]+) ([\d.]+)"', head)
    return float(m.group(3)), float(m.group(4))

def rasterize_svg(svg_path, out_png_path, target_width, target_height=None):
    w, h = svg_size(svg_path)
    tw = int(target_width)
    th = int(target_height or round(target_width * h / w))
    if BROWSER:
        with tempfile.TemporaryDirectory() as tmp:
            page = Path(tmp) / "page.html"
            page.write_text(f'<!doctype html><html><body style="margin:0;background:transparent">'
                            f'<img src="{Path(svg_path).resolve().as_uri()}" width="{tw}" height="{th}" '
                            f'style="display:block"></body></html>', encoding="utf-8")
            shot = Path(tmp) / "shot.png"
            subprocess.run([BROWSER, "--headless=new", "--disable-gpu", "--hide-scrollbars",
                            "--allow-file-access-from-files", "--force-device-scale-factor=1",
                            "--default-background-color=00000000", f"--window-size={tw},{th}",
                            f"--screenshot={shot}", page.as_uri()],
                           check=True, capture_output=True, timeout=120)
            Image.open(shot).convert("RGBA").crop((0, 0, tw, th)).save(out_png_path)
        return
    import pymupdf  # PyMuPDF
    doc = pymupdf.open(str(svg_path))
    page = doc[0]
    mat = pymupdf.Matrix(tw / page.rect.width, th / page.rect.height)
    page.get_pixmap(matrix=mat, alpha=True).save(str(out_png_path))
    doc.close()

# Export pngs for all svg files in branding/svg
for svg_file in SVG_DIR.glob("*.svg"):
    base_name = svg_file.stem
    for size in [512, 1024, 2048]:
        out_png = PNG_DIR / f"{base_name}-{size}.png"
        rasterize_svg(svg_file, out_png, size)

# Export icones
for size in [64, 128, 180, 192, 256, 512, 1024]:
    rasterize_svg(SVG_DIR / "janus-icone.svg", ICONES_DIR / f"icone-{size}.png", size, size)
    rasterize_svg(SVG_DIR / "janus-icone-quadrado.svg", ICONES_DIR / f"icone-quadrado-{size}.png", size, size)

# Favicons
for size in [16, 24, 32, 48]:
    rasterize_svg(SVG_DIR / "janus-icone.svg", FAVICON_DIR / f"favicon-{size}.png", size, size)
    # also svg favicon
    shutil.copy(SVG_DIR / "janus-simbolo.svg", FAVICON_DIR / f"favicon-{size}.svg")

shutil.copy(SVG_DIR / "janus-simbolo.svg", FAVICON_DIR / "favicon.svg")
shutil.copy(SVG_DIR / "janus-simbolo.svg", FAVICON_DIR / "favicon-32.svg")

# -------------------------------------------------------------
# 4. Multi-resolution Windows .ico and macOS .icns
# -------------------------------------------------------------
print("Building Windows .ico and macOS .icns...")
# Windows ICO: 16, 24, 32, 48, 64, 128, 256
ico_layers = []
for s in [16, 24, 32, 48]:
    ico_layers.append(Image.open(FAVICON_DIR / f"favicon-{s}.png"))
for s in [64, 128, 256]:
    ico_layers.append(Image.open(ICONES_DIR / f"icone-{s}.png"))

# Save ICO
ico_main = Image.open(ICONES_DIR / "icone-256.png")
ico_main.save(
    ICONES_DIR / "janus.ico",
    format="ICO",
    sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
    append_images=ico_layers[:-1]
)
shutil.copy(ICONES_DIR / "janus.ico", FAVICON_DIR / "favicon.ico")

# macOS ICNS
try:
    img_1024 = Image.open(ICONES_DIR / "icone-1024.png")
    img_1024.save(ICONES_DIR / "janus.icns", format="ICNS")
    print("janus.icns created successfully!")
except Exception as e:
    print(f"ICNS export note: {e}")

# -------------------------------------------------------------
# 5. Sync to resources/
# -------------------------------------------------------------
print("Syncing to resources/...")
shutil.copy(ICONES_DIR / "janus.ico", RESOURCES / "janus.ico")
if (ICONES_DIR / "janus.icns").exists():
    shutil.copy(ICONES_DIR / "janus.icns", RESOURCES / "janus.icns")
shutil.copy(ICONES_DIR / "icone-256.png", RESOURCES / "janus.png")

# -------------------------------------------------------------
# 6. Sync to site/
# -------------------------------------------------------------
print("Syncing to site/...")
shutil.copy(SVG_DIR / "janus-simbolo.svg", SITE_IMG / "janus-simbolo.svg")
shutil.copy(SVG_DIR / "janus-nome.svg", SITE_IMG / "janus-nome.svg")
shutil.copy(SVG_DIR / "janus-icone.svg", SITE_IMG / "janus-icone.svg")
shutil.copy(ICONES_DIR / "icone-192.png", SITE_IMG / "icon-192.png")
shutil.copy(ICONES_DIR / "icone-512.png", SITE_IMG / "icon-512.png")
shutil.copy(ICONES_DIR / "icone-180.png", SITE / "apple-touch-icon.png")
shutil.copy(FAVICON_DIR / "favicon.ico", SITE / "favicon.ico")
shutil.copy(SVG_DIR / "janus-simbolo.svg", SITE / "favicon.svg")

print("Asset pipeline complete!")
