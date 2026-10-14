"""
Generator for the 10 Janus Conceptual Logos:
Exploring the Roman Myth, The Eye, Time, and Time Series.

Produces:
1. 10 standalone SVG logos in branding/10-logos-janus/
2. High-res 512x512 PNGs rendered via PyMuPDF in branding/10-logos-janus/png/
3. An interactive standalone HTML showcase in branding/10-logos-janus/showcase-10-logos.html
"""

import os
from pathlib import Path
import pymupdf

ROOT = Path(r"c:\Users\eduar\Documents\dev\janus")
OUT_DIR = ROOT / "branding" / "10-logos-janus"
PNG_DIR = OUT_DIR / "png"

OUT_DIR.mkdir(parents=True, exist_ok=True)
PNG_DIR.mkdir(parents=True, exist_ok=True)

# Cores canônicas
BLUE = "#4296FA"
BLUE_LIGHT = "#6AADFF"
BLUE_DARK = "#1E3A8A"
NAVY_BG = "#0B111E"
NAVY_PANEL = "#162447"
CYAN = "#00F2FE"
CYAN_LIGHT = "#A5F3FC"
ORANGE = "#F29A38"
ORANGE_GLOW = "#FF6D00"
WHITE = "#FFFFFF"
MUTED_GRID = "#2A3B5C"

# ----------------------------------------------------------------------
# 1. LOGO 01: O Olho da Onda Fenológica (The Phenological Waveform Eye)
# ----------------------------------------------------------------------
def make_logo_01(uid="l01", size=200):
    return f'''<svg viewBox="0 0 200 200" width="{size}" height="{size}" xmlns="http://www.w3.org/2000/svg">
  <defs>
    <linearGradient id="{uid}-wave-top" x1="0%" y1="0%" x2="100%" y2="0%">
      <stop offset="0%" stop-color="{BLUE_DARK}"/>
      <stop offset="30%" stop-color="{BLUE}"/>
      <stop offset="50%" stop-color="{CYAN}"/>
      <stop offset="70%" stop-color="{BLUE}"/>
      <stop offset="100%" stop-color="{BLUE_DARK}"/>
    </linearGradient>
    <linearGradient id="{uid}-wave-bot" x1="0%" y1="0%" x2="100%" y2="0%">
      <stop offset="0%" stop-color="{BLUE_DARK}"/>
      <stop offset="50%" stop-color="{BLUE_LIGHT}"/>
      <stop offset="100%" stop-color="{BLUE_DARK}"/>
    </linearGradient>
    <radialGradient id="{uid}-pupil" cx="50%" cy="50%" r="50%">
      <stop offset="0%" stop-color="{ORANGE}"/>
      <stop offset="70%" stop-color="{ORANGE_GLOW}"/>
      <stop offset="100%" stop-color="{BLUE_DARK}"/>
    </radialGradient>
  </defs>

  <!-- Background grid subtle -->
  <g opacity="0.25" stroke="{MUTED_GRID}" stroke-width="1">
    <line x1="20" y1="100" x2="180" y2="100"/>
    <line x1="60" y1="40" x2="60" y2="160" stroke-dasharray="2 4"/>
    <line x1="100" y1="30" x2="100" y2="170" stroke="{ORANGE}" opacity="0.6"/>
    <line x1="140" y1="40" x2="140" y2="160" stroke-dasharray="2 4"/>
  </g>

  <!-- Eye Almond formed by Dual Phenological Waves (harmonic seasonal curves) -->
  <!-- Upper eyelid: seasonal spring-summer peak NDVI -->
  <path d="M 22 100 C 45 65, 75 52, 100 52 C 125 52, 155 65, 178 100" 
        fill="none" stroke="url(#{uid}-wave-top)" stroke-width="4.5" stroke-linecap="round"/>
        
  <!-- Lower eyelid: senescent autumn-winter valley -->
  <path d="M 22 100 C 45 135, 75 148, 100 148 C 125 148, 155 135, 178 100" 
        fill="none" stroke="url(#{uid}-wave-bot)" stroke-width="4.5" stroke-linecap="round"/>

  <!-- In-fill area chart between waves (confidence band) -->
  <path d="M 22 100 C 45 75, 75 66, 100 66 C 125 66, 155 75, 178 100 C 155 125, 125 134, 100 134 C 75 134, 45 125, 22 100 Z"
        fill="{BLUE}" opacity="0.12"/>

  <!-- Harmonic Time Series Signal traversing through the pupil -->
  <path d="M 28 100 Q 45 82 62 100 T 96 100 L 100 86 L 104 114 L 108 100 Q 125 118 142 100 T 172 100" 
        fill="none" stroke="{CYAN}" stroke-width="2" stroke-linecap="round"/>

  <!-- Central Iris & Pupil (The Observer / Orbital Lens) -->
  <circle cx="100" cy="100" r="28" fill="{NAVY_BG}" stroke="{BLUE_LIGHT}" stroke-width="2.5"/>
  <circle cx="100" cy="100" r="18" fill="url(#{uid}-pupil)"/>
  <circle cx="100" cy="100" r="7" fill="{WHITE}"/>

  <!-- The "NOW" Cursor (ImPlot Orange Timeline) -->
  <line x1="100" y1="28" x2="100" y2="48" stroke="{ORANGE}" stroke-width="3" stroke-linecap="round"/>
  <line x1="100" y1="152" x2="100" y2="172" stroke="{ORANGE}" stroke-width="3" stroke-linecap="round"/>
  <polygon points="100,49 97,42 103,42" fill="{ORANGE}"/>
  <polygon points="100,151 97,158 103,158" fill="{ORANGE}"/>

  <!-- Subtle time markers on the curve -->
  <circle cx="62" cy="100" r="3" fill="{BLUE_LIGHT}"/>
  <circle cx="138" cy="100" r="3" fill="{CYAN}"/>
</svg>'''

