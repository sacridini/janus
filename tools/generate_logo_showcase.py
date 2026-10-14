import math, os

ARTIFACT_DIR = r"C:\Users\eduar\.gemini\antigravity\brain\4748f837-1fb5-40b7-b843-58222e575296"
os.makedirs(ARTIFACT_DIR, exist_ok=True)

# -------------------------------------------------------------
# LOGO 1: O Cubo Espaço-Temporal (Spatiotemporal Data Cube)
# -------------------------------------------------------------
def make_logo1(uid="l1", size=200):
    # Isometric Data Cube with 3x3 raster on top and temporal strata on sides
    # Origin at (100, 102)
    cx, cy = 100, 102
    w_iso = 62
    h_top = 36
    h_side = 68
    
    # Grid of 3x3 on top
    top_tiles = []
    colors_top = [
        ["#2563EB", "#3B82F6", "#60A5FA"],
        ["#1D4ED8", "#00F2FE", "#3B82F6"],  # center-active pixel
        ["#1E40AF", "#2563EB", "#60A5FA"]
    ]
    
    # 3x3 isometric cells
    for r in range(3):
        for c in range(3):
            # parametric u (right), v (left) from 0 to 3
            # u goes from 0..3 along (w_iso/3, -h_top/3)
            # v goes from 0..3 along (-w_iso/3, -h_top/3)
            u0, u1 = c / 3.0, (c + 0.90) / 3.0
            v0, v1 = r / 3.0, (r + 0.90) / 3.0
            
            p1 = (cx + (u0 - v0) * w_iso, cy - (u0 + v0) * h_top)
            p2 = (cx + (u1 - v0) * w_iso, cy - (u1 + v0) * h_top)
            p3 = (cx + (u1 - v1) * w_iso, cy - (u1 + v1) * h_top)
            p4 = (cx + (u0 - v1) * w_iso, cy - (u0 + v1) * h_top)
            
            color = colors_top[r][c]
            stroke = "#00F2FE" if (r == 1 and c == 1) else "none"
            sw = "1.5" if stroke != "none" else "0"
            filter_str = f'filter="url(#{uid}-glow)"' if (r == 1 and c == 1) else ""
            
            pts = f"{p1[0]:.2f},{p1[1]:.2f} {p2[0]:.2f},{p2[1]:.2f} {p3[0]:.2f},{p3[1]:.2f} {p4[0]:.2f},{p4[1]:.2f}"
            top_tiles.append(f'<polygon points="{pts}" fill="{color}" stroke="{stroke}" stroke-width="{sw}" {filter_str}/>')

    # Left face (Past - Temporal strata: 4 layers)
    left_strata = []
    left_colors = ["#1E3A8A", "#1D4ED8", "#2563EB", "#1E40AF"]
    for i in range(4):
        y0 = cy + (i / 4.0) * h_side
        y1 = cy + ((i + 0.88) / 4.0) * h_side
        p1 = (cx, y0)
        p2 = (cx - w_iso, y0 - h_top)
        p3 = (cx - w_iso, y1 - h_top)
        p4 = (cx, y1)
        pts = f"{p1[0]:.2f},{p1[1]:.2f} {p2[0]:.2f},{p2[1]:.2f} {p3[0]:.2f},{p3[1]:.2f} {p4[0]:.2f},{p4[1]:.2f}"
        left_strata.append(f'<polygon points="{pts}" fill="{left_colors[i]}" opacity="0.95"/>')

    # Right face (Future - Temporal strata: 4 layers)
    right_strata = []
    right_colors = ["#0284C7", "#0EA5E9", "#38BDF8", "#0369A1"]
    for i in range(4):
        y0 = cy + (i / 4.0) * h_side
        y1 = cy + ((i + 0.88) / 4.0) * h_side
        p1 = (cx, y0)
        p2 = (cx + w_iso, y0 - h_top)
        p3 = (cx + w_iso, y1 - h_top)
        p4 = (cx, y1)
        pts = f"{p1[0]:.2f},{p1[1]:.2f} {p2[0]:.2f},{p2[1]:.2f} {p3[0]:.2f},{p3[1]:.2f} {p4[0]:.2f},{p4[1]:.2f}"
        right_strata.append(f'<polygon points="{pts}" fill="{right_colors[i]}" opacity="0.95"/>')

    svg = f'''<svg viewBox="0 0 200 200" width="{size}" height="{size}" xmlns="http://www.w3.org/2000/svg">
  <defs>
    <filter id="{uid}-glow" x="-20%" y="-20%" width="140%" height="140%">
      <feGaussianBlur stdDeviation="3" result="blur" />
      <feComposite in="SourceGraphic" in2="blur" operator="over" />
    </filter>
    <linearGradient id="{uid}-grad1" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="#00F2FE" />
      <stop offset="100%" stop-color="#4FACFE" />
    </linearGradient>
  </defs>
  <g transform="translate(0, 5)">
    <!-- Shadow base -->
    <ellipse cx="100" cy="180" rx="68" ry="14" fill="#000000" opacity="0.25" filter="blur(6px)"/>
    <!-- Strata Left & Right -->
    <g id="strata-left">{''.join(left_strata)}</g>
    <g id="strata-right">{''.join(right_strata)}</g>
    <!-- Top Grid -->
    <g id="top-grid">{''.join(top_tiles)}</g>
    <!-- Center spine accent line (Past/Future boundary) -->
    <line x1="100" y1="102" x2="100" y2="168" stroke="#FFFFFF" stroke-width="1.5" stroke-linecap="round" opacity="0.6"/>
  </g>
</svg>'''
    return svg


