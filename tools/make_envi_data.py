#!/usr/bin/env python3
"""The inputs of `janus --shots envi` (site/envi.html): a Landsat series and a Sentinel-1 series of Rondonia.

    python tools/make_envi_data.py landsat SRC OUT
    python tools/make_envi_data.py s1 OUT [BOX]

landsat: SRC is the folder `tools/make_guide_data.py landsat` makes from the author's Google Earth Engine stack
         (ls_<date>.tif, the CCDC guide's data, 125 Landsat 8/9 dates of central Rondonia, 1690 x 1675 px of 30 m); the
         six July dates of 2020 to 2025 are copied to OUT (the dry season, no cloud; 2024-07-16 is hazy with the smoke of
         that year's fires, which the normalization has to undo).
s1:      Sentinel-1 RTC of 8.8 km of the south of Rondonia (about 10.7 S and 62.8 W, Sentinel-2 tile 20LNP) where forest
         was cleared in 2022 to 2024: Planetary Computer's sentinel-1-rtc, 2022-01-01 to 2024-12-31, the pass with the most
         scenes (78 dates, 12 days apart), VV and VH in linear gamma0 at 10 m, a GeoTIFF a date in OUT/linear. The box was
         found by comparing Sentinel-2 L2A NDVI over the region: forest in the winter of 2022 (0.75 or more) and below 0.5 in
         both June and October-December 2024 (a smoke haze lowers the NDVI of one date, never of two months apart): 37 % of
         the box was forest in 2022 and 12 % of it is cleared by 2024. The series is made by the example of the library that
         the guides' Sentinel-1 benchmark uses (cargo build --release -p janus-zeit --example vs_s1), which needs the
         network. BOX is W,S,E,N in degrees.

Then:  JANUS_ENVI_S1=OUT/linear janus --threads 4 --shots envi LANDSAT_OUT <shots folder>
       python tools/make_envi_figures.py <shots folder>
"""

import os
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DATES = ["2020-07-29", "2021-07-16", "2022-07-03", "2023-07-06", "2024-07-16", "2025-07-03"]
BOX = "-62.839,-10.757,-62.759,-10.677"
START, END = "2022-01-01", "2024-12-31"


def landsat(src: Path, out: Path) -> None:
    out.mkdir(parents=True, exist_ok=True)
    for d in DATES:
        shutil.copyfile(src / f"ls_{d}.tif", out / f"ls_{d}.tif")
    print(f"{len(DATES)} dates in {out}")


def s1(out: Path, box: str) -> None:
    exe = ROOT / "target" / "release" / "examples" / ("vs_s1.exe" if os.name == "nt" else "vs_s1")
    if not exe.exists():
        sys.exit(f"{exe} missing: cargo build --release -p janus-zeit --example vs_s1")
    # the example needs the GDAL that Janus stages next to janus.exe on its PATH
    env = dict(os.environ)
    release = str(ROOT / "target" / "release")
    env["PATH"] = release + os.pathsep + env.get("PATH", "")
    env.setdefault("PROJ_DATA", str(ROOT / "target" / "release" / "share" / "proj"))
    r = subprocess.run([str(exe), str(out), box, START, END, "4"], capture_output=True, text=True, env=env)
    if r.returncode:
        sys.exit(r.stderr)
    print(f"{len(list((out / 'linear').glob('*.tif')))} dates in {out / 'linear'}")


def main() -> None:
    a = sys.argv[1:]
    if len(a) == 3 and a[0] == "landsat":
        landsat(Path(a[1]), Path(a[2]))
    elif len(a) in (2, 3) and a[0] == "s1":
        s1(Path(a[1]), a[2] if len(a) == 3 else BOX)
    else:
        raise SystemExit(__doc__)


if __name__ == "__main__":
    main()
