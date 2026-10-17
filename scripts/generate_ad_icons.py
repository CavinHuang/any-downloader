#!/usr/bin/env python3
"""Create desktop, website and extension icon PNGs directly from the approved PNG.

This script does NOT redraw the logo. It only crops the outside presentation margin
and resizes the original using Pillow. Run: python scripts/generate_ad_icons.py
Requires: python -m pip install Pillow
"""
from pathlib import Path
from PIL import Image

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "branding" / "ad-logo-source.png"
source = Image.open(SOURCE).convert("RGB")
if source.size != (1254, 1254):
    raise ValueError(f"Unexpected AD source image dimensions: {source.size}")
icon = source.crop((80, 80, 1174, 1174))

def save_png(path: str, size: int) -> None:
    destination = ROOT / path
    destination.parent.mkdir(parents=True, exist_ok=True)
    icon.resize((size, size), Image.Resampling.LANCZOS).save(
        destination, format="PNG", optimize=True
    )

for dimension in (16, 32, 48, 128):
    save_png(f"extension/icons/{dimension}.png", dimension)
save_png("src-tauri/icons/32x32.png", 32)
save_png("src-tauri/icons/128x128.png", 128)
save_png("ui/public/favicon.png", 32)
save_png("ui/public/brand-mark.png", 128)
save_png("landing/assets/icon-128.png", 128)

destination = ROOT / "src-tauri" / "icons" / "icon.ico"
icon.save(destination, format="ICO", sizes=[
    (16, 16), (24, 24), (32, 32), (48, 48),
    (64, 64), (128, 128), (256, 256),
])
print("Updated app, desktop, extension and website PNG/ICO brand assets.")
