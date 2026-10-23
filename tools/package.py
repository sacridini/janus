"""Janus's packages: dist/janus-<version>-setup.exe (Inno Setup), dist/janus-<version>-linux-x86_64.tar.xz and
dist/janus-<version>-macos-arm64.dmg (Janus.app).

GDAL and its whole chain (PROJ, GEOS, libtiff, curl...) come from the environment the program was built against
(a conda-forge environment, docs/building.md): every library the program loads, the GDAL drivers built as plugins
(NetCDF, HDF5), the PROJ and GDAL data and, off Windows, CA certificates for curl. The system's own libraries
(Windows' DLLs; glibc, OpenGL and X11 on Linux; macOS's) are left to the system. Zeit is part of the program: there
is no Python to ship. Every package has Janus's LICENSE and THIRD_PARTY_NOTICES.txt (tools/notices.py: the crates,
libraries, data and fonts it distributes, with their licenses' texts).

Usage: python tools/package.py [--no-build] [--prefix PREFIX] [--out DIR] [--strict-notices]
  PREFIX  GDAL's installation prefix (default: the conda environment, $CONDA_PREFIX; on Windows its Library/)
  DIR     where the package is assembled (default: target/package)
  --strict-notices  fails if a work has no license text in THIRD_PARTY_NOTICES.txt (the CI's releases)

Layouts:
  Windows  janus.exe, jn.com (the command line), the DLLs, gdalplugins/, share/gdal, share/proj, LICENSE,
           THIRD_PARTY_NOTICES.txt; the installer
           (installer/janus.iss) puts them in Program Files or the user's folder, on PATH, in the context menu.
  Linux    janus-<version>/: janus (RPATH $ORIGIN/lib), jn (the command line), lib/ (lib/gdalplugins/), share/gdal,
           share/proj, share/ssl/cacert.pem, janus.desktop, janus.png, README.md, LICENSE, THIRD_PARTY_NOTICES.txt.
  macOS    Janus.app/Contents/: MacOS/janus and jn (RPATH @executable_path/../Frameworks), Frameworks/
           (Frameworks/gdalplugins/), Resources/ (janus.icns, share/..., LICENSE, THIRD_PARTY_NOTICES.txt),
           Info.plist; signed ad hoc. The two texts also beside the app in the disk image.
"""
import argparse
import os
import platform
import re
import shutil
import struct
import subprocess
import sys
import tarfile
import time
from pathlib import Path

import notices

ROOT = Path(__file__).resolve().parent.parent
PLUGINS = ["gdal_netCDF", "gdal_HDF5"]
# the files copied from GDAL's environment (their notices: the conda packages they come from)
SHIPPED = []
# --strict-notices
STRICT = False


def version():
    text = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    m = re.search(r'\[workspace\.package\][^\[]*?version\s*=\s*"([^"]+)"', text, re.S)
    if not m:
        sys.exit("package: no version in Cargo.toml's [workspace.package]")
    return m.group(1)


def run(*cmd, **kw):
    print("+", " ".join(str(c) for c in cmd), flush=True)
    return subprocess.run([str(c) for c in cmd], check=True, **kw)


def copy(src, dst):
    """The real file (not a link) under the name `dst`."""
    dst.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(os.path.realpath(src), dst)
    shutil.copymode(os.path.realpath(src), dst)


def copy_data(prefix_share, share):
    (share / "proj").mkdir(parents=True, exist_ok=True)
    shutil.copyfile(prefix_share / "proj" / "proj.db", share / "proj" / "proj.db")
    shutil.copytree(prefix_share / "gdal", share / "gdal", dirs_exist_ok=True)
    SHIPPED.append(prefix_share / "proj" / "proj.db")
    SHIPPED.extend(p for p in (prefix_share / "gdal").rglob("*") if p.is_file())


def legal(*dirs):
    """Janus's LICENSE and the third-party notices (of everything in SHIPPED) in each of `dirs`."""
    prefix = os.environ.get("CONDA_PREFIX")
    first = dirs[0] / "THIRD_PARTY_NOTICES.txt"
    missing = notices.write(first, version(), SHIPPED,
                            prefix if prefix and (Path(prefix) / "conda-meta").is_dir() else None)
    if STRICT and missing:
        sys.exit(f"package: {len(missing)} works without a license text (--strict-notices)")
    for d in dirs:
        d.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ROOT / "LICENSE", d / "LICENSE")
        if d != dirs[0]:
            shutil.copyfile(first, d / "THIRD_PARTY_NOTICES.txt")


def megabytes(path):
    return path.stat().st_size / 1048576


