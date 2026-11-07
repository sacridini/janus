#!/usr/bin/env python3
"""Make a WebP twin of every JPEG of the site: site/assets/img/**/<name>.jpg -> <name>.webp, next to it.

`python tools/build_site.py` then wraps each <img> of such a JPEG in
<picture><source type="image/webp" srcset="...webp"><img ...></picture>, so a browser that reads WebP
loads the smaller file and the JPEG stays the fallback, the Open Graph image and the link of the
lightbox. The screenshots are 31% smaller as WebP at quality 85 (39.9 MB of JPEG, 27.5 MB of WebP),
with the text of the interface as sharp as in the JPEG. Files whose .webp is newer than the .jpg are
skipped:

    python tools/make_webp.py          # make the missing or out-of-date ones
    python tools/make_webp.py --force  # make all again

Needs ffmpeg with libwebp on PATH (or at the path of the WinGet install below); 4 threads. Run it
after a screenshot changes, then run `python tools/build_site.py`. Python 3 standard library only.
"""

import argparse
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
IMG = ROOT / "site" / "assets" / "img"
WINGET = Path(
    r"C:\Users\eduar\AppData\Local\Microsoft\WinGet\Packages"
    r"\Gyan.FFmpeg_Microsoft.Winget.Source_8wekyb3d8bbwe\ffmpeg-9.0.2-full_build\bin\ffmpeg.exe"
)
QUALITY = "85"


def ffmpeg():
    found = shutil.which("ffmpeg")
    if found:
        return found
    if WINGET.exists():
        return str(WINGET)
    raise SystemExit("ffmpeg not found: put it on PATH")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--force", action="store_true", help="make every file, even the up-to-date ones")
    args = ap.parse_args()

    exe = ffmpeg()
    made = kept = jpeg = webp = 0
    for src in sorted(IMG.rglob("*.jpg")):
        dst = src.with_suffix(".webp")
        if args.force or not dst.exists() or dst.stat().st_mtime < src.stat().st_mtime:
            cmd = [exe, "-v", "error", "-y", "-threads", "4", "-i", str(src), "-frames:v", "1", "-c:v", "libwebp",
                   "-quality", QUALITY, str(dst)]
            subprocess.run(cmd, check=True)
            made += 1
        else:
            kept += 1
        jpeg += src.stat().st_size
        webp += dst.stat().st_size
    print(f"{made} made, {kept} kept; JPEG {jpeg / 1e6:.1f} MB, WebP {webp / 1e6:.1f} MB")
    return 0


if __name__ == "__main__":
    sys.exit(main())
