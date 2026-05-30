"""Generate 512x320 PNG preview images for each template.

These previews ship in `apps/desktop/renderer/public/templates/<key>/preview.png`
so the home-page template gallery shows a hero image instead of just the
SVG icon glyph (Phase 17 Group B Task 13). Each preview is a clean
schematic: a gradient background tinted to the category, a thin
floor-plan line drawing of the template's defining shapes, and a
purple AEC Studio brand corner mark.

The images are intentionally schematic, not photorealistic — they're
illustrations of what the template gives you on day 1, not a render of
the final building. Authoring them programmatically keeps the gallery
consistent (same composition, same accent palette) and avoids shipping
hundreds of KB of bitmap art for v1 of the gallery.
"""

import os
from PIL import Image, ImageDraw, ImageFont


W, H = 512, 320

# Tinted hero backgrounds per category. The colors match the design
# tokens (`--aec-color-accent-soft` etc.) so the previews feel native
# to the rest of the home page.
CATEGORY_BG_TOP = {
    "interior": (243, 240, 251),       # --aec-color-surface-muted
    "architecture": (239, 234, 255),   # lighter lavender
    "drafting": (240, 246, 252),       # cool blue-tinted neutral
}
CATEGORY_BG_BOTTOM = {
    "interior": (213, 197, 245),
    "architecture": (197, 178, 240),
    "drafting": (190, 211, 235),
}

ACCENT = (124, 58, 237)   # --aec-color-accent
INK = (31, 27, 45)        # --aec-color-text-primary
INK_SOFT = (90, 86, 107)  # --aec-color-text-secondary
WALL = (60, 50, 90)

TEMPLATES = [
    # (key, name, category, draw_fn_name)
    ("interior.apartment", "Apartment", "interior", "draw_apartment"),
    ("interior.kitchen", "Kitchen", "interior", "draw_kitchen"),
    ("interior.bathroom", "Bathroom", "interior", "draw_bathroom"),
    ("interior.renovation", "Renovation", "interior", "draw_renovation"),
    ("architecture.cafe", "Cafe", "architecture", "draw_cafe"),
    ("architecture.office", "Office", "architecture", "draw_office"),
    ("architecture.villa", "Villa", "architecture", "draw_villa"),
    ("architecture.retail", "Retail", "architecture", "draw_retail"),
]


def vertical_gradient(image, top, bottom):
    """Paint a top->bottom gradient on `image`."""
    draw = ImageDraw.Draw(image)
    for y in range(H):
        t = y / (H - 1)
        r = int(top[0] * (1 - t) + bottom[0] * t)
        g = int(top[1] * (1 - t) + bottom[1] * t)
        b = int(top[2] * (1 - t) + bottom[2] * t)
        draw.line([(0, y), (W - 1, y)], fill=(r, g, b))


def brand_mark(draw, name):
    """Top-left wordmark + top-right category pill."""
    try:
        font_big = ImageFont.truetype("/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf", 22)
        font_pill = ImageFont.truetype("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", 12)
    except Exception:
        font_big = ImageFont.load_default()
        font_pill = ImageFont.load_default()
    draw.text((20, 20), name, fill=INK, font=font_big)
    draw.text((20, 50), "AEC Studio template", fill=INK_SOFT, font=font_pill)


# -------- per-template line drawings --------
# Each draws a schematic floor plan or massing diagram. All shapes
# are stroked in WALL color, stroke width 3, to read as a clean
# blueprint regardless of category.


def draw_apartment(draw):
    # 60 m² apartment — living/dining + bedroom + bath.
    draw.rectangle([60, 110, 452, 290], outline=WALL, width=3)
    draw.line([(60, 200), (452, 200)], fill=WALL, width=2)
    draw.line([(240, 200), (240, 290)], fill=WALL, width=2)
    draw.line([(340, 110), (340, 200)], fill=WALL, width=2)
    # door swings (quarter circles approximated by arcs)
    draw.arc([(85, 175), (140, 225)], 270, 360, fill=ACCENT, width=2)
    draw.arc([(245, 175), (290, 225)], 270, 360, fill=ACCENT, width=2)
    # furniture hints
    draw.rectangle([90, 230, 180, 280], outline=ACCENT, width=2)   # sofa
    draw.rectangle([270, 220, 320, 270], outline=ACCENT, width=2)  # bed


def draw_kitchen(draw):
    # L-shaped kitchen with island.
    draw.rectangle([60, 110, 452, 290], outline=WALL, width=3)
    draw.rectangle([60, 110, 200, 160], outline=WALL, width=2, fill=None)
    draw.rectangle([60, 110, 110, 290], outline=WALL, width=2, fill=None)
    # island
    draw.rectangle([220, 200, 380, 240], outline=ACCENT, width=3)
    # stove
    draw.rectangle([130, 120, 170, 150], outline=ACCENT, width=2)
    # sink
    draw.rectangle([72, 200, 102, 240], outline=ACCENT, width=2)