# ---------------------------------------------------------------------------------------------------------------
# Windows
# ---------------------------------------------------------------------------------------------------------------


def pe_imports(path):
    """The DLLs a PE file imports (its import table and its delay-load table)."""
    data = Path(path).read_bytes()
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    if data[pe : pe + 4] != b"PE\0\0":
        return []
    coff = pe + 4
    nsec, = struct.unpack_from("<H", data, coff + 2)
    opt_size, = struct.unpack_from("<H", data, coff + 16)
    opt = coff + 20
    magic, = struct.unpack_from("<H", data, opt)
    dirs = opt + (112 if magic == 0x20B else 96)
    sections = [struct.unpack_from("<8sIIII", data, opt + opt_size + 40 * i) for i in range(nsec)]

    def offset(rva):
        for _, vsize, vaddr, rawsize, rawptr in sections:
            if vaddr <= rva < vaddr + max(vsize, rawsize):
                return rva - vaddr + rawptr
        return None

    def name_at(rva):
        o = offset(rva)
        return data[o : data.index(b"\0", o)].decode("ascii", "replace") if o is not None else None

    names = []
    # (directory entry, descriptor size, where the name's RVA is in it)
    for entry, size, at in [(1, 20, 12), (13, 32, 4)]:
        rva, = struct.unpack_from("<I", data, dirs + 8 * entry)
        o = offset(rva) if rva else None
        while o is not None and o + size <= len(data):
            name_rva, = struct.unpack_from("<I", data, o + at)
            if name_rva == 0:
                break
            n = name_at(name_rva)
            if n:
                names.append(n)
            o += size
    return names


def windows(args, ver, exe_dir):
    conda = Path(os.environ.get("CONDA_PREFIX", ""))
    prefix = Path(args.prefix) if args.prefix else conda / "Library"
    bins = [prefix / "bin", conda]  # GDAL's DLLs; the environment's MSVC runtime
    stage = Path(args.out) / f"janus-{ver}"
    shutil.rmtree(stage, ignore_errors=True)
    stage.mkdir(parents=True)
    shutil.copyfile(exe_dir / "janus.exe", stage / "janus.exe")
    shutil.copyfile(exe_dir / "jn.exe", stage / "jn.com")  # "jn" in a terminal finds jn.com first (PATHEXT)

    plugins = [p for p in (prefix / "lib" / "gdalplugins" / f"{n}.dll" for n in PLUGINS) if p.exists()]
    if len(plugins) < len(PLUGINS):
        print("package: warning: a GDAL plugin is missing (conda install libgdal-netcdf libgdal-hdf5)")

    def find(name):
        for d in bins:
            p = d / name
            if p.exists():
                return p
        return None  # the system's

    seen, todo, copied = set(), [stage / "janus.exe", *plugins], 0
    while todo:
        for name in pe_imports(todo.pop()):
            key = name.lower()
            if key in seen or key.startswith(("api-ms-", "ext-ms-")):
                continue
            seen.add(key)
            p = find(name)
            if p:
                shutil.copyfile(p, stage / p.name)
                SHIPPED.append(p)
                todo.append(stage / p.name)
                copied += 1
    for p in plugins:
        (stage / "gdalplugins").mkdir(exist_ok=True)
        shutil.copyfile(p, stage / "gdalplugins" / p.name)
        SHIPPED.append(p)
    copy_data(prefix / "share", stage / "share")
    print(f"package: {copied} DLLs")
    legal(stage)

    iscc = shutil.which("ISCC") or next(
        (str(p) for p in [Path(os.environ.get("ProgramFiles(x86)", "")) / "Inno Setup 6" / "ISCC.exe",
                          Path(os.environ.get("LOCALAPPDATA", "")) / "Programs" / "Inno Setup 6" / "ISCC.exe"]
         if p.exists()), None)
    if not iscc:
        print(f"package: Inno Setup (ISCC) not found: the files are in {stage}, no installer made")
        return
    run(iscc, "/Q", f"/DAppVersion={ver}", f"/DBuildDir={stage}", ROOT / "installer" / "janus.iss")
    setup = ROOT / "dist" / f"janus-{ver}-setup.exe"
    print(f"package: {setup} ({megabytes(setup):.0f} MB)")


# ---------------------------------------------------------------------------------------------------------------
# Linux
# ---------------------------------------------------------------------------------------------------------------

