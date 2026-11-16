#!/usr/bin/env python3
"""Make the small images of the guide cards: site/assets/img/thumbs/<slug>.jpg.

The cards of guides.html and of the home page show each guide's `thumb` (a screenshot of
300-500 KB, see tools/build_site.py); this writes a copy 640 px wide at about JPEG quality 80
for each, which build_site.py uses when the file exists. It regenerates all of them and skips
those newer than their source:

    python tools/make_thumbs.py          # make the missing or out-of-date ones
    python tools/make_thumbs.py --force  # make all again

Needs ffmpeg on PATH (or at the path of the WinGet install below) and the "thumb" fields of
site/data/guides.json, which `python tools/build_site.py` fills. Python 3 standard library only.
Then run `python tools/build_site.py` to point the cards at the new files.
"""

import argparse
import json
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SITE = ROOT / "site"
WINGET = Path(
    r"C:\Users\eduar\AppData\Local\Microsoft\WinGet\Packages"
    r"\Gyan.FFmpeg_Microsoft.Winget.Source_8wekyb3d8bbwe\ffmpeg-9.0.2-full_build\bin\ffmpeg.exe"
)
WIDTH = 640
QSCALE = "4"  # ffmpeg's MJPEG scale, 2 (best) to 31: 4 is about quality 80


def ffmpeg():
    found = shutil.which("ffmpeg")
    if found:
        return found
    if WINGET.exists():
        return str(WINGET)
    raise SystemExit("ffmpeg not found: put it on PATH")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--force", action="store_true", help="make every thumbnail, even the up-to-date ones")
    args = ap.parse_args()

    with open(SITE / "data" / "guides.json", encoding="utf-8") as f:
        guides = json.load(f)
    out_dir = SITE / "assets" / "img" / "thumbs"
    out_dir.mkdir(parents=True, exist_ok=True)
    exe = ffmpeg()
    made = before = after = 0
    for g in guides:
        if not g.get("thumb"):
            continue
        src, dst = SITE / g["thumb"], out_dir / f"{g['slug']}.jpg"
        before += src.stat().st_size
        if args.force or not dst.exists() or dst.stat().st_mtime < src.stat().st_mtime:
            cmd = [
                exe, "-v", "error", "-y", "-threads", "4", "-i", str(src),
                "-vf", f"scale=w='min({WIDTH},iw)':h=-2:flags=lanczos",
                "-frames:v", "1", "-update", "1", "-q:v", QSCALE, "-pix_fmt", "yuvj420p", str(dst),
            ]
            subprocess.run(cmd, check=True)
            made += 1
        after += dst.stat().st_size
    print(f"{made} made, {len(guides) - made} kept; sources {before / 1024:.0f} KB, thumbnails {after / 1024:.0f} KB")
    return 0


if __name__ == "__main__":
    sys.exit(main())
