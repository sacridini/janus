// `tsv --selftest-ui A B`: drives the layers workflow in a hidden window, one
// step per frame, without touching the mouse or keyboard. Opens A, adds B as a
// layer, checks the georeferenced alignment, the series of both layers under a
// cursor and a pin, reads back map pixels (layer drawn, layer hidden), switches
// the active layer and closes one.
#include "app.hpp"

#include <chrono>
#include <cmath>
#include <cstdio>

namespace {

double now() {
    return std::chrono::duration<double>(std::chrono::steady_clock::now().time_since_epoch()).count();
}

} // namespace

// Map pixel color at (x, y) of a w x h render (y from the top).
static void readMapPixel(GLuint tex, int w, int h, int x, int y, unsigned char rgba[4]) {
    std::vector<unsigned char> buf(size_t(w) * h * 4);
    glBindTexture(GL_TEXTURE_2D, tex);
    glPixelStorei(GL_PACK_ALIGNMENT, 1);
    glGetTexImage(GL_TEXTURE_2D, 0, GL_RGBA, GL_UNSIGNED_BYTE, buf.data());
    const int row = h - 1 - y; // the framebuffer is bottom-up
    for (int c = 0; c < 4; ++c) rgba[c] = buf[(size_t(row) * w + x) * 4 + c];
}

int App::selfTestStep(const std::vector<std::string>& a, const std::vector<std::string>& b) {
    auto fail = [&](const char* what) {
        std::printf("FAIL (stage %d): %s\n", st_.stage, what);
        return 1;
    };
    auto next = [&](const char* done) {
        std::printf("[%5.1f s] %s\n", now() - st_.t0, done);
        ++st_.stage;
        st_.since = now();
    };
    auto loaded = [&](const SeriesLayer& L) {
        return L.session->overview.complete() && (L.session->info->T() < 2 || L.session->gpu.statsValid);
    };
    if (st_.stage > 0 && now() - st_.since > 120) return fail("timeout");

    switch (st_.stage) {
    case 0:
        st_.t0 = st_.since = now();
        openInputs(a);
        next("open A");
        break;
    case 1:
        if (layers_.size() == 1 && loaded(layers_[0])) next("A loaded");
        break;
    case 2:
        openInputs(b, true);
        next("add B as a layer");
        break;
    case 3:
        if (layers_.size() == 2 && loaded(layers_[1])) {
            if (active_ != 1) return fail("the added layer should be active");
            const SeriesLayer& A = layers_[0];
            std::printf("    layer A in B's grid: x' = %.3f + %.3f x, y' = %.3f + %.3f y (aligned %d)\n", A.ax, A.bx,
                        A.ay, A.by, int(A.aligned));
            if (!A.aligned) return fail("A should be aligned with B (same CRS)");
            next("B loaded, alignment checked");
        }
        break;
    case 4: {
        // Cursor and a pin at the same place, in the overlap of both layers.
        chartLayers_ = 1;
        const int ix = 50, iy = 100; // B's pixel 50 = A's pixel 150 (B starts 100 px east)
        hover_ = SeriesView{};
        hover_.x = ix;
        hover_.y = iy;
        hover_.values = approxSeries(ix, iy);
        hover_.color = ImVec4(1, 1, 1, 1);
        hover_.request = s_->requestSeries(ix, iy, true);
        updateOtherHover(ix, iy);
        requestOtherSeries(true);
        addPin(ix, iy);
        addOtherPins(pins_.back());
        next("cursor and pin placed");
        break;
    }
    case 5: {
        const SeriesLayer& A = layers_[0];
        const bool ready = hover_.exact && A.hover.exact && !pins_.empty() && pins_[0].exact && !A.pins.empty() &&
                           A.pins[0].exact;
        if (!ready) break;
        std::printf("    cursor: B(%d,%d) = %.3f at %s | A(%d,%d) = %.3f at %s\n", hover_.x, hover_.y,
                    hover_.values[0], s_->info->layers[0].label.c_str(), A.hover.x, A.hover.y, A.hover.values[0],
                    A.session->info->layers[0].label.c_str());
        if (A.hover.x != hover_.x + 100) return fail("A's cursor pixel should be B's + 100");
        if (std::fabs(hover_.values[0] - 2.0f) > 1e-4) return fail("B's first value should be 2.0");
        next("exact series of both layers under the cursor and the pin");
        break;
    }
    case 6: {
        canvasSize_ = ImVec2(400, 300);
        fitView(canvasSize_);
        renderMap(400, 300);
        unsigned char c[4];
        readMapPixel(gpu_.mapTexture(), 400, 300, 200, 150, c);
        std::printf("    map center with both layers: rgb(%d, %d, %d)\n", c[0], c[1], c[2]);
        st_.color[0] = c[0];
        st_.color[1] = c[1];
        st_.color[2] = c[2];
        layers_[1].visible = false; // hide the active layer: A must show through
        renderMap(400, 300);
        readMapPixel(gpu_.mapTexture(), 400, 300, 200, 150, c);
        std::printf("    map center with B hidden:    rgb(%d, %d, %d)\n", c[0], c[1], c[2]);
        if (c[0] == st_.color[0] && c[1] == st_.color[1] && c[2] == st_.color[2])
            return fail("hiding the top layer did not change the map");
        layers_[0].visible = false;
        renderMap(400, 300);
        readMapPixel(gpu_.mapTexture(), 400, 300, 200, 150, c);
        std::printf("    map center with both hidden: rgb(%d, %d, %d)\n", c[0], c[1], c[2]);
        if (c[0] > 40 || c[1] > 40 || c[2] > 40) return fail("with every layer hidden the map should be empty");
        layers_[0].visible = layers_[1].visible = true;
        next("layer drawing and visibility");
        break;
    }
    case 7: {
        const int pinBefore = pins_[0].x;
        setActive(0);
        if (s_ != layers_[0].session.get()) return fail("A should be active");
        std::printf("    pin x: %d in B's grid -> %d in A's grid; B in A's grid: x' = %.1f + %.1f x\n", pinBefore,
                    pins_.empty() ? -1 : pins_[0].x, layers_[1].ax, layers_[1].bx);
        if (pins_.empty() || pins_[0].x != pinBefore + 100) return fail("the pin should move to A's grid (+100)");
        next("active layer switched, pin remapped");
        break;
    }
    case 8:
        removeLayer(1);
        if (layers_.size() != 1 || active_ != 0) return fail("closing B should leave A active");
        next("layer closed");
        std::printf("OK (%.1f s)\n", now() - st_.t0);
        return 0;
    }
    return -1;
}
