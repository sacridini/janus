#include "vectors.hpp"

#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdio>
#include <filesystem>
#include <fstream>

#include <cpl_conv.h>
#include <cpl_error.h>
#include <cpl_string.h>
#include <gdal_priv.h>
#include <ogr_api.h>
#include <ogr_geometry.h>
#include <ogr_spatialref.h>
#include <ogrsf_frmts.h>

namespace fs = std::filesystem;

namespace {

std::string lowerExt(const std::string& path) {
    std::string e = fs::u8path(path).extension().u8string();
    std::transform(e.begin(), e.end(), e.begin(), [](unsigned char c) { return char(std::tolower(c)); });
    return e;
}

std::string crsText(const OGRSpatialReference& s) {
    const char* auth = s.GetAuthorityName(nullptr);
    const char* code = s.GetAuthorityCode(nullptr);
    if (auth && code) return std::string(auth) + ":" + code;
    const char* name = s.GetName();
    return name ? name : "unknown CRS";
}

std::string wkt2(const OGRSpatialReference& s) {
    char* w = nullptr;
    const char* opts[] = {"FORMAT=WKT2_2019", nullptr};
    s.exportToWkt(&w, opts);
    std::string out = w ? w : "";
    CPLFree(w);
    return out;
}

void addRing(const OGRSimpleCurve& c, Ring& r) {
    const int n = c.getNumPoints();
    r.reserve(r.size() + size_t(n) * 2);
    for (int i = 0; i < n; ++i) {
        r.push_back(c.getX(i));
        r.push_back(c.getY(i));
    }
}

// Collects the parts of a (multi, collection, curved) geometry; `kind` the highest seen.
void collect(const OGRGeometry* g, VectorFeature& f) {
    if (!g || g->IsEmpty()) return;
    if (g->hasCurveGeometry()) {
        std::unique_ptr<OGRGeometry> lin(g->getLinearGeometry());
        if (lin && !lin->hasCurveGeometry()) collect(lin.get(), f);
        return;
    }
    switch (wkbFlatten(g->getGeometryType())) {
    case wkbPoint: {
        const OGRPoint* p = g->toPoint();
        f.parts.push_back({p->getX(), p->getY()});
        f.kind = std::max(f.kind, VectorFeature::Point);
        break;
    }
    case wkbLineString:
    case wkbLinearRing: {
        Ring r;
        addRing(*g->toSimpleCurve(), r);
        if (r.size() >= 4) f.parts.push_back(std::move(r));
        f.kind = std::max(f.kind, VectorFeature::Line);
        break;
    }
    case wkbPolygon: {
        const OGRPolygon* p = g->toPolygon();
        for (const OGRLinearRing* ring : *p) {
            Ring r;
            addRing(*ring, r);
            if (r.size() >= 4 && r[0] == r[r.size() - 2] && r[1] == r.back()) r.resize(r.size() - 2); // closed implicitly
            if (r.size() >= 6) f.parts.push_back(std::move(r));
        }
        f.kind = VectorFeature::Area;
        break;
    }
    case wkbMultiPoint:
    case wkbMultiLineString:
    case wkbMultiPolygon:
    case wkbGeometryCollection:
        for (const OGRGeometry* part : *g->toGeometryCollection()) collect(part, f);
        break;
    default:
        break;
    }
}

} // namespace

std::string VectorData::kindText() const {
    auto n = [](int k, const char* one, const char* many) {
        return std::to_string(k) + " " + (k == 1 ? one : many);
    };
    std::string s;
    if (points) s += n(points, "point", "points");
    if (areas) s += (s.empty() ? "" : ", ") + n(areas, "polygon", "polygons");
    if (lines) s += (s.empty() ? "" : ", ") + n(lines, "line", "lines");
    if (empty) s += (s.empty() ? "" : ", ") + n(empty, "row without geometry", "rows without geometry");
    return s.empty() ? "no features" : s;
}

bool isVectorPath(const std::string& path) {
    static const char* exts[] = {".geojson", ".json", ".shp", ".gpkg", ".kml", ".kmz", ".gml",
                                 ".fgb",     ".csv",  ".gpx", ".tab", ".geojsonl"};
    const std::string e = lowerExt(path);
    for (const char* x : exts)
        if (e == x) return true;
    return false;
}

