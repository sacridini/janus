"""Janus's web layers (IDEIAS: camadas OGC) against Python on the real servers: the same features and the same map.

    python tools/bench_ogc_vs_python.py        (an environment with geopandas, pyogrio, OWSLib, requests, Pillow)

Janus's side is crates/janus-geo/examples/vs_python_ogc.rs (build it first: cargo build [--release] -p janus-geo
--example vs_python_ogc). Features: INPE's TerraBrasilis DETER (WFS: the alerts of 2023 in a box around Novo
Progresso, PA), IBGE's municipalities (WFS, the same box) and pygeoapi's demo lakes (OGC API - Features, the whole
collection). Each fetched by Janus (GDAL's WFS / OAPIF drivers, paged, into a GeoPackage), by
geopandas.read_file on the same source (pyogrio, GDAL's same drivers), by OWSLib (GetFeature / items as GeoJSON) and
by requests (the server's GeoJSON directly, CQL for the dates). Same count and the same attributes (the fields' names
and each feature's key). A WMS tile (DETER, zoom 10 over the box): Janus's (GDAL's WMS driver, as the map reads it)
against a GetMap of the tile's box asked for directly, pixel by pixel. Each timed three times (the median), network
included. Writes bench/ogc-vs-python.json.
"""
import faulthandler
import io
import json
import math
import os
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

faulthandler.enable()
for k in ("GDAL_NUM_THREADS", "OMP_NUM_THREADS", "OPENBLAS_NUM_THREADS", "MKL_NUM_THREADS"):
    os.environ.setdefault(k, "2")
import requests

# OWSLib in a process of its own (`--owslib CASE OUT`): its parse of IBGE's capabilities (11 MB) crashes on Windows
# once GDAL's libraries (geopandas) are loaded; timed inside that process
if __name__ == "__main__" and len(sys.argv) == 4 and sys.argv[1] == "--owslib":
    from owslib.ogcapi.features import Features
    from owslib.wfs import WebFeatureService
else:
    import geopandas as gpd
    import numpy as np
    from PIL import Image

ROOT = Path(__file__).resolve().parents[1]
NAME = "vs_python_ogc.exe" if os.name == "nt" else "vs_python_ogc"
# the release build if there is one (the network dominates: a debug build times the same)
EXE = next((p for p in (ROOT / "target" / d / "examples" / NAME for d in ("release", "debug")) if p.exists()),
           ROOT / "target" / "release" / "examples" / NAME)
# Janus's own GDAL (staged next to janus.exe), never the environment's
ENV = dict(os.environ, PATH=str(EXE.parents[1]) + os.pathsep + os.environ.get("PATH", ""),
           PROJ_DATA=str(EXE.parents[1] / "share" / "proj"), GDAL_DRIVER_PATH=str(EXE.parents[1] / "gdalplugins"))
RUNS = 3
UA = {"User-Agent": "Janus benchmark (github.com/sacridini/janus)"}

DETER = "https://terrabrasilis.dpi.inpe.br/geoserver/deter-amz/ows"
IBGE = "https://geoservicos.ibge.gov.br/geoserver/ows"
PYGEOAPI = "https://demo.pygeoapi.io/master"
BOX = (-55.6, -7.2, -55.2, -6.8)  # around Novo Progresso (PA)

CASES = [
    dict(name="deter_2023_wfs", service="wfs", url=DETER, layer="deter-amz:deter_amz", bbox=BOX,
         date=("view_date", "2023-01-01", "2023-12-31"), key="gid"),
    dict(name="ibge_municipalities_wfs", service="wfs", url=IBGE, layer="CGMAT:pbqg22_04_Municipios_cd_mun", bbox=BOX,
         date=None, key="CD_MUN"),
    dict(name="pygeoapi_lakes_oapif", service="oapif", url=PYGEOAPI, layer="lakes", bbox=None, date=None, key="id"),
]


def timed(f):
    out, ts = None, []
    for _ in range(RUNS):
        t = time.perf_counter()
        out = f()
        ts.append(time.perf_counter() - t)
    return out, statistics.median(ts), ts


def janus_features(c, out):
    d = c["date"] or ("-", "-", "-")
    bbox = ",".join(map(str, c["bbox"])) if c["bbox"] else "-"
    r = subprocess.run([str(EXE), "features", c["service"], c["url"], c["layer"], bbox, *d, "100000", str(out)],
                       capture_output=True, text=True, env=ENV)
    j = json.loads(r.stdout or "{}")
    if "error" in j or r.returncode != 0:
        raise RuntimeError(f"Janus: {j.get('error', r.stderr)}")
    return gpd.read_file(out), j


def where_of(c):
    if not c["date"]:
        return None
    f, a, b = c["date"]
    return f"{f} >= '{a}' AND {f} <= '{b}'"


def geopandas_features(c):
    if c["service"] == "wfs":
        src = f"WFS:{c['url']}?TYPENAME={c['layer']}"
        return gpd.read_file(src, layer=c["layer"], bbox=c["bbox"], where=where_of(c))
    return gpd.read_file(f"OAPIF:{c['url']}", layer=c["layer"], bbox=c["bbox"])


