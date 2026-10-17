#!/usr/bin/env python3
"""Render the approved forest AD mark as native icons at each display size.

Source is branding/ad-forest.svg. That vector is pixel-identical at 1024px to
branding/ad-logo-source.png, the approved candidate 01. The smaller icons are
rendered directly from the vector, *not* resized from a screenshot or preview.

Install design-time dependencies: python -m pip install cairosvg Pillow
Run from repository root: python scripts/generate_ad_icons.py
"""
from io import BytesIO
from pathlib import Path
import struct

import cairosvg
from PIL import Image, ImageChops

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "branding" / "ad-forest.svg"
SVG = SOURCE.read_bytes()


def render(size: int) -> Image.Image:
    bitmap = cairosvg.svg2png(bytestring=SVG, output_width=size, output_height=size)
    return Image.open(BytesIO(bitmap)).convert("RGBA")


def write_png(path: str, size: int) -> None:
    target = ROOT / path
    target.parent.mkdir(parents=True, exist_ok=True)
    render(size).save(target, format="PNG", optimize=True)


# Guard against accidentally changing the approved appearance during regeneration.
reference = Image.open(ROOT / "branding" / "ad-logo-source.png").convert("RGBA")
if reference.size != (1024, 1024) or ImageChops.difference(reference, render(1024)).getbbox():
    raise ValueError("SVG no longer matches the approved 1024px AD artwork")

for size in (16, 32, 48, 128):
    write_png(f"extension/icons/{size}.png", size)
write_png("ui/public/brand-mark.png", 128)
write_png("ui/public/favicon.png", 32)
write_png("landing/assets/icon-128.png", 128)
for size in (32, 64, 128):
    write_png(f"src-tauri/icons/{size}x{size}.png", size)
write_png("src-tauri/icons/128x128@2x.png", 256)
write_png("src-tauri/icons/icon.png", 512)
write_png("src-tauri/icons/app-source-1024.png", 1024)
for size in (30, 44, 71, 89, 107, 142, 150, 284, 310):
    write_png(f"src-tauri/icons/Square{size}x{size}Logo.png", size)
write_png("src-tauri/icons/StoreLogo.png", 50)

# Windows ICO embeds PNGs independently rasterized at native icon sizes.
windows_sizes = (16, 24, 32, 48, 64, 128, 256)
windows_entries = []
windows_payloads = []
offset = 6 + 16 * len(windows_sizes)
for size in windows_sizes:
    buf = BytesIO()
    render(size).save(buf, format="PNG", optimize=True)
    data = buf.getvalue()
    windows_entries.append(struct.pack("<BBBBHHII", size if size < 256 else 0,
                                       size if size < 256 else 0, 0, 0,
                                       1, 32, len(data), offset))
    windows_payloads.append(data)
    offset += len(data)
(ROOT / "src-tauri/icons/icon.ico").write_bytes(
    struct.pack("<HHH", 0, 1, len(windows_sizes))
    + b"".join(windows_entries) + b"".join(windows_payloads)
)

# PNG-backed macOS ICNS with standard and HiDPI sizes.
mac_sizes = (("icp4", 16), ("icp5", 32), ("icp6", 64), ("ic07", 128),
             ("ic08", 256), ("ic09", 512), ("ic10", 1024), ("ic11", 32),
             ("ic12", 64), ("ic13", 256), ("ic14", 512))
chunks = []
for name, size in mac_sizes:
    buf = BytesIO()
    render(size).save(buf, format="PNG", optimize=True)
    data = buf.getvalue()
    chunks.append(name.encode() + struct.pack(">I", len(data) + 8) + data)
(ROOT / "src-tauri/icons/icon.icns").write_bytes(
    b"icns" + struct.pack(">I", 8 + sum(map(len, chunks))) + b"".join(chunks)
)
print("Regenerated vector-native Windows, macOS, Linux, extension and web icons")
