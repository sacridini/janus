#!/usr/bin/env python3
"""Writes the manifest of Janus's sample data (the samples.json asset of a release).

Janus's Welcome window (Open sample data...) reads it: for each sample its files with their size and SHA-256, and
what to open. Give it the files of one sample at a time; run it again with --merge to add the next one.

    python tools/make_sample_manifest.py --id rondonia-ndvi \
        --title "Annual NDVI of a Rondonia forest frontier, 1985-2024" \
        --description "Forty years of Landsat NDVI, one band a year." \
        --guide landtrendr \
        --base-url https://github.com/sacridini/janus/releases/download/sample-data-v1 \
        --open rondonia_ndvi_1985_2024.tif \
        -o samples.json rondonia_ndvi_1985_2024.tif

The files are the release's assets: upload them (and samples.json) to the release named in --base-url.
"""

import argparse
import hashlib
import json
import os
import sys


def sha256_of(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("files", nargs="+", help="the sample's files")
    ap.add_argument("--id", required=True, help="the sample's id (also its folder's name)")
    ap.add_argument("--title", required=True)
    ap.add_argument("--description", default="")
    ap.add_argument("--guide", default="", help="the guide's page on the site, without .html")
    ap.add_argument("--base-url", default="", help="where the files are; without it the URL is the file's name")
    ap.add_argument("--open", default="", help="what to open, relative to the sample's folder (default: the only file)")
    ap.add_argument("-o", "--output", default="samples.json")
    ap.add_argument("--merge", action="store_true", help="add to the manifest in --output instead of replacing it")
    args = ap.parse_args()

    files = []
    total = 0
    for p in args.files:
        name = os.path.basename(p)
        total += os.path.getsize(p)
        url = args.base_url.rstrip("/") + "/" + name if args.base_url else name
        files.append({"url": url, "name": name, "sha256": sha256_of(p)})
    open_path = args.open or (files[0]["name"] if len(files) == 1 else "")
    entry = {
        "id": args.id,
        "title": args.title,
        "description": args.description,
        "size_mb": round(total / 1048576, 1),
        "guide": args.guide,
        "files": files,
        "open": open_path,
    }
    manifest = []
    if args.merge and os.path.exists(args.output):
        with open(args.output, encoding="utf-8") as f:
            manifest = [m for m in json.load(f) if m.get("id") != args.id]
    manifest.append(entry)
    with open(args.output, "w", encoding="utf-8", newline="\n") as f:
        json.dump(manifest, f, indent=2, ensure_ascii=False)
        f.write("\n")
    print(f"{args.output}: {len(manifest)} sample(s); {args.id}: {len(files)} file(s), {entry['size_mb']} MB")
    return 0


if __name__ == "__main__":
    sys.exit(main())
