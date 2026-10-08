// Export: the map as a PNG figure, the active layer's values and the rendered
// view as georeferenced GeoTIFFs (for QGIS), and Zeit results. The GPU renders
// on the main thread (offscreen, at the chosen resolution); reading the data
// and writing the files run in background threads, with progress and cancel in
// the Exports window. Files are written with GDAL (no image library) under a
// temporary name and renamed when complete.
#define IMGUI_DEFINE_MATH_OPERATORS
#include "app.hpp"

#include <algorithm>
#include <cctype>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <functional>
#include <thread>

#include <cpl_conv.h>
#include <cpl_error.h>
#include <cpl_string.h>
#include <gdal_priv.h>
#include <imgui_internal.h>
#include <implot.h>

#include "glfw.hpp"
#include "platform.hpp"

namespace fs = std::filesystem;

namespace {

constexpr int kExportSlot = -1;            // GPU target of the offscreen renders
constexpr int kMaxExportSide = 16384;      // pixels per side of a rendered export
constexpr double kMaxExportPixels = 128e6; // 512 MB of RGBA

double secondsSince(std::chrono::steady_clock::time_point t) {
    return std::chrono::duration<double>(std::chrono::steady_clock::now() - t).count();
}

// Progress of a job; wakes the UI every 2% so the Exports window follows it.
void setProgress(ExportJob& j, double p) {
    const double old = j.progress.exchange(p);
    if (int(p * 50) != int(old * 50)) glfwPostEmptyEvent();
}

int CPL_STDCALL gdalProgress(double done, const char*, void* arg) {
    ExportJob& j = *static_cast<ExportJob*>(arg);
    setProgress(j, done);
    return !j.cancel;
}

// Runs `work` in a background thread; it returns false with `error` set on failure.
std::shared_ptr<ExportJob> startJob(std::string title, std::string path,
                                    std::function<bool(ExportJob&, std::string&)> work) {
    auto job = std::make_shared<ExportJob>();
    job->title = std::move(title);
    job->path = std::move(path);
    ExportJob* j = job.get(); // the job outlives the thread: its last member waits for it
    job->done = std::async(std::launch::async, [j, work = std::move(work)]() mutable {
        std::string error;
        const bool ok = work(*j, error);
        work = nullptr; // frees what it captured (images) now, not when the job is removed
        j->seconds = secondsSince(j->started);
        if (!ok) {
            std::lock_guard<std::mutex> lk(j->m);
            j->error = j->cancel ? "" : error;
        }
        j->state = j->cancel ? ExportJob::State::Cancelled : ok ? ExportJob::State::Done : ExportJob::State::Failed;
        glfwPostEmptyEvent();
    });
    return job;
}

std::string gdalError(const std::string& what) {
    const char* msg = CPLGetLastErrorMsg();
    return what + (msg && *msg ? std::string(": ") + msg : std::string());
}

// The file is written as <path>.part and renamed once complete: a cancelled or
// failed export leaves nothing behind, and never a half-written file.
bool finishFile(const std::string& tmp, const std::string& path, bool ok, std::string& error) {
    std::error_code ec;
    if (!ok) {
        fs::remove(fs::u8path(tmp), ec);
        return false;
    }
    fs::remove(fs::u8path(path), ec);
    fs::rename(fs::u8path(tmp), fs::u8path(path), ec);
    if (ec) {
        error = "could not rename " + tmp + ": " + ec.message();
        fs::remove(fs::u8path(tmp), ec);
        return false;
    }
    return true;
}

// The CRS (WKT) of a raster file, empty without one.
std::string fileWkt(const std::string& path) {
    std::string wkt;
    if (GDALDataset* ds = GDALDataset::Open(path.c_str(), GDAL_OF_RASTER | GDAL_OF_READONLY)) {
        if (const char* w = ds->GetProjectionRef()) wkt = w;
        GDALClose(ds);
    }
    return wkt;
}

// An RGB(A) image (pixel-interleaved RGBA8, rows top-down) written through a
// MEM dataset and CreateCopy: PNG, or GTiff with a geotransform and a CRS.
bool writeImage(const std::string& path, const char* driver, const std::vector<unsigned char>& rgba, int w, int h,
                bool alpha, const double* gt, const std::string& wkt, char** options, ExportJob& job,
                std::string& error) {
    GDALDriver* mem = GetGDALDriverManager()->GetDriverByName("MEM");
    GDALDriver* drv = GetGDALDriverManager()->GetDriverByName(driver);
    if (!mem || !drv) {
        error = std::string("GDAL has no ") + driver + " driver";
        return false;
    }
    const int bands = alpha ? 4 : 3;
    GDALDataset* src = mem->Create("", w, h, bands, GDT_Byte, nullptr);
    if (!src) {
        error = gdalError("could not allocate the image");
        return false;
    }
    int map[4] = {1, 2, 3, 4};
    bool ok = src->RasterIO(GF_Write, 0, 0, w, h, const_cast<unsigned char*>(rgba.data()), w, h, GDT_Byte, bands, map,
                            4, GSpacing(w) * 4, 1, nullptr) == CE_None;
    const GDALColorInterp ci[4] = {GCI_RedBand, GCI_GreenBand, GCI_BlueBand, GCI_AlphaBand};
    for (int b = 0; b < bands; ++b) src->GetRasterBand(b + 1)->SetColorInterpretation(ci[b]);
    if (gt) src->SetGeoTransform(const_cast<double*>(gt));
    if (!wkt.empty()) src->SetProjection(wkt.c_str());
    const std::string tmp = path + ".part";
    if (ok) {
        CPLErrorReset();
        GDALDataset* out = drv->CreateCopy(tmp.c_str(), src, FALSE, options, gdalProgress, &job);
        ok = out != nullptr;
        if (out) GDALClose(out);
        ok = ok && CPLGetLastErrorType() < CE_Failure && !job.cancel;
        if (!ok) error = gdalError("could not write the file");
    }
    GDALClose(src);
    return finishFile(tmp, path, ok, error);
}

// File name made of the parts of a layer name or a date.
std::string sanitize(const std::string& s) {
    std::string out;
    for (unsigned char c : s) {
        const bool keep = std::isalnum(c) || c == '-' || c == '_' || c == '.' || c >= 0x80;
        if (keep) out += char(c);
        else if (!out.empty() && out.back() != '_') out += '_';
    }
    while (!out.empty() && (out.back() == '_' || out.back() == '.')) out.pop_back();
    return out.empty() ? "janus" : out;
}

// RGBA8 image (straight alpha, rows top-down) with the few marks a figure needs,
// drawn like the map draws them through ImGui. Text uses ImGui's font baked at
// the export size, so it stays sharp at 2x and 4x (main thread only: the font
// atlas is ImGui's).
struct Canvas {
    int w = 0, h = 0;
    unsigned char* px = nullptr;

