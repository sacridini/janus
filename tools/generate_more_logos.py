import math, os

ARTIFACT_DIR = r"C:\Users\eduar\.gemini\antigravity\brain\4748f837-1fb5-40b7-b843-58222e575296"
REPO_DIR = r"c:\Users\eduar\Documents\dev\janus"
PROPOSTAS_DIR = os.path.join(REPO_DIR, "branding", "novas-opcoes-lote2")
os.makedirs(PROPOSTAS_DIR, exist_ok=True)
os.makedirs(ARTIFACT_DIR, exist_ok=True)

# Cores oficiais da interface do Janus
WIN = "#0F0F0F"
PANEL = "#1C2A3C"
BLUE = "#4296FA"
BLUE_HI = "#6AADFF"
PALE = "#CEE3FF"
DATE_ORANGE = "#F29A38"  # O marcador de data do ImPlot no Janus!
CYAN = "#00F2FE"

# -------------------------------------------------------------
# OPÇÃO A: A Ampulheta de Pixels (The Pixel Hourglass)
# -------------------------------------------------------------
def make_logo_a(uid="opt-a", size=200):
    # An elegant architectural hourglass made of modern glass triangles,
    # where the sand inside is a cascade of structured raster pixels,
    # with the date marker orange glowing at the bottleneck!
    svg = f'''<svg viewBox="0 0 200 200" width="{size}" height="{size}" xmlns="http://www.w3.org/2000/svg">
  <defs>
    <linearGradient id="{uid}-frame" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="#4296FA"/>
      <stop offset="100%" stop-color="#1C2A3C"/>
    </linearGradient>
    <linearGradient id="{uid}-glow" x1="0%" y1="0%" x2="0%" y2="100%">
      <stop offset="0%" stop-color="#F29A38"/>
      <stop offset="100%" stop-color="#FF5722"/>
    </linearGradient>
  </defs>
  
  <g transform="translate(0, 0)">
    <!-- Top & Bottom Plates -->
    <rect x="52" y="24" width="96" height="8" rx="4" fill="#4296FA"/>
    <rect x="52" y="168" width="96" height="8" rx="4" fill="#4296FA"/>
    
    <!-- Outer Glass Vessel (Geometric Facets) -->
    <!-- Top bulb -->
    <path d="M 58, 32 L 142, 32 L 108, 92 L 92, 92 Z" fill="#1C2A3C" opacity="0.6" stroke="#4296FA" stroke-width="2.5" stroke-linejoin="round"/>
    <!-- Bottom bulb -->
    <path d="M 92, 108 L 108, 108 L 142, 168 L 58, 168 Z" fill="#1C2A3C" opacity="0.6" stroke="#4296FA" stroke-width="2.5" stroke-linejoin="round"/>
    
    <!-- Top Chamber: Past Pixels (Stacking Cube of Rasters) -->
    <!-- Row 1 -->
    <rect x="68" y="40" width="10" height="10" rx="2" fill="#6AADFF"/>
    <rect x="82" y="40" width="10" height="10" rx="2" fill="#4296FA"/>
    <rect x="96" y="40" width="10" height="10" rx="2" fill="#294A7A"/>
    <rect x="110" y="40" width="10" height="10" rx="2" fill="#4296FA"/>
    <rect x="124" y="40" width="10" height="10" rx="2" fill="#6AADFF"/>
    <!-- Row 2 -->
    <rect x="75" y="54" width="10" height="10" rx="2" fill="#4296FA"/>
    <rect x="89" y="54" width="10" height="10" rx="2" fill="#CEE3FF"/>
    <rect x="103" y="54" width="10" height="10" rx="2" fill="#4296FA"/>
    <rect x="117" y="54" width="10" height="10" rx="2" fill="#294A7A"/>
    <!-- Row 3 -->
    <rect x="82" y="68" width="10" height="10" rx="2" fill="#294A7A"/>
    <rect x="96" y="68" width="10" height="10" rx="2" fill="#4296FA"/>
    <rect x="110" y="68" width="10" height="10" rx="2" fill="#6AADFF"/>
    
    <!-- Central Bottleneck: The "Now" / Date Cursor -->
    <circle cx="100" cy="100" r="7" fill="url(#{uid}-glow)"/>
    <circle cx="100" cy="100" r="3" fill="#FFFFFF"/>
    
    <!-- Flowing Stream of Pixels through the neck -->
    <rect x="97" y="84" width="6" height="6" rx="1.5" fill="#F29A38"/>
    <rect x="97" y="112" width="6" height="6" rx="1.5" fill="#F29A38"/>
    
    <!-- Bottom Chamber: Accumulating Future / History -->
    <!-- Mound of pixels -->
    <rect x="95" y="124" width="10" height="10" rx="2" fill="#F29A38"/>
    <rect x="88" y="138" width="10" height="10" rx="2" fill="#4296FA"/>
    <rect x="102" y="138" width="10" height="10" rx="2" fill="#6AADFF"/>
    <rect x="76" y="152" width="10" height="10" rx="2" fill="#294A7A"/>
    <rect x="90" y="152" width="10" height="10" rx="2" fill="#4296FA"/>
    <rect x="104" y="152" width="10" height="10" rx="2" fill="#CEE3FF"/>
    <rect x="118" y="152" width="10" height="10" rx="2" fill="#4296FA"/>
  </g>
</svg>'''
    return svg