# -------------------------------------------------------------
# LOGO 2: A Dualidade Geométrica (Modern Minimalist Janus)
# -------------------------------------------------------------
def make_logo2(uid="l2", size=200):
    # Two stylized, sleek aerodynamic profiles facing outward, forming a unified portal glyph.
    # Left face in deep azure/indigo, Right face in electric cyan/sky blue.
    svg = f'''<svg viewBox="0 0 200 200" width="{size}" height="{size}" xmlns="http://www.w3.org/2000/svg">
  <defs>
    <linearGradient id="{uid}-left-grad" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="#3B82F6"/>
      <stop offset="100%" stop-color="#1D4ED8"/>
    </linearGradient>
    <linearGradient id="{uid}-right-grad" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="#38BDF8"/>
      <stop offset="100%" stop-color="#0284C7"/>
    </linearGradient>
    <linearGradient id="{uid}-center-line" x1="0%" y1="0%" x2="0%" y2="100%">
      <stop offset="0%" stop-color="#00F2FE"/>
      <stop offset="50%" stop-color="#FFFFFF"/>
      <stop offset="100%" stop-color="#00F2FE"/>
    </linearGradient>
  </defs>
  <g transform="translate(0, 0)">
    <!-- Base Glow -->
    <circle cx="100" cy="100" r="75" fill="#2563EB" opacity="0.08" filter="blur(16px)"/>
    
    <!-- Left Profile (Looking Backward / Past) -->
    <!-- Sleek forehead, nose, lips, chin, swept jaw and neck -->
    <path d="M 97, 28
             C 80, 28 62, 38 52, 54
             C 45, 65 44, 76 38, 86
             L 28, 98
             L 39, 102
             C 36, 109 37, 114 43, 116
             L 37, 126
             C 44, 134 54, 137 66, 137
             C 76, 137 86, 148 88, 162
             L 97, 164
             C 97, 120 97, 72 97, 28 Z"
          fill="url(#{uid}-left-grad)"/>

    <!-- Left Eye & Temporal Cutout -->
    <path d="M 68, 82 C 60, 85 54, 91 50, 93 C 58, 93 64, 89 72, 85 Z" fill="#FFFFFF" opacity="0.9"/>
    
    <!-- Right Profile (Looking Forward / Future - Mirrored & Refined) -->
    <path d="M 103, 28
             C 120, 28 138, 38 148, 54
             C 155, 65 156, 76 162, 86
             L 172, 98
             L 161, 102
             C 164, 109 163, 114 157, 116
             L 163, 126
             C 156, 134 146, 137 134, 137
             C 124, 137 114, 148 112, 162
             L 103, 164
             C 103, 120 103, 72 103, 28 Z"
          fill="url(#{uid}-right-grad)"/>

    <!-- Right Eye & Temporal Cutout -->
    <path d="M 132, 82 C 140, 85 146, 91 150, 93 C 142, 93 136, 89 128, 85 Z" fill="#FFFFFF" opacity="0.9"/>
    
    <!-- Central Temporal Gateway Line (Negative Space Bridge) -->
    <line x1="100" y1="26" x2="100" y2="166" stroke="url(#{uid}-center-line)" stroke-width="2.5" stroke-linecap="round" opacity="0.85"/>
    
    <!-- Central Sensor Focal Point -->
    <circle cx="100" cy="96" r="4.5" fill="#FFFFFF" stroke="#00F2FE" stroke-width="2"/>
  </g>
</svg>'''
    return svg