    void blend(int x, int y, ImU32 col, float cov) {
        if (x < 0 || y < 0 || x >= w || y >= h || cov <= 0) return;
        const float sa = float((col >> IM_COL32_A_SHIFT) & 0xFF) / 255.f * std::min(cov, 1.f);
        unsigned char* d = px + (size_t(y) * w + x) * 4;
        const float da = d[3] / 255.f, oa = sa + da * (1 - sa);
        if (oa <= 0) return;
        const unsigned shift[3] = {IM_COL32_R_SHIFT, IM_COL32_G_SHIFT, IM_COL32_B_SHIFT};
        for (int c = 0; c < 3; ++c) {
            const float s = float((col >> shift[c]) & 0xFF);
            d[c] = (unsigned char)std::lround((s * sa + d[c] * da * (1 - sa)) / oa);
        }
        d[3] = (unsigned char)std::lround(oa * 255);
    }
    // Pixels covered by the rectangle (partly at fractional edges).
    void fillRect(float x0, float y0, float x1, float y1, ImU32 col) {
        for (int y = int(std::floor(y0)); y < int(std::ceil(y1)); ++y) {
            const float cy = std::min(y1, y + 1.f) - std::max(y0, float(y));
            for (int x = int(std::floor(x0)); x < int(std::ceil(x1)); ++x)
                blend(x, y, col, cy * (std::min(x1, x + 1.f) - std::max(x0, float(x))));
        }
    }
    void rect(float x0, float y0, float x1, float y1, ImU32 col, float t) {
        fillRect(x0 - t / 2, y0 - t / 2, x1 + t / 2, y0 + t / 2, col);
        fillRect(x0 - t / 2, y1 - t / 2, x1 + t / 2, y1 + t / 2, col);
        fillRect(x0 - t / 2, y0 + t / 2, x0 + t / 2, y1 - t / 2, col);
        fillRect(x1 - t / 2, y0 + t / 2, x1 + t / 2, y1 - t / 2, col);
    }
    // Disc (thickness 0) or ring of radius r, antialiased.
    void circle(float cx, float cy, float r, ImU32 col, float t = 0) {
        const float ext = r + t / 2 + 1;
        for (int y = int(std::floor(cy - ext)); y <= int(std::ceil(cy + ext)); ++y)
            for (int x = int(std::floor(cx - ext)); x <= int(std::ceil(cx + ext)); ++x) {
                const float d = std::hypot(x + 0.5f - cx, y + 0.5f - cy);
                blend(x, y, col, std::clamp(t > 0 ? t / 2 + 0.5f - std::fabs(d - r) : r + 0.5f - d, 0.f, 1.f));
            }
    }
    static ImFontBaked* font(float size) { return ImGui::GetFont()->GetFontBaked(size, 1.0f); }
    static float textWidth(const char* s, float size) {
        ImFontBaked* f = font(size);
        float wsum = 0;
        for (const char* p = s; *p;) {
            unsigned int c = 0;
            p += ImTextCharFromUtf8(&c, p, nullptr);
            if (const ImFontGlyph* g = f->FindGlyph(ImWchar(c))) wsum += g->AdvanceX;
        }
        return wsum;
    }
    // (x, y): top left of the line, in pixels.
    void text(float x, float y, const char* s, float size, ImU32 col) {
        ImFontBaked* f = font(size);
        ImFontAtlas* atlas = ImGui::GetIO().Fonts;
        for (const char* p = s; *p;) {
            unsigned int c = 0;
            p += ImTextCharFromUtf8(&c, p, nullptr);
            const ImFontGlyph* g = f->FindGlyph(ImWchar(c));
            if (!g) continue;
            const ImTextureData* tex = atlas->TexData; // read after FindGlyph: baking may grow the atlas
            const int gw = int(std::lround(g->X1 - g->X0)), gh = int(std::lround(g->Y1 - g->Y0));
            if (g->Visible && tex && tex->Pixels && gw > 0 && gh > 0) {
                const float u0 = g->U0 * tex->Width, v0 = g->V0 * tex->Height;
                const float su = (g->U1 - g->U0) * tex->Width / gw, sv = (g->V1 - g->V0) * tex->Height / gh;
                const int ox = int(std::floor(x + g->X0)), oy = int(std::floor(y + g->Y0));
                for (int j = 0; j < gh; ++j)
                    for (int i = 0; i < gw; ++i) {
                        const int tx = std::clamp(int(u0 + (i + 0.5f) * su), 0, tex->Width - 1);
                        const int ty = std::clamp(int(v0 + (j + 0.5f) * sv), 0, tex->Height - 1);
                        const unsigned char* t = tex->Pixels + (size_t(ty) * tex->Width + tx) * tex->BytesPerPixel;
                        const unsigned char a = tex->Format == ImTextureFormat_RGBA32 ? t[3] : t[0];
                        if (a) blend(ox + i, oy + j, col, a / 255.f);
                    }
            }
            x += g->AdvanceX;
        }
    }
};

// A legend block: a caption and a colour bar (lo..hi) or a list of classes.
struct Legend {
    std::string caption;
    int cmap = -1;                 // colour bar
    float lo = 0, hi = 1;
    std::vector<std::pair<ImU32, std::string>> classes;
};

// Draws legends from the bottom left corner up, each on a dark box (readable
// over any map and on a white background). Sizes in points x ps.
void drawLegends(Canvas& cv, const std::vector<Legend>& legends, float ps) {
    const float fs = ImGui::GetFontSize() * ps, line = fs + 3 * ps, pad = 6 * ps;
    const ImU32 box = IM_COL32(0, 0, 0, 165), fg = IM_COL32(235, 235, 235, 255);
    float bottom = cv.h - 10 * ps;
    for (const Legend& L : legends) {
        const int maxRows = 20;
        const int rows = std::min(int(L.classes.size()), maxRows) + (int(L.classes.size()) > maxRows ? 1 : 0);
        const float barW = 220 * ps, barH = 10 * ps;
        float width = L.cmap >= 0 ? barW : 0;
        width = std::max(width, Canvas::textWidth(L.caption.c_str(), fs));
        for (int k = 0; k < std::min(int(L.classes.size()), maxRows); ++k)
            width = std::max(width, 18 * ps + Canvas::textWidth(L.classes[k].second.c_str(), fs));
        const float height = (L.caption.empty() ? 0 : line) + (L.cmap >= 0 ? barH + 2 * ps + line : rows * line);
        const float x0 = 10 * ps, y0 = bottom - height;
        cv.fillRect(x0 - pad, y0 - pad, x0 + width + pad, bottom + pad - 2 * ps, box);
        float y = y0;
        if (!L.caption.empty()) {
            cv.text(x0, y, L.caption.c_str(), fs, fg);
            y += line;
        }
        if (L.cmap >= 0) {
            const int n = std::max(1, int(barW)); // one colour per pixel
            for (int i = 0; i < n; ++i) {
                const ImVec4 c = ImPlot::SampleColormap((i + 0.5f) / n, L.cmap);
                cv.fillRect(x0 + barW * i / n, y, x0 + barW * (i + 1) / n, y + barH, ImGui::ColorConvertFloat4ToU32(c));
            }
            cv.rect(x0, y, x0 + barW, y + barH, IM_COL32(0, 0, 0, 255), ps);
            char lo[32], hi[32];
            std::snprintf(lo, sizeof(lo), "%.4g", L.lo);
            std::snprintf(hi, sizeof(hi), "%.4g", L.hi);
            cv.text(x0, y + barH + 2 * ps, lo, fs, fg);
            cv.text(x0 + barW - Canvas::textWidth(hi, fs), y + barH + 2 * ps, hi, fs, fg);
        } else {
            for (int k = 0; k < rows; ++k, y += line) {
                if (k == maxRows) {
                    const std::string more = "... " + std::to_string(L.classes.size() - maxRows) + " more";
                    cv.text(x0, y, more.c_str(), fs, fg);
                    break;
                }
                cv.fillRect(x0, y + 1 * ps, x0 + 12 * ps, y + 13 * ps, L.classes[k].first);
                cv.rect(x0, y + 1 * ps, x0 + 12 * ps, y + 13 * ps, IM_COL32(0, 0, 0, 255), ps);
                cv.text(x0 + 18 * ps, y, L.classes[k].second.c_str(), fs, fg);
            }
        }
        bottom = y0 - pad - 8 * ps;
    }
}

} // namespace

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

