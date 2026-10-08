"""Builds resources/janus.ico, janus.png and janus.icns (macOS): stacked layers (the time
cube) plus a pixel series.

Usage: python tools/make_icon.py   (requires Pillow)
"""
from pathlib import Path

from PIL import Image, ImageDraw

S = 1024  # draw large, then downsample (antialiasing)


def layer(d: ImageDraw.ImageDraw, x: int, y: int, fill, outline):
    w, h = 560, 400
    d.rounded_rectangle([x, y, x + w, y + h], radius=56, fill=fill, outline=outline, width=14)


def main():
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.rounded_rectangle([24, 24, S - 24, S - 24], radius=210, fill=(22, 27, 38, 255))

    # Three stacked dates, oldest (back) to newest (front).
    layer(d, 300, 170, (31, 79, 122, 255), (58, 110, 160, 255))
    layer(d, 232, 290, (38, 116, 170, 255), (80, 150, 205, 255))
    layer(d, 164, 410, (56, 160, 214, 255), (140, 205, 240, 255))

    # A pixel's time series crossing the front layer.
    pts = [(200, 720), (300, 640), (390, 690), (480, 560), (575, 600), (665, 480), (770, 520), (860, 380)]
    d.line(pts, fill=(255, 158, 64, 255), width=46, joint="curve")
    for x, y in pts[::2] + [pts[-1]]:
        d.ellipse([x - 34, y - 34, x + 34, y + 34], fill=(255, 196, 120, 255), outline=(22, 27, 38, 255), width=10)

    out = Path(__file__).resolve().parent.parent / "resources" / "janus.ico"
    sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
    img.resize((256, 256), Image.LANCZOS).save(out, sizes=[(s, s) for s in sizes])
    img.resize((256, 256), Image.LANCZOS).save(out.with_suffix(".png"))
    img.save(out.with_suffix(".icns"))  # every size up to 1024 (512@2x)
    print(f"ok: {out}")


if __name__ == "__main__":
    main()