# ----------------------------------------------------------------------
# 2. LOGO 02: O Perfil Bifronte Espectral (The Dual-Profile Chrono-Face)
# ----------------------------------------------------------------------
def make_logo_02(uid="l02", size=200):
    return f'''<svg viewBox="0 0 200 200" width="{size}" height="{size}" xmlns="http://www.w3.org/2000/svg">
  <defs>
    <linearGradient id="{uid}-face-l" x1="100%" y1="0%" x2="0%" y2="100%">
      <stop offset="0%" stop-color="{BLUE_LIGHT}"/>
      <stop offset="100%" stop-color="{BLUE_DARK}"/>
    </linearGradient>
    <linearGradient id="{uid}-face-r" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="{CYAN}"/>
      <stop offset="100%" stop-color="{BLUE}"/>
    </linearGradient>
  </defs>

  <!-- Left Face: Praeterita (Looking into the Past / Baseline Data) -->
  <!-- Stylized geometric profile rendered as time-series steps -->
  <path d="M 94 36 L 68 36 L 56 50 L 56 68 L 46 80 L 46 94 L 58 106 L 52 120 L 66 136 L 76 150 L 94 164" 
        fill="none" stroke="url(#{uid}-face-l)" stroke-width="3.5" stroke-linejoin="round" stroke-linecap="round"/>

  <!-- Left Face Fill Plate (Semi-transparent Voxel) -->
  <polygon points="94,40 70,40 60,52 60,68 50,80 50,94 62,106 56,120 70,136 78,148 94,160" 
           fill="{BLUE}" opacity="0.15"/>

  <!-- Right Face: Futura (Looking into Future / Forecast & Trends) -->
  <path d="M 106 36 L 132 36 L 144 50 L 144 68 L 154 80 L 154 94 L 142 106 L 148 120 L 134 136 L 124 150 L 106 164" 
        fill="none" stroke="url(#{uid}-face-r)" stroke-width="3.5" stroke-linejoin="round" stroke-linecap="round"/>

  <!-- Right Face Fill Plate -->
  <polygon points="106,40 130,40 140,52 140,68 150,80 150,94 138,106 144,120 130,136 122,148 106,160" 
           fill="{CYAN}" opacity="0.15"/>

  <!-- Negative Space in the Middle creates a Radiant Vertical Eye / Chrono-Aperture -->
  <path d="M 100 56 C 88 78, 88 122, 100 144 C 112 122, 112 78, 100 56 Z" 
        fill="{NAVY_BG}" stroke="{BLUE_LIGHT}" stroke-width="2"/>

  <!-- Central Pupil: The Present Moment ("Now") -->
  <circle cx="100" cy="100" r="10" fill="{ORANGE}"/>
  <circle cx="100" cy="100" r="4" fill="{WHITE}"/>

  <!-- Horizontal Time Horizon connecting the two eyes of the profiles -->
  <line x1="38" y1="80" x2="88" y2="80" stroke="{BLUE_LIGHT}" stroke-width="1.5" stroke-dasharray="3 3"/>
  <circle cx="50" cy="80" r="3" fill="{BLUE_LIGHT}"/>
  <line x1="112" y1="80" x2="162" y2="80" stroke="{CYAN}" stroke-width="1.5" stroke-dasharray="3 3"/>
  <circle cx="150" cy="80" r="3" fill="{CYAN}"/>

  <!-- Vertical Temporal Divider (The Axis of Janus) -->
  <line x1="100" y1="20" x2="100" y2="48" stroke="{ORANGE}" stroke-width="2.5" stroke-linecap="round"/>
  <line x1="100" y1="152" x2="100" y2="180" stroke="{ORANGE}" stroke-width="2.5" stroke-linecap="round"/>
</svg>'''

# ----------------------------------------------------------------------
# 3. LOGO 03: O Portal de Lâminas Temporais (The Chrono-Slice Portal)
# ----------------------------------------------------------------------
def make_logo_03(uid="l03", size=200):
    return f'''<svg viewBox="0 0 200 200" width="{size}" height="{size}" xmlns="http://www.w3.org/2000/svg">
  <defs>
    <linearGradient id="{uid}-slice-1" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="{BLUE_DARK}"/>
      <stop offset="100%" stop-color="#0A192F"/>
    </linearGradient>
    <linearGradient id="{uid}-slice-2" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="{BLUE}"/>
      <stop offset="100%" stop-color="{BLUE_DARK}"/>
    </linearGradient>
    <linearGradient id="{uid}-slice-3" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="{CYAN}"/>
      <stop offset="100%" stop-color="{BLUE}"/>
    </linearGradient>
  </defs>

  <!-- Roman Ianua (Archway Portal) built of Isometric Raster Time-Slices -->
  <!-- Layer 1: Deep Past (Back Layer) -->
  <path d="M 44 160 L 44 92 C 44 60, 156 60, 156 92 L 156 160" 
        fill="none" stroke="{BLUE_DARK}" stroke-width="5" opacity="0.4"/>

  <!-- Layer 2: Mid Past (Offset Layer) -->
  <path d="M 54 160 L 54 94 C 54 68, 146 68, 146 94 L 146 160" 
        fill="none" stroke="{BLUE}" stroke-width="4.5" opacity="0.7"/>

  <!-- Layer 3: Present Front Portal Frame -->
  <path d="M 64 160 L 64 96 C 64 76, 136 76, 136 96 L 136 160" 
        fill="none" stroke="{CYAN}" stroke-width="4"/>

  <!-- Bottom Baseline / Ground Line of the Temple of Janus -->
  <line x1="30" y1="160" x2="170" y2="160" stroke="{BLUE_LIGHT}" stroke-width="3" stroke-linecap="round"/>

  <!-- Isometric Pixel Slices flanking the gateway (The Data Cube Slices) -->
  <!-- Left Side: Past Stacks -->
  <polygon points="36,110 52,100 52,120 36,130" fill="{BLUE}" opacity="0.5"/>
  <polygon points="36,136 52,126 52,146 36,156" fill="{BLUE_DARK}" opacity="0.6"/>

  <!-- Right Side: Future Stacks -->
  <polygon points="148,100 164,110 164,130 148,120" fill="{CYAN}" opacity="0.5"/>
  <polygon points="148,126 164,136 164,156 148,146" fill="{BLUE}" opacity="0.6"/>

  <!-- The Portal Opening forms the Cosmic Eye Looking Through Time -->
  <!-- Eye Almond nested inside the archway -->
  <path d="M 64 105 C 80 85, 120 85, 136 105 C 120 125, 80 125, 64 105 Z" 
        fill="{NAVY_BG}" stroke="{BLUE_LIGHT}" stroke-width="2"/>

  <!-- Iris & Focal Sensor -->
  <circle cx="100" cy="105" r="16" fill="{NAVY_PANEL}" stroke="{CYAN}" stroke-width="2"/>
  <circle cx="100" cy="105" r="7" fill="{ORANGE}"/>
  <circle cx="100" cy="105" r="2.5" fill="{WHITE}"/>

  <!-- Laser Keystone / Time Cursor dividing the portal entrance -->
  <line x1="100" y1="36" x2="100" y2="84" stroke="{ORANGE}" stroke-width="3" stroke-linecap="round"/>
  <circle cx="100" cy="36" r="4" fill="{ORANGE}"/>
</svg>'''