bool App::exportScaleFits(int scale) const {
    const double w = std::lround(canvasSize_.x * mapPixelScale_ * scale), h = std::lround(canvasSize_.y * mapPixelScale_ * scale);
    const int side = std::min(kMaxExportSide, gpu_.maxCubeSide());
    return w <= side && h <= side && w * h <= kMaxExportPixels;
}

void App::renderExport(int scale, bool transparent, const float bg[4], std::vector<unsigned char>& rgba, int& w,
                       int& h) {
    const float ps = mapPixelScale_ * float(scale);
    const int cw = int(canvasSize_.x), ch = int(canvasSize_.y);
    if (!transparent) {
        renderMap(cw, ch, ps, kExportSlot, bg);
        gpu_.readMap(rgba, w, h, kExportSlot);
    } else {
        // Drawn over black and over white: a pixel covered with opacity a reads
        // c*a over black and c*a + (1 - a) over white, which gives a and c back
        // with the renderer as it is (no separate alpha pass in the shaders).
        const float black[4] = {0, 0, 0, 1}, white[4] = {1, 1, 1, 1};
        std::vector<unsigned char> overWhite;
        int w2 = 0, h2 = 0;
        renderMap(cw, ch, ps, kExportSlot, black);
        gpu_.readMap(rgba, w, h, kExportSlot);
        renderMap(cw, ch, ps, kExportSlot, white);
        gpu_.readMap(overWhite, w2, h2, kExportSlot);
        // In parallel: ~150 ms on one core for 4x a full HD map.
        const size_t n = rgba.size() / 4;
        const unsigned threads = std::clamp(std::thread::hardware_concurrency(), 1u, 16u);
        std::vector<std::thread> pool;
        for (unsigned k = 0; k < threads; ++k)
            pool.emplace_back([&, k] {
                for (size_t i = n * k / threads; i < n * (k + 1) / threads; ++i) {
                    unsigned char* p = &rgba[i * 4];
                    const unsigned char* q = &overWhite[i * 4];
                    const int d = std::max(std::max(q[0] - p[0], q[1] - p[1]), std::max(q[2] - p[2], 0));
                    if (d == 0) { // opaque (most pixels): as drawn
                        p[3] = 255;
                        continue;
                    }
                    const int a = 255 - d;
                    for (int c = 0; c < 3; ++c) p[c] = a ? (unsigned char)std::min(255, (p[c] * 255 + a / 2) / a) : 0;
                    p[3] = (unsigned char)a;
                }
            });
        for (std::thread& th : pool) th.join();
    }
    gpu_.releaseMap(kExportSlot);
}

