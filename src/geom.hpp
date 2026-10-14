#pragma once

// Plane geometry shared by the polygonal ROI and the vector layers: polygons
// as rings of (x, y) pairs, filled by the even-odd rule (so holes and parts of
// a multipolygon need no flags: a hole is one more ring inside its shell).

#include <functional>
#include <vector>

using Ring = std::vector<double>;   // x0, y0, x1, y1, ... (closed implicitly)
using Polygon = std::vector<Ring>;  // rings, even-odd

// Bounding box (x0, y0, x1, y1) of every vertex; false without any.
bool polygonBounds(const Polygon& p, double box[4]);
// Even-odd: inside an odd number of rings.
bool pointInPolygon(const Polygon& p, double x, double y);
// Area enclosed (even-odd rings: shells minus holes, as their signed areas would
// not tell which is which, the absolute areas of rings inside an even number of
// others count positive, the rest negative).
double polygonArea(const Polygon& p);
// Every vertex through f (in place); rings left with fewer than 3 points dropped.
void transformPolygon(Polygon& p, const std::function<bool(double&, double&)>& f);
// Each edge split so no piece is longer than `maxLen` (before a curved transformation).
Polygon densifyPolygon(const Polygon& p, double maxLen);
// Cells of a bw x bh grid laid over the window (x0, y0, w, h) whose centres are
// inside (one scanline per row): 1 inside, 0 outside.
std::vector<char> polygonMask(const Polygon& p, double x0, double y0, double w, double h, int bw, int bh);
// Fewer vertices along nearly straight runs (Douglas-Peucker, tolerance `tol`), per ring.
Polygon simplifyPolygon(const Polygon& p, double tol);