def draw_bathroom(draw):
    draw.rectangle([100, 120, 412, 280], outline=WALL, width=3)
    # tub
    draw.rectangle([120, 140, 240, 200], outline=ACCENT, width=2)
    # vanity
    draw.rectangle([120, 220, 220, 260], outline=ACCENT, width=2)
    # WC
    draw.ellipse([300, 140, 360, 190], outline=ACCENT, width=2)
    # shower
    draw.rectangle([300, 210, 380, 260], outline=ACCENT, width=2)
    draw.line([(300, 210), (380, 260)], fill=ACCENT, width=1)
    draw.line([(380, 210), (300, 260)], fill=ACCENT, width=1)


def draw_renovation(draw):
    # demo (dashed) + keep (solid) + new (accent)
    draw.rectangle([60, 110, 452, 290], outline=WALL, width=3)
    # demo wall (dashed)
    for x in range(80, 230, 12):
        draw.line([(x, 200), (x + 6, 200)], fill=INK_SOFT, width=2)
    # keep wall
    draw.line([(230, 110), (230, 290)], fill=WALL, width=3)
    # new wall (accent)
    draw.line([(340, 200), (340, 290)], fill=ACCENT, width=4)
    draw.line([(340, 200), (452, 200)], fill=ACCENT, width=4)


def draw_cafe(draw):
    # 120 m² café — banquette, counter, scattered tables
    draw.rectangle([60, 110, 452, 290], outline=WALL, width=3)
    # banquette along top
    draw.rectangle([80, 130, 432, 160], outline=ACCENT, width=2)
    # counter
    draw.rectangle([80, 240, 200, 270], outline=WALL, width=2)
    # tables (circles)
    for cx in (260, 320, 380):
        draw.ellipse([cx - 12, 220, cx + 12, 244], outline=ACCENT, width=2)
        draw.ellipse([cx - 12, 250, cx + 12, 274], outline=ACCENT, width=2)


def draw_office(draw):
    # open plan + 3 meeting rooms
    draw.rectangle([60, 110, 452, 290], outline=WALL, width=3)
    # meeting rooms along bottom
    draw.line([(60, 230), (452, 230)], fill=WALL, width=2)
    draw.line([(190, 230), (190, 290)], fill=WALL, width=2)
    draw.line([(320, 230), (320, 290)], fill=WALL, width=2)
    # desks
    for i in range(3):
        x = 90 + i * 110
        draw.rectangle([x, 140, x + 70, 165], outline=ACCENT, width=2)
        draw.rectangle([x, 180, x + 70, 205], outline=ACCENT, width=2)


def draw_villa(draw):
    # multi-storey isometric (massing)
    draw.polygon(
        [(150, 220), (256, 170), (362, 220), (362, 280), (150, 280)],
        outline=WALL,
        width=3,
    )
    draw.line([(256, 170), (256, 230)], fill=WALL, width=2)
    draw.line([(150, 220), (256, 230)], fill=WALL, width=2)
    draw.line([(256, 230), (362, 220)], fill=WALL, width=2)
    # roof
    draw.polygon(
        [(150, 220), (256, 130), (362, 220)],
        outline=ACCENT,
        width=3,
    )


def draw_retail(draw):
    # boutique storefront — store + back-of-house
    draw.rectangle([60, 110, 452, 290], outline=WALL, width=3)
    draw.line([(340, 110), (340, 290)], fill=WALL, width=2)
    # storefront glazing (dashed)
    for x in range(64, 336, 14):
        draw.line([(x, 290), (x + 8, 290)], fill=ACCENT, width=3)
    # display fixtures
    draw.rectangle([90, 150, 180, 180], outline=ACCENT, width=2)
    draw.rectangle([220, 150, 310, 180], outline=ACCENT, width=2)
    draw.rectangle([90, 210, 310, 240], outline=ACCENT, width=2)


DRAW_FNS = {
    "draw_apartment": draw_apartment,
    "draw_kitchen": draw_kitchen,
    "draw_bathroom": draw_bathroom,
    "draw_renovation": draw_renovation,
    "draw_cafe": draw_cafe,
    "draw_office": draw_office,
    "draw_villa": draw_villa,
    "draw_retail": draw_retail,
}


def render(key, name, category, draw_fn_name, out_dir):
    img = Image.new("RGB", (W, H), CATEGORY_BG_TOP[category])
    vertical_gradient(img, CATEGORY_BG_TOP[category], CATEGORY_BG_BOTTOM[category])
    draw = ImageDraw.Draw(img)
    brand_mark(draw, name)
    DRAW_FNS[draw_fn_name](draw)
    out_path = os.path.join(out_dir, key, "preview.png")
    os.makedirs(os.path.dirname(out_path), exist_ok=True)
    img.save(out_path, optimize=True)
    print(f"wrote {out_path} ({os.path.getsize(out_path)} bytes)")


if __name__ == "__main__":
    base = "/home/ubuntu/repos/AEC-Studio/apps/desktop/renderer/public/templates"
    os.makedirs(base, exist_ok=True)
    for key, name, category, draw_fn_name in TEMPLATES:
        render(key, name, category, draw_fn_name, base)