bool App::visibleWindow(int win[4]) const {
    if (!s_ || scale_ <= 0) return false;
    const CubeInfo& info = *s_->info;
    win[0] = std::clamp(int(std::floor(-offset_.x / scale_)), 0, info.width);
    win[1] = std::clamp(int(std::floor(-offset_.y / scale_)), 0, info.height);
    win[2] = std::clamp(int(std::ceil((canvasSize_.x - offset_.x) / scale_)), 0, info.width);
    win[3] = std::clamp(int(std::ceil((canvasSize_.y - offset_.y) / scale_)), 0, info.height);
    return win[2] > win[0] && win[3] > win[1];
}

std::string App::exportName(const std::string& suffix) const {
    const SeriesLayer* L = activeLayer();
    std::string name = L ? sanitize(L->name) : "janus";
    if (s_) name += "_" + sanitize(s_->info->layers[t_].label);
    return name + suffix;
}

std::string App::askSavePath(const char* title, const std::string& name, const char* filter, const char* ext) {
    std::string path = platform::saveFileDialog(title, name, filter, ext, exportDir_);
    if (!path.empty()) exportDir_ = fs::u8path(path).parent_path().u8string();
    return path;
}

// ---------------------------------------------------------------------------
// Exports
// ---------------------------------------------------------------------------