# -------------------------------------------------------------
# OPÇÃO B: O Olho Bitemporal / "Look Back, Look Ahead"
# -------------------------------------------------------------
def make_logo_b(uid="opt-b", size=200):
    # Two opposing eye contours / satellite optics looking left (past) and right (future),
    # unified into a single cosmic aperture with the orange time cursor at the focal point!
    svg = f'''<svg viewBox="0 0 200 200" width="{size}" height="{size}" xmlns="http://www.w3.org/2000/svg">
  <defs>
    <linearGradient id="{uid}-iris-l" x1="100%" y1="0%" x2="0%" y2="100%">
      <stop offset="0%" stop-color="#4296FA"/>
      <stop offset="100%" stop-color="#1E3A8A"/>
    </linearGradient>
    <linearGradient id="{uid}-iris-r" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="#6AADFF"/>
      <stop offset="100%" stop-color="#00F2FE"/>
    </linearGradient>
  </defs>
  
  <g transform="translate(0, 0)">
    <!-- Outer Elliptical Eye Contours -->
    <!-- Looking Back (Left wing) -->
    <path d="M 100, 48 C 65, 48 35, 75 22, 100 C 35, 125 65, 152 100, 152"
          fill="none" stroke="#4296FA" stroke-width="4.5" stroke-linecap="round"/>
          
    <!-- Looking Ahead (Right wing) -->
    <path d="M 100, 48 C 135, 48 165, 75 178, 100 C 165, 125 135, 152 100, 152"
          fill="none" stroke="#00F2FE" stroke-width="4.5" stroke-linecap="round"/>
          
    <!-- Left Iris Half (Past - Stepped Raster Rings) -->
    <path d="M 100, 64 A 36,36 0 0,0 100, 136 Z" fill="url(#{uid}-iris-l)"/>
    <path d="M 100, 76 A 24,24 0 0,0 100, 124 Z" fill="#1C2A3C"/>
    
    <!-- Right Iris Half (Future - Stepped Raster Rings) -->
    <path d="M 100, 64 A 36,36 0 0,1 100, 136 Z" fill="url(#{uid}-iris-r)"/>
    <path d="M 100, 76 A 24,24 0 0,1 100, 124 Z" fill="#0E1E38"/>
    
    <!-- Central Sensor Core (Orange Date/Time Marker) -->
    <circle cx="100" cy="100" r="10" fill="#F29A38"/>
    <circle cx="100" cy="100" r="4.5" fill="#FFFFFF"/>
    
    <!-- Directional Sight Arrows (Left/Past and Right/Future) -->
    <path d="M 22, 100 L 32, 93 L 32, 107 Z" fill="#4296FA"/>
    <path d="M 178, 100 L 168, 93 L 168, 107 Z" fill="#00F2FE"/>
    
    <!-- Vertical Temporal Meridian Line -->
    <line x1="100" y1="36" x2="100" y2="48" stroke="#F29A38" stroke-width="3" stroke-linecap="round"/>
    <line x1="100" y1="152" x2="100" y2="164" stroke="#F29A38" stroke-width="3" stroke-linecap="round"/>
  </g>
</svg>'''
    return svg

