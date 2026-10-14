// The ROI's shape: a rectangle (Shift + drag) or a polygon (R, then clicks for
// vertices or a freehand drag), kept in the active layer's pixels with its
// bounding box. The active layer reads the bounding box and keeps the pixels
// whose centres are inside (Session::startRoi); every other user of the ROI
// (other layers, embeddings, scatter) asks roiContains / roiOnLayer.
#define IMGUI_DEFINE_MATH_OPERATORS
#include "app.hpp"

#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdio>
#include <filesystem>
#include <iterator>

#include <gdal_priv.h>
#include <imgui_internal.h>

namespace {

const ImU32 kRoiCol = IM_COL32(255, 210, 60, 255);

} // namespace

void App::setRoiShape(const int r[4], const Polygon& poly) {
    clearRoi();
    setRoiRect(r); // the bounding box, for every layer (updateOtherRois)
    roiPoly_ = poly;
    if (s_->deferRandomReads()) { // HDD still building: start when done
        std::copy(roiRectXY_, roiRectXY_ + 4, pendingRoiRect_);
        pendingRoi_ = true;
    } else {
        roi_ = s_->startRoi(roiRectXY_[0], roiRectXY_[1], roiRectXY_[2], roiRectXY_[3], roiPoly_);
        roiSeen_ = -1;
    }
}

bool App::setRoiPolygon(const Polygon& poly) {
    double b[4];
    if (!s_ || !polygonBounds(poly, b)) return false;
    const CubeInfo& info = *s_->info;
    const int r[4] = {std::clamp(int(std::floor(b[0])), 0, info.width), std::clamp(int(std::floor(b[1])), 0, info.height),
                      std::clamp(int(std::ceil(b[2])), 0, info.width), std::clamp(int(std::ceil(b[3])), 0, info.height)};
    if (r[2] <= r[0] || r[3] <= r[1]) return false; // off the image
    // At least one pixel's centre inside (a sliver between centres has no statistics).
    const int w = r[2] - r[0], h = r[3] - r[1];
    if (double(w) * h <= 16e6) {
        const std::vector<char> m = polygonMask(poly, r[0], r[1], w, h, w, h);
        if (std::find(m.begin(), m.end(), 1) == m.end()) return false;
    }
    setRoiShape(r, poly);
    return true;
}

bool App::roiContains(double x, double y) const {
    if (!roiRect_) return true;
    if (x < roiRectXY_[0] || y < roiRectXY_[1] || x >= roiRectXY_[2] || y >= roiRectXY_[3]) return false;
    return roiPoly_.empty() || pointInPolygon(roiPoly_, x, y);
}

Polygon App::roiOnLayer(const SeriesLayer& L) const {
    if (&L == activeLayer()) return roiPoly_;
    Polygon p = roiPoly_;
    if (p.empty()) {
        if (!L.reproj) return {}; // straight: the rectangle maps to a rectangle
        const double x0 = roiRectXY_[0], y0 = roiRectXY_[1], x1 = roiRectXY_[2], y1 = roiRectXY_[3];
        p = {Ring{x0, y0, x1, y0, x1, y1, x0, y1}};
    }
    if (L.reproj) {
        // Edges become curves in another CRS: about 16 points along each side of the box.
        double b[4];
        polygonBounds(p, b);
        p = densifyPolygon(p, std::max(1.0, std::max(b[2] - b[0], b[3] - b[1]) / 16));
        transformPolygon(p, [&](double& x, double& y) { return L.reproj->toLayer(x, y, x, y); });
    } else {
        transformPolygon(p, [&](double& x, double& y) { return activeToLayer(L, x, y, x, y); });
    }
    return p;
}

// ---------------------------------------------------------------------------
// Drawing a polygon
// ---------------------------------------------------------------------------

