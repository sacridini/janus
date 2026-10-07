"""Assembles the private Python runtime that runs Zeit for tsv.

The result is a self-contained folder (no installer, no registry, not on PATH):

    <out>/
      python/                 Python embeddable distribution
      python/sp/              site-packages (short name: Windows path limit)
      tsv_zeit_bridge.py      the bridge (copied from zeit_bridge/)
      runtime.json            versions, for diagnostics

Usage (any Python >= 3.9 with pip, used only to download wheels):
    python tools/build_zeit_runtime.py --out build/Release/runtime
"""
import argparse
import hashlib
import json
import shutil
import subprocess
import sys
import urllib.request
import zipfile
from pathlib import Path

PY_VERSION = "3.12.10"
PY_URL = f"https://www.python.org/ftp/python/{PY_VERSION}/python-{PY_VERSION}-embed-amd64.zip"
# Pinned on first download (trust on first use); update together with PY_VERSION.
PY_SHA256 = "4acbed6dd1c744b0376e3b1cf57ce906f9dc9e95e68824584c8099a63025a3c3"

ROOT = Path(__file__).resolve().parent.parent
BRIDGE = ROOT / "zeit_bridge"


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def download_python(cache: Path) -> Path:
    cache.mkdir(parents=True, exist_ok=True)
    zpath = cache / f"python-{PY_VERSION}-embed-amd64.zip"
    if not zpath.exists():
        print(f"downloading {PY_URL}")
        tmp = zpath.with_suffix(".part")
        urllib.request.urlretrieve(PY_URL, tmp)
        tmp.rename(zpath)
    digest = sha256(zpath)
    if PY_SHA256 and digest != PY_SHA256:
        raise SystemExit(f"checksum mismatch for {zpath}: {digest}")
    print(f"python embeddable {PY_VERSION} sha256 {digest}")
    return zpath


def pip_install(target: Path, args):
    cmd = [sys.executable, "-m", "pip", "install", "--quiet", "--disable-pip-version-check", "--no-warn-conflicts",
           "--target", str(target), "--platform", "win_amd64", "--python-version", "3.12",
           "--implementation", "cp", "--only-binary=:all:", "--upgrade"] + args
    subprocess.run(cmd, check=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True, help="output folder (e.g. build/Release/runtime)")
    ap.add_argument("--cache", default=str(ROOT / "build" / "_downloads"), help="download cache")
    ap.add_argument("--zeit-wheel", help="install this local Zeit wheel instead of the PyPI pin")
    a = ap.parse_args()

    out = Path(a.out).resolve()
    py = out / "python"
    # Short site-packages folder: keeps the deepest file path well under Windows'
    # 260-character limit even when tsv is installed in a long folder.
    site = py / "sp"

    # 1. Python embeddable (fresh copy every time: reproducible).
    if py.exists():
        shutil.rmtree(py)
    py.mkdir(parents=True)
    with zipfile.ZipFile(download_python(Path(a.cache))) as z:
        z.extractall(py)
    # The ._pth file fixes sys.path for the embeddable build: add site-packages
    # and enable `import site` (needed for .pth files of some wheels).
    pth = next(py.glob("python3*._pth"))
    lines = [l for l in pth.read_text().splitlines() if l.strip() and not l.startswith("#")]
    lines += ["sp", "import site"]
    pth.write_text("\n".join(lines) + "\n")

    # 2. Dependencies (pinned) + Zeit without its heavy optional dependencies.
    site.mkdir(parents=True, exist_ok=True)
    pip_install(site, ["-r", str(BRIDGE / "requirements.txt")])
    if a.zeit_wheel:
        pip_install(site, ["--no-deps", a.zeit_wheel])
    else:
        pip_install(site, ["--no-deps", "-r", str(BRIDGE / "zeit-requirements.txt")])

    # Test suites are never imported at runtime and are ~30% of the size.
    removed = 0
    for d in sorted(site.rglob("tests"), key=lambda p: len(p.parts), reverse=True):
        if d.is_dir() and d.exists():
            removed += sum(f.stat().st_size for f in d.rglob("*") if f.is_file())
            shutil.rmtree(d)
    print(f"removed {removed / 2**20:.0f} MB of test suites")

    # 3. Bridge + metadata.
    for f in BRIDGE.glob("*.py"):
        shutil.copy2(f, out / f.name)
    pyexe = py / "python.exe"
    info = subprocess.run(
        [str(pyexe), "-c", "import sys, zeit, numpy, rasterio; import json; print(json.dumps({"
         "'python': sys.version.split()[0], 'zeit': getattr(zeit, '__version__', None) or "
         "__import__('importlib.metadata').metadata.version('zeit-cdts'), 'numpy': numpy.__version__, "
         "'rasterio': rasterio.__version__}))"],
        check=True, capture_output=True, text=True).stdout.strip()
    (out / "runtime.json").write_text(info + "\n")

    # 4. Precompile to bytecode: faster first import on the user's machine.
    subprocess.run([str(pyexe), "-m", "compileall", "-q", "-j", "0", str(site)], check=False)

    size = sum(p.stat().st_size for p in out.rglob("*") if p.is_file())
    print(f"runtime ready: {out} ({size / 2**20:.0f} MB) {info}")


if __name__ == "__main__":
    main()
