"""Assembles the private Python runtime that runs Zeit for Janus.

The result is a self-contained folder (no installer, no registry, not on PATH):

    <out>/
      python/                 relocatable Python 3.12
                                Windows: the official embeddable distribution,
                                  site-packages in python/sp (short: path limit)
                                Linux / macOS: python-build-standalone
                                  ("install_only"), python/bin/python3
      janus_zeit_bridge.py      the bridge and tool_*.py (copied from zeit_bridge/)
      runtime.json            versions, for diagnostics

Usage (any Python >= 3.9 with pip, used only to download wheels for the
target platform, which defaults to the one this runs on):
    python tools/build_zeit_runtime.py --out build/Release/runtime
"""
import argparse
import hashlib
import json
import platform
import shutil
import subprocess
import sys
import tarfile
import urllib.request
import zipfile
from pathlib import Path

PY_VERSION = "3.12.10"
PBS_TAG = "20250409"  # python-build-standalone release with PY_VERSION
PBS = "https://github.com/astral-sh/python-build-standalone/releases/download"

# Per target: Python archive (sha256 pinned: python.org's on first download,
# python-build-standalone's from the release's SHA256SUMS) and the wheel tags.
PLATFORMS = {
    "win_amd64": {
        "url": f"https://www.python.org/ftp/python/{PY_VERSION}/python-{PY_VERSION}-embed-amd64.zip",
        "sha256": "4acbed6dd1c744b0376e3b1cf57ce906f9dc9e95e68824584c8099a63025a3c3",
        "wheels": ["win_amd64"],
    },
    "linux_x86_64": {
        "url": f"{PBS}/{PBS_TAG}/cpython-{PY_VERSION}+{PBS_TAG}-x86_64-unknown-linux-gnu-install_only_stripped.tar.gz",
        "sha256": "8c59b9ac6bff2dc3934181d7bc82594f9f59a613afed8d72c9e89d7194e790ee",
        "wheels": ["manylinux_2_28_x86_64", "manylinux_2_17_x86_64", "manylinux2014_x86_64"],
    },
    "macos_arm64": {
        "url": f"{PBS}/{PBS_TAG}/cpython-{PY_VERSION}+{PBS_TAG}-aarch64-apple-darwin-install_only_stripped.tar.gz",
        "sha256": "0be1fe0b35a4d3c382141764ef16ed3b8cc2b4620b657f678daa7b7f8df39699",
        "wheels": ["macosx_15_0_arm64", "macosx_14_0_arm64", "macosx_13_0_arm64", "macosx_12_0_arm64", "macosx_11_0_arm64"],
    },
}


def host_platform():
    m = platform.machine().lower()
    if sys.platform == "win32":
        return "win_amd64"
    if sys.platform.startswith("linux") and m in ("x86_64", "amd64"):
        return "linux_x86_64"
    if sys.platform == "darwin" and m in ("arm64", "aarch64"):
        return "macos_arm64"
    raise SystemExit(f"no Zeit runtime recipe for {sys.platform} {m}")

ROOT = Path(__file__).resolve().parent.parent
BRIDGE = ROOT / "zeit_bridge"


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def download_python(cache: Path, plat: dict) -> Path:
    cache.mkdir(parents=True, exist_ok=True)
    url = plat["url"]
    path = cache / url.rsplit("/", 1)[1].replace("+", "_")
    if not path.exists():
        print(f"downloading {url}")
        tmp = path.with_suffix(".part")
        urllib.request.urlretrieve(url, tmp)
        tmp.rename(path)
    digest = sha256(path)
    if plat["sha256"] and digest != plat["sha256"]:
        raise SystemExit(f"checksum mismatch for {path}: {digest}")
    print(f"python {PY_VERSION} ({path.name}) sha256 {digest}")
    return path