# -------------------------------------------------------------
# OPÇÃO C: O Selo / Medalhão Científico Monumental
# -------------------------------------------------------------
def make_logo_c(uid="opt-c", size=200):
    # A prestigious, high-end mission badge / classical chiseled coin:
    # A circular planetary/astronomy ring with coordinate ticks,
    # containing two chiselled geometric profiles of Janus facing outward,
    # sculpted with bold polygonal facets (like a modern relief).
    svg = f'''<svg viewBox="0 0 200 200" width="{size}" height="{size}" xmlns="http://www.w3.org/2000/svg">
  <defs>
    <linearGradient id="{uid}-ring" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="#4296FA"/>
      <stop offset="100%" stop-color="#1E3A8A"/>
    </linearGradient>
  </defs>
  
  <g transform="translate(0, 0)">
    <!-- Outer Celestial / Coordinate Ring -->
    <circle cx="100" cy="100" r="82" fill="#0F172A" stroke="#4296FA" stroke-width="3"/>
    <circle cx="100" cy="100" r="74" fill="none" stroke="#2563EB" stroke-width="1" stroke-dasharray="3, 3"/>
    
    <!-- Cardinal Ticks (Geospatial Coordinates) -->
    <line x1="100" y1="14" x2="100" y2="24" stroke="#F29A38" stroke-width="3"/>
    <line x1="100" y1="176" x2="100" y2="186" stroke="#F29A38" stroke-width="3"/>
    <line x1="14" y1="100" x2="24" y2="100" stroke="#4296FA" stroke-width="3"/>
    <line x1="176" y1="100" x2="186" y2="100" stroke="#00F2FE" stroke-width="3"/>

    <!-- Chiselled Dual Janus Bust (Polygonal / Brutalist Facets) -->
    <!-- Left Forehead & Nose -->
    <polygon points="100,45 68,58 56,76 72,78 100,74" fill="#3B82F6"/>
    <!-- Left Nose & Mouth -->
    <polygon points="56,76 44,92 58,98 72,78" fill="#2563EB"/>
    <!-- Left Chin & Beard -->
    <polygon points="44,92 50,112 68,126 78,106 58,98" fill="#1D4ED8"/>
    <!-- Left Neck & Throat -->
    <polygon points="68,126 84,148 100,152 100,74 72,78 78,106" fill="#1E3A8A"/>
    
    <!-- Right Forehead & Nose (Mirrored in lighter tones) -->
    <polygon points="100,45 132,58 144,76 128,78 100,74" fill="#60A5FA"/>
    <!-- Right Nose & Mouth -->
    <polygon points="144,76 156,92 142,98 128,78" fill="#38BDF8"/>
    <!-- Right Chin & Beard -->
    <polygon points="156,92 150,112 132,126 122,106 142,98" fill="#0EA5E9"/>
    <!-- Right Neck & Throat -->
    <polygon points="132,126 116,148 100,152 100,74 128,78 122,106" fill="#0284C7"/>
    
    <!-- Central Meridian Laser Line (Date marker) -->
    <line x1="100" y1="42" x2="100" y2="155" stroke="#F29A38" stroke-width="2.5" stroke-linecap="round"/>
    <circle cx="100" cy="100" r="3.5" fill="#FFFFFF"/>
  </g>
</svg>'''
    return svg