std::shared_ptr<ExportJob> App::exportPng(const std::string& path, const PngOptions& o) {
    const float white[4] = {1, 1, 1, 1};
    const bool transparent = o.background == 2;
    auto img = std::make_shared<std::vector<unsigned char>>();
    int w = 0, h = 0;
    renderExport(o.scale, transparent, o.background == 1 ? white : nullptr, *img, w, h);

    // Marks, label and legend, as the map draws them, in points x ps.
    const float ps = mapPixelScale_ * float(o.scale);
    Canvas cv{w, h, img->data()};
    auto toPx = [&](double x, double y) {
        return ImVec2(float((offset_.x + x * scale_) * ps), float((offset_.y + y * scale_) * ps));
    };
    const float fs = ImGui::GetFontSize() * ps;
    if (o.marks) {
        if (roi_) {
            const ImVec2 a = toPx(roi_->x0, roi_->y0), b = toPx(roi_->x1, roi_->y1);
            cv.rect(a.x, a.y, b.x, b.y, IM_COL32(255, 210, 60, 255), 2 * ps);
        }
        for (const SeriesView& p : pins_) {
            const ImVec2 c = toPx(p.x + 0.5, p.y + 0.5);
            const ImU32 col = ImGui::ColorConvertFloat4ToU32(p.color);
            cv.circle(c.x, c.y, 8.5f * ps, IM_COL32(0, 0, 0, 220), 3 * ps);
            cv.circle(c.x, c.y, 6.5f * ps, col);
            cv.circle(c.x, c.y, 6.5f * ps, IM_COL32(255, 255, 255, 255), 1.5f * ps);
            const std::string num = std::to_string(p.id);
            const float tx = c.x + 11 * ps, ty = c.y - 18 * ps;
            cv.fillRect(tx - 3 * ps, ty - ps, tx + Canvas::textWidth(num.c_str(), fs) + 3 * ps, ty + fs + ps,
                        IM_COL32(0, 0, 0, 190));
            cv.text(tx, ty, num.c_str(), fs, col);
        }
    }
    const CubeInfo& info = *s_->info;
    if (o.label) {
        char title[200];
        if (mode_ == ModeRGB)
            std::snprintf(title, sizeof(title), "RGB: %s / %s / %s", info.layers[rgb_[0]].label.c_str(),
                          info.layers[rgb_[1]].label.c_str(), info.layers[rgb_[2]].label.c_str());
        else if (mode_ == ModeValue || mode_ == ModeAnomaly)
            std::snprintf(title, sizeof(title), "%s  |  %s", info.layers[t_].label.c_str(), modeName(mode_));
        else
            std::snprintf(title, sizeof(title), "%s  |  %d dates", modeName(mode_), info.T());
        const float x = 10 * ps, y = 8 * ps;
        cv.fillRect(x - 5 * ps, y - 4 * ps, x + Canvas::textWidth(title, fs) + 5 * ps, y + fs + 4 * ps,
                    IM_COL32(0, 0, 0, 165));
        cv.text(x, y, title, fs, IM_COL32(255, 255, 255, 255));
    }
    if (o.legend) {
        std::vector<Legend> legends; // bottom up: the active layer, then the result drawn on top
        const SeriesLayer* A = activeLayer();
        if (A && A->visible && mode_ != ModeRGB) {
            Legend L;
            L.caption = A->name;
            if (const LayerClasses* C = activeClasses()) {
                for (const ClassEntry& e : C->list)
                    if (e.visible) L.classes.emplace_back(ImGui::ColorConvertFloat4ToU32(e.color), e.name);
            } else {
                L.caption += std::string(" - ") + modeName(mode_) + (mode_ == ModeSlope ? " (" + slopeUnit() + ")" : "");
                L.cmap = cmap_[mode_];
                L.lo = range_[mode_].lo;
                L.hi = range_[mode_].hi;
            }
            legends.push_back(std::move(L));
        }
        const ResultLayer* top = nullptr; // as on the map: the last visible result of a visible layer
        for (const SeriesLayer& Ly : layers_)
            if (Ly.visible && Ly.aligned)
                for (const ResultLayer& R : results_)
                    if (R.visible && R.cubeId == Ly.session->info->id) top = &R;
        if (top) {
            Legend L;
            L.caption = top->name;
            if (!top->classes.empty()) {
                const int n = ImPlot::GetColormapSize(top->cmap);
                for (size_t k = 0; k < top->classes.size(); ++k)
                    L.classes.emplace_back(ImGui::ColorConvertFloat4ToU32(ImPlot::GetColormapColor(int(k) % n, top->cmap)),
                                           top->classes[k]);
            } else {
                L.cmap = top->cmap;
                L.lo = top->lo;
                L.hi = top->hi;
            }
            legends.push_back(std::move(L));
        }
        drawLegends(cv, legends, ps);
    }
    char title[160];
    std::snprintf(title, sizeof(title), "Map as PNG, %d x %d px", w, h);
    auto job = startJob(title, path, [img, w, h, transparent, path](ExportJob& j, std::string& error) {
        char** opts = CSLSetNameValue(nullptr, "ZLEVEL", "6");
        const bool ok = writeImage(path, "PNG", *img, w, h, transparent, nullptr, "", opts, j, error);
        CSLDestroy(opts);
        return ok;
    });
    exports_.push_back(job);
    showExports_ = true;
    return job;
}

