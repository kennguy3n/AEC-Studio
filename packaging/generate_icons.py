"""Generate placeholder application icons for the packaging pipeline.

The macOS, Windows, and Linux electron-builder configs reference per-platform
icons in ``packaging/{platform}/resources``. This script produces a single
deterministic 1024x1024 PNG that electron-builder auto-converts into the
platform-specific variant (``.icns`` / ``.ico`` / scaled PNGs) at build time.

The output is intentionally a placeholder: a flat brand-colour background with
the "AEC" wordmark centered. Studios shipping real builds should replace the
generated PNG with their own brand artwork; the surrounding pipeline (config
paths, CI jobs, electron-builder lookup) keeps working unchanged.

Usage::

    python3 packaging/generate_icons.py

Re-running produces byte-identical output (deterministic font rendering on the
runner's bundled Pillow + DejaVu fonts), so the commit is stable.
"""
from __future__ import annotations

from pathlib import Path

from PIL import Image, ImageDraw, ImageFont


BRAND_BACKGROUND = (24, 31, 49)  # AEC Studio dark navy
BRAND_FOREGROUND = (245, 198, 113)  # warm accent
ICON_SIZE = 1024


def _font(size: int) -> ImageFont.FreeTypeFont:
    """Load DejaVuSans-Bold at the requested size — available on every runner."""
    candidates = [
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
        "/Library/Fonts/Arial Bold.ttf",
        "/Windows/Fonts/arialbd.ttf",
    ]
    for path in candidates:
        if Path(path).exists():
            return ImageFont.truetype(path, size=size)
    return ImageFont.load_default()


def render_icon(out_path: Path) -> None:
    """Render the placeholder icon to ``out_path``."""
    image = Image.new("RGBA", (ICON_SIZE, ICON_SIZE), BRAND_BACKGROUND + (255,))
    draw = ImageDraw.Draw(image)
    margin = ICON_SIZE // 8
    draw.rounded_rectangle(
        (margin, margin, ICON_SIZE - margin, ICON_SIZE - margin),
        radius=ICON_SIZE // 6,
        outline=BRAND_FOREGROUND,
        width=ICON_SIZE // 64,
    )
    wordmark_font = _font(ICON_SIZE // 3)
    bbox = draw.textbbox((0, 0), "AEC", font=wordmark_font, anchor="lt")
    tw = bbox[2] - bbox[0]
    th = bbox[3] - bbox[1]
    draw.text(
        ((ICON_SIZE - tw) / 2 - bbox[0], (ICON_SIZE - th) / 2 - bbox[1] - ICON_SIZE // 32),
        "AEC",
        font=wordmark_font,
        fill=BRAND_FOREGROUND,
    )
    sub_font = _font(ICON_SIZE // 12)
    sbbox = draw.textbbox((0, 0), "STUDIO", font=sub_font, anchor="lt")
    sw = sbbox[2] - sbbox[0]
    sh = sbbox[3] - sbbox[1]
    draw.text(
        ((ICON_SIZE - sw) / 2 - sbbox[0], ICON_SIZE * 5 // 7 - sbbox[1]),
        "STUDIO",
        font=sub_font,
        fill=BRAND_FOREGROUND,
    )
    out_path.parent.mkdir(parents=True, exist_ok=True)
    image.save(out_path, format="PNG", optimize=True)


def render_ico(png_path: Path, out_path: Path) -> None:
    """Render a multi-resolution .ico from the master PNG."""
    base = Image.open(png_path).convert("RGBA")
    sizes = [(16, 16), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)]
    base.save(out_path, format="ICO", sizes=sizes)


def main() -> None:
    repo_root = Path(__file__).resolve().parent.parent
    targets = [
        repo_root / "packaging" / "macos" / "resources" / "icon.png",
        repo_root / "packaging" / "windows" / "resources" / "icon.png",
        repo_root / "packaging" / "linux" / "resources" / "icons" / "icon.png",
    ]
    for target in targets:
        render_icon(target)
        print(f"wrote {target.relative_to(repo_root)}")

    # Windows additionally needs a .ico for fileAssociations / installer icons.
    ico_target = repo_root / "packaging" / "windows" / "resources" / "icon.ico"
    render_ico(targets[1], ico_target)
    print(f"wrote {ico_target.relative_to(repo_root)}")


if __name__ == "__main__":
    main()