bool App::polygonInput(int panel, double sx, double sy) {
    if (!polyMode_) return false;
    const ImGuiIO& io = ImGui::GetIO();
    const ImVec2 p{float(sx), float(sy)};
    if (ImGui::IsItemActivated()) {
        if (io.KeyShift) return false; // a rectangle, as ever
        polyPanel_ = panel;
        polyPress_ = p;
        polyLasso_ = false;
        polyDouble_ = io.MouseClickedCount[ImGuiMouseButton_Left] == 2;
        return true;
    }
    if (polyPanel_ != panel) return false;
    if (ImGui::IsItemActive()) {
        if (!polyLasso_ && ImGui::IsMouseDragging(ImGuiMouseButton_Left, 4.0f)) {
            polyLasso_ = true;
            polyDraft_.push_back(polyPress_); // the stroke starts where the button went down
        }
        if (polyLasso_) {
            const ImVec2 d = (p - polyDraft_.back()) * float(scale_);
            if (d.x * d.x + d.y * d.y >= 9) polyDraft_.push_back(p); // a vertex every 3 screen points
        }
        return true;
    }
    // Released: a stroke closes at once; a click closes on the first vertex or a
    // double click, else adds a vertex.
    polyPanel_ = -1;
    if (polyLasso_) {
        polyDraft_.push_back(p);
        finishPolygon();
        return true;
    }
    if (polyDraft_.size() >= 3) {
        const ImVec2 d = (p - polyDraft_.front()) * float(scale_);
        if (polyDouble_ || d.x * d.x + d.y * d.y <= 64) {
            finishPolygon();
            return true;
        }
    }
    if (!polyDouble_) polyDraft_.push_back(p);
    return true;
}

void App::finishPolygon() {
    Ring r;
    for (const ImVec2& v : polyDraft_) // without repeated vertices (a double click is two clicks)
        if (r.empty() || std::fabs(r[r.size() - 2] - v.x) * scale_ > 1 || std::fabs(r.back() - v.y) * scale_ > 1) {
            r.push_back(v.x);
            r.push_back(v.y);
        }
    polyDraft_.clear();
    if (r.size() >= 6 && setRoiPolygon({r})) polyMode_ = false;
}

void App::roiShortcuts() {
    if (ImGui::IsKeyChordPressed(ImGuiKey_R)) {
        polyMode_ = !polyMode_;
        polyDraft_.clear();
        polyPanel_ = -1;
    }
    if (!polyMode_) return;
    if (ImGui::IsKeyPressed(ImGuiKey_Enter, false) || ImGui::IsKeyPressed(ImGuiKey_KeypadEnter, false)) finishPolygon();
    if ((ImGui::IsKeyPressed(ImGuiKey_Backspace) || ImGui::IsKeyPressed(ImGuiKey_Delete)) && !polyDraft_.empty())
        polyDraft_.pop_back();
    if (ImGui::IsKeyPressed(ImGuiKey_Escape, false)) {
        polyMode_ = false;
        polyDraft_.clear();
        polyPanel_ = -1;
    }
}