# Provided by every Linux system, and must match its kernel, drivers and display: glibc, the OpenGL and Vulkan
# loaders and drivers, X11 / Wayland client libraries.
SYSTEM_LIBS = re.compile(
    r"^(ld-linux|linux-vdso|libc\.so|libm\.so|libdl\.so|libpthread\.so|librt\.so|libutil\.so|libresolv\.so|libGL|"
    r"libEGL|libOpenGL|libvulkan|libX|libxcb|libwayland|libxkbcommon|libgcc_s\.so)"
)


def ldd(path, lib_dir):
    env = dict(os.environ, LD_LIBRARY_PATH=str(lib_dir))
    out = subprocess.run(["ldd", str(path)], env=env, capture_output=True, text=True).stdout
    deps = {}
    for line in out.splitlines():
        m = re.match(r"\s*(\S+)\s+=>\s+(\S+)", line)
        if m and m.group(2) != "not":
            deps[m.group(1)] = m.group(2)
        elif m:
            print(f"package: warning: {m.group(1)} not found (needed by {path})")
    return deps


def linux(args, ver, exe_dir):
    prefix = Path(args.prefix or os.environ.get("CONDA_PREFIX", "/usr"))
    out = Path(args.out)
    name = f"janus-{ver}"
    stage = out / name
    shutil.rmtree(stage, ignore_errors=True)
    (stage / "lib" / "gdalplugins").mkdir(parents=True)
    plugins = [p for p in (prefix / "lib" / "gdalplugins" / f"{n}.so" for n in PLUGINS) if p.exists()]
    copied = 0
    for f in [exe_dir / "janus", *plugins]:
        for soname, path in ldd(f, prefix / "lib").items():
            if SYSTEM_LIBS.match(soname) or path.startswith(("/lib", "/usr/lib")):
                continue
            if not (stage / "lib" / soname).exists():
                copy(path, stage / "lib" / soname)
                SHIPPED.append(path)
                copied += 1
    print(f"package: {copied} libraries")
    for p in plugins:
        copy(p, stage / "lib" / "gdalplugins" / p.name)
        SHIPPED.append(p)
        run("patchelf", "--set-rpath", "$ORIGIN/..", stage / "lib" / "gdalplugins" / p.name)
    for exe in ["janus", "jn"]:
        copy(exe_dir / exe, stage / exe)
    # only the bundled libraries: not the build machine's library paths
    run("patchelf", "--set-rpath", "$ORIGIN/lib", stage / "janus")
    copy_data(prefix / "share", stage / "share")
    if (prefix / "ssl" / "cacert.pem").exists():
        copy(prefix / "ssl" / "cacert.pem", stage / "share" / "ssl" / "cacert.pem")
        SHIPPED.append(prefix / "ssl" / "cacert.pem")
    shutil.copyfile(ROOT / "resources" / "janus.png", stage / "janus.png")
    shutil.copyfile(ROOT / "README.md", stage / "README.md")
    legal(stage)
    (stage / "janus.desktop").write_text(
        "[Desktop Entry]\nType=Application\nName=Janus\nComment=Raster time series analysis\n"
        "Exec=janus %F\nIcon=janus\nTerminal=false\nCategories=Science;Geography;Graphics;\nMimeType=image/tiff;\n"
    )
    (ROOT / "dist").mkdir(exist_ok=True)
    archive = ROOT / "dist" / f"{name}-linux-x86_64.tar.xz"
    with tarfile.open(archive, "w:xz", preset=9) as t:
        t.add(stage, arcname=name)
    print(f"package: {archive} ({megabytes(archive):.0f} MB)")


# ---------------------------------------------------------------------------------------------------------------
# macOS
# ---------------------------------------------------------------------------------------------------------------


def otool_libs(path):
    out = subprocess.run(["otool", "-L", str(path)], capture_output=True, text=True, check=True).stdout
    return [line.split(" (")[0].strip() for line in out.splitlines()[1:] if line.strip()]