std::vector<std::shared_ptr<VectorData>> loadVectorFile(const std::string& path, std::string& error, size_t maxVertices) {
    std::vector<std::shared_ptr<VectorData>> out;
    const auto t0 = std::chrono::steady_clock::now();
    // A table of points: the coordinate columns by their usual names.
    char** open = nullptr;
    if (lowerExt(path) == ".csv") {
        open = CSLSetNameValue(open, "X_POSSIBLE_NAMES", "x,lon,long,longitude,lng,easting,coord_x,x_coord,xcoord");
        open = CSLSetNameValue(open, "Y_POSSIBLE_NAMES", "y,lat,latitude,northing,coord_y,y_coord,ycoord");
        open = CSLSetNameValue(open, "KEEP_GEOM_COLUMNS", "YES");
        open = CSLSetNameValue(open, "AUTODETECT_TYPE", "NO");
    }
    CPLErrorReset();
    GDALDataset* ds = static_cast<GDALDataset*>(GDALOpenEx(path.c_str(), GDAL_OF_VECTOR | GDAL_OF_READONLY, nullptr, open, nullptr));
    CSLDestroy(open);
    if (!ds) {
        const char* m = CPLGetLastErrorMsg();
        error = "cannot open " + path + " as vector data" + (m && *m ? std::string(": ") + m : std::string());
        return out;
    }
    size_t vertices = 0;
    for (int li = 0; li < ds->GetLayerCount(); ++li) {
        OGRLayer* L = ds->GetLayer(li);
        if (!L) continue;
        auto v = std::make_shared<VectorData>();
        v->path = path;
        v->layer = L->GetName();
        if (const OGRSpatialReference* s = L->GetSpatialRef()) {
            v->crsWkt = wkt2(*s);
            v->crsName = crsText(*s);
        }
        OGRFeatureDefn* defn = L->GetLayerDefn();
        for (int k = 0; k < defn->GetFieldCount(); ++k) v->fields.push_back(defn->GetFieldDefn(k)->GetNameRef());
        L->ResetReading();
        while (OGRFeature* f = L->GetNextFeature()) {
            VectorFeature vf;
            vf.fid = f->GetFID();
            vf.values.resize(v->fields.size());
            for (int k = 0; k < int(v->fields.size()); ++k)
                if (f->IsFieldSetAndNotNull(k)) vf.values[k] = f->GetFieldAsString(k);
            const OGRGeometry* g = f->GetGeometryRef();
            collect(g, vf);
            if (vf.parts.empty()) vf.kind = VectorFeature::None;
            if (vf.kind == VectorFeature::Area) {
                // A point inside (GEOS), else the centroid.
                OGRGeometryH p = OGR_G_PointOnSurface(OGRGeometry::ToHandle(const_cast<OGRGeometry*>(g)));
                OGRPoint c;
                if (p && wkbFlatten(OGR_G_GetGeometryType(p)) == wkbPoint) {
                    vf.cx = OGR_G_GetX(p, 0);
                    vf.cy = OGR_G_GetY(p, 0);
                } else if (g->Centroid(&c) == OGRERR_NONE) {
                    vf.cx = c.getX();
                    vf.cy = c.getY();
                }
                if (p) OGR_G_DestroyGeometry(p);
            } else if (vf.kind == VectorFeature::Line) {
                OGRPoint c;
                if (g->Centroid(&c) == OGRERR_NONE) {
                    vf.cx = c.getX();
                    vf.cy = c.getY();
                }
            } else if (vf.kind == VectorFeature::Point) {
                vf.cx = vf.parts[0][0];
                vf.cy = vf.parts[0][1];
            }
            OGRFeature::DestroyFeature(f);
            for (const Ring& r : vf.parts) vertices += r.size() / 2;
            switch (vf.kind) {
            case VectorFeature::Point: ++v->points; break;
            case VectorFeature::Line: ++v->lines; break;
            case VectorFeature::Area: ++v->areas; break;
            default: ++v->empty; break;
            }
            v->features.push_back(std::move(vf));
            if (vertices > maxVertices) {
                v->truncated = true;
                break;
            }
        }
        if (v->features.empty()) continue;
        // No CRS (e.g. a CSV of points): lon/lat when every coordinate fits.
        if (v->crsWkt.empty()) {
            bool fits = true;
            for (const VectorFeature& f : v->features)
                for (const Ring& r : f.parts)
                    for (size_t i = 0; i + 1 < r.size() && fits; i += 2)
                        fits = std::fabs(r[i]) <= 180 && std::fabs(r[i + 1]) <= 90;
            if (fits) {
                OGRSpatialReference s;
                s.SetWellKnownGeogCS("WGS84");
                v->crsWkt = wkt2(s);
                v->crsName = "EPSG:4326";
                v->lonLatAssumed = true;
            }
        }
        for (const VectorFeature& f : v->features)
            for (const Ring& r : f.parts) v->vertices += r.size() / 2;
        v->loadMs = std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - t0).count();
        out.push_back(std::move(v));
        if (vertices > maxVertices) break;
    }
    GDALClose(ds);
    if (out.empty()) error = path + " has no features";
    return out;
}

