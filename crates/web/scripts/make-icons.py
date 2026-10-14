#!/usr/bin/env python3
"""Draws the app icons: a checklist on the accent colour.

Run from anywhere (needs Pillow); writes the PNGs to crates/web/icons.
The PNGs are committed, so this only needs rerunning to change the design.
"""

from pathlib import Path

from PIL import Image, ImageDraw

ACCENT = (37, 99, 235)  # --accent in style.css
INK = (250, 250, 249)  # --bg in style.css
OUT = Path(__file__).parent.parent / "icons"
SS = 4  # supersampling factor for smooth edges


def draw(size, *, maskable=False, opaque=False):
    s = size * SS
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    if maskable or opaque:
        # Full bleed: launchers (maskable) and iOS (apple-touch-icon) apply
        # their own shape.
        d.rectangle((0, 0, s, s), fill=ACCENT)
    else:
        d.rounded_rectangle((0, 0, s - 1, s - 1), radius=s * 0.22, fill=ACCENT)
    # Maskable icons must keep their content inside the central 80% circle.
    scale = 0.62 if maskable else 0.8
    left = s * (1 - scale) / 2
    unit = s * scale / 10
    stroke = round(unit * 0.9)
    for row in range(3):
        cy = left + unit * (2 + row * 3)
        box = (left + unit * 0.5, cy - unit, left + unit * 2.5, cy + unit)
        if row < 2:
            # A tick for the first two items.
            d.line(
                [
                    (box[0] + unit * 0.1, cy),
                    (box[0] + unit * 0.8, cy + unit * 0.7),
                    (box[2] + unit * 0.2, cy - unit * 0.9),
                ],
                fill=INK,
                width=stroke,
                joint="curve",
            )
        else:
            d.rounded_rectangle(box, radius=unit * 0.3, outline=INK, width=round(stroke * 0.7))
        d.rounded_rectangle(
            (left + unit * 3.8, cy - stroke / 2, left + unit * 9.5, cy + stroke / 2),
            radius=stroke / 2,
            fill=INK,
        )
    img = img.resize((size, size), Image.LANCZOS)
    if opaque:
        img = img.convert("RGB")
    return img


draw(192).save(OUT / "icon-192.png", optimize=True)
draw(512).save(OUT / "icon-512.png", optimize=True)
draw(512, maskable=True).save(OUT / "icon-maskable-512.png", optimize=True)
draw(180, opaque=True).save(OUT / "apple-touch-icon.png", optimize=True)