# -------------------------------------------------------------
# OPÇÃO D: As Fases Orbitais (Orbital Crescents / Passado & Futuro)
# -------------------------------------------------------------
def make_logo_d(uid="opt-d", size=200):
    # Two opposing planetary crescent orbits:
    # Waning crescent facing Left (Past) & Waxing crescent facing Right (Future),
    # forming a sleek modern astronomical symbol with satellite sensor geometry.
    svg = f'''<svg viewBox="0 0 200 200" width="{size}" height="{size}" xmlns="http://www.w3.org/2000/svg">
  <defs>
    <linearGradient id="{uid}-past" x1="100%" y1="0%" x2="0%" y2="100%">
      <stop offset="0%" stop-color="#4296FA"/>
      <stop offset="100%" stop-color="#1D4ED8"/>
    </linearGradient>
    <linearGradient id="{uid}-future" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="#00F2FE"/>
      <stop offset="100%" stop-color="#38BDF8"/>
    </linearGradient>
  </defs>
  
  <g transform="translate(0, 0)">
    <!-- Outer Subtle Orbit Rings -->
    <ellipse cx="100" cy="100" rx="84" ry="42" fill="none" stroke="#262A31" stroke-width="1.5" transform="rotate(-25 100 100)"/>
    <ellipse cx="100" cy="100" rx="84" ry="42" fill="none" stroke="#262A31" stroke-width="1.5" transform="rotate(25 100 100)"/>

    <!-- Left Crescent (Past / Look Back) -->
    <path d="M 96, 28
             A 72,72 0 0,0 96, 172
             A 54,54 0 0,1 96, 28 Z"
          fill="url(#{uid}-past)"/>
          
    <!-- Right Crescent (Future / Look Ahead) -->
    <path d="M 104, 28
             A 72,72 0 0,1 104, 172
             A 54,54 0 0,0 104, 28 Z"
          fill="url(#{uid}-future)"/>

    <!-- Central Temporal Gateway (Vertical Datum) -->
    <rect x="98" y="24" width="4" height="152" rx="2" fill="#F29A38"/>
    
    <!-- Orbital Nodes / Satellites -->
    <circle cx="100" cy="100" r="14" fill="#0F0F0F" stroke="#F29A38" stroke-width="2.5"/>
    <!-- Sensor Pixels inside central node -->
    <rect x="94" y="94" width="5" height="5" rx="1" fill="#4296FA"/>
    <rect x="101" y="94" width="5" height="5" rx="1" fill="#00F2FE"/>
    <rect x="94" y="101" width="5" height="5" rx="1" fill="#6AADFF"/>
    <rect x="101" y="101" width="5" height="5" rx="1" fill="#FFFFFF"/>
  </g>
</svg>'''
    return svg