# ----------------------------------------------------------------------
# 4. LOGO 04: A Íris Dendrocronológica / Anel Orbital (The Dendro-Temporal Orbit Eye)
# ----------------------------------------------------------------------
def make_logo_04(uid="l04", size=200):
    return f'''<svg viewBox="0 0 200 200" width="{size}" height="{size}" xmlns="http://www.w3.org/2000/svg">
  <defs>
    <linearGradient id="{uid}-ring" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="{BLUE_DARK}"/>
      <stop offset="50%" stop-color="{BLUE}"/>
      <stop offset="100%" stop-color="{CYAN}"/>
    </linearGradient>
  </defs>

  <!-- Outer Orbit Rings (Annual Seasons / Satellite Repeat Cycles) -->
  <circle cx="100" cy="100" r="76" fill="none" stroke="{BLUE_DARK}" stroke-width="1.5" stroke-dasharray="4 6" opacity="0.6"/>
  <circle cx="100" cy="100" r="62" fill="none" stroke="{BLUE}" stroke-width="2" stroke-dasharray="8 4" opacity="0.8"/>
  <circle cx="100" cy="100" r="48" fill="none" stroke="{CYAN}" stroke-width="2"/>

  <!-- Segmented Dendrochronology Raster Pixels around the Ring -->
  <g fill="{BLUE}" opacity="0.7">
    <rect x="96" y="26" width="8" height="6" rx="1.5"/>
    <rect x="136" y="38" width="8" height="6" rx="1.5" transform="rotate(30 140 41)"/>
    <rect x="162" y="70" width="8" height="6" rx="1.5" transform="rotate(60 166 73)"/>
    <rect x="166" y="112" width="8" height="6" rx="1.5" transform="rotate(100 170 115)"/>
    <rect x="140" y="152" width="8" height="6" rx="1.5" transform="rotate(135 144 155)"/>
    <rect x="96" y="168" width="8" height="6" rx="1.5"/>
    <rect x="52" y="152" width="8" height="6" rx="1.5" transform="rotate(225 56 155)"/>
    <rect x="26" y="112" width="8" height="6" rx="1.5" transform="rotate(260 30 115)"/>
    <rect x="30" y="70" width="8" height="6" rx="1.5" transform="rotate(300 34 73)"/>
    <rect x="56" y="38" width="8" height="6" rx="1.5" transform="rotate(330 60 41)"/>
  </g>

  <!-- Horizontal Time Series Wave cutting through the iris like an Equator of Time -->
  <path d="M 20 100 Q 50 78 80 100 T 120 100 Q 140 122 180 100" 
        fill="none" stroke="{WHITE}" stroke-width="2" stroke-linecap="round"/>

  <!-- Dual Eye Horizons forming the central aperture -->
  <path d="M 44 100 C 60 70, 140 70, 156 100 C 140 130, 60 130, 44 100 Z" 
        fill="{NAVY_BG}" stroke="{BLUE_LIGHT}" stroke-width="2.5"/>

  <!-- Core Pupil & Sensor -->
  <circle cx="100" cy="100" r="22" fill="{NAVY_PANEL}" stroke="{CYAN}" stroke-width="2"/>
  <circle cx="100" cy="100" r="11" fill="{ORANGE}"/>
  <circle cx="100" cy="100" r="4" fill="{WHITE}"/>

  <!-- Vertical "Now" Needle -->
  <line x1="100" y1="42" x2="100" y2="70" stroke="{ORANGE}" stroke-width="2.5" stroke-linecap="round"/>
  <line x1="100" y1="130" x2="100" y2="158" stroke="{ORANGE}" stroke-width="2.5" stroke-linecap="round"/>
</svg>'''

# ----------------------------------------------------------------------
# 5. LOGO 05: O Diafragma Óptico Bitemporal (The Dual-Aperture Shutter)
# ----------------------------------------------------------------------
def make_logo_05(uid="l05", size=200):
    return f'''<svg viewBox="0 0 200 200" width="{size}" height="{size}" xmlns="http://www.w3.org/2000/svg">
  <defs>
    <linearGradient id="{uid}-blade-l" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="{BLUE_DARK}"/>
      <stop offset="100%" stop-color="{BLUE}"/>
    </linearGradient>
    <linearGradient id="{uid}-blade-r" x1="100%" y1="0%" x2="0%" y2="100%">
      <stop offset="0%" stop-color="{CYAN}"/>
      <stop offset="100%" stop-color="{BLUE_LIGHT}"/>
    </linearGradient>
  </defs>

  <!-- Outer Camera / Satellite Sensor Housing -->
  <circle cx="100" cy="100" r="82" fill="none" stroke="{MUTED_GRID}" stroke-width="3"/>
  <circle cx="100" cy="100" r="76" fill="{NAVY_BG}" stroke="{BLUE_DARK}" stroke-width="2"/>

  <!-- Dual Opposite Aperture Blades (Janus Rotation: Past Clockwise, Future Counter-Clockwise) -->
  <!-- Left Wing Blades (Past) -->
  <path d="M 100 24 L 62 52 L 78 96 L 100 100 Z" fill="url(#{uid}-blade-l)" opacity="0.85"/>
  <path d="M 62 52 L 32 94 L 70 124 L 100 100 Z" fill="url(#{uid}-blade-l)" opacity="0.75"/>
  <path d="M 32 94 L 48 144 L 88 142 L 100 100 Z" fill="url(#{uid}-blade-l)" opacity="0.65"/>
  <path d="M 48 144 L 100 176 L 100 134 L 100 100 Z" fill="url(#{uid}-blade-l)" opacity="0.8"/>

  <!-- Right Wing Blades (Future) -->
  <path d="M 100 24 L 138 52 L 122 96 L 100 100 Z" fill="url(#{uid}-blade-r)" opacity="0.85"/>
  <path d="M 138 52 L 168 94 L 130 124 L 100 100 Z" fill="url(#{uid}-blade-r)" opacity="0.75"/>
  <path d="M 168 94 L 152 144 L 112 142 L 100 100 Z" fill="url(#{uid}-blade-r)" opacity="0.65"/>
  <path d="M 152 144 L 100 176 L 100 134 L 100 100 Z" fill="url(#{uid}-blade-r)" opacity="0.8"/>

  <!-- Center Anamorphic Eye Aperture created by the blades -->
  <ellipse cx="100" cy="100" rx="30" ry="18" fill="{NAVY_BG}" stroke="{CYAN}" stroke-width="2.5"/>
  
  <!-- Temporal Scan Line / Sensor Slit -->
  <line x1="72" y1="100" x2="128" y2="100" stroke="{WHITE}" stroke-width="1.5" stroke-dasharray="2 3"/>

  <!-- The Orange Focal Sensor ("Now") -->
  <circle cx="100" cy="100" r="10" fill="{ORANGE}"/>
  <circle cx="100" cy="100" r="4" fill="{WHITE}"/>

  <!-- Calibrated tick marks on the sensor perimeter -->
  <line x1="100" y1="18" x2="100" y2="28" stroke="{ORANGE}" stroke-width="3"/>
  <line x1="100" y1="172" x2="100" y2="182" stroke="{ORANGE}" stroke-width="3"/>
  <line x1="18" y1="100" x2="28" y2="100" stroke="{BLUE_LIGHT}" stroke-width="2"/>
  <line x1="172" y1="100" x2="182" y2="100" stroke="{CYAN}" stroke-width="2"/>
</svg>'''