# -------------------------------------------------------------
# LOGO 3: Grade Raster & Pulso Temporal (Raster Matrix & Time Wave)
# -------------------------------------------------------------
def make_logo3(uid="l3", size=200):
    # 4x4 rounded pixel grid + smooth continuous time-series wave
    # Grid dimensions:
    # 4 cells, cell_size = 24, gap = 6 -> total = 4*24 + 3*6 = 114px
    # Offset x0 = 43, y0 = 43
    cells = []
    cell_w = 24
    gap = 6
    x0, y0 = 43, 43
    
    # Coordinates of cells
    for r in range(4):
        for c in range(4):
            x = x0 + c * (cell_w + gap)
            y = y0 + r * (cell_w + gap)
            # Default pixel style
            is_active = (r == 1 and c == 1) # hovered pixel (target)
            if is_active:
                fill = "url(#" + uid + "-active)"
                stroke = "#00F2FE"
                sw = "2"
            else:
                # subtle gradient based on col/row
                fill = f"url(#{uid}-cell-{r}-{c})"
                stroke = "none"
                sw = "0"
            cells.append(f'<rect x="{x}" y="{y}" width="{cell_w}" height="{cell_w}" rx="4.5" fill="{fill}" stroke="{stroke}" stroke-width="{sw}"/>')

    svg = f'''<svg viewBox="0 0 200 200" width="{size}" height="{size}" xmlns="http://www.w3.org/2000/svg">
  <defs>
    <linearGradient id="{uid}-active" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="#00F2FE"/>
      <stop offset="100%" stop-color="#10B981"/>
    </linearGradient>
    <linearGradient id="{uid}-wave" x1="0%" y1="0%" x2="100%" y2="0%">
      <stop offset="0%" stop-color="#3B82F6" stop-opacity="0.3"/>
      <stop offset="35%" stop-color="#00F2FE" stop-opacity="1"/>
      <stop offset="70%" stop-color="#60A5FA" stop-opacity="0.9"/>
      <stop offset="100%" stop-color="#38BDF8" stop-opacity="0.4"/>
    </linearGradient>
    <!-- Dynamic gradients for cells -->
    {''.join([f'<linearGradient id="{uid}-cell-{r}-{c}" x1="0%" y1="0%" x2="100%" y2="100%"><stop offset="0%" stop-color="#1E3A8A" stop-opacity="{0.4 + 0.1*(r+c)}"/><stop offset="100%" stop-color="#2563EB" stop-opacity="{0.6 + 0.08*(r+c)}"/></linearGradient>' for r in range(4) for c in range(4)])}
    <filter id="{uid}-glow" x="-20%" y="-20%" width="140%" height="140%">
      <feGaussianBlur stdDeviation="4" result="blur" />
      <feComposite in="SourceGraphic" in2="blur" operator="over" />
    </filter>
  </defs>
  
  <g transform="translate(0, 0)">
    <!-- Base Background Rounded Tile -->
    <rect x="30" y="30" width="140" height="140" rx="24" fill="#0E1E38" stroke="#1E3A8A" stroke-width="1.5"/>
    
    <!-- Cells Grid -->
    {''.join(cells)}
    
    <!-- Time-Series Wave Curve (Spline through active pixel) -->
    <!-- Active pixel is at (43 + 30 = 73, 43 + 30 = 73) center is (85, 85) -->
    <path d="M 28, 128
             C 52, 134 62, 105 85, 85
             C 108, 65 125, 118 145, 102
             C 158, 92 165, 78 172, 70"
          fill="none" stroke="url(#{uid}-wave)" stroke-width="4.5" stroke-linecap="round" filter="url(#{uid}-glow)"/>
    <path d="M 28, 128
             C 52, 134 62, 105 85, 85
             C 108, 65 125, 118 145, 102
             C 158, 92 165, 78 172, 70"
          fill="none" stroke="#FFFFFF" stroke-width="2" stroke-linecap="round"/>
          
    <!-- Cursor Target on Hovered Pixel (85, 85) -->
    <circle cx="85" cy="85" r="7.5" fill="none" stroke="#FFFFFF" stroke-width="2"/>
    <circle cx="85" cy="85" r="2.5" fill="#00F2FE"/>
    <!-- Crosshairs -->
    <line x1="85" y1="72" x2="85" y2="76" stroke="#FFFFFF" stroke-width="2" stroke-linecap="round"/>
    <line x1="85" y1="94" x2="85" y2="98" stroke="#FFFFFF" stroke-width="2" stroke-linecap="round"/>
    <line x1="72" y1="85" x2="76" y2="85" stroke="#FFFFFF" stroke-width="2" stroke-linecap="round"/>
    <line x1="94" y1="85" x2="98" y2="85" stroke="#FFFFFF" stroke-width="2" stroke-linecap="round"/>
  </g>
</svg>'''
    return svg


