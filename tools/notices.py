"""THIRD_PARTY_NOTICES.txt of a package (tools/package.py): what the package distributes besides Janus's own code,
each with its license and the license's text.

- The Rust crates compiled into the program: the dependencies of janus-app on this platform (`cargo metadata`),
  without the workspace's own crates, the procedural macros and the build-time dependencies (none of them is in the
  program); their texts are the LICENSE / COPYING / NOTICE files the crates carry (and the licenses in a fonts/
  folder). A crate published without them gets its license's standard text: MIT (the one chosen of MIT OR
  Apache-2.0, as such a crate allows) with its authors' copyright, another license's text as a crate under that
  license alone carries it; a GPL crate (Zeit's core) is under Janus's own LICENSE.
- The libraries and data the package copies from the conda-forge environment (GDAL, PROJ, GEOS, libtiff, curl...,
  the MSVC runtime on Windows): the conda package each file belongs to (conda-meta/*.json), its texts those the
  package keeps in its info/licenses folder (in the package cache) and those it installs under share/licenses (GCC's
  runtime libraries on Linux: the GCC Runtime Library Exception). The GPL-3.0's text, which no package carries, is
  tools/licenses/GPL-3.0.txt (gnu.org's).
- The fonts embedded in the program.

The same text for several works (Apache-2.0 for hundreds of crates) is written once, with the list of its works.

Usage (by tools/package.py): notices.write(path, version, shipped, conda_prefix) -> the works without a text
  shipped       the files copied from the environment (their paths in it)
  conda_prefix  the environment's root (where conda-meta is); None: the libraries are listed by file only
"""
import hashlib
import json
import os
import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# name, authors, home, license, its text: the MIT's under a copyright line, or a file's
FONTS = [
    ("ProggyClean", "Tristan Grimmer", "https://github.com/bluescan/proggyfonts", "MIT",
     "Copyright (c) 2004, 2005 Tristan Grimmer"),
    ("ProggyForever (minimal subset)", "Disco Hello, Tristan Grimmer", "https://github.com/ocornut/proggyforever",
     "MIT", "Copyright (c) Disco Hello, Tristan Grimmer"),
    ("Inter Regular and SemiBold (Latin subsets)", "The Inter Project Authors", "https://github.com/rsms/inter",
     "OFL-1.1", ROOT / "crates" / "janus-app" / "assets" / "Inter-OFL.txt"),
]

MIT = """Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated
documentation files (the "Software"), to deal in the Software without restriction, including without limitation the
rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit
persons to whom the Software is furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial portions of the
Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE
WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR
COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR
OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE."""

LICENSE_FILE = re.compile(r"^(licen[cs]e|copying|notice|unlicense|copyright)", re.I)
# a fonts folder's licenses (egui's default fonts: OFL.txt, UFL.txt, Hack-Regular.txt...)
FONT_LICENSE = re.compile(r"\.(txt|md)$", re.I)
# the license files a conda package installs
INSTALLED_LICENSE = re.compile(r"(^|/)share/licenses/", re.I)


def host_triple():
    out = subprocess.run(["rustc", "-vV"], capture_output=True, encoding="utf-8", check=True).stdout
    return re.search(r"^host: (\S+)", out, re.M).group(1)