# ----------------------------------------------------------------------
# 6. LOGO 06: A Ruptura de Changepoint / Anomalia (The Changepoint Disturbance Eye)
# ----------------------------------------------------------------------
def make_logo_06(uid="l06", size=200):
    return f'''<svg viewBox="0 0 200 200" width="{size}" height="{size}" xmlns="http://www.w3.org/2000/svg">
  <defs>
    <linearGradient id="{uid}-break-grad" x1="0%" y1="0%" x2="100%" y2="0%">
      <stop offset="0%" stop-color="{BLUE_DARK}"/>
      <stop offset="48%" stop-color="{BLUE}"/>
      <stop offset="50%" stop-color="{ORANGE}"/>
      <stop offset="52%" stop-color="{CYAN}"/>
      <stop offset="100%" stop-color="{BLUE_LIGHT}"/>
    </linearGradient>
  </defs>

  <!-- Coordinate box subtly showing time series axis -->
  <rect x="24" y="36" width="152" height="128" rx="8" fill="{NAVY_BG}" stroke="{MUTED_GRID}" stroke-width="1.5"/>

  <!-- Confidence Intervals Envelope (p10-p90 / Trend Ribbon forming the eye silhouette) -->
  <path d="M 28 92 C 55 64, 85 64, 100 88 C 115 112, 145 112, 172 84 L 172 116 C 145 144, 115 144, 100 120 C 85 96, 55 96, 28 124 Z"
        fill="{BLUE}" opacity="0.12"/>

  <!-- Upper Eye Boundary: Past Healthy Stable Trajectory -> Sudden Sharp Abrupt Drop (Deforestation/Fire) -> Recovery -->
  <path d="M 28 88 L 60 84 L 80 86 L 98 88 L 100 124 L 120 122 L 145 114 L 172 96" 
        fill="none" stroke="url(#{uid}-break-grad)" stroke-width="4" stroke-linecap="round" stroke-linejoin="round"/>

  <!-- Lower Eye Boundary: Balancing Curve -->
  <path d="M 28 120 C 55 146, 80 146, 100 126 C 120 146, 145 146, 172 120" 
        fill="none" stroke="{BLUE_LIGHT}" stroke-width="3" stroke-linecap="round" opacity="0.8"/>

  <!-- Vertical Rupture Vector (The Changepoint Event at T0) -->
  <line x1="100" y1="46" x2="100" y2="154" stroke="{ORANGE}" stroke-width="2.5" stroke-dasharray="4 3"/>

  <!-- Center Focal Sensor / Anomaly Dot (The Eye's Glowing Core) -->
  <circle cx="100" cy="106" r="14" fill="{NAVY_PANEL}" stroke="{ORANGE}" stroke-width="2"/>
  <circle cx="100" cy="106" r="7" fill="{ORANGE}"/>
  <circle cx="100" cy="106" r="2.5" fill="{WHITE}"/>

  <!-- Historical Sample Points on the curve -->
  <circle cx="44" cy="86" r="3" fill="{BLUE}"/>
  <circle cx="68" cy="85" r="3" fill="{BLUE_LIGHT}"/>
  <circle cx="88" cy="87" r="3" fill="{BLUE_LIGHT}"/>
  <!-- Future Projected Points -->
  <circle cx="116" cy="122" r="3" fill="{CYAN}"/>
  <circle cx="138" cy="116" r="3" fill="{CYAN}"/>
  <circle cx="160" cy="104" r="3" fill="{CYAN}"/>
</svg>'''

# ----------------------------------------------------------------------
# 7. LOGO 07: A Moeda Imperial de Séries Temporais (The Roman Coin of Time Series)
# ----------------------------------------------------------------------
def make_logo_07(uid="l07", size=200):
    return f'''<svg viewBox="0 0 200 200" width="{size}" height="{size}" xmlns="http://www.w3.org/2000/svg">
  <defs>
    <linearGradient id="{uid}-coin-rim" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="{BLUE_LIGHT}"/>
      <stop offset="50%" stop-color="{BLUE_DARK}"/>
      <stop offset="100%" stop-color="{CYAN}"/>
    </linearGradient>
    <linearGradient id="{uid}-past-fill" x1="0%" y1="0%" x2="100%" y2="0%">
      <stop offset="0%" stop-color="{BLUE_DARK}"/>
      <stop offset="100%" stop-color="{BLUE}"/>
    </linearGradient>
    <linearGradient id="{uid}-future-fill" x1="0%" y1="0%" x2="100%" y2="0%">
      <stop offset="0%" stop-color="{BLUE}"/>
      <stop offset="100%" stop-color="{CYAN}"/>
    </linearGradient>
  </defs>

  <!-- Ancient Roman 'As' Coin Outer Beaded Border (Time Markers / Calendar Ticks) -->
  <circle cx="100" cy="100" r="82" fill="none" stroke="url(#{uid}-coin-rim)" stroke-width="3"/>
  <circle cx="100" cy="100" r="75" fill="{NAVY_BG}" stroke="{MUTED_GRID}" stroke-width="1.5"/>
  <circle cx="100" cy="100" r="71" fill="none" stroke="{BLUE_LIGHT}" stroke-width="1" stroke-dasharray="2 5" opacity="0.7"/>

  <!-- Inside the Medallion: Two Opposing Classical Silhouettes Sculpted from Area Curves -->
  <!-- Left Side: Past Area Curves (Beard / Profile of Janus looking left) -->
  <path d="M 100 48 C 82 48, 64 60, 54 78 C 66 84, 52 98, 48 110 C 62 112, 54 130, 68 144 C 80 152, 92 152, 100 152 Z"
        fill="url(#{uid}-past-fill)" opacity="0.65"/>
        
  <!-- Right Side: Future Area Curves (Profile of Janus looking right) -->
  <path d="M 100 48 C 118 48, 136 60, 146 78 C 134 84, 148 98, 152 110 C 138 112, 146 130, 132 144 C 120 152, 108 152, 100 152 Z"
        fill="url(#{uid}-future-fill)" opacity="0.65"/>

  <!-- Central Shared Eye of the Bifrons Medallion -->
  <path d="M 72 100 C 84 84, 116 84, 128 100 C 116 116, 84 116, 72 100 Z" 
        fill="{NAVY_BG}" stroke="{WHITE}" stroke-width="2.5"/>

  <!-- The Golden/Orange Pupil (The Eternal Now) -->
  <circle cx="100" cy="100" r="11" fill="{ORANGE}"/>
  <circle cx="100" cy="100" r="4" fill="{WHITE}"/>

  <!-- Roman Numerals / Temporal Meridian Markers (I / N) -->
  <text x="100" y="38" text-anchor="middle" fill="{ORANGE}" font-size="10" font-weight="900" font-family="sans-serif">T₀</text>
  <text x="100" y="172" text-anchor="middle" fill="{ORANGE}" font-size="9" font-weight="bold" font-family="sans-serif">JANVS</text>

  <!-- Left/Right Roman Indicators: PAST (-) and FUTURE (+) -->
  <text x="32" y="104" text-anchor="middle" fill="{BLUE_LIGHT}" font-size="11" font-weight="bold" font-family="sans-serif">◀</text>
  <text x="168" y="104" text-anchor="middle" fill="{CYAN}" font-size="11" font-weight="bold" font-family="sans-serif">▶</text>
</svg>'''

