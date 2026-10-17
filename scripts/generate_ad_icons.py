#!/usr/bin/env python3
"""Regenerate AD artwork from branding/ad-mark.svg.

Requirements: pip install cairosvg Pillow
Usage: python scripts/generate_ad_icons.py
"""
from io import BytesIO
from pathlib import Path
import shutil
import cairosvg
from PIL import Image

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "branding" / "ad-mark.svg"
for dst in (
    ROOT / "ui" / "public" / "brand-mark.svg",
    ROOT / "extension" / "icons" / "mark.svg",
    ROOT / "landing" / "assets" / "mark.svg",
):
    dst.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(SOURCE, dst)

large = Image.open(BytesIO(cairosvg.svg2png(url=str(SOURCE), output_width=1024, output_height=1024))).convert("RGBA")

def icon(path: Path, size: int) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    large.resize((size, size), Image.Resampling.LANCZOS).save(path)

for size in (16, 32, 48, 128):
    icon(ROOT / "extension" / "icons" / f"{size}.png", size)
icon(ROOT / "landing" / "assets" / "icon-128.png", 128)
icon(ROOT / "src-tauri" / "icons" / "32x32.png", 32)
icon(ROOT / "src-tauri" / "icons" / "128x128.png", 128)
large.save(ROOT / "src-tauri" / "icons" / "icon.ico", format="ICO", sizes=[(16,16), (24,24), (32,32), (48,48), (64,64), (128,128), (256,256)])
print("Generated desktop ICO/PNG, extension PNG and website PNG from AD SVG.")
