#!/usr/bin/env python3
"""Draws RFdyno's icon: a meter dial with a needle on a dark rounded square.

Writes into apps/rfdyno/assets/:
- icon.png      256 x 256, for docs and the like
- icon.ico      16 to 256 px, embedded in the Windows executable (build.rs)
- icon-64.rgba  64 x 64 raw RGBA, the window's icon (no image decoder needed at run time)

Usage: python3 tools/icon.py   (needs Pillow)
"""

import math
import pathlib

from PIL import Image, ImageDraw

S = 1024  # drawn at 1024 px, scaled down for each size
OUT = pathlib.Path(__file__).resolve().parent.parent / "apps" / "rfdyno" / "assets"

INK_TOP = (20, 64, 74)  # the Teal theme's colours
INK_BOTTOM = (10, 34, 40)
GREEN = (146, 208, 80)  # iotta green
AMBER = (245, 196, 81)
CORAL = (255, 122, 102)
NEEDLE = (238, 244, 234)


def rounded_mask(size: int, radius: int) -> Image.Image:
    mask = Image.new("L", (size, size), 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, size - 1, size - 1), radius, fill=255)
    return mask


def point(cx: float, cy: float, r: float, degrees: float) -> tuple[float, float]:
    """A point on a circle; 0 degrees is 3 o'clock, angles grow clockwise (as Pillow's)."""
    rad = math.radians(degrees)
    return (cx + r * math.cos(rad), cy + r * math.sin(rad))


def draw(small: bool = False) -> Image.Image:
    """The icon at 1024 px; `small` leaves out the ticks and draws thicker, for 32 px and less."""
    # Background: a vertical gradient in a rounded square.
    bg = Image.new("RGBA", (S, S))
    for y in range(S):
        t = y / (S - 1)
        colour = tuple(round(a + (b - a) * t) for a, b in zip(INK_TOP, INK_BOTTOM))
        ImageDraw.Draw(bg).line([(0, y), (S, y)], fill=colour + (255,))
    icon = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    icon.paste(bg, (0, 0), rounded_mask(S, int(S * 0.22)))

    d = ImageDraw.Draw(icon)
    cx, cy = S * 0.5, S * 0.64
    r = S * 0.34
    width = int(S * (0.13 if small else 0.095))
    box = (cx - r, cy - r, cx + r, cy + r)
    # The dial: a 220-degree arc over the top, green, then amber, then coral.
    start, end = 160, 380
    for (a, b), colour in [((start, 300), GREEN), ((300, 345), AMBER), ((345, end), CORAL)]:
        d.arc(box, a, b, fill=colour, width=width)
    # Round ends on the arc.
    for angle, colour in [(start, GREEN), (end, CORAL)]:
        x, y = point(cx, cy, r - width / 2, angle)
        d.ellipse((x - width / 2, y - width / 2, x + width / 2, y + width / 2), fill=colour)

    # Ticks inside the arc (not on small sizes, where they'd be noise).
    for i in range(0 if small else 7):
        angle = start + (end - start) * i / 6
        outer = point(cx, cy, r - width * 1.35, angle)
        inner = point(cx, cy, r - width * 1.9, angle)
        d.line([inner, outer], fill=NEEDLE + (150,), width=int(S * 0.022))

    # The needle, well into the green, and its hub.
    tip = point(cx, cy, r * 0.86, 268)
    d.line([(cx, cy), tip], fill=NEEDLE, width=int(S * (0.075 if small else 0.05)))
    d.ellipse((tip[0] - S * 0.025, tip[1] - S * 0.025, tip[0] + S * 0.025, tip[1] + S * 0.025),
              fill=NEEDLE)
    hub = S * 0.075
    d.ellipse((cx - hub, cy - hub, cx + hub, cy + hub), fill=NEEDLE)
    d.ellipse((cx - hub * 0.45, cy - hub * 0.45, cx + hub * 0.45, cy + hub * 0.45), fill=INK_BOTTOM)
    return icon


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    big, small = draw(), draw(small=True)
    big.resize((256, 256), Image.LANCZOS).save(OUT / "icon.png")
    sizes = (256, 128, 64, 48, 40, 32, 24, 20, 16)
    images = [(small if n <= 32 else big).resize((n, n), Image.LANCZOS) for n in sizes]
    images[0].save(OUT / "icon.ico", sizes=[(n, n) for n in sizes], append_images=images[1:])
    (OUT / "icon-64.rgba").write_bytes(big.resize((64, 64), Image.LANCZOS).tobytes())
    print(f"wrote icon.png, icon.ico, icon-64.rgba in {OUT}")


if __name__ == "__main__":
    main()
