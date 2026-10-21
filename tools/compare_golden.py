"""Compares two --dump-golden folders (the C++'s and the Rust port's) for the same series.

    python tools/compare_golden.py CPP_DIR RUST_DIR

The size lines must be equal; statistics and ranges within a relative 1e-4 (the GPU's float sums in another
order); maps within 2 per channel but for nearest-neighbour ties. Exit code 0 when everything agrees.
"""

import sys
from pathlib import Path


def lines(folder):
    out = {}
    for line in (Path(folder) / "golden.txt").read_text().splitlines():
        kind, *rest = line.split()
        if kind == "size":
            out["size"] = rest
        else:
            out[(kind, rest[0])] = rest[1:]
    return out


def close(a, b, rel=1e-4, abs_=1e-6):
    if a == b:
        return True
    try:
        x, y = float(a), float(b)
    except ValueError:
        return False
    if x != x and y != y:  # both NaN
        return True
    return abs(x - y) <= max(abs_, rel * max(abs(x), abs(y)))


def map_diff(ma, mb, w=400, h=300):
    """Pixels over 2 per channel apart, but for nearest-neighbour ties: where a pixel's centre falls on a texel's
    edge, GL's sampler (fixed-point) and the port's floor(uv * size) take the texels on either side. Zoomed in, the
    pixel then has its neighbour's colour; zoomed out, a whole row or column takes the next texels (a row or column
    is a tie when a quarter of the image's pixels in it differ). Returns (differences, ties)."""

    def px(m, x, y):
        i = (y * w + x) * 4
        return m[i : i + 4]

    def near(p, q):
        return max(abs(a - b) for a, b in zip(p, q)) <= 2

    diff = {
        (x, y)
        for y in range(h)
        for x in range(w)
        if not near(px(ma, x, y), px(mb, x, y))
    }
    # the image's pixels (not the background) in each row and column
    bg = px(ma, 0, 0)
    img_rows = [sum(px(ma, x, y) != bg for x in range(w)) for y in range(h)]
    img_cols = [sum(px(ma, x, y) != bg for y in range(h)) for x in range(w)]
    rows = {
        y
        for y in range(h)
        if sum((x, y) in diff for x in range(w)) > 0.25 * max(1, img_rows[y])
    }
    cols = {
        x
        for x in range(w)
        if sum((x, y) in diff for y in range(h)) > 0.25 * max(1, img_cols[x])
    }

    def tie(x, y):
        if y in rows or x in cols:
            return True
        around = ((x + 1, y), (x - 1, y), (x, y + 1), (x, y - 1))
        return any(
            0 <= u < w and 0 <= v < h and near(px(mb, x, y), px(ma, u, v))
            for u, v in around
        )

    over = sum(1 for x, y in diff if not tie(x, y))
    return over, len(diff) - over


def main():
    if len(sys.argv) != 3:
        print(__doc__)
        return 2
    cpp, rust = sys.argv[1], sys.argv[2]
    a, b = lines(cpp), lines(rust)
    bad = 0
    if a.get("size") != b.get("size"):
        print(f"size: {a.get('size')} vs {b.get('size')}")
        bad += 1
    stats = [k for k in a if k != "size" and k[0] == "stats"]
    stats_bad = 0
    for k in stats:
        if k not in b or not all(close(x, y) for x, y in zip(a[k], b[k])):
            stats_bad += 1
            if stats_bad <= 5:
                print(f"stats {k[1]}: {a[k]} vs {b.get(k)}")
    print(f"statistics: {len(stats) - stats_bad}/{len(stats)} pixels agree")
    bad += stats_bad
    for k in sorted(
        (k for k in a if k != "size" and k[0] == "range"), key=lambda k: int(k[1])
    ):
        mode = int(k[1])
        r = (
            "ok"
            if k in b and all(close(x, y) for x, y in zip(a[k], b[k]))
            else f"{a[k]} vs {b.get(k)}"
        )
        bad += r != "ok"
        pa = Path(cpp) / f"mode_{mode:02d}.rgba"
        pb = Path(rust) / f"mode_{mode:02d}.rgba"
        ma, mb = pa.read_bytes(), pb.read_bytes() if pb.exists() else b""
        if len(ma) != len(mb):
            print(f"mode {mode:2d}: range {r}, map sizes {len(ma)} vs {len(mb)}")
            bad += 1
            continue
        over, ties = map_diff(ma, mb)
        bad += over > 0
        print(
            f"mode {mode:2d}: range {r}, map: {over} pixels differ ({ties} nearest-neighbour ties)"
        )
    print("agree" if bad == 0 else f"{bad} differences")
    return 0 if bad == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