def macos(args, ver, exe_dir):
    prefix = Path(args.prefix or os.environ.get("CONDA_PREFIX", "/usr/local"))
    stage = Path(args.out) / "dmg"
    app = stage / "Janus.app" / "Contents"
    shutil.rmtree(stage, ignore_errors=True)
    for d in ["MacOS", "Frameworks/gdalplugins", "Resources"]:
        (app / d).mkdir(parents=True)
    plugins = [p for p in (prefix / "lib" / "gdalplugins" / f"{n}.dylib" for n in PLUGINS) if p.exists()]

    def resolve(ref, loader):
        if ref.startswith(("/usr/lib", "/System")):
            return None
        name = ref.split("/")[-1]
        if ref.startswith("@loader_path/"):
            p = Path(loader).parent / ref[len("@loader_path/"):]
        elif ref.startswith("@"):
            p = prefix / "lib" / name
        else:
            p = Path(ref)
        return p if p.exists() else None

    seen, todo, copied = set(), [exe_dir / "janus", *plugins], 0
    while todo:
        f = todo.pop()
        for ref in otool_libs(f):
            p = resolve(ref, f)
            name = ref.split("/")[-1]
            if p is None or name in seen:
                continue
            seen.add(name)
            copy(p, app / "Frameworks" / name)
            SHIPPED.append(p)
            todo.append(p)
            copied += 1
    print(f"package: {copied} libraries")
    for p in plugins:
        dst = app / "Frameworks" / "gdalplugins" / p.name
        copy(p, dst)
        SHIPPED.append(p)
        subprocess.run(["install_name_tool", "-add_rpath", "@loader_path/..", str(dst)], capture_output=True)
    for exe in ["janus", "jn"]:
        copy(exe_dir / exe, app / "MacOS" / exe)
    janus = app / "MacOS" / "janus"
    # only the bundled libraries: the build machine's library path replaced
    rpaths = re.findall(r"cmd LC_RPATH\n\s+cmdsize \d+\n\s+path (\S+)",
                        subprocess.run(["otool", "-l", str(janus)], capture_output=True, text=True).stdout)
    for r in rpaths:
        run("install_name_tool", "-delete_rpath", r, janus)
    run("install_name_tool", "-add_rpath", "@executable_path/../Frameworks", janus)
    copy_data(prefix / "share", app / "Resources" / "share")
    if (prefix / "ssl" / "cacert.pem").exists():
        copy(prefix / "ssl" / "cacert.pem", app / "Resources" / "share" / "ssl" / "cacert.pem")
        SHIPPED.append(prefix / "ssl" / "cacert.pem")
    shutil.copyfile(ROOT / "resources" / "janus.icns", app / "Resources" / "janus.icns")
    # in the bundle (sealed by its signature below) and beside it in the disk image
    legal(app / "Resources", stage)
    plist = (ROOT / "resources" / "Info.plist.in").read_text(encoding="utf-8")
    (app / "Info.plist").write_text(plist.replace("@VERSION@", ver).replace("@MIN_MACOS@", args.min_macos))
    # install_name_tool invalidated the signatures (on Apple Silicon unsigned code is killed on load): every
    # library, then the bundle (which seals Resources)
    for lib in sorted((app / "Frameworks").rglob("*")):
        if lib.is_file():
            run("codesign", "--force", "--sign", "-", lib)
    run("codesign", "--force", "--sign", "-", stage / "Janus.app")
    run("codesign", "--verify", "--strict", stage / "Janus.app")
    (stage / "Applications").symlink_to("/Applications")
    (ROOT / "dist").mkdir(exist_ok=True)
    dmg = ROOT / "dist" / f"janus-{ver}-macos-arm64.dmg"
    dmg.unlink(missing_ok=True)
    # hdiutil fails now and then with "Resource busy" while the system still scans the new folder: again
    for attempt in range(5):
        r = subprocess.run(["hdiutil", "create", "-volname", f"Janus {ver}", "-srcfolder", str(stage), "-fs", "APFS",
                            "-format", "ULMO", "-ov", str(dmg)], capture_output=True, text=True)
        if r.returncode == 0:
            break
        print(f"package: hdiutil failed (attempt {attempt + 1}): {r.stderr.strip()}")
        time.sleep(10)
    else:
        sys.exit("package: hdiutil create failed")
    print(f"package: {dmg} ({megabytes(dmg):.0f} MB)")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--no-build", action="store_true", help="package the release build as it is")
    ap.add_argument("--prefix", help="GDAL's installation prefix (default: the conda environment)")
    ap.add_argument("--out", default=str(ROOT / "target" / "package"), help="where the package is assembled")
    ap.add_argument("--min-macos", default="15.0", help="macOS: the oldest system the app is for")
    ap.add_argument("--strict-notices", action="store_true", help="fail if a work has no license text")
    args = ap.parse_args()
    global STRICT
    STRICT = args.strict_notices
    ver = version()
    if not args.no_build:
        run("cargo", "build", "--release", "-p", "janus-app", cwd=ROOT)
    exe_dir = ROOT / "target" / "release"
    system = platform.system()
    if system == "Windows":
        windows(args, ver, exe_dir)
    elif system == "Linux":
        linux(args, ver, exe_dir)
    elif system == "Darwin":
        macos(args, ver, exe_dir)
    else:
        sys.exit(f"package: {system} is not packaged")


if __name__ == "__main__":
    main()
