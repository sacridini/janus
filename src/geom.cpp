#include "geom.hpp"

#include <algorithm>
#include <cmath>

bool polygonBounds(const Polygon& p, double box[4]) {
    box[0] = box[1] = HUGE_VAL;
    box[2] = box[3] = -HUGE_VAL;
    for (const Ring& r : p)
        for (size_t i = 0; i + 1 < r.size(); i += 2) {
            box[0] = std::min(box[0], r[i]);
            box[1] = std::min(box[1], r[i + 1]);
            box[2] = std::max(box[2], r[i]);
            box[3] = std::max(box[3], r[i + 1]);
        }
    return box[0] <= box[2];
}

static bool inRing(const Ring& r, double x, double y) {
    const size_t n = r.size() / 2;
    bool in = false;
    for (size_t i = 0, j = n - 1; i < n; j = i++) {
        const double xi = r[i * 2], yi = r[i * 2 + 1], xj = r[j * 2], yj = r[j * 2 + 1];
        if ((yi <= y) != (yj <= y) && x < xi + (y - yi) * (xj - xi) / (yj - yi)) in = !in;
    }
    return in;
}

bool pointInPolygon(const Polygon& p, double x, double y) {
    bool in = false;
    for (const Ring& r : p)
        if (r.size() >= 6 && inRing(r, x, y)) in = !in;
    return in;
}

static double ringArea(const Ring& r) {
    const size_t n = r.size() / 2;
    double a = 0;
    for (size_t i = 0, j = n - 1; i < n; j = i++) a += (r[j * 2] - r[i * 2]) * (r[j * 2 + 1] + r[i * 2 + 1]);
    return std::fabs(a) * 0.5;
}

double polygonArea(const Polygon& p) {
    double total = 0;
    for (size_t k = 0; k < p.size(); ++k) {
        if (p[k].size() < 6) continue;
        int depth = 0; // rings this one lies in (tested at its first vertex)
        for (size_t m = 0; m < p.size(); ++m)
            if (m != k && p[m].size() >= 6 && inRing(p[m], p[k][0], p[k][1])) ++depth;
        total += (depth % 2 == 0 ? 1 : -1) * ringArea(p[k]);
    }
    return total;
}

void transformPolygon(Polygon& p, const std::function<bool(double&, double&)>& f) {
    for (Ring& r : p) {
        Ring out;
        out.reserve(r.size());
        for (size_t i = 0; i + 1 < r.size(); i += 2) {
            double x = r[i], y = r[i + 1];
            if (!f(x, y)) continue;
            out.push_back(x);
            out.push_back(y);
        }
        r.swap(out);
    }
    p.erase(std::remove_if(p.begin(), p.end(), [](const Ring& r) { return r.size() < 6; }), p.end());
}

Polygon densifyPolygon(const Polygon& p, double maxLen) {
    Polygon out;
    for (const Ring& r : p) {
        const size_t n = r.size() / 2;
        Ring d;
        for (size_t i = 0; i < n; ++i) {
            const double ax = r[i * 2], ay = r[i * 2 + 1], bx = r[(i + 1) % n * 2], by = r[(i + 1) % n * 2 + 1];
            const int k = std::max(1, int(std::ceil(std::hypot(bx - ax, by - ay) / maxLen)));
            for (int s = 0; s < k; ++s) {
                d.push_back(ax + (bx - ax) * s / k);
                d.push_back(ay + (by - ay) * s / k);
            }
        }
        out.push_back(std::move(d));
    }
    return out;
}

std::vector<char> polygonMask(const Polygon& p, double x0, double y0, double w, double h, int bw, int bh) {
    std::vector<char> mask(size_t(bw) * bh, 0);
    std::vector<double> xs;
    for (int j = 0; j < bh; ++j) {
        const double sy = y0 + (j + 0.5) * h / bh;
        xs.clear();
        for (const Ring& r : p) {
            const size_t n = r.size() / 2;
            if (n < 3) continue;
            for (size_t k = 0; k < n; ++k) {
                const double ax = r[k * 2], ay = r[k * 2 + 1];
                const double bx = r[(k + 1) % n * 2], by = r[(k + 1) % n * 2 + 1];
                if ((ay <= sy) != (by <= sy)) xs.push_back(ax + (sy - ay) * (bx - ax) / (by - ay));
            }
        }
        std::sort(xs.begin(), xs.end());
        for (size_t k = 0; k + 1 < xs.size(); k += 2) {
            // Centres x0 + (i + 0.5) w / bw in [xs[k], xs[k + 1]).
            const int i0 = std::max(0, int(std::ceil((xs[k] - x0) * bw / w - 0.5)));
            const int i1 = std::min(bw, int(std::ceil((xs[k + 1] - x0) * bw / w - 0.5)));
            for (int i = i0; i < i1; ++i) mask[size_t(j) * bw + i] = 1;
        }
    }
    return mask;
}

static void douglasPeucker(const Ring& r, size_t a, size_t b, double tol2, std::vector<char>& keep) {
    const double ax = r[a * 2], ay = r[a * 2 + 1], bx = r[b * 2], by = r[b * 2 + 1];
    const double dx = bx - ax, dy = by - ay, len2 = dx * dx + dy * dy;
    double worst = -1;
    size_t at = 0;
    for (size_t i = a + 1; i < b; ++i) {
        const double px = r[i * 2] - ax, py = r[i * 2 + 1] - ay;
        double d2;
        if (len2 <= 0) {
            d2 = px * px + py * py;
        } else {
            const double t = std::clamp((px * dx + py * dy) / len2, 0.0, 1.0);
            const double ex = px - t * dx, ey = py - t * dy;
            d2 = ex * ex + ey * ey;
        }
        if (d2 > worst) {
            worst = d2;
            at = i;
        }
    }
    if (worst <= tol2) return;
    keep[at] = 1;
    douglasPeucker(r, a, at, tol2, keep);
    douglasPeucker(r, at, b, tol2, keep);
}

Polygon simplifyPolygon(const Polygon& p, double tol) {
    Polygon out;
    for (const Ring& r : p) {
        const size_t n = r.size() / 2;
        if (n <= 4) {
            out.push_back(r);
            continue;
        }
        // Split at the vertex farthest from the first, so a closed ring keeps its shape.
        size_t far = 0;
        double best = -1;
        for (size_t i = 1; i < n; ++i) {
            const double d = std::hypot(r[i * 2] - r[0], r[i * 2 + 1] - r[1]);
            if (d > best) {
                best = d;
                far = i;
            }
        }
        Ring closed(r);
        closed.push_back(r[0]);
        closed.push_back(r[1]);
        std::vector<char> keep(n + 1, 0);
        keep[0] = keep[far] = keep[n] = 1;
        douglasPeucker(closed, 0, far, tol * tol, keep);
        douglasPeucker(closed, far, n, tol * tol, keep);
        Ring s;
        for (size_t i = 0; i < n; ++i)
            if (keep[i]) {
                s.push_back(r[i * 2]);
                s.push_back(r[i * 2 + 1]);
            }
        if (s.size() >= 6) out.push_back(std::move(s));
    }
    return out;
}