// ---------------------------------------------------------------------------
// Placing
// ---------------------------------------------------------------------------

std::unique_ptr<VectorPlacer> VectorPlacer::create(const VectorData& v, const std::string& rasterWkt,
                                                   bool hasGeoTransform, const std::array<double, 6>& gt,
                                                   std::string& why) {
    std::unique_ptr<VectorPlacer> p(new VectorPlacer());
    if (!hasGeoTransform) {
        why = "the raster has no georeferencing";
        return nullptr;
    }
    if (!GDALInvGeoTransform(const_cast<double*>(gt.data()), p->inv_)) {
        why = "the raster's geotransform cannot be inverted";
        return nullptr;
    }
    OGRSpatialReference src, dst, wgs;
    wgs.SetWellKnownGeogCS("WGS84");
    wgs.SetAxisMappingStrategy(OAMS_TRADITIONAL_GIS_ORDER);
    const bool hasSrc = !v.crsWkt.empty() && src.importFromWkt(v.crsWkt.c_str()) == OGRERR_NONE;
    const bool hasDst = !rasterWkt.empty() && dst.importFromWkt(rasterWkt.c_str()) == OGRERR_NONE;
    src.SetAxisMappingStrategy(OAMS_TRADITIONAL_GIS_ORDER); // x = easting / longitude, whatever the CRS says
    dst.SetAxisMappingStrategy(OAMS_TRADITIONAL_GIS_ORDER);
    if (hasSrc && !hasDst) {
        why = "the raster has no CRS";
        return nullptr;
    }
    if (hasSrc && hasDst && !src.IsSame(&dst)) {
        p->toRaster_ = OGRCreateCoordinateTransformation(&src, &dst);
        if (!p->toRaster_) {
            why = "no transformation from " + v.crsName + " to the raster's CRS";
            return nullptr;
        }
    }
    // lon/lat for tables: from the file's CRS, else (none in the file) from the raster's.
    const OGRSpatialReference* from = hasSrc ? &src : hasDst ? &dst : nullptr;
    if (from) {
        if (from->IsGeographic() && from->IsSame(&wgs)) p->lonLatSame_ = true;
        else p->toLonLat_ = OGRCreateCoordinateTransformation(from, &wgs);
    }
    return p;
}

std::unique_ptr<VectorPlacer> VectorPlacer::lonLat(const VectorData& v) {
    std::unique_ptr<VectorPlacer> p(new VectorPlacer());
    OGRSpatialReference src, wgs;
    wgs.SetWellKnownGeogCS("WGS84");
    wgs.SetAxisMappingStrategy(OAMS_TRADITIONAL_GIS_ORDER);
    if (v.crsWkt.empty() || src.importFromWkt(v.crsWkt.c_str()) != OGRERR_NONE) return p; // toLonLat fails
    src.SetAxisMappingStrategy(OAMS_TRADITIONAL_GIS_ORDER);
    if (src.IsGeographic() && src.IsSame(&wgs)) p->lonLatSame_ = true;
    else p->toLonLat_ = OGRCreateCoordinateTransformation(&src, &wgs);
    return p;
}

VectorPlacer::~VectorPlacer() {
    if (toRaster_) OGRCoordinateTransformation::DestroyCT(toRaster_);
    if (toLonLat_) OGRCoordinateTransformation::DestroyCT(toLonLat_);
}

