#!/usr/bin/env python3
"""Generates the CallXL app icon: callxl.svg, png/callxl-<size>.png (16…1024) and callxl.ico.

One geometry definition drives both the SVG and the raster renderings (Pillow, 4x
supersampled). Colours come from design/tokens.json (dark accent). Run from anywhere:
    python3 design/icons/generate.py
Requires Pillow. Outputs are committed so builds never need Python.
"""
import json
import os

from PIL import Image, ImageDraw

HERE = os.path.dirname(os.path.abspath(__file__))
TOKENS = json.load(open(os.path.join(HERE, "..", "tokens.json")))
ACCENT = TOKENS["color"]["dark"]["accent"]
BG = TOKENS["color"]["dark"]["bg"]
SIZES = [16, 24, 32, 48, 64, 128, 256, 512, 1024]

# Geometry on a 1024 canvas: rounded square + five rounded "voice" bars.
CANVAS = 1024
CORNER = 228
BAR_W = 92
GAP = 58
HEIGHTS = [300, 520, 720, 520, 300]


def hex_rgb(h):
    h = h.lstrip("#")
    return tuple(int(h[i:i + 2], 16) for i in (0, 2, 4))


def mix(a, b, t):
    return tuple(round(x + (y - x) * t) for x, y in zip(a, b))


TOP = mix(hex_rgb(ACCENT), (255, 255, 255), 0.18)
BOTTOM = mix(hex_rgb(ACCENT), hex_rgb(BG), 0.30)


def bars():
    total = len(HEIGHTS) * BAR_W + (len(HEIGHTS) - 1) * GAP
    x = (CANVAS - total) / 2
    for h in HEIGHTS:
        y = (CANVAS - h) / 2
        yield x, y, BAR_W, h
        x += BAR_W + GAP


def svg():
    hexc = lambda c: "#%02X%02X%02X" % c
    rects = "\n".join(
        f'  <rect x="{x:g}" y="{y:g}" width="{w}" height="{h}" rx="{w / 2:g}" fill="#FFFFFF"/>' for x, y, w, h in bars()
    )
    return f"""<svg xmlns="http://www.w3.org/2000/svg" width="{CANVAS}" height="{CANVAS}" viewBox="0 0 {CANVAS} {CANVAS}">
  <defs>
    <linearGradient id="g" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="{hexc(TOP)}"/>
      <stop offset="1" stop-color="{hexc(BOTTOM)}"/>
    </linearGradient>
  </defs>
  <rect width="{CANVAS}" height="{CANVAS}" rx="{CORNER}" fill="url(#g)"/>
{rects}
</svg>
"""


def raster(size):
    ss = 4
    n = size * ss
    k = n / CANVAS
    grad = Image.new("RGBA", (1, n))
    for y in range(n):
        grad.putpixel((0, y), mix(TOP, BOTTOM, y / (n - 1)) + (255,))
    grad = grad.resize((n, n))
    mask = Image.new("L", (n, n), 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, n - 1, n - 1], radius=CORNER * k, fill=255)
    img = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    img.paste(grad, (0, 0), mask)
    d = ImageDraw.Draw(img)
    for x, y, w, h in bars():
        d.rounded_rectangle([x * k, y * k, (x + w) * k, (y + h) * k], radius=w * k / 2, fill=(255, 255, 255, 255))
    return img.resize((size, size), Image.LANCZOS)


def main():
    os.makedirs(os.path.join(HERE, "png"), exist_ok=True)
    with open(os.path.join(HERE, "callxl.svg"), "w") as f:
        f.write(svg())
    imgs = {}
    for s in SIZES:
        imgs[s] = raster(s)
        imgs[s].save(os.path.join(HERE, "png", f"callxl-{s}.png"), optimize=True)
    imgs[256].save(
        os.path.join(HERE, "callxl.ico"),
        sizes=[(s, s) for s in (16, 24, 32, 48, 64, 128, 256)],
    )
    print("wrote callxl.svg, png/callxl-{%s}.png, callxl.ico" % ",".join(map(str, SIZES)))


if __name__ == "__main__":
    main()
