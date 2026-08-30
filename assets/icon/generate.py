#!/usr/bin/env python3
"""Emit the bedouin icon set from one geometry definition.

Subject: bayt al-sha'ar, the black goat-hair tent -- the house Bedouin carry,
strike, and pitch again identically on new ground. That is the tool.

Two decisions the drawing turns on:

  CONCAVITY. Every slope curves inward: steep at the pole, flattening toward
  the eave, because fabric hangs. A mountain silhouette is convex or straight,
  so the concavity is the single thing that stops the mark reading as terrain.
  Earlier passes with near-straight flanks rendered as a pyramid at every size.

  THE SAG. The ridge dips between the poles. A generic tent icon is an
  isosceles triangle; slack cloth is what this tent actually looks like, and
  the dip is what survives down to 16px as a distinguishing feature.

The accent is the guy lines, because the ropes are what let you strike the
tent and pitch it again somewhere else.

Run:  python3 assets/icon/generate.py
"""
import pathlib
import xml.etree.ElementTree as ET

HAIR_L = "#17161A"    # goat-hair black on light ground; warm-neutral, not pure black
HAIR_D = "#F2EFE9"    # inverted for dark ground -- bone, not cream
MADDER = "#A82A24"    # sadu madder: deeper and browner than the usual vermilion
MADDER_D = "#D4443C"  # lifted for dark ground, same hue family

# --------------------------------------------------------------- hero, ~3:1
# Two poles, one sagging bay, walls flaring to the ground. The wall segment
# earns its place by giving the guy lines an eave to spring from.
HERO_W, HERO_H, GROUND = 260, 82, 68
OUTLINE = ("M 58 68 L 66 46 Q 76 38 98 14 "
           "Q 130 40 162 14 "
           "Q 184 38 194 46 L 202 68 Z")
# The entrance, cut as a true hole (fill-rule evenodd) so the ground shows
# through and one path serves both themes. Trapezoid rather than an arch: it
# is the tent-door convention, and flaring downward echoes the tent's own
# silhouette, so the opening is a small tent.
DOOR = "M 116 68 L 123 44 L 137 44 L 144 68 Z"
GUYS = [(66, 46, 20, 66), (194, 46, 240, 66)]


def hero(hair, madder, bg=None):
    bgr = f'<rect width="{HERO_W}" height="{HERO_H}" fill="{bg}"/>' if bg else ""
    ropes = "".join(f'<path d="M {a} {b} L {c} {d}"/>' for a, b, c, d in GUYS)
    stakes = "".join(f'<path d="M {c} {d-4} L {c} {d+3}"/>' for *_, c, d in GUYS)
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {HERO_W} {HERO_H}" width="{HERO_W}" height="{HERO_H}" role="img" aria-label="Bedouin">
  <title>Bedouin</title>{bgr}
  <path d="M 12 {GROUND} L 248 {GROUND}" fill="none" stroke="{hair}" stroke-width="1.8" opacity="0.35"/>
  <g fill="none" stroke="{madder}" stroke-width="2.4" stroke-linecap="round">{ropes}{stakes}</g>
  <path d="{OUTLINE} {DOOR}" fill="{hair}" fill-rule="evenodd"/>
</svg>
'''


# ------------------------------------------------------------- reduced, 1:1
# The same object with the detail stripped: walls and ropes drop away, the
# concave flanks run straight to the ground. Solid rather than monoline --
# below ~24px a hairline on a hanging curve is exactly what fills in, which
# is the constraint the handoff's own exploration found.
MARK = "M 3 50 Q 12 46 22 12 Q 32 34 42 12 Q 52 46 61 50 Z"
MARK_DOOR = "M 26 50 L 26 37 Q 32 32 38 37 L 38 50 Z"


def mark(hair, madder, bg=None, accent=True, door=True):
    bgr = f'<rect width="64" height="64" fill="{bg}"/>' if bg else ""
    # The ground runs wider than the tent, so it reads as terrain the tent is
    # pitched on rather than as an underline attached to the shape.
    foot = (f'<path d="M 2 53 L 62 53" fill="none" stroke="{madder}" '
            f'stroke-width="2.6" stroke-linecap="round"/>') if accent else ""
    # The entrance holds down to 24px. At 16 it thins to a two-pixel notch, so
    # the tiny variant drops it rather than letting it turn to mush.
    body = f'{MARK} {MARK_DOOR}' if door else MARK
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" width="64" height="64" role="img" aria-label="Bedouin">
  <title>Bedouin</title>{bgr}
  <path d="{body}" fill="{hair}" fill-rule="evenodd"/>{foot}
</svg>
'''


VARIANTS = {
    "bedouin-hero-light.svg": hero(HAIR_L, MADDER),
    "bedouin-hero-dark.svg": hero(HAIR_D, MADDER_D),
    "bedouin-mark-light.svg": mark(HAIR_L, MADDER),
    "bedouin-mark-dark.svg": mark(HAIR_D, MADDER_D),
    # For inline embedding where the mark should take the surrounding text
    # colour -- terminal help output, docs that already theme themselves.
    "bedouin-mark-mono.svg": mark("currentColor", "currentColor", accent=False),
    # For 16px and below, where the entrance stops resolving.
    "bedouin-mark-tiny.svg": mark(HAIR_L, MADDER, door=False),
}

if __name__ == "__main__":
    here = pathlib.Path(__file__).parent
    for name, svg in VARIANTS.items():
        (here / name).write_text(svg)
        print(f"wrote {name}")
    NS = "{http://www.w3.org/2000/svg}"
    for name in VARIANTS:
        root = ET.parse(here / name).getroot()
        assert any(p.get("d") for p in root.iter(f"{NS}path")), f"{name}: nothing drawn"
        assert root.get("viewBox"), f"{name}: no viewBox, will not scale"
    print("all variants parse, scale, and draw")