# -------------------------------------------------------------
# OPÇÃO E: O Espectro Temporal Simétrico (Interferometry Waveform)
# -------------------------------------------------------------
def make_logo_e(uid="opt-e", size=200):
    # Pure scientific data visualization:
    # A central vertical axis (time cursor in #F29A38),
    # flanked by stepped horizontal bar charts (time series profiles),
    # forming an organic butterfly / acoustic / radar interferometry signature!
    svg = f'''<svg viewBox="0 0 200 200" width="{size}" height="{size}" xmlns="http://www.w3.org/2000/svg">
  <defs>
    <linearGradient id="{uid}-bars-l" x1="100%" y1="0%" x2="0%" y2="0%">
      <stop offset="0%" stop-color="#4296FA"/>
      <stop offset="100%" stop-color="#1E3A8A"/>
    </linearGradient>
    <linearGradient id="{uid}-bars-r" x1="0%" y1="0%" x2="100%" y2="0%">
      <stop offset="0%" stop-color="#00F2FE"/>
      <stop offset="100%" stop-color="#0284C7"/>
    </linearGradient>
  </defs>
  
  <g transform="translate(0, 0)">
    <!-- Background Frame -->
    <rect x="25" y="25" width="150" height="150" rx="16" fill="#15161A" stroke="#262A31" stroke-width="1.5"/>
    
    <!-- Central Date Marker Line -->
    <line x1="100" y1="30" x2="100" y2="170" stroke="#F29A38" stroke-width="3" stroke-linecap="round"/>
    
    <!-- Stepped Horizontal Series Bars (Left = Past History, Right = Future/Change) -->
    <!-- Heights and widths mimicking real NDVI / harmonic curves -->
    <!-- Row 1 -->
    <rect x="76" y="38" width="20" height="6" rx="2" fill="url(#{uid}-bars-l)"/>
    <rect x="104" y="38" width="20" height="6" rx="2" fill="url(#{uid}-bars-r)"/>
    <!-- Row 2 -->
    <rect x="62" y="48" width="34" height="6" rx="2" fill="url(#{uid}-bars-l)"/>
    <rect x="104" y="48" width="38" height="6" rx="2" fill="url(#{uid}-bars-r)"/>
    <!-- Row 3 -->
    <rect x="48" y="58" width="48" height="6" rx="2" fill="url(#{uid}-bars-l)"/>
    <rect x="104" y="58" width="56" height="6" rx="2" fill="url(#{uid}-bars-r)"/>
    <!-- Row 4 -->
    <rect x="40" y="68" width="56" height="6" rx="2" fill="url(#{uid}-bars-l)"/>
    <rect x="104" y="68" width="64" height="6" rx="2" fill="url(#{uid}-bars-r)"/>
    <!-- Row 5 - Major Peak -->
    <rect x="34" y="78" width="62" height="6" rx="2" fill="url(#{uid}-bars-l)"/>
    <rect x="104" y="78" width="68" height="6" rx="2" fill="url(#{uid}-bars-r)"/>
    <!-- Row 6 -->
    <rect x="42" y="88" width="54" height="6" rx="2" fill="url(#{uid}-bars-l)"/>
    <rect x="104" y="88" width="54" height="6" rx="2" fill="url(#{uid}-bars-r)"/>
    <!-- Row 7 - Center Transition -->
    <rect x="52" y="98" width="44" height="6" rx="2" fill="url(#{uid}-bars-l)"/>
    <rect x="104" y="98" width="42" height="6" rx="2" fill="url(#{uid}-bars-r)"/>
    <!-- Row 8 -->
    <rect x="64" y="108" width="32" height="6" rx="2" fill="url(#{uid}-bars-l)"/>
    <rect x="104" y="108" width="30" height="6" rx="2" fill="url(#{uid}-bars-r)"/>
    <!-- Row 9 -->
    <rect x="50" y="118" width="46" height="6" rx="2" fill="url(#{uid}-bars-l)"/>
    <rect x="104" y="118" width="50" height="6" rx="2" fill="url(#{uid}-bars-r)"/>
    <!-- Row 10 - Secondary Peak -->
    <rect x="36" y="128" width="60" height="6" rx="2" fill="url(#{uid}-bars-l)"/>
    <rect x="104" y="128" width="62" height="6" rx="2" fill="url(#{uid}-bars-r)"/>
    <!-- Row 11 -->
    <rect x="46" y="138" width="50" height="6" rx="2" fill="url(#{uid}-bars-l)"/>
    <rect x="104" y="138" width="48" height="6" rx="2" fill="url(#{uid}-bars-r)"/>
    <!-- Row 12 -->
    <rect x="68" y="148" width="28" height="6" rx="2" fill="url(#{uid}-bars-l)"/>
    <rect x="104" y="148" width="32" height="6" rx="2" fill="url(#{uid}-bars-r)"/>
    <!-- Row 13 -->
    <rect x="78" y="158" width="18" height="6" rx="2" fill="url(#{uid}-bars-l)"/>
    <rect x="104" y="158" width="18" height="6" rx="2" fill="url(#{uid}-bars-r)"/>
    
    <!-- Central Cursor Reticle -->
    <circle cx="100" cy="100" r="5" fill="#F29A38"/>
    <circle cx="100" cy="100" r="2" fill="#FFFFFF"/>
  </g>
</svg>'''
    return svg