void App::drawRoiMarks(ImDrawList* dl, ImVec2 origin) {
    auto toScreen = [&](double x, double y) {
        return origin + ImVec2(float(offset_.x + x * scale_), float(offset_.y + y * scale_));
    };
    // The ROI's outline (a dark line under it: visible over any colormap).
    auto outline = [&](const int r[4], float thick) {
        if (roiPoly_.empty()) {
            const ImVec2 a = toScreen(r[0], r[1]), b = toScreen(r[2], r[3]);
            dl->AddRect(a, b, IM_COL32(0, 0, 0, 160), 0.0f, ImDrawFlags_None, thick + 2);
            dl->AddRect(a, b, kRoiCol, 0.0f, ImDrawFlags_None, thick);
            return;
        }
        std::vector<ImVec2> pts;
        for (const Ring& ring : roiPoly_) {
            pts.clear();
            for (size_t i = 0; i + 1 < ring.size(); i += 2) pts.push_back(toScreen(ring[i], ring[i + 1]));
            dl->AddPolyline(pts.data(), int(pts.size()), IM_COL32(0, 0, 0, 160), ImDrawFlags_Closed, thick + 2);
            dl->AddPolyline(pts.data(), int(pts.size()), kRoiCol, ImDrawFlags_Closed, thick);
        }
    };
    const int T = s_->info->T();
    if (roiDragging_) {
        dl->AddRect(toScreen(roiStart_.x, roiStart_.y), toScreen(roiEnd_.x, roiEnd_.y), kRoiCol, 0.0f, ImDrawFlags_None,
                    2.0f);
    } else if (pendingRoi_) {
        outline(pendingRoiRect_, 1.0f);
        dl->AddText(toScreen(pendingRoiRect_[0], pendingRoiRect_[1]) + ImVec2(4, 2), kRoiCol,
                    "ROI: waiting for the overview (HDD)");
    } else if (roi_) {
        outline(roiRectXY_, 2.0f);
        if (roi_->done < T) {
            char buf[64];
            std::snprintf(buf, sizeof(buf), "ROI %d/%d", roi_->done.load(), T);
            dl->AddText(toScreen(roi_->x0, roi_->y0) + ImVec2(4, 2), kRoiCol, buf);
        }
    }
    if (!polyMode_) return;
    // The polygon being drawn: its vertices, and the edge that follows the cursor.
    const bool hovered = ImGui::IsItemHovered();
    std::vector<ImVec2> pts;
    for (const ImVec2& v : polyDraft_) pts.push_back(toScreen(v.x, v.y));
    if (hovered && !polyLasso_ && !pts.empty()) pts.push_back(ImGui::GetIO().MousePos);
    if (pts.size() >= 2) {
        dl->AddPolyline(pts.data(), int(pts.size()), IM_COL32(0, 0, 0, 160), ImDrawFlags_None, 4.0f);
        dl->AddPolyline(pts.data(), int(pts.size()), kRoiCol, ImDrawFlags_None, 2.0f);
    }
    if (!polyLasso_)
        for (size_t i = 0; i < polyDraft_.size(); ++i) {
            const ImVec2 c = toScreen(polyDraft_[i].x, polyDraft_[i].y);
            const float r = i == 0 && polyDraft_.size() >= 3 ? 6.0f : 3.5f; // the first closes it
            dl->AddCircleFilled(c, r, kRoiCol);
            dl->AddCircle(c, r, IM_COL32(0, 0, 0, 220), 0, 1.5f);
        }
    if (hovered) {
        const char* hint = polyDraft_.empty() ? "ROI polygon: click vertices or drag a freehand outline (Esc cancels)"
                           : polyDraft_.size() < 3 ? "click: next vertex, Backspace: undo, Esc: cancel"
                                                   : "click the first vertex, double click or Enter: close";
        const ImVec2 p = ImGui::GetIO().MousePos + ImVec2(16, 12), ts = ImGui::CalcTextSize(hint);
        dl->AddRectFilled(p - ImVec2(4, 2), p + ts + ImVec2(4, 2), IM_COL32(0, 0, 0, 190), 3.0f);
        dl->AddText(p, kRoiCol, hint);
    }
}

// ---------------------------------------------------------------------------
// --selftest-ui stages 110-115: a concave polygon drawn on B (vertices clicked,
// the last one twice as a double click does), its statistics on B, on A (same
// CRS, shifted) and on F (another CRS) against the pixels whose centres are
// inside; the outline in a PNG export.
// ---------------------------------------------------------------------------