def crates():
    """(name, version, license, repository, [license files], authors) of the crates in the program, by name."""
    meta = json.loads(subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", host_triple()],
        cwd=ROOT, capture_output=True, encoding="utf-8", check=True).stdout)
    pkgs = {p["id"]: p for p in meta["packages"]}
    members = set(meta["workspace_members"])
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    start = next(i for i in members if pkgs[i]["name"] == "janus-app")
    proc_macro = lambda p: all("proc-macro" in t["kind"] for t in p["targets"])
    seen, todo = {start}, [start]
    while todo:
        for d in nodes[todo.pop()]["deps"]:
            # normal dependencies only: build scripts' and tests' are not in the program
            if d["pkg"] not in seen and any(k["kind"] is None for k in d["dep_kinds"]):
                if proc_macro(pkgs[d["pkg"]]):
                    continue
                seen.add(d["pkg"])
                todo.append(d["pkg"])
    out = []
    for i in seen - members:
        p = pkgs[i]
        folder = Path(p["manifest_path"]).parent
        files = sorted(f for f in folder.iterdir() if f.is_file() and LICENSE_FILE.match(f.name))
        if (folder / "fonts").is_dir():
            files += sorted(f for f in (folder / "fonts").iterdir() if f.is_file() and FONT_LICENSE.search(f.name))
        if p.get("license_file"):
            lf = folder / p["license_file"]
            if lf.is_file() and lf not in files:
                files.append(lf)
        out.append((p["name"], p["version"], p.get("license") or "see its license file", p.get("repository") or "",
                    files, p.get("authors") or []))
    return sorted(out, key=lambda c: (c[0].lower(), c[1]))


def spdx_ids(expr):
    return re.findall(r"[A-Za-z0-9.+-]+", expr.replace("/", " OR ").replace("(", " ").replace(")", " "))


def standard_text(name, expr, authors, crates_):
    """The text of a crate published without its license files: (text, None) or (None, why not)."""
    ids = [i for i in spdx_ids(expr) if i not in ("OR", "AND", "WITH")]
    if " AND " in expr:
        return None, f"several licenses together ({expr})"
    if "MIT" in ids:
        who = ", ".join(re.sub(r"\s*<[^>]*>", "", a) for a in authors) or f"the {name} authors"
        return f"Copyright (c) {who}\n\n{MIT}", None
    if any(i.startswith("GPL-") for i in ids):
        return "Under the GNU General Public License, version 2 or later: its text is Janus's LICENSE.", None
    # a crate under that license alone that carries a single file: the license's text
    want = ids[0] if ids else ""
    for c in crates_:
        if c[2] == want and len(c[4]) == 1:
            return read(c[4][0]), None
    return None, f"no crate carries the text of {want}"


def conda_packages(shipped, prefix):
    """The conda packages the shipped files belong to: {name: (version, license, home, [license files])}, and the
    files of none (copied from outside the environment)."""
    prefix = Path(os.path.realpath(prefix))
    owner, records = {}, []
    for meta in sorted((prefix / "conda-meta").glob("*.json")):
        rec = json.loads(meta.read_text(encoding="utf-8"))
        records.append(rec)
        for f in rec.get("files", []):
            owner[f.replace("\\", "/").lower()] = rec
    roots = [Path(p) for p in os.environ.get("CONDA_PKGS_DIRS", "").split(os.pathsep) if p]
    if os.environ.get("MAMBA_ROOT_PREFIX"):
        roots.append(Path(os.environ["MAMBA_ROOT_PREFIX"]) / "pkgs")
    roots += [prefix.parent.parent / "pkgs", prefix / "pkgs"]  # <root>/envs/<name>: <root>/pkgs

    def license_files(rec):
        stem = re.sub(r"\.(conda|tar\.bz2)$", "", rec.get("fn", ""))
        dirs = [Path(rec["extracted_package_dir"])] if rec.get("extracted_package_dir") else []
        dirs += [r / stem for r in roots if stem]
        lic = next((d / "info" / "licenses" for d in dirs if (d / "info" / "licenses").is_dir()), None)
        files = sorted(p for p in lic.rglob("*") if p.is_file()) if lic else []
        # and those it installs (share/licenses/<name>/..., Library/share/licenses on Windows)
        files += sorted(prefix / f for f in rec.get("files", []) if INSTALLED_LICENSE.search(f.replace("\\", "/"))
                        and (prefix / f).is_file())
        return files

    used, strays = {}, []
    for f in shipped:
        try:
            rel = Path(os.path.realpath(f)).relative_to(prefix).as_posix().lower()
        except ValueError:
            strays.append(Path(f).name)
            continue
        rec = owner.get(rel)
        if rec is None:
            strays.append(Path(f).name)
            continue
        if rec["name"] in used:
            continue
        home = f"https://anaconda.org/conda-forge/{rec['name']}"
        used[rec["name"]] = (rec["version"], rec.get("license") or "unknown", home, license_files(rec))
    return used, sorted(set(strays))