std::shared_ptr<ExportJob> App::exportValues(const std::string& path, bool wholeImage) {
    std::shared_ptr<const CubeInfo> info = s_->info;
    int win[4] = {0, 0, info->width, info->height};
    if (!wholeImage && !visibleWindow(win)) return nullptr;
    const int t = t_;
    char title[200];
    std::snprintf(title, sizeof(title), "%s at %s, %d x %d px (Float32)", activeLayer()->name.c_str(),
                  info->layers[t].label.c_str(), win[2] - win[0], win[3] - win[1]);
    auto job = startJob(title, path, [info, t, win, path](ExportJob& j, std::string& error) {
        const int x0 = win[0], y0 = win[1], w = win[2] - win[0], h = win[3] - win[1];
        GDALDriver* drv = GetGDALDriverManager()->GetDriverByName("GTiff");
        if (!drv) {
            error = "GDAL has no GTiff driver";
            return false;
        }
        char** opts = nullptr;
        opts = CSLSetNameValue(opts, "TILED", "YES");
        opts = CSLSetNameValue(opts, "COMPRESS", "DEFLATE");
        opts = CSLSetNameValue(opts, "PREDICTOR", "3");
        opts = CSLSetNameValue(opts, "BIGTIFF", "IF_SAFER");
        opts = CSLSetNameValue(opts, "NUM_THREADS", "ALL_CPUS");
        const std::string tmp = path + ".part";
        CPLErrorReset();
        GDALDataset* out = drv->Create(tmp.c_str(), w, h, 1, GDT_Float32, opts);
        CSLDestroy(opts);
        if (!out) {
            error = gdalError("could not create the file");
            return false;
        }
        if (info->hasGeoTransform) {
            const auto& g = info->geoTransform;
            double gt[6] = {g[0] + x0 * g[1] + y0 * g[2], g[1], g[2], g[3] + x0 * g[4] + y0 * g[5], g[4], g[5]};
            out->SetGeoTransform(gt);
        }
        const std::string wkt = fileWkt(info->layers[t].path);
        if (!wkt.empty()) out->SetProjection(wkt.c_str());
        GDALRasterBand* band = out->GetRasterBand(1);
        band->SetNoDataValue(NAN);
        const std::string what = info->selectionText();
        band->SetDescription((info->layers[t].label + (what.empty() ? "" : " " + what)).c_str());
        std::error_code ec;
        out->SetMetadataItem("JANUS_DATE", info->layers[t].label.c_str());
        if (!what.empty()) out->SetMetadataItem("JANUS_QUANTITY", what.c_str());
        out->SetMetadataItem("JANUS_SOURCE", fs::absolute(fs::u8path(info->layers[t].path), ec).u8string().c_str());

        // Strips of whole tiles, read with a reader of our own (GDAL handles are per thread).
        CubeReader reader(info);
        const int strip = 256;
        std::vector<float> buf(size_t(w) * strip);
        bool ok = true;
        for (int r = 0; r < h && ok && !j.cancel; r += strip) {
            const int n = std::min(strip, h - r);
            if (!reader.readWindow(t, x0, y0 + r, w, n, buf.data(), w, n)) {
                error = gdalError("could not read " + info->layers[t].path);
                ok = false;
            } else if (band->RasterIO(GF_Write, 0, r, w, n, buf.data(), w, n, GDT_Float32, 0, 0, nullptr) != CE_None) {
                error = gdalError("could not write the file");
                ok = false;
            }
            setProgress(j, double(r + n) / h);
        }
        CPLErrorReset();
        GDALClose(out);
        if (ok && CPLGetLastErrorType() >= CE_Failure) {
            error = gdalError("could not write the file");
            ok = false;
        }
        return finishFile(tmp, path, ok && !j.cancel, error);
    });
    exports_.push_back(job);
    showExports_ = true;
    return job;
}

std::shared_ptr<ExportJob> App::exportView(const std::string& path, int scale) {
    auto img = std::make_shared<std::vector<unsigned char>>();
    int w = 0, h = 0;
    renderExport(scale, true, nullptr, *img, w, h);
    // Target pixel (px, py) -> active layer pixel ((px / ps - offset) / scale_).
    const CubeInfo& info = *s_->info;
    const double ps = double(mapPixelScale_) * scale, k = 1.0 / (scale_ * ps);
    const double ax = -offset_.x / scale_, ay = -offset_.y / scale_;
    const auto& g = info.geoTransform;
    std::array<double, 6> gt = {g[0] + ax * g[1] + ay * g[2], g[1] * k, g[2] * k,
                                g[3] + ax * g[4] + ay * g[5], g[4] * k, g[5] * k};
    const bool geo = info.hasGeoTransform;
    const std::string src = info.layers[t_].path;
    char title[160];
    std::snprintf(title, sizeof(title), "Rendered view as GeoTIFF, %d x %d px (RGBA)", w, h);
    auto job = startJob(title, path, [img, w, h, gt, geo, src, path](ExportJob& j, std::string& error) {
        char** opts = nullptr;
        opts = CSLSetNameValue(opts, "TILED", "YES");
        opts = CSLSetNameValue(opts, "COMPRESS", "DEFLATE");
        opts = CSLSetNameValue(opts, "PHOTOMETRIC", "RGB");
        opts = CSLSetNameValue(opts, "ALPHA", "UNASSOCIATED");
        opts = CSLSetNameValue(opts, "BIGTIFF", "IF_SAFER");
        const bool ok = writeImage(path, "GTiff", *img, w, h, true, geo ? gt.data() : nullptr, geo ? fileWkt(src) : "",
                                   opts, j, error);
        CSLDestroy(opts);
        return ok;
    });
    exports_.push_back(job);
    showExports_ = true;
    return job;
}