# ----------------------------------------------------------------------
# 8. LOGO 08: O Cubo Espaço-Tempo / Voxel Hiperdimensional (The Spacetime Voxel Eye)
# ----------------------------------------------------------------------
def make_logo_08(uid="l08", size=200):
    return f'''<svg viewBox="0 0 200 200" width="{size}" height="{size}" xmlns="http://www.w3.org/2000/svg">
  <defs>
    <linearGradient id="{uid}-top-cube" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="{BLUE_LIGHT}"/>
      <stop offset="100%" stop-color="{BLUE}"/>
    </linearGradient>
    <linearGradient id="{uid}-left-cube" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="{BLUE}"/>
      <stop offset="100%" stop-color="{BLUE_DARK}"/>
    </linearGradient>
    <linearGradient id="{uid}-right-cube" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="{CYAN}"/>
      <stop offset="100%" stop-color="{BLUE}"/>
    </linearGradient>
  </defs>

  <!-- Isometric Spacetime Data Cube [T, Y, X] -->
  <!-- Top Face: Spatial Matrix (The Map / X-Y Raster Canvas) -->
  <polygon points="100,28 156,60 100,92 44,60" 
           fill="url(#{uid}-top-cube)" stroke="{WHITE}" stroke-width="1.5" opacity="0.85"/>
  <!-- Spatial grid lines on top -->
  <line x1="72" y1="44" x2="128" y2="76" stroke="{WHITE}" stroke-width="1" opacity="0.4"/>
  <line x1="128" y1="44" x2="72" y2="76" stroke="{WHITE}" stroke-width="1" opacity="0.4"/>

  <!-- Left Face: Past Temporal Depth (Slices going back in history) -->
  <polygon points="44,60 100,92 100,164 44,132" 
           fill="url(#{uid}-left-cube)" stroke="{WHITE}" stroke-width="1.5" opacity="0.9"/>
  <!-- Historical Time Series waves on left face -->
  <path d="M 48 95 Q 68 85 82 108 T 96 125" fill="none" stroke="{BLUE_LIGHT}" stroke-width="2" opacity="0.8"/>
  <path d="M 48 115 Q 68 105 82 128 T 96 145" fill="none" stroke="{BLUE_LIGHT}" stroke-width="1.5" opacity="0.5"/>

  <!-- Right Face: Future Temporal Projection (Forecast & Anomalies) -->
  <polygon points="100,92 156,60 156,132 100,164" 
           fill="url(#{uid}-right-cube)" stroke="{WHITE}" stroke-width="1.5" opacity="0.9"/>
  <!-- Future Trend wave on right face -->
  <path d="M 104 125 Q 118 108 132 118 T 152 95" fill="none" stroke="{CYAN_LIGHT}" stroke-width="2" opacity="0.8"/>
  <path d="M 104 145 Q 118 128 132 138 T 152 115" fill="none" stroke="{CYAN_LIGHT}" stroke-width="1.5" opacity="0.5"/>

  <!-- The Central Vertex Unfolds into the Cosmic Eye of Janus -->
  <!-- Translucent Eye Almond floating precisely at the space-time origin (100, 92) -->
  <path d="M 64 92 C 80 72, 120 72, 136 92 C 120 112, 80 112, 64 92 Z" 
        fill="{NAVY_BG}" stroke="{WHITE}" stroke-width="2.5"/>

  <!-- Core Pupil: The GPU Voxel Cursor ("Now" Marker in Orange) -->
  <circle cx="100" cy="92" r="14" fill="{NAVY_PANEL}" stroke="{ORANGE}" stroke-width="2"/>
  <circle cx="100" cy="92" r="7" fill="{ORANGE}"/>
  <circle cx="100" cy="92" r="2.5" fill="{WHITE}"/>

  <!-- Vertical Temporal Axis Laser Pin -->
  <line x1="100" y1="14" x2="100" y2="28" stroke="{ORANGE}" stroke-width="3" stroke-linecap="round"/>
  <line x1="100" y1="164" x2="100" y2="182" stroke="{ORANGE}" stroke-width="3" stroke-linecap="round"/>
</svg>'''

# ----------------------------------------------------------------------
# 9. LOGO 09: O Olho de Fourier / Decomposição Harmônica (The Harmonic Streamgraph Eye)
# ----------------------------------------------------------------------
def make_logo_09(uid="l09", size=200):
    return f'''<svg viewBox="0 0 200 200" width="{size}" height="{size}" xmlns="http://www.w3.org/2000/svg">
  <defs>
    <linearGradient id="{uid}-stream-top" x1="0%" y1="0%" x2="100%" y2="0%">
      <stop offset="0%" stop-color="{BLUE_DARK}"/>
      <stop offset="50%" stop-color="{BLUE_LIGHT}"/>
      <stop offset="100%" stop-color="{CYAN}"/>
    </linearGradient>
    <linearGradient id="{uid}-stream-mid" x1="0%" y1="0%" x2="100%" y2="0%">
      <stop offset="0%" stop-color="{BLUE}"/>
      <stop offset="50%" stop-color="{CYAN}"/>
      <stop offset="100%" stop-color="{BLUE_LIGHT}"/>
    </linearGradient>
    <linearGradient id="{uid}-stream-bot" x1="0%" y1="0%" x2="100%" y2="0%">
      <stop offset="0%" stop-color="{BLUE_DARK}"/>
      <stop offset="50%" stop-color="{BLUE}"/>
      <stop offset="100%" stop-color="{BLUE_DARK}"/>
    </linearGradient>
  </defs>

  <!-- Flowing Multitemporal Streamgraph Ribbons converging to form an Eye Silhouette -->
  <!-- Top Ribbon (Annual Season Harmonic) -->
  <path d="M 20 100 C 45 68, 75 58, 100 58 C 125 58, 155 68, 180 100 C 155 78, 125 70, 100 70 C 75 70, 45 78, 20 100 Z" 
        fill="url(#{uid}-stream-top)" opacity="0.85"/>

  <!-- Upper-Middle Ribbon (Semiannual Harmonic) -->
  <path d="M 20 100 C 45 78, 75 70, 100 70 C 125 70, 155 78, 180 100 C 155 88, 125 82, 100 82 C 75 82, 45 88, 20 100 Z" 
        fill="url(#{uid}-stream-mid)" opacity="0.75"/>

  <!-- Lower-Middle Ribbon -->
  <path d="M 20 100 C 45 112, 75 118, 100 118 C 125 118, 155 112, 180 100 C 155 122, 125 130, 100 130 C 75 130, 45 122, 20 100 Z" 
        fill="url(#{uid}-stream-mid)" opacity="0.75"/>

  <!-- Bottom Ribbon (Long-term Interannual Trend) -->
  <path d="M 20 100 C 45 122, 75 130, 100 130 C 125 130, 155 122, 180 100 C 155 132, 125 142, 100 142 C 75 142, 45 132, 20 100 Z" 
        fill="url(#{uid}-stream-bot)" opacity="0.85"/>

  <!-- Central Optical Core (Negative space iris created by the converging waves) -->
  <circle cx="100" cy="100" r="26" fill="{NAVY_BG}" stroke="{CYAN}" stroke-width="2"/>
  
  <!-- Sinuous Wave Signal inside the Core -->
  <path d="M 76 100 Q 88 88 100 100 T 124 100" fill="none" stroke="{WHITE}" stroke-width="2" stroke-linecap="round"/>

  <!-- Orange Pulse ("Now" Point) -->
  <circle cx="100" cy="100" r="9" fill="{ORANGE}"/>
  <circle cx="100" cy="100" r="3" fill="{WHITE}"/>

  <!-- High-Precision Vertical Temporal Horizon -->
  <line x1="100" y1="36" x2="100" y2="52" stroke="{ORANGE}" stroke-width="2.5" stroke-linecap="round"/>
  <line x1="100" y1="148" x2="100" y2="164" stroke="{ORANGE}" stroke-width="2.5" stroke-linecap="round"/>
</svg>'''