def owslib_geojson(c):
    """The features as GeoJSON bytes by OWSLib (its capabilities read, then GetFeature or the items)."""
    if c["service"] == "wfs":
        w = WebFeatureService(c["url"], version="2.0.0", headers=UA)
        if c["date"]:
            # GetFeature sends a box or a filter, not both: the box as CQL with the dates, by OWSLib's URL
            f, a, b = c["date"]
            r = requests.get(w.getOperationByName("GetFeature").methods[0]["url"], headers=UA, timeout=120, params=dict(
                service="WFS", version="2.0.0", request="GetFeature", typeNames=c["layer"],
                outputFormat="application/json", srsName="EPSG:4326",
                CQL_FILTER=f"BBOX(geom,{BOX[0]},{BOX[1]},{BOX[2]},{BOX[3]},'EPSG:4326') AND {f} BETWEEN '{a}' AND '{b}'"))
            r.raise_for_status()
            return r.content
        # longitude, latitude: OWSLib puts an URN's axes in its order (latitude first) itself
        bbox = (*c["bbox"], "urn:ogc:def:crs:EPSG::4326") if c["bbox"] else None
        return w.getfeature(typename=[c["layer"]], bbox=bbox, outputFormat="application/json").read()
    # its pages (the server's largest may be smaller than the limit asked for)
    f, feats = Features(c["url"], headers=UA), []
    while True:
        items = f.collection_items(c["layer"], limit=1000, offset=len(feats))
        feats += items["features"]
        if not items["features"] or not any(link.get("rel") == "next" for link in items.get("links", [])):
            break
    return json.dumps(dict(type="FeatureCollection", features=feats)).encode()


def owslib_features(c, out):
    """OWSLib's features, each run in a process of its own: the frame and the median of its own timings."""
    ts = []
    for _ in range(RUNS):
        r = subprocess.run([sys.executable, __file__, "--owslib", c["name"], str(out)], capture_output=True, text=True)
        if r.returncode != 0:
            raise RuntimeError(r.stderr.strip().splitlines()[-1] if r.stderr.strip() else f"exit {r.returncode}")
        ts.append(float(r.stdout.strip().splitlines()[-1]))
    return gpd.read_file(out), statistics.median(ts), ts


def requests_features(c):
    if c["service"] == "wfs":
        p = dict(service="WFS", version="2.0.0", request="GetFeature", typeNames=c["layer"],
                 outputFormat="application/json", srsName="EPSG:4326")
        geom = "geom" if c["date"] else None
        if c["date"]:
            f, a, b = c["date"]
            p["CQL_FILTER"] = f"BBOX({geom},{BOX[0]},{BOX[1]},{BOX[2]},{BOX[3]},'EPSG:4326') AND {f} BETWEEN '{a}' AND '{b}'"
        elif c["bbox"]:
            b = c["bbox"]
            p["bbox"] = f"{b[1]},{b[0]},{b[3]},{b[2]},urn:ogc:def:crs:EPSG::4326"  # latitude, longitude
        r = requests.get(c["url"], params=p, headers=UA, timeout=120)
        r.raise_for_status()
        return gpd.read_file(io.BytesIO(r.content))
    feats, url, params = [], f"{c['url']}/collections/{c['layer']}/items", dict(f="json", limit=1000)
    while url:
        r = requests.get(url, params=params, headers=UA, timeout=120)
        r.raise_for_status()
        j = r.json()
        feats += j["features"]
        url = next((l["href"] for l in j.get("links", []) if l.get("rel") == "next"), None)
        params = None
    return gpd.GeoDataFrame.from_features(feats, crs="EPSG:4326")


def keys(df, key):
    for k in (key, key.lower(), key.upper()):
        if k in df.columns:
            return sorted(map(str, df[k].tolist()))
    return None


def exact(df, bbox):
    """The features that meet the box (a server's BBOX may answer by envelopes; GDAL keeps the exact ones)."""
    if bbox is None:
        return df
    from shapely.geometry import box
    g = df.to_crs(4326).geometry if df.crs else df.geometry
    return df[g.intersects(box(*bbox)).values]


def compare(c, j, other):
    jf = [x for x in j.columns if x != "geometry"]
    of = [x for x in other.columns if x != "geometry"]
    ex = exact(other, c["bbox"])
    return dict(count=len(other), count_meeting_the_box=len(ex), same_count=len(ex) == len(j),
                fields_missing_in_janus=sorted(set(of) - set(jf) - {"id", "gml_id", "bbox"})), ex


def tile_of(lon, lat, z):
    n = 2 ** z
    x = int((lon + 180) / 360 * n)
    lr = math.radians(lat)
    y = int((1 - math.log(math.tan(lr) + 1 / math.cos(lr)) / math.pi) / 2 * n)
    return x, y