void VectorPlacer::toPixel(size_t n, double* x, double* y, int* ok) const {
    if (toRaster_) toRaster_->Transform(n, x, y, nullptr, ok);
    else std::fill(ok, ok + n, 1);
    for (size_t i = 0; i < n; ++i) {
        if (!ok[i] || !std::isfinite(x[i]) || !std::isfinite(y[i])) {
            ok[i] = 0;
            continue;
        }
        const double gx = x[i], gy = y[i];
        x[i] = inv_[0] + gx * inv_[1] + gy * inv_[2];
        y[i] = inv_[3] + gx * inv_[4] + gy * inv_[5];
    }
}

bool VectorPlacer::toPixel(double& x, double& y) const {
    int ok = 0;
    toPixel(1, &x, &y, &ok);
    return ok != 0;
}

bool VectorPlacer::toLonLat(double& x, double& y) const {
    if (lonLatSame_) return true;
    return toLonLat_ && toLonLat_->Transform(1, &x, &y) && std::isfinite(x) && std::isfinite(y);
}

Polygon VectorPlacer::placed(const Polygon& p) const {
    Polygon out;
    out.reserve(p.size());
    std::vector<double> xs, ys;
    std::vector<int> ok;
    for (const Ring& r : p) {
        const size_t n = r.size() / 2;
        xs.resize(n);
        ys.resize(n);
        ok.resize(n);
        for (size_t i = 0; i < n; ++i) {
            xs[i] = r[i * 2];
            ys[i] = r[i * 2 + 1];
        }
        toPixel(n, xs.data(), ys.data(), ok.data());
        Ring o;
        o.reserve(r.size());
        for (size_t i = 0; i < n; ++i)
            if (ok[i]) {
                o.push_back(xs[i]);
                o.push_back(ys[i]);
            }
        out.push_back(std::move(o)); // kept even if short: parts stay aligned with the file's
    }
    return out;
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

std::string csvField(const std::string& s) {
    if (s.find_first_of(",\"\r\n") == std::string::npos) return s;
    std::string q = "\"";
    for (char c : s) {
        if (c == '"') q += '"';
        q += c;
    }
    return q + "\"";
}

bool writeVectorTable(const std::string& path, const VectorData& v, const std::vector<std::string>& extraNames,
                      const std::function<std::string(size_t, size_t)>& extra, std::string& error) {
    const std::string ext = lowerExt(path);
    const std::string tmp = path + ".part" + ext; // OGR picks the format by extension: keep it last
    std::error_code ec;
    fs::remove(fs::u8path(tmp), ec);
    if (ext == ".csv") {
        // Plain text: the fields, a representative point in lon/lat, the extra columns.
        const std::unique_ptr<VectorPlacer> P = VectorPlacer::lonLat(v);
        std::ofstream f(fs::u8path(tmp), std::ios::binary | std::ios::trunc);
        if (!f) {
            error = "cannot write " + tmp;
            return false;
        }
        std::string line = "fid";
        for (const std::string& n : v.fields) line += "," + csvField(n);
        line += ",lon,lat";
        for (const std::string& n : extraNames) line += "," + csvField(n);
        line += "\n";
        bool ok = bool(f.write(line.data(), std::streamsize(line.size())));
        for (size_t i = 0; i < v.features.size() && ok; ++i) {
            const VectorFeature& F = v.features[i];
            line = std::to_string(F.fid);
            for (const std::string& s : F.values) line += "," + csvField(s);
            double lon = F.cx, lat = F.cy;
            char b[64];
            if (F.kind != VectorFeature::None && P && P->toLonLat(lon, lat)) {
                std::snprintf(b, sizeof(b), ",%.8f,%.8f", lon, lat);
                line += b;
            } else {
                line += ",,";
            }
            for (size_t k = 0; k < extraNames.size(); ++k) line += "," + csvField(extra(i, k));
            line += "\n";
            ok = bool(f.write(line.data(), std::streamsize(line.size())));
        }
        f.close();
        ok = ok && !f.fail();
        if (!ok) {
            error = "cannot write " + tmp;
            fs::remove(fs::u8path(tmp), ec);
            return false;
        }
    } else {
        const char* driverName = ext == ".gpkg" ? "GPKG" : ext == ".shp" ? "ESRI Shapefile"
                                 : (ext == ".geojson" || ext == ".json") ? "GeoJSON" : nullptr;
        GDALDriver* drv = driverName ? GetGDALDriverManager()->GetDriverByName(driverName) : nullptr;
        if (!drv) {
            error = "unsupported format " + ext + " (use .csv, .gpkg, .geojson or .shp)";
            return false;
        }
        GDALDataset* src = static_cast<GDALDataset*>(GDALOpenEx(v.path.c_str(), GDAL_OF_VECTOR | GDAL_OF_READONLY,
                                                                 nullptr, nullptr, nullptr));
        OGRLayer* sl = src ? src->GetLayerByName(v.layer.c_str()) : nullptr;
        if (!sl) {
            if (src) GDALClose(src);
            error = "cannot read " + v.path + " again";
            return false;
        }
        CPLErrorReset();
        GDALDataset* dst = drv->Create(tmp.c_str(), 0, 0, 0, GDT_Unknown, nullptr);
        OGRSpatialReference srs;
        const bool hasSrs = !v.crsWkt.empty() && srs.importFromWkt(v.crsWkt.c_str()) == OGRERR_NONE;
        srs.SetAxisMappingStrategy(OAMS_TRADITIONAL_GIS_ORDER);
        OGRLayer* dl = dst ? dst->CreateLayer(v.layer.c_str(), hasSrs ? &srs : nullptr, sl->GetGeomType(), nullptr) : nullptr;
        bool ok = dl != nullptr;
        OGRFeatureDefn* sd = sl->GetLayerDefn();
        for (int k = 0; ok && k < sd->GetFieldCount(); ++k) ok = dl->CreateField(sd->GetFieldDefn(k)) == OGRERR_NONE;
        for (size_t k = 0; ok && k < extraNames.size(); ++k) {
            OGRFieldDefn fd(extraNames[k].c_str(), OFTString);
            ok = dl->CreateField(&fd) == OGRERR_NONE;
        }
        const int nSrc = sd->GetFieldCount();
        if (ok) dl->StartTransaction();
        sl->ResetReading();
        size_t i = 0;
        while (ok) {
            OGRFeature* sf = sl->GetNextFeature();
            if (!sf) break;
            if (i < v.features.size()) { // the rows in the order they were read
                OGRFeature* df = OGRFeature::CreateFeature(dl->GetLayerDefn());
                df->SetFrom(sf, TRUE);
                for (size_t k = 0; k < extraNames.size(); ++k) {
                    const std::string val = extra(i, k);
                    if (!val.empty()) df->SetField(nSrc + int(k), val.c_str());
                }
                ok = dl->CreateFeature(df) == OGRERR_NONE;
                OGRFeature::DestroyFeature(df);
            }
            OGRFeature::DestroyFeature(sf);
            ++i;
        }
        if (dl) ok = dl->CommitTransaction() == OGRERR_NONE && ok;
        GDALClose(src);
        if (dst) GDALClose(dst);
        if (!ok || CPLGetLastErrorType() >= CE_Failure) {
            const char* m = CPLGetLastErrorMsg();
            error = "cannot write " + path + (m && *m ? std::string(": ") + m : std::string());
            if (drv) drv->Delete(tmp.c_str());
            fs::remove(fs::u8path(tmp), ec);
            return false;
        }
    }
    // A Shapefile is several files: each one renamed.
    if (ext == ".shp") {
        for (const char* side : {".shp", ".shx", ".dbf", ".prj", ".cpg"}) {
            const fs::path a = fs::u8path(path + ".part" + side), b = fs::u8path(path).replace_extension(side);
            if (!fs::exists(a, ec)) continue;
            fs::remove(b, ec);
            fs::rename(a, b, ec);
        }
        return true;
    }
    if (ext == ".gpkg" || ext == ".geojson" || ext == ".json" || ext == ".csv") {
        GDALDriver* drv = ext == ".gpkg" ? GetGDALDriverManager()->GetDriverByName("GPKG") : nullptr;
        if (drv) drv->Delete(path.c_str()); // a GeoPackage replaced whole (and its -wal/-shm)
        fs::remove(fs::u8path(path), ec);
        fs::rename(fs::u8path(tmp), fs::u8path(path), ec);
        if (ec) {
            error = "cannot rename " + tmp + ": " + ec.message();
            return false;
        }
    }
    return true;
}