# ----------------------------------------------------------------------
# 10. LOGO 10: O Glifo Bitemporal "J" / Monograma Orbital (The Janus Double-J Monogram)
# ----------------------------------------------------------------------
def make_logo_10(uid="l10", size=200):
    return f'''<svg viewBox="0 0 200 200" width="{size}" height="{size}" xmlns="http://www.w3.org/2000/svg">
  <defs>
    <linearGradient id="{uid}-j-left" x1="100%" y1="0%" x2="0%" y2="100%">
      <stop offset="0%" stop-color="{BLUE_LIGHT}"/>
      <stop offset="100%" stop-color="{BLUE_DARK}"/>
    </linearGradient>
    <linearGradient id="{uid}-j-right" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="{CYAN}"/>
      <stop offset="100%" stop-color="{BLUE}"/>
    </linearGradient>
  </defs>

  <!-- Left 'J' (Mirrored, Facing the Past) -->
  <!-- Top Crossbar, Stem, and Bottom Curve sweeping backwards like an orbit / lower eyelid -->
  <path d="M 100 48 L 52 48 L 52 62 L 78 62 L 78 116 C 78 136, 62 144, 46 144 C 36 144, 28 140, 24 136 L 20 152 C 28 158, 40 162, 52 162 C 82 162, 98 144, 98 116 Z" 
        fill="url(#{uid}-j-left)"/>

  <!-- Right 'J' (Facing the Future) -->
  <!-- Top Crossbar, Stem, and Bottom Curve sweeping forward -->
  <path d="M 100 48 L 148 48 L 148 62 L 122 62 L 122 116 C 122 136, 138 144, 154 144 C 164 144, 172 140, 176 136 L 180 152 C 172 158, 160 162, 148 162 C 118 162, 102 144, 102 116 Z" 
        fill="url(#{uid}-j-right)"/>

  <!-- The Interior Dynamic Opening Creates an Eye Contour -->
  <path d="M 78 98 C 88 82, 112 82, 122 98 C 112 114, 88 114, 78 98 Z" 
        fill="{NAVY_BG}" stroke="{WHITE}" stroke-width="2"/>

  <!-- Orange Pulse ("Now" Cursor at the Center of the Glyphs) -->
  <circle cx="100" cy="98" r="9" fill="{ORANGE}"/>
  <circle cx="100" cy="98" r="3.5" fill="{WHITE}"/>

  <!-- Horizontal Time Signal Wave crossing the heart of the monogram -->
  <line x1="42" y1="98" x2="74" y2="98" stroke="{BLUE_LIGHT}" stroke-width="2" stroke-linecap="round"/>
  <line x1="126" y1="98" x2="158" y2="98" stroke="{CYAN}" stroke-width="2" stroke-linecap="round"/>

  <!-- Upper Beacon / Time Needle -->
  <polygon points="100,24 94,40 106,40" fill="{ORANGE}"/>
</svg>'''

# ----------------------------------------------------------------------
# List of all 10 logos with metadata
# ----------------------------------------------------------------------
LOGOS = [
    {
        "id": "01-olho-onda-fenologica",
        "title": "01. O Olho da Onda Fenológica",
        "subtitle": "The Phenological Waveform Eye",
        "fn": make_logo_01,
        "tags": ["Séries Temporais", "Olho / Óptica", "Ciclo Sazonal"],
        "concept": "A clássica amêndoa do olho é gerada geometricamente pelo cruzamento de duas ondas senoidais harmônicas: o pico fenológico de crescimento vegetal no topo e o vale de senescência na base. Um sinal espectral atravessa a pupila, onde o cursor laranja (#F29A38) marca o instante presente.",
        "mythology": "Representa o ciclo eterno das estações que Janus presidia no calendário romano arcaico, unindo a visão biológica à matemática analítica."
    },
    {
        "id": "02-perfil-bifronte-espectral",
        "title": "02. O Perfil Bifronte Espectral",
        "subtitle": "The Dual-Profile Chrono-Face",
        "fn": make_logo_02,
        "tags": ["Mito Romano", "Duas Faces", "Série em Degraus"],
        "concept": "A iconografia mais clássica de Janus — dois perfis opostos olhando para o passado e para o futuro — esculpida através de linhas poligonais cartesianas de séries temporais. O espaço negativo central revela um olho vertical luminoso onde pulsa o cursor temporal.",
        "mythology": "Direto à lenda romana: Praeterita (olhar o arquivo histórico à esquerda) e Futura (projetar a tendência à direita), sem mover a cabeça."
    },
    {
        "id": "03-portal-laminas-temporais",
        "title": "03. O Portal de Lâminas Temporais",
        "subtitle": "The Chrono-Slice Portal (Ianua)",
        "fn": make_logo_03,
        "tags": ["Mito Romano", "Data Cube", "Portal Ianua"],
        "concept": "O templo de Janus no Fórum Romano era uma passagem em arco (Ianua) aberta nos dois sentidos. Aqui, o portal é erguido por camadas empilhadas de rasters (slices do data cube). No interior do arco, surge a óptica de observação da Terra com um feixe vertical de corte temporal.",
        "mythology": "Janus como o guardião das portas e transições de estado, simbolizando a passagem da imagem estática para o cubo multitemporal."
    },
    {
        "id": "04-anel-dendrocronologico",
        "title": "04. A Íris Dendrocronológica & Orbital",
        "subtitle": "The Dendro-Temporal Orbit Eye",
        "fn": make_logo_04,
        "tags": ["Dendrocronologia", "Órbitas", "Tempo Cíclico"],
        "concept": "Assim como os anéis das árvores registram a história do clima, os satélites registram o tempo em órbitas periódicas repetitivas. A íris é composta por anéis concêntricos de pixels multiespectrais, cortados por uma onda de série temporal que atravessa o centro focal.",
        "mythology": "O tempo circular dos ciclos cósmicos e celestes em harmonia com a observação contínua da Terra pelo satélite."
    },
    {
        "id": "05-diafragma-bitemporal",
        "title": "05. O Diafragma Óptico Bitemporal",
        "subtitle": "The Dual-Aperture Shutter",
        "fn": make_logo_05,
        "tags": ["Sensor de Satélite", "Óptica", "Rotação Dual"],
        "concept": "O olho como telescópio e câmera orbital. Um obturador mecânico de satélite com lâminas que giram em sentidos opostos: a metade esquerda gira em sentido horário (retrospectiva temporal) e a direita em sentido anti-horário (prospectiva), formando uma pupila anamórfica.",
        "mythology": "A rotação simultânea das duas perspectivas temporais de Janus convertida em engenharia aeroespacial e precisão de CCDs."
    },
    {
        "id": "06-ruptura-changepoint",
        "title": "06. O Ponto de Ruptura / Anomalia",
        "subtitle": "The Changepoint Disturbance Eye",
        "fn": make_logo_06,
        "tags": ["Detecção de Mudança", "BFAST / LandTrendr", "Anomalia"],
        "concept": "Na análise de séries temporais de sensoriamento remoto, o evento mais crucial é a quebra estrutural (desmatamento, queimada, desastre). O contorno superior do olho é uma série temporal estável que sofre uma queda abrupta no instante T0, acendendo o sensor de alerta laranja.",
        "mythology": "Janus como o deus da transição de estados: o momento da mudança onde o estado anterior cessa e um novo ecossistema se estabelece."
    },
    {
        "id": "07-medalhao-romano-series",
        "title": "07. A Moeda Imperial de Séries",
        "subtitle": "The Roman Coin of Time Series",
        "fn": make_logo_07,
        "tags": ["Numismática", "Mito Romano", "Gráficos de Área"],
        "concept": "Inspirado no clássico 'As' republicano de bronze com o busto duplo de Janus. No interior da moeda, as silhuetas de Janus são desenhadas por curvas de área preenchida (streamgraphs de envelope estatístico), compartilhando uma pupila central solar com a inscrição T0 e JANVS.",
        "mythology": "Homenagem à herança latina e numismática clássica romana, transmitindo solidez institucional, prestígio acadêmico e perenidade."
    },
    {
        "id": "08-cubo-espaco-tempo-voxel",
        "title": "08. O Cubo Espaço-Tempo / Voxel",
        "subtitle": "The Spacetime Voxel Eye",
        "fn": make_logo_08,
        "tags": ["Data Cube [T,Y,X]", "Computação GPU", "Isometria"],
        "concept": "A própria estrutura de dados do Janus: o cubo [T, Y, X]. Em perspectiva isométrica, a face superior exibe a matriz espacial (mapa 2D), a face esquerda exibe a história temporal passada e a direita exibe projeções futuras. No vértice onde espaço e tempo colidem, nasce o olho do observador.",
        "mythology": "A onipresença dimensional de Janus sintetizada na arquitetura de data cube em memória de GPU."
    },
    {
        "id": "09-streamgraph-harmonico",
        "title": "09. O Olho de Fourier / Harmônico",
        "subtitle": "The Harmonic Streamgraph Eye",
        "fn": make_logo_09,
        "tags": ["Decomposição de Fourier", "Streamgraph", "Fluidez"],
        "concept": "Fitas fluídas de densidade de séries temporais (anual, semestral e interanual) que se entrelaçam e convergem no espaço central, esculpindo por sobreposição a forma exata de uma amêndoa de olho. Uma agulha de data laranja marca o instante presente cortando as faixas espectrais.",
        "mythology": "O fluxo contínuo e irreversível do rio do tempo, orquestrado pela métrica harmônica de Fourier."
    },
    {
        "id": "10-glifo-duplo-j",
        "title": "10. O Glifo Bitemporal 'J'",
        "subtitle": "The Janus Double-J Monogram",
        "fn": make_logo_10,
        "tags": ["Identidade Tipográfica", "Monograma", "App Icon / 'jn'"],
        "concept": "Dois caracteres 'J' espelhados costas com costas em perfeita simetria dorsal. As hastes verticais formam o meridiano temporal, as curvas inferiores formam o arco orbital e a pálpebra inferior, e o espaço interno emoldura um pulso de sinal com o cursor laranja. Perfeito para ícone de dock e terminal.",
        "mythology": "A máxima simplificação modernista do mito de Janus: a letra do seu próprio nome convertida no portal e no olho bitemporal."
    }
]

