"""The Awesome Spectral Indices (https://github.com/awesome-spectral-indices/awesome-spectral-indices, MIT) as the
raster calculator embeds them: crates/janus-cube/src/calc/spectral-indices.json, each index's formula and bands, and
the constants' defaults. Kernel indices (computed by spyndex from kernels, not bands) are left out.

    python tools/spectral_indices.py            # downloads the catalogue
    python tools/spectral_indices.py <folder>   # from spectral-indices-dict.json and constants.json in a folder
"""

import json
import sys
import urllib.request
from pathlib import Path

BASE = "https://raw.githubusercontent.com/awesome-spectral-indices/awesome-spectral-indices/main/output/"
ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "crates" / "janus-cube" / "src" / "calc" / "spectral-indices.json"


def load(name, folder):
    if folder:
        return json.loads((Path(folder) / name).read_text(encoding="utf-8"))
    with urllib.request.urlopen(BASE + name) as r:
        return json.loads(r.read().decode("utf-8"))


def main():
    folder = sys.argv[1] if len(sys.argv) > 1 else None
    indices = load("spectral-indices-dict.json", folder)["SpectralIndices"]
    constants = load("constants.json", folder)
    out = []
    for name in sorted(indices, key=str.lower):
        i = indices[name]
        if i["application_domain"] == "kernel":
            continue
        out.append({
            "n": i["short_name"],
            "l": i["long_name"],
            "d": i["application_domain"],
            "f": i["formula"],
            "b": i["bands"],
            "r": i.get("reference", ""),
        })
    consts = {k: v["default"] for k, v in sorted(constants.items())}
    OUT.write_text(json.dumps({"indices": out, "constants": consts}, ensure_ascii=False, indent=0) + "\n",
                   encoding="utf-8")
    print(f"{OUT}: {len(out)} indices, {len(consts)} constants")


if __name__ == "__main__":
    main()