def read(path):
    return path.read_bytes().decode("utf-8", errors="replace").replace("\r\n", "\n").strip()


def write(path, version, shipped, conda_prefix):
    """The notices at `path`; the works without a license text (printed as warnings)."""
    texts = {}  # hash -> [text, [works]]

    def add(text, work):
        key = hashlib.sha256(text.encode()).hexdigest()
        texts.setdefault(key, [text, []])[1].append(work)

    lines = [f"Janus {version}: third-party notices", "",
             "Janus is free software under the GNU General Public License, version 2 or later (see LICENSE). It",
             "contains, or is distributed with, the works below, each under its own license; the license texts",
             "follow the lists, each once with the works under it.", ""]
    warn = []

    lines += ["FONTS IN THE PROGRAM", ""]
    for name, by, url, lic, text in FONTS:
        lines.append(f"  {name}, by {by}: {lic} ({url})")
        add(read(text) if isinstance(text, Path) else f"{text}\n\n{MIT}", f"font {name}")
    lines.append("")

    cs = crates()
    lines += [f"RUST CRATES COMPILED INTO THE PROGRAM ({len(cs)})", ""]
    for name, ver, lic, repo, files, authors in cs:
        lines.append(f"  {name} {ver}: {lic}" + (f" ({repo})" if repo else ""))
        work = f"crate {name} {ver}"
        if files:
            for f in files:
                add(read(f), work)
            continue
        text, why = standard_text(name, lic, authors, cs)
        if text:
            add(text, work)
        else:
            warn.append(f"{work}: {why}")
    lines.append("")

    if conda_prefix:
        pkgs, strays = conda_packages(shipped, conda_prefix)
        lines += [f"LIBRARIES AND DATA DISTRIBUTED WITH THE PROGRAM (conda-forge packages, {len(pkgs)})", "",
                  "  Separate shared libraries, as built by conda-forge; the source of each: its recipe at",
                  "  https://github.com/conda-forge/<name>-feedstock, which names the upstream source it builds.", ""]
        for name in sorted(pkgs, key=str.lower):
            ver, lic, home, files = pkgs[name]
            lines.append(f"  {name} {ver}: {lic} ({home})")
            work = f"{name} {ver}"
            if "GPL-3.0" in lic and "LGPL-3.0" not in lic:
                files = files + [ROOT / "tools" / "licenses" / "GPL-3.0.txt"]
            if files:
                for f in files:
                    add(read(f), work)
            elif lic.lower() in ("blessing", "public-domain", "unlicense", "cc0-1.0"):
                # SQLite's blessing and the like: no conditions, no text to carry
                add(f"In the public domain ({lic}): no conditions.", work)
            else:
                warn.append(work)
        if strays:
            lines += ["", "  Also (from outside the environment): " + ", ".join(strays)]
        lines.append("")
    elif shipped:
        lines += ["LIBRARIES DISTRIBUTED WITH THE PROGRAM", ""] + [f"  {Path(f).name}" for f in shipped] + [""]

    lines += ["LICENSE TEXTS", ""]
    for text, works in sorted(texts.values(), key=lambda t: (-len(t[1]), t[1][0].lower())):
        lines += ["=" * 117, "", "For: " + ", ".join(sorted(set(works), key=str.lower)), "", text, ""]
    Path(path).write_text("\n".join(lines) + "\n", encoding="utf-8", newline="\n")
    print(f"package: {Path(path).name}: {len(cs)} crates, "
          f"{len(pkgs) if conda_prefix else 0} conda packages, {len(texts)} license texts")
    for w in warn:
        print(f"package: warning: no license text found for {w}")
    return warn