# ----------------------------------------------------------------------
# Generation Script Execution
# ----------------------------------------------------------------------
print("Generating 10 SVG files...")
for item in LOGOS:
    svg_code = item["fn"](uid=item["id"], size=200)
    svg_path = OUT_DIR / f"{item['id']}.svg"
    svg_path.write_text(svg_code, encoding="utf-8")
    print(f"  -> Written {svg_path.name}")

print("\nRasterizing 512x512 PNGs via PyMuPDF...")
for item in LOGOS:
    svg_path = OUT_DIR / f"{item['id']}.svg"
    png_path = PNG_DIR / f"{item['id']}-512.png"
    
    doc = pymupdf.open(str(svg_path))
    page = doc[0]
    rect = page.rect
    scale = 512 / rect.width
    mat = pymupdf.Matrix(scale, scale)
    pix = page.get_pixmap(matrix=mat, alpha=True)
    pix.save(str(png_path))
    doc.close()
    print(f"  -> Rendered PNG {png_path.name}")

# ----------------------------------------------------------------------
# Generate Standalone Interactive HTML Showcase
# ----------------------------------------------------------------------
print("\nGenerating interactive HTML showcase...")

cards_html = []
for item in LOGOS:
    svg_raw = item["fn"](uid=f"showcase-{item['id']}", size=150)
    tags_html = "".join([f'<span class="tag">{t}</span>' for t in item["tags"]])
    
    card = f'''
    <div class="logo-card" data-id="{item['id']}">
      <div class="card-preview">
        <div class="svg-container">
          {svg_raw}
        </div>
      </div>
      <div class="card-info">
        <div class="card-header">
          <span class="card-sub">{item['subtitle']}</span>
          <h3 class="card-title">{item['title']}</h3>
          <div class="tags-container">{tags_html}</div>
        </div>
        <div class="card-desc">
          <p><strong>Conceito Gráfico:</strong> {item['concept']}</p>
          <p class="myth-note"><strong>Raiz Mitológica:</strong> {item['mythology']}</p>
        </div>
        <div class="card-actions">
          <a href="{item['id']}.svg" download class="btn btn-outline">Baixar SVG</a>
          <a href="png/{item['id']}-512.png" target="_blank" class="btn btn-primary">Ver PNG 512px</a>
        </div>
      </div>
    </div>
    '''
    cards_html.append(card)