# -------------------------------------------------------------
# LOGO 4: O Monograma 'J' Bitemporal (Dual-Vector 'J')
# -------------------------------------------------------------
def make_logo4(uid="l4", size=200):
    # A monogram 'J' formed by two dynamic continuous ribbons representing past (left-arrow) & future (forward-hook)
    svg = f'''<svg viewBox="0 0 200 200" width="{size}" height="{size}" xmlns="http://www.w3.org/2000/svg">
  <defs>
    <linearGradient id="{uid}-past" x1="100%" y1="0%" x2="0%" y2="100%">
      <stop offset="0%" stop-color="#1D4ED8"/>
      <stop offset="100%" stop-color="#3B82F6"/>
    </linearGradient>
    <linearGradient id="{uid}-future" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="#38BDF8"/>
      <stop offset="100%" stop-color="#00F2FE"/>
    </linearGradient>
    <linearGradient id="{uid}-core" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="#2563EB"/>
      <stop offset="100%" stop-color="#0EA5E9"/>
    </linearGradient>
    <filter id="{uid}-drop" x="-10%" y="-10%" width="120%" height="120%">
      <feDropShadow dx="0" dy="4" stdDeviation="6" flood-color="#000000" flood-opacity="0.35"/>
    </filter>
  </defs>
  
  <g transform="translate(0, 0)" filter="url(#{uid}-drop)">
    <!-- Backward / Past Ribbon (Extends to top-left arrow/serif) -->
    <!-- Starts from top-right (145, 38) -> sweeps left to (55, 38), forming top temporal bar pointing back -->
    <path d="M 68, 26
             L 45, 45
             L 68, 64
             L 68, 52
             L 125, 52
             C 133, 52 140, 58 140, 66
             L 140, 118
             C 140, 142 122, 162 98, 162
             C 82, 162 68, 152 62, 138
             L 44, 147
             C 54, 169 74, 182 98, 182
             C 134, 182 162, 154 162, 118
             L 162, 66
             C 162, 45 145, 28 125, 28
             L 68, 28
             Z"
          fill="url(#{uid}-future)"/>

    <!-- Foreground Interlocking Stem & Hook (Inner J core) -->
    <path d="M 116, 52
             L 116, 118
             C 116, 128 108, 136 98, 136
             C 88, 136 80, 128 80, 118
             L 80, 102
             L 58, 102
             L 58, 118
             C 58, 140 76, 158 98, 158
             C 120, 158 138, 140 138, 118
             L 138, 52
             Z"
          fill="url(#{uid}-past)"/>
          
    <!-- Temporal Indicator Nodes (Past tick & Future tick) -->
    <circle cx="45" cy="45" r="4" fill="#00F2FE"/>
    <circle cx="69" cy="102" r="4" fill="#FFFFFF"/>
    
    <!-- Forward Arrow Tip on Hook -->
    <path d="M 58, 102 L 69, 86 L 80, 102 Z" fill="#00F2FE"/>
  </g>
</svg>'''
    return svg