def pip_install(target: Path, plat: dict, args):
    tags = []
    for w in plat["wheels"]:
        tags += ["--platform", w]
    cmd = [sys.executable, "-m", "pip", "install", "--quiet", "--disable-pip-version-check", "--no-warn-conflicts",
           "--target", str(target), *tags, "--python-version", "3.12",
           "--implementation", "cp", "--only-binary=:all:", "--upgrade"] + args
    subprocess.run(cmd, check=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True, help="output folder (e.g. build/Release/runtime)")
    ap.add_argument("--cache", default=str(ROOT / "build" / "_downloads"), help="download cache")
    ap.add_argument("--zeit-wheel", help="install this local Zeit wheel instead of the PyPI pin")
    ap.add_argument("--platform", choices=sorted(PLATFORMS), help="target (default: this machine)")
    a = ap.parse_args()
    target = a.platform or host_platform()
    plat = PLATFORMS[target]
    windows = target.startswith("win")

    final = Path(a.out).resolve()
    # Built in a sibling staging folder and swapped in only at the end: a failed
    # build (or a runtime in use by a running Janus) never leaves a half-deleted
    # runtime behind.
    out = final.parent / (final.name + ".new")
    if out.exists():
        shutil.rmtree(out)
    py = out / "python"
    archive = download_python(Path(a.cache), plat)
    if windows:
        # Short site-packages folder: keeps the deepest file path well under
        # Windows' 260-character limit even when Janus is installed in a long folder.
        site = py / "sp"
        py.mkdir(parents=True)
        with zipfile.ZipFile(archive) as z:
            z.extractall(py)
        # The ._pth file fixes sys.path for the embeddable build: add
        # site-packages and enable `import site` (needed for .pth files of some wheels).
        pth = next(py.glob("python3*._pth"))
        lines = [l for l in pth.read_text().splitlines() if l.strip() and not l.startswith("#")]
        lines += ["sp", "import site"]
        pth.write_text("\n".join(lines) + "\n")
        pyexe = py / "python.exe"
    else:
        # python-build-standalone "install_only": a relocatable tree in python/.
        out.mkdir(parents=True)
        with tarfile.open(archive) as t:
            t.extractall(out, filter="tar") if hasattr(tarfile, "data_filter") else t.extractall(out)
        site = py / "lib" / "python3.12" / "site-packages"
        pyexe = py / "bin" / "python3"

    # 2. Dependencies (pinned) + Zeit without its heavy optional dependencies.
    site.mkdir(parents=True, exist_ok=True)
    pip_install(site, plat, ["-r", str(BRIDGE / "requirements.txt")])
    if a.zeit_wheel:
        pip_install(site, plat, ["--no-deps", a.zeit_wheel])
    else:
        pip_install(site, plat, ["--no-deps", "-r", str(BRIDGE / "zeit-requirements.txt")])

    # Test suites are never imported at runtime and are ~30% of the size.
    removed = 0
    for d in sorted(site.rglob("tests"), key=lambda p: len(p.parts), reverse=True):
        if d.is_dir() and d.exists():
            removed += sum(f.stat().st_size for f in d.rglob("*") if f.is_file())
            shutil.rmtree(d)
    print(f"removed {removed / 2**20:.0f} MB of test suites")

    # Linux/macOS wheels often ship native libraries with debug information
    # (llvmlite, Zeit's own module: hundreds of MB); strip only that.
    if not windows and shutil.which("strip"):
        before = sum(f.stat().st_size for f in site.rglob("*.so*") if f.is_file())
        flag = "-S" if target.startswith("macos") else "--strip-debug"
        for f in site.rglob("*.so*"):
            if f.is_file() and not f.is_symlink():
                subprocess.run(["strip", flag, str(f)], check=False, capture_output=True)
                # On arm64 macOS, stripping invalidates the code signature and the
                # kernel kills the process that loads the file: sign it again (ad hoc).
                if target.startswith("macos"):
                    subprocess.run(["codesign", "--force", "--sign", "-", str(f)],
                                   check=False, capture_output=True)
        after = sum(f.stat().st_size for f in site.rglob("*.so*") if f.is_file())
        print(f"stripped debug information: {(before - after) / 2**20:.0f} MB")

    # 3. Bridge + metadata.
    for f in BRIDGE.glob("*.py"):
        shutil.copy2(f, out / f.name)
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

    # 5. Swap: rename the old runtime away first (fails cleanly if it is in use).
    if final.exists():
        old = final.parent / (final.name + ".old")
        if old.exists():
            shutil.rmtree(old, ignore_errors=True)
        try:
            final.rename(old)
        except OSError as e:
            raise SystemExit(f"{final} is in use (close Janus and try again); the new runtime is in {out}: {e}")
        shutil.rmtree(old, ignore_errors=True)
    out.rename(final)
    print(f"runtime ready: {final} ({size / 2**20:.0f} MB) {info}")


if __name__ == "__main__":
    main()