int App::selfTestRoiPolygon(const std::vector<std::string>& in) {
    namespace fs = std::filesystem;
    auto now = [] { return std::chrono::duration<double>(std::chrono::steady_clock::now().time_since_epoch()).count(); };
    auto fail = [&](const char* what) {
        std::printf("FAIL (stage %d): %s\n", st_.stage, what);
        return 1;
    };
    auto next = [&](const char* done) {
        std::printf("[%5.1f s] %s\n", now() - st_.t0, done);
        ++st_.stage;
        st_.since = now();
    };
    auto loaded = [&](size_t i) {
        return !opening_.valid() && layers_.size() > i && layers_[i].session->overview.complete() &&
               layers_[i].session->gpu.statsValid;
    };
    // B's pixels; A's are B's + 100 in x. Concave (an L), so its bounding box is not the ROI.
    static const double kPoly[] = {20, 30, 150, 30, 150, 80, 70.4, 80, 70.4, 170, 20, 170};
    switch (st_.stage) {
    case 110:
        openInputs({in[1]});
        next("polygonal ROI: open B");
        break;
    case 111:
        if (!loaded(0)) break;
        openInputs({in[0]}, true);
        next("add A");
        break;
    case 112:
        if (!loaded(1)) break;
        openInputs({in[5]}, true);
        next("add F (another CRS)");
        break;
    case 113: {
        if (!loaded(2)) break;
        setActive(0); // B
        chartLayers_ = 1;
        canvasSize_ = ImVec2(400, 300);
        fitView(canvasSize_);
        // The vertices as clicked; the last one twice (a double click's two clicks).
        polyMode_ = true;
        polyDraft_.clear();
        for (size_t i = 0; i < std::size(kPoly); i += 2) polyDraft_.push_back(ImVec2(float(kPoly[i]), float(kPoly[i + 1])));
        polyDraft_.push_back(polyDraft_.back());
        finishPolygon();
        if (polyMode_ || roiPoly_.size() != 1 || roiPoly_[0].size() != std::size(kPoly))
            return fail("closing the polygon should leave polygon mode with its 6 vertices as the ROI");
        if (roiRectXY_[0] != 20 || roiRectXY_[1] != 30 || roiRectXY_[2] != 150 || roiRectXY_[3] != 170)
            return fail("the ROI's bounding box should be the polygon's");
        const double area = polygonArea(roiPoly_);
        if (std::fabs(area - (130.0 * 50 + 50.4 * 90)) > 1e-3) return fail("polygonArea of the L");
        if (roiContains(100, 120) || !roiContains(30, 120) || !roiContains(100, 50))
            return fail("roiContains: inside the L, not in its bounding box's notch");
        next("polygon closed as the ROI of every layer");
        break;
    }
    case 114: {
        const SeriesLayer &B = layers_[0], &A = layers_[1], &F = layers_[2];
        const int T = s_->info->T();
        if (!roi_ || roi_->done.load() < T || !A.roi || A.roi->done.load() < A.session->info->T() || !F.roi ||
            F.roi->done.load() < F.session->info->T())
            break;
        // The active layer: every date's statistics from the pixels whose centres are inside.
        CubeReader rb(B.session->info);
        const int x0 = roi_->x0, y0 = roi_->y0, w = roi_->x1 - x0, h = roi_->y1 - y0;
        if (roi_->sampled || roi_->bw != w) return fail("the L should be read at full resolution");
        std::vector<float> buf(size_t(w) * h);
        size_t inside = 0;
        double worst = 0;
        for (int t = 0; t < T; ++t) {
            if (!rb.readWindow(t, x0, y0, w, h, buf.data(), w, h)) return fail("B cannot be read");
            std::vector<float> v;
            for (int j = 0; j < h; ++j)
                for (int i = 0; i < w; ++i)
                    if (pointInPolygon(roiPoly_, x0 + i + 0.5, y0 + j + 0.5)) v.push_back(buf[size_t(j) * w + i]);
            inside = v.size();
            const SampleStats e = computeSampleStats(v), &g = roi_->perT[t];
            if (e.n != g.n) return fail("the ROI's pixel count on B differs from the pixels inside");
            if (e.n) worst = std::max({worst, double(std::fabs(e.mean - g.mean)), double(std::fabs(e.p90 - g.p90))});
        }
        std::printf("    B: %zu of %d x %d pixels of the bounding box inside the L, max |mean, p90 - CPU| %.2g\n", inside,
                    w, h, worst);
        if (worst > 1e-5) return fail("the ROI's statistics on B differ from the CPU's");
        // A (B + 100 px) and F (UTM 22S): the cells of their masks against the centres inside.
        auto checkMask = [&](const SeriesLayer& L, const char* name, const std::function<bool(double, double)>& inside,
                             double tolerance) -> bool {
            const RoiData& r = *L.roi;
            size_t ones = 0, wrong = 0;
            for (int j = 0; j < r.bh; ++j)
                for (int i = 0; i < r.bw; ++i) {
                    const bool m = r.mask.empty() ? true : r.mask[size_t(j) * r.bw + i] != 0;
                    ones += m;
                    wrong += m != inside(r.x0 + i + 0.5, r.y0 + j + 0.5);
                }
            std::printf("    %s: %d x %d px read at (%d, %d), %zu inside, %zu differ from the exact test\n", name, r.bw,
                        r.bh, r.x0, r.y0, ones, wrong);
            return ones > 0 && double(wrong) <= tolerance * double(ones);
        };
        if (!checkMask(A, "A", [&](double x, double y) { return pointInPolygon(roiPoly_, x - 100, y); }, 0))
            return fail("A's ROI should be the L moved 100 px");
        // F: the L's straight edges are curves there (16 points per side of its box).
        if (!checkMask(F, "F", [&](double x, double y) {
                double bx, by;
                return F.reproj->toActive(x, y, bx, by) && pointInPolygon(roiPoly_, bx, by);
            }, 0.01))
            return fail("F's ROI should be the L reprojected");
        // The outline in a PNG with the marks.
        PngOptions o;
        o.marks = true;
        o.label = o.legend = false;
        const fs::path dir = fs::temp_directory_path() / "janus-selftest-roi";
        std::error_code ec;
        fs::create_directories(dir, ec);
        st_.jobs = {exportPng((dir / "roi.png").u8string(), o)};
        next("the L's statistics on B, A and F");
        break;
    }
    case 115: {
        auto& j = st_.jobs[0];
        j->done.wait();
        if (j->state != ExportJob::State::Done) return fail("the PNG with the ROI should be written");
        GDALDataset* ds = GDALDataset::Open(j->path.c_str(), GDAL_OF_RASTER | GDAL_OF_READONLY);
        if (!ds) return fail("the PNG cannot be opened");
        const int W = ds->GetRasterXSize(), H = ds->GetRasterYSize();
        std::vector<unsigned char> px(size_t(W) * H * 3);
        int bands[3] = {1, 2, 3};
        const bool read = ds->RasterIO(GF_Read, 0, 0, W, H, px.data(), W, H, GDT_Byte, 3, bands, 3, GSpacing(W) * 3, 1,
                                       nullptr) == CE_None;
        GDALClose(ds);
        if (!read) return fail("the PNG cannot be read");
        // Middle of the notch's two edges (yellow), and the notch itself (the map's colours).
        auto at = [&](double x, double y) {
            const int X = int((offset_.x + x * scale_) * mapPixelScale_), Y = int((offset_.y + y * scale_) * mapPixelScale_);
            return &px[(size_t(std::clamp(Y, 0, H - 1)) * W + std::clamp(X, 0, W - 1)) * 3];
        };
        auto yellow = [](const unsigned char* c) { return c[0] > 200 && c[1] > 160 && c[2] < 120; };
        if (!yellow(at(110, 80)) || !yellow(at(70.4, 125))) return fail("the PNG should show the L's edges in yellow");
        if (yellow(at(110, 125))) return fail("the notch of the L should not be outlined");
        std::error_code ec;
        fs::remove_all(fs::u8path(j->path).parent_path(), ec);
        st_.jobs.clear();
        clearRoi();
        next("the L's outline in the exported PNG");
        st_.stage = 120;
        break;
    }
    }
    return -1;
}
