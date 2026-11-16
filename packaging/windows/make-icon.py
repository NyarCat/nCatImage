"""Render packaging/linux/ncatimage.svg into a multi-size Windows icon.

The SVG uses only a rounded rectangle, polygons, a line and a circle, so it is redrawn
here with Pillow (supersampled) instead of requiring an SVG renderer on Windows.
Usage: python packaging/windows/make-icon.py  (needs Pillow; output is committed)
"""
from pathlib import Path

from PIL import Image, ImageDraw

SCALE = 4
SIZE = 256 * SCALE
NAVY, PAPER, TEAL, SUN = "#243047", "#e9edf5", "#60b9b0", "#e6ae59"


def pts(*xy):
    return [(x * SCALE, y * SCALE) for x, y in xy]


def render() -> Image.Image:
    image = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    draw = ImageDraw.Draw(image)
    draw.rounded_rectangle(
        [16 * SCALE, 16 * SCALE, 240 * SCALE, 240 * SCALE], radius=48 * SCALE, fill=NAVY
    )
    draw.polygon(
        pts((52, 64), (118, 64), (128, 74), (138, 64), (204, 64), (204, 192),
            (138, 192), (128, 202), (118, 192), (52, 192)),
        fill=PAPER,
    )
    draw.line(pts((128, 80), (128, 190)), fill=NAVY, width=8 * SCALE)
    draw.polygon(pts((64, 158), (84, 130), (102, 150), (114, 136), (114, 176), (64, 176)), fill=TEAL)
    draw.polygon(pts((142, 158), (160, 122), (182, 148), (192, 136), (192, 176), (142, 176)), fill=TEAL)
    r = 12 * SCALE
    draw.ellipse([86 * SCALE - r, 102 * SCALE - r, 86 * SCALE + r, 102 * SCALE + r], fill=SUN)
    return image.resize((256, 256), Image.LANCZOS)


if __name__ == "__main__":
    out = Path(__file__).with_name("ncatimage.ico")
    render().save(out, sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
    print(out)