html_content = f'''<!DOCTYPE html>
<html lang="pt-BR">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>Janus — Exploração dos 10 Logos Conceituais</title>
  <style>
    :root {{
      --bg: #090E17;
      --card-bg: #111A2E;
      --card-preview: #070B12;
      --card-border: #1E2D4A;
      --text: #F1F5F9;
      --text-muted: #94A3B8;
      --blue: #4296FA;
      --blue-light: #6AADFF;
      --cyan: #00F2FE;
      --orange: #F29A38;
      --tag-bg: rgba(66, 150, 250, 0.12);
      --tag-text: #6AADFF;
      --font-main: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
    }}
    
    [data-theme="light"] {{
      --bg: #F8FAFC;
      --card-bg: #FFFFFF;
      --card-preview: #F1F5F9;
      --card-border: #E2E8F0;
      --text: #0F172A;
      --text-muted: #64748B;
      --blue: #2563EB;
      --blue-light: #3B82F6;
      --cyan: #0284C7;
      --orange: #EA580C;
      --tag-bg: rgba(37, 99, 235, 0.08);
      --tag-text: #2563EB;
    }}

    * {{ box-sizing: border-box; margin: 0; padding: 0; transition: background-color 0.2s, border-color 0.2s; }}
    body {{
      background-color: var(--bg);
      color: var(--text);
      font-family: var(--font-main);
      line-height: 1.6;
      padding: 40px 24px;
    }}

    .container {{
      max-width: 1300px;
      margin: 0 auto;
    }}

    header {{
      margin-bottom: 40px;
      border-bottom: 1px solid var(--card-border);
      padding-bottom: 30px;
    }}

    .top-bar {{
      display: flex;
      justify-content: space-between;
      align-items: center;
      margin-bottom: 16px;
    }}

    .badge {{
      display: inline-block;
      background: rgba(242, 154, 56, 0.15);
      color: var(--orange);
      padding: 4px 12px;
      border-radius: 9999px;
      font-size: 13px;
      font-weight: 600;
      text-transform: uppercase;
      letter-spacing: 0.5px;
    }}

    .theme-toggle {{
      background: var(--card-bg);
      color: var(--text);
      border: 1px solid var(--card-border);
      padding: 8px 16px;
      border-radius: 8px;
      font-size: 14px;
      cursor: pointer;
      font-weight: 500;
    }}
    .theme-toggle:hover {{
      border-color: var(--blue);
    }}

    h1 {{
      font-size: 32px;
      font-weight: 800;
      letter-spacing: -0.5px;
      margin-bottom: 8px;
    }}

    p.lead {{
      font-size: 16px;
      color: var(--text-muted);
      max-width: 900px;
    }}

    .concept-pillars {{
      display: grid;
      grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
      gap: 16px;
      margin: 32px 0 48px 0;
    }}

    .pillar {{
      background: var(--card-bg);
      border: 1px solid var(--card-border);
      padding: 20px;
      border-radius: 12px;
    }}

    .pillar-num {{
      font-size: 12px;
      font-weight: 700;
      color: var(--orange);
      text-transform: uppercase;
      margin-bottom: 4px;
    }}

    .pillar-title {{
      font-size: 16px;
      font-weight: 700;
      margin-bottom: 6px;
      color: var(--blue-light);
    }}

    .pillar-desc {{
      font-size: 13px;
      color: var(--text-muted);
    }}

    .grid {{
      display: grid;
      grid-template-columns: repeat(auto-fill, minmax(380px, 1fr));
      gap: 24px;
    }}

    .logo-card {{
      background: var(--card-bg);
      border: 1px solid var(--card-border);
      border-radius: 14px;
      overflow: hidden;
      display: flex;
      flex-direction: column;
      box-shadow: 0 4px 20px rgba(0,0,0,0.15);
    }}

    .card-preview {{
      background: var(--card-preview);
      padding: 36px 20px;
      display: flex;
      align-items: center;
      justify-content: center;
      border-bottom: 1px solid var(--card-border);
      min-height: 220px;
    }}

    .svg-container svg {{
      display: block;
      filter: drop-shadow(0 6px 16px rgba(0,0,0,0.3));
    }}

    .card-info {{
      padding: 24px;
      display: flex;
      flex-direction: column;
      flex-grow: 1;
    }}

    .card-sub {{
      font-size: 11px;
      font-weight: 700;
      text-transform: uppercase;
      letter-spacing: 0.5px;
      color: var(--cyan);
      display: block;
      margin-bottom: 4px;
    }}

    .card-title {{
      font-size: 18px;
      font-weight: 700;
      margin-bottom: 10px;
    }}

    .tags-container {{
      display: flex;
      flex-wrap: wrap;
      gap: 6px;
      margin-bottom: 16px;
    }}

    .tag {{
      background: var(--tag-bg);
      color: var(--tag-text);
      font-size: 11px;
      font-weight: 600;
      padding: 3px 8px;
      border-radius: 6px;
    }}

    .card-desc {{
      font-size: 13px;
      color: var(--text-muted);
      margin-bottom: 20px;
      flex-grow: 1;
    }}
    .card-desc p {{
      margin-bottom: 8px;
    }}
    .card-desc strong {{
      color: var(--text);
    }}
    .myth-note {{
      border-left: 2px solid var(--orange);
      padding-left: 8px;
      font-style: italic;
    }}

    .card-actions {{
      display: flex;
      gap: 10px;
      margin-top: auto;
    }}

    .btn {{
      display: inline-block;
      text-decoration: none;
      padding: 8px 14px;
      border-radius: 8px;
      font-size: 12px;
      font-weight: 600;
      text-align: center;
      cursor: pointer;
    }}

    .btn-outline {{
      background: transparent;
      border: 1px solid var(--card-border);
      color: var(--text);
    }}
    .btn-outline:hover {{
      border-color: var(--blue);
      color: var(--blue-light);
    }}

    .btn-primary {{
      background: var(--blue);
      border: 1px solid var(--blue);
      color: #FFFFFF;
    }}
    .btn-primary:hover {{
      background: var(--blue-light);
    }}

    footer {{
      margin-top: 60px;
      border-top: 1px solid var(--card-border);
      padding-top: 24px;
      text-align: center;
      font-size: 13px;
      color: var(--text-muted);
    }}
  </style>
</head>
<body>
  <div class="container">
    <header>
      <div class="top-bar">
        <span class="badge">Estudo de Identidade Visual</span>
        <button class="theme-toggle" onclick="toggleTheme()">Alternar Tema Claro / Escuro</button>
      </div>
      <h1>Janus — 10 Propostas de Logos</h1>
      <p class="lead">
        Uma exploração conceitual da lenda romana de Janus, a óptica do olho de satélite, a 4ª dimensão do tempo e a morfologia das séries temporais raster.
      </p>

      <div class="concept-pillars">
        <div class="pillar">
          <div class="pillar-num">Pilar 1</div>
          <div class="pillar-title">Mito Romano de Janus</div>
          <div class="pillar-desc">Ianus Bifrons, deus dos começos, portais (ianuae) e passagens. Duas faces que contemplam o passado e o futuro simultaneamente.</div>
        </div>
        <div class="pillar">
          <div class="pillar-num">Pilar 2</div>
          <div class="pillar-title">O Olho & Sensor Orbital</div>
          <div class="pillar-desc">O olho como lente de satélite (Landsat, Sentinel). A pupila como ponto focal que transforma radiação eletromagnética em percepção analítica.</div>
        </div>
        <div class="pillar">
          <div class="pillar-num">Pilar 3</div>
          <div class="pillar-title">O Tempo & Cursor Presente</div>
          <div class="pillar-desc">O tempo físico do cubo multitemporal e o marcador de data presente em laranja (#F29A38), cor canônica do ImPlot no Janus.</div>
        </div>
        <div class="pillar">
          <div class="pillar-num">Pilar 4</div>
          <div class="pillar-title">Séries Temporais & Ondas</div>
          <div class="pillar-desc">Curvas fenológicas harmônicas, envelopes estatísticos de incerteza (p10-p90) e quebras bruscas por detecção de mudanças (BFAST / LandTrendr).</div>
        </div>
      </div>
    </header>

    <div class="grid">
      {"".join(cards_html)}
    </div>

    <footer>
      Janus — A fast desktop viewer for raster time series and data cubes. Cores oficiais da interface ImGui/ImPlot.
    </footer>
  </div>

  <script>
    function toggleTheme() {{
      const current = document.documentElement.getAttribute('data-theme');
      const target = current === 'light' ? 'dark' : 'light';
      document.documentElement.setAttribute('data-theme', target);
      localStorage.setItem('janus-theme', target);
    }}
    const saved = localStorage.getItem('janus-theme') || 'dark';
    document.documentElement.setAttribute('data-theme', saved);
  </script>
</body>
</html>
'''

showcase_path = OUT_DIR / "showcase-10-logos.html"
showcase_path.write_text(html_content, encoding="utf-8")
print(f"  -> Generated showcase: {showcase_path}")
print("\nAll 10 logos generated and rasterized successfully!")