# -------------------------------------------------------------
# OPÇÃO F: O Monólito Terminal 'jn' (Brutalist CLI & Gateway)
# -------------------------------------------------------------
def make_logo_f(uid="opt-f", size=200):
    # Developer-first brutalist CLI aesthetic:
    # Two terminal angle brackets `« »` forming the gateway of Janus,
    # enclosing an inner raster pixel matrix with the command line cursor!
    svg = f'''<svg viewBox="0 0 200 200" width="{size}" height="{size}" xmlns="http://www.w3.org/2000/svg">
  <defs>
    <linearGradient id="{uid}-bracket-l" x1="100%" y1="0%" x2="0%" y2="100%">
      <stop offset="0%" stop-color="#4296FA"/>
      <stop offset="100%" stop-color="#1D4ED8"/>
    </linearGradient>
    <linearGradient id="{uid}-bracket-r" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="#00F2FE"/>
      <stop offset="100%" stop-color="#0284C7"/>
    </linearGradient>
  </defs>
  
  <g transform="translate(0, 0)">
    <!-- Left Bracket: Pointing Left (Past / `«`) -->
    <path d="M 64, 40
             L 30, 100
             L 64, 160
             L 50, 160
             L 16, 100
             L 50, 40 Z"
          fill="url(#{uid}-bracket-l)"/>

    <!-- Right Bracket: Pointing Right (Future / `»`) -->
    <path d="M 136, 40
             L 170, 100
             L 136, 160
             L 150, 160
             L 184, 100
             L 150, 40 Z"
          fill="url(#{uid}-bracket-r)"/>

    <!-- Central 3x3 Raster Matrix (The Data Cube) -->
    <!-- Col 1 -->
    <rect x="68" y="72" width="16" height="16" rx="3" fill="#294A7A"/>
    <rect x="68" y="92" width="16" height="16" rx="3" fill="#4296FA"/>
    <rect x="68" y="112" width="16" height="16" rx="3" fill="#1E3A8A"/>
    <!-- Col 2 -->
    <rect x="88" y="72" width="16" height="16" rx="3" fill="#4296FA"/>
    <rect x="88" y="92" width="16" height="16" rx="3" fill="#F29A38"/> <!-- Date Cursor -->
    <rect x="88" y="112" width="16" height="16" rx="3" fill="#6AADFF"/>
    <!-- Col 3 -->
    <rect x="108" y="72" width="16" height="16" rx="3" fill="#00F2FE"/>
    <rect x="108" y="92" width="16" height="16" rx="3" fill="#38BDF8"/>
    <rect x="108" y="112" width="16" height="16" rx="3" fill="#0284C7"/>

    <!-- Terminal Prompt Underscore `_` -->
    <rect x="86" y="142" width="28" height="5" rx="2" fill="#F29A38"/>
  </g>
</svg>'''
    return svg

# Save new SVGs
logos_lote2 = [
    ("opcao-A-ampulheta-de-pixels", make_logo_a, "Opção A: A Ampulheta de Pixels", "O símbolo clássico do Tempo reinterpretado com cubos e cascata de pixels raster fluindo através do cursor temporal laranja (#F29A38)."),
    ("opcao-B-olho-bitemporal", make_logo_b, "Opção B: O Olho Bitemporal (Look Back, Look Ahead)", "Diretamente inspirado no lema do Janus: dois horizontes de visão (passado e futuro) formando uma abertura focal de satélite."),
    ("opcao-C-medalhao-cientifico", make_logo_c, "Opção C: O Medalhão Científico Monumental", "Para quem preza a raiz romana de Janus, mas com escultura poligonal brutalista de alta precisão e anel de coordenadas orbitais."),
    ("opcao-D-fases-orbitais", make_logo_d, "Opção D: As Fases Orbitais (Luas de Passado & Futuro)", "Duas luas crescentes opostas e anéis orbitais de satélite com um núcleo de sensores centrais. Minimalismo astronômico refinado."),
    ("opcao-E-espectro-interferometria", make_logo_e, "Opção E: O Espectro Temporal (Interferometria)", "Pura visualização de dados científicos: histograma simétrico de séries temporais com eixo central de data e perfis espectrais."),
    ("opcao-F-terminal-jn", make_logo_f, "Opção F: O Monólito Terminal 'jn'", "Inspirado no comando rápido de terminal: suportes angulares de código « » emoldurando a matriz de pixels do cubo de dados.")
]

for slug, fn, title, desc in logos_lote2:
    svg_content = fn(slug, 256)
    with open(os.path.join(PROPOSTAS_DIR, f"{slug}.svg"), "w", encoding="utf-8") as f:
        f.write(f'<?xml version="1.0" encoding="UTF-8"?>\n{svg_content}\n')

print("Novas opções salvas em branding/novas-opcoes-lote2/")