std::shared_ptr<ExportJob> App::exportResult(const ResultLayer& r, const std::string& path) {
    // Results are GeoTIFFs already (Float32, georeferenced, nodata NaN, written by
    // the bridge at full resolution): a copy, in chunks for the progress.
    const std::string src = r.path;
    auto job = startJob(r.name + " as GeoTIFF", path, [src, path](ExportJob& j, std::string& error) {
        std::error_code ec;
        if (fs::equivalent(fs::u8path(src), fs::u8path(path), ec)) return true;
        const uintmax_t total = std::max<uintmax_t>(1, fs::file_size(fs::u8path(src), ec));
        const std::string tmp = path + ".part";
        bool ok = true;
        {
            std::ifstream in(fs::u8path(src), std::ios::binary);
            std::ofstream out(fs::u8path(tmp), std::ios::binary | std::ios::trunc);
            if (!in || !out) {
                error = "could not open " + std::string(!in ? src : tmp);
                ok = false;
            }
            std::vector<char> buf(4 << 20);
            uintmax_t done = 0;
            while (ok && !j.cancel && in) {
                in.read(buf.data(), std::streamsize(buf.size()));
                const std::streamsize n = in.gcount();
                if (n <= 0) break;
                if (!out.write(buf.data(), n)) {
                    error = "could not write " + tmp;
                    ok = false;
                }
                done += uintmax_t(n);
                setProgress(j, double(done) / double(total));
            }
        }
        if (!finishFile(tmp, path, ok && !j.cancel, error)) return false;
        // A side file with statistics or metadata, if any, goes along.
        if (fs::exists(fs::u8path(src + ".aux.xml"), ec))
            fs::copy_file(fs::u8path(src + ".aux.xml"), fs::u8path(path + ".aux.xml"),
                          fs::copy_options::overwrite_existing, ec);
        return true;
    });
    exports_.push_back(job);
    showExports_ = true;
    return job;
}

// ---------------------------------------------------------------------------
// Interface
// ---------------------------------------------------------------------------

void App::uiExportMenu() {
    if (ImGui::MenuItem("Export map as PNG...", nullptr, false, s_ != nullptr)) {
        exportKind_ = 1;
        exportPopup_ = true;
    }
    if (ImGui::MenuItem("Export values as GeoTIFF...", nullptr, false, s_ != nullptr)) {
        exportKind_ = 2;
        exportPopup_ = true;
    }
    if (ImGui::MenuItem("Export rendered view as GeoTIFF...", nullptr, false, s_ && s_->info->hasGeoTransform)) {
        exportKind_ = 3;
        exportPopup_ = true;
    }
    if (ImGui::BeginMenu("Export Zeit result as GeoTIFF", !results_.empty())) {
        for (const ResultLayer& r : results_) {
            ImGui::PushID(&r);
            uiResultExportMenu(r, r.name.c_str());
            ImGui::PopID();
        }
        ImGui::EndMenu();
    }
    ImGui::MenuItem("Exports", nullptr, &showExports_);
}

void App::uiResultExportMenu(const ResultLayer& r, const char* label) {
    if (!ImGui::MenuItem(label)) return;
    const std::string name = sanitize(fs::u8path(r.path).stem().u8string()) + ".tif";
    const std::string path = askSavePath("Save the result as GeoTIFF", name, "GeoTIFF", "tif");
    if (!path.empty()) exportResult(r, path);
}