# -------------------------------------------------------------
# LOGO 5: O Portal Hexagonal / Prisma Multitemporal
# -------------------------------------------------------------
def make_logo5(uid="l5", size=200):
    # Hexagonal aperture divided into two temporal halves (Past / Future) with stratified prism bands
    svg = f'''<svg viewBox="0 0 200 200" width="{size}" height="{size}" xmlns="http://www.w3.org/2000/svg">
  <defs>
    <linearGradient id="{uid}-hex-left" x1="100%" y1="0%" x2="0%" y2="100%">
      <stop offset="0%" stop-color="#2563EB"/>
      <stop offset="50%" stop-color="#1D4ED8"/>
      <stop offset="100%" stop-color="#1E3A8A"/>
    </linearGradient>
    <linearGradient id="{uid}-hex-right" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="#00F2FE"/>
      <stop offset="50%" stop-color="#38BDF8"/>
      <stop offset="100%" stop-color="#0284C7"/>
    </linearGradient>
    <linearGradient id="{uid}-core" x1="0%" y1="0%" x2="0%" y2="100%">
      <stop offset="0%" stop-color="#FFFFFF"/>
      <stop offset="100%" stop-color="#00F2FE"/>
    </linearGradient>
  </defs>
  
  <g transform="translate(0, 0)">
    <!-- Left Hexagon Half (Past / Reference) -->
    <!-- Points: Top(100, 30), TopLeft(42, 64), BottomLeft(42, 136), Bottom(100, 170) -->
    <path d="M 97, 34
             L 45, 66
             L 45, 134
             L 97, 166
             Z"
          fill="url(#{uid}-hex-left)"/>
          
    <!-- Left Internal Strata (3 stepping bands) -->
    <path d="M 97, 56 L 60, 78 L 60, 122 L 97, 144 Z" fill="#1E3A8A" opacity="0.6"/>
    <path d="M 97, 74 L 75, 87 L 75, 113 L 97, 126 Z" fill="#172554" opacity="0.8"/>

    <!-- Right Hexagon Half (Future / Anomaly) -->
    <!-- Points: Top(100, 30), TopRight(158, 64), BottomRight(158, 136), Bottom(100, 170) -->
    <path d="M 103, 34
             L 155, 66
             L 155, 134
             L 103, 166
             Z"
          fill="url(#{uid}-hex-right)"/>
          
    <!-- Right Internal Strata (3 stepping bands) -->
    <path d="M 103, 56 L 140, 78 L 140, 122 L 103, 144 Z" fill="#0284C7" opacity="0.6"/>
    <path d="M 103, 74 L 125, 87 L 125, 113 L 103, 126 Z" fill="#0369A1" opacity="0.8"/>
    
    <!-- Central Portal Threshold (Vertical Slit of Time) -->
    <line x1="100" y1="28" x2="100" y2="172" stroke="url(#{uid}-core)" stroke-width="3" stroke-linecap="round"/>
    
    <!-- Central Pixel Coordinate / Core Singularity -->
    <polygon points="100,90 109,100 100,110 91,100" fill="#FFFFFF"/>
    <circle cx="100" cy="100" r="2.5" fill="#00F2FE"/>
    
    <!-- Top & Bottom Gateway Caps -->
    <circle cx="100" cy="28" r="4.5" fill="#00F2FE"/>
    <circle cx="100" cy="172" r="4.5" fill="#00F2FE"/>
  </g>
</svg>'''
    return svg


# -------------------------------------------------------------
# CURRENT LOGO (For comparison)
# -------------------------------------------------------------
def get_current_logo(size=200):
    svg_path = r"c:\Users\eduar\Documents\dev\janus\branding\svg\janus-simbolo.svg"
    with open(svg_path, "r", encoding="utf-8") as f:
        content = f.read()
    # update width/height
    return content.replace('width="200" height="270"', f'width="{size}" height="{size}" style="max-height:{size}px; width:auto;"')


# -------------------------------------------------------------
# HORIZONTAL LOCKUP (Logo + Wordmark)
# -------------------------------------------------------------
def make_horizontal_lockup(logo_svg_fn, uid, title="JANUS", subtitle="Raster Time Series & Data Cubes"):
    logo_svg = logo_svg_fn(uid=f"{uid}-h", size=70)
    return f'''
    <div class="flex items-center gap-4 py-2 px-3">
      <div class="flex-shrink-0 w-[70px] h-[70px] flex items-center justify-center">
        {logo_svg}
      </div>
      <div>
        <div class="text-2xl font-bold tracking-wider font-mono text-[var(--foreground)]">{title}</div>
        <div class="text-xs text-[var(--muted-foreground)] uppercase tracking-widest font-sans mt-0.5">{subtitle}</div>
      </div>
    </div>
    '''

# Generate SVGs
l1 = make_logo1("l1", 140)
l2 = make_logo2("l2", 140)
l3 = make_logo3("l3", 140)
l4 = make_logo4("l4", 140)
l5 = make_logo5("l5", 140)
current_logo = get_current_logo(140)

print("All SVGs successfully computed!")