def tile_bbox(z, x, y):
    e = 20037508.342789244
    s = 2 * e / 2 ** z
    return (-e + x * s, e - (y + 1) * s, -e + (x + 1) * s, e - y * s)


def wms_case(out_dir):
    z = 10
    x, y = tile_of((BOX[0] + BOX[2]) / 2, (BOX[1] + BOX[3]) / 2, z)
    raw = out_dir / "tile.rgba"

    tile_s = []

    def janus():
        r = subprocess.run([str(EXE), "tile", DETER, "deter-amz:deter_amz", "-", str(z), str(x), str(y), str(raw)],
                           capture_output=True, text=True, env=ENV)
        j = json.loads(r.stdout or "{}")
        if "error" in j or r.returncode != 0:
            raise RuntimeError(f"Janus: {j.get('error', r.stderr)}")
        tile_s.append(j["seconds"])  # the tile's request alone (not the capabilities read before)
        return np.frombuffer(raw.read_bytes(), np.uint8).reshape(256, 256, 4), j

    (jt, jinfo), _, _ = timed(janus)
    jmed, jts = statistics.median(tile_s), tile_s
    b = tile_bbox(z, x, y)

    def direct():
        p = dict(SERVICE="WMS", VERSION="1.3.0", REQUEST="GetMap", LAYERS=jinfo["layer"], STYLES="",
                 CRS="EPSG:3857", BBOX=",".join(f"{v:.8f}" for v in b), WIDTH=256, HEIGHT=256, FORMAT="image/png",
                 TRANSPARENT="TRUE")
        r = requests.get(DETER, params=p, headers=UA, timeout=120)
        r.raise_for_status()
        return np.asarray(Image.open(io.BytesIO(r.content)).convert("RGBA"))

    dt, dmed, dts = timed(direct)
    differ = int((jt != dt).any(axis=2).sum())
    return dict(tile=[z, x, y], bbox_3857=b, janus_s=jmed, janus_runs=jts, direct_getmap_s=dmed, direct_runs=dts,
                drawn_pixels=int((dt[..., 3] > 0).sum()), pixels_differ=differ, janus_request=jinfo)


def main():
    if not EXE.exists():
        sys.exit(f"{EXE} missing: cargo build --release -p janus-geo --example vs_python_ogc")
    out = {}
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        for c in CASES:
            print(f"== {c['name']}", flush=True)
            res = {}
            try:
                infos = []

                def run():
                    df, j = janus_features(c, tmp / f"{c['name']}.gpkg")
                    infos.append(j)
                    return df, j

                (jdf, jinfo), _, _ = timed(run)
                # the fetch alone (as the Python ones: no process started, no file read back)
                jts = [j["seconds"] for j in infos]
                jmed = statistics.median(jts)
                res["janus"] = dict(features=len(jdf), seconds=jmed, runs=jts, matched=jinfo.get("matched"),
                                    capped=jinfo.get("capped"), fields=[x for x in jdf.columns if x != "geometry"])
                print(f"   Janus: {len(jdf)} features, {jmed:.2f} s")
            except Exception as e:  # a server down: said, the rest still run
                res["janus"] = dict(error=str(e))
                print(f"   Janus: {e}")
                out[c["name"]] = res
                continue
            jkeys = keys(jdf, c["key"])
            for name, f in (("geopandas_read_file", geopandas_features), ("owslib", None),
                            ("requests_geojson", requests_features)):
                try:
                    if f is None:
                        df, med, ts = owslib_features(c, tmp / f"{c['name']}.owslib.geojson")
                    else:
                        df, med, ts = timed(lambda: f(c))
                    r, df = compare(c, jdf, df)
                    k = keys(df, c["key"])
                    r["same_keys"] = None if k is None or jkeys is None else k == jkeys
                    r.update(seconds=med, runs=ts)
                    res[name] = r
                    print(f"   {name}: {len(df)} features, {med:.2f} s, same keys {r['same_keys']}")
                except Exception as e:
                    res[name] = dict(error=str(e))
                    print(f"   {name}: {e}")
            out[c["name"]] = res
        print("== deter_wms_tile", flush=True)
        try:
            out["deter_wms_tile"] = w = wms_case(tmp)
            print(f"   Janus {w['janus_s']:.2f} s, direct {w['direct_getmap_s']:.2f} s, "
                  f"{w['drawn_pixels']} drawn pixels, {w['pixels_differ']} differ")
        except Exception as e:
            out["deter_wms_tile"] = dict(error=str(e))
            print(f"   {e}")
    path = ROOT / "bench" / "ogc-vs-python.json"
    path.write_text(json.dumps(out, indent=2) + "\n")
    print(f"written {path}")


if __name__ == "__main__":
    if sys.argv[1:2] == ["--owslib"]:
        case = next(c for c in CASES if c["name"] == sys.argv[2])
        t = time.perf_counter()
        data = owslib_geojson(case)
        print(time.perf_counter() - t)
        Path(sys.argv[3]).write_bytes(data)
    else:
        main()