void App::uiExport() {
    if (exportPopup_) {
        ImGui::OpenPopup("Export###export");
        exportPopup_ = false;
    }
    static const char* kTitles[] = {"", "Export map as PNG", "Export values as GeoTIFF",
                                    "Export rendered view as GeoTIFF"};
    const std::string name = std::string(kTitles[std::clamp(exportKind_, 0, 3)]) + "###export";
    if (!ImGui::BeginPopupModal(name.c_str(), nullptr, ImGuiWindowFlags_AlwaysAutoResize)) return;
    if (!s_ || exportKind_ < 1 || exportKind_ > 3) {
        ImGui::CloseCurrentPopup();
        ImGui::EndPopup();
        return;
    }
    const CubeInfo& info = *s_->info;
    // Scale of a rendered export: 1x, 2x, 4x the map's resolution on screen.
    auto scaleChoice = [&](int& scale) {
        ImGui::TextUnformatted("Resolution (rendered at it, not enlarged):");
        for (int s : {1, 2, 4}) {
            char label[64];
            std::snprintf(label, sizeof(label), "%dx  (%ld x %ld px)", s, std::lround(canvasSize_.x * mapPixelScale_ * s),
                          std::lround(canvasSize_.y * mapPixelScale_ * s));
            const bool fits = exportScaleFits(s);
            if (!fits && scale == s) scale = 1;
            ImGui::BeginDisabled(!fits);
            ImGui::RadioButton(label, &scale, s);
            ImGui::EndDisabled();
            if (!fits) ImGui::SetItemTooltip("Too large for the GPU");
        }
        if (detailLevel_ >= 0 || mode_ != ModeValue)
            ImGui::TextDisabled("Data drawn as on screen: overview or the detail tiles already read.");
    };
    bool save = false, canSave = true;
    if (exportKind_ == 1) {
        scaleChoice(png_.scale);
        ImGui::Spacing();
        ImGui::SetNextItemWidth(200);
        ImGui::Combo("Background", &png_.background, "Map's (dark)\0White\0Transparent\0");
        ImGui::Checkbox("Date and mode label", &png_.label);
        ImGui::Checkbox("Legend (colour bar or classes)", &png_.legend);
        ImGui::Checkbox("Pins and ROI", &png_.marks);
    } else if (exportKind_ == 2) {
        const SeriesLayer* L = activeLayer();
        ImGui::TextColored(ImVec4(0.55f, 0.80f, 1.0f, 1), "%s", L ? L->name.c_str() : "");
        const std::string what = info.selectionText();
        ImGui::Text("Date %s%s%s", info.layers[t_].label.c_str(), what.empty() ? "" : ", ", what.c_str());
        int win[4];
        const bool visible = visibleWindow(win);
        char label[96];
        std::snprintf(label, sizeof(label), "Visible area  (%d x %d px)", visible ? win[2] - win[0] : 0,
                      visible ? win[3] - win[1] : 0);
        ImGui::BeginDisabled(!visible);
        if (ImGui::RadioButton(label, !valuesWhole_)) valuesWhole_ = false;
        ImGui::EndDisabled();
        if (!visible) valuesWhole_ = true;
        std::snprintf(label, sizeof(label), "Whole image  (%d x %d px)", info.width, info.height);
        if (ImGui::RadioButton(label, valuesWhole_)) valuesWhole_ = true;
        const double px = valuesWhole_ ? double(info.width) * info.height : double(win[2] - win[0]) * (win[3] - win[1]);
        ImGui::TextDisabled("Full resolution, Float32, no data = NaN, %.0f MB before compression.", px * 4 / 1e6);
        ImGui::TextDisabled(info.hasGeoTransform ? "Georeferenced like the layer (same grid and CRS)."
                                                 : "The layer has no georeferencing: pixel coordinates only.");
        if (mode_ != ModeValue)
            ImGui::TextColored(ImVec4(1.0f, 0.7f, 0.4f, 1), "The map shows %s; the file holds the values at the date.",
                               modeName(mode_));
        if (s_->deferRandomReads()) { // as pins and tiles: the HDD's head stays on the overview
            ImGui::TextColored(ImVec4(1.0f, 0.7f, 0.4f, 1), "Building the overview from an HDD: available once it ends.");
            canSave = false;
        }
    } else {
        scaleChoice(viewScale_);
        ImGui::TextDisabled("RGB + alpha (transparent where nothing is drawn), as shown,\n"
                            "georeferenced in the active layer's CRS.");
    }
    ImGui::Spacing();
    ImGui::BeginDisabled(!canSave);
    if (ImGui::Button("Save...", ImVec2(120, 0))) save = true;
    ImGui::EndDisabled();
    ImGui::SameLine();
    if (ImGui::Button("Cancel", ImVec2(120, 0)) || ImGui::IsKeyPressed(ImGuiKey_Escape, false))
        ImGui::CloseCurrentPopup();
    if (save) {
        ImGui::CloseCurrentPopup();
        if (exportKind_ == 1) {
            const std::string path = askSavePath("Export map as PNG", exportName(".png"), "PNG image", "png");
            if (!path.empty()) exportPng(path, png_);
        } else if (exportKind_ == 2) {
            const std::string path = askSavePath("Export values as GeoTIFF", exportName(".tif"), "GeoTIFF", "tif");
            if (!path.empty()) exportValues(path, valuesWhole_);
        } else {
            const std::string path = askSavePath("Export rendered view as GeoTIFF", exportName("_view.tif"), "GeoTIFF", "tif");
            if (!path.empty()) exportView(path, viewScale_);
        }
    }
    ImGui::EndPopup();
}

void App::uiExports() {
    if (!showExports_) return;
    ImGui::SetNextWindowSize(ImVec2(600, 260), ImGuiCond_FirstUseEver);
    if (!ImGui::Begin("Exports", &showExports_)) {
        ImGui::End();
        return;
    }
    if (exports_.empty()) ImGui::TextDisabled("Nothing exported yet (File > Export ...).");
    int remove = -1;
    for (size_t i = 0; i < exports_.size(); ++i) {
        ExportJob& j = *exports_[i];
        ImGui::PushID(int(i));
        const ExportJob::State st = j.state;
        ImGui::TextUnformatted(j.title.c_str());
        ImGui::TextDisabled("%s", j.path.c_str());
        const char* stName = st == ExportJob::State::Running ? "writing" : st == ExportJob::State::Done ? "done"
                             : st == ExportJob::State::Cancelled ? "cancelled" : "failed";
        char overlay[96];
        std::snprintf(overlay, sizeof(overlay), "%s  %.0f%%  %.1f s", stName, j.progress * 100.0,
                      st == ExportJob::State::Running ? secondsSince(j.started) : j.seconds);
        ImGui::ProgressBar(st == ExportJob::State::Done ? 1.0f : float(j.progress.load()), ImVec2(-160, 0), overlay);
        ImGui::SameLine();
        if (st == ExportJob::State::Running) {
            if (ImGui::Button("Cancel", ImVec2(-1, 0))) j.cancel = true;
        } else {
            if (ImGui::Button("Folder", ImVec2(75, 0)))
                platform::openInExplorer(fs::u8path(j.path).parent_path().u8string());
            ImGui::SameLine();
            if (ImGui::Button("Remove", ImVec2(-1, 0))) remove = int(i);
        }
        if (st == ExportJob::State::Failed) {
            std::lock_guard<std::mutex> lk(j.m);
            ImGui::PushTextWrapPos(0);
            ImGui::TextColored(ImVec4(1.0f, 0.55f, 0.45f, 1), "%s", j.error.c_str());
            ImGui::PopTextWrapPos();
        }
        ImGui::Separator();
        ImGui::PopID();
    }
    if (remove >= 0) exports_.erase(exports_.begin() + remove);
    ImGui::End();
}
