#!/usr/bin/env python3
"""Dependency-free regression checks for Anything Downloader desktop brand icons."""
from pathlib import Path
import struct
import hashlib

ROOT = Path(__file__).resolve().parents[1]
PNG_MAGIC = b"\x89PNG\r\n\x1a\n"


def check_png(path: str, dimension: int) -> None:
    b = (ROOT / path).read_bytes()
    assert b.startswith(PNG_MAGIC), f"Not a PNG: {path}"
    assert b[12:16] == b"IHDR", f"Invalid PNG IHDR: {path}"
    w, h, depth, color = struct.unpack_from(">IIBB", b, 16)
    assert (w, h, depth, color) == (dimension, dimension, 8, 6), (
        path, (w, h, depth, color), "expected 8-bit RGBA"
    )


for n in (16, 32, 48, 128):
    check_png(f"extension/icons/{n}.png", n)
for n in (32, 64, 128):
    check_png(f"src-tauri/icons/{n}x{n}.png", n)
for p, s in {
    "branding/ad-logo-source.png": 1024,
    "src-tauri/icons/app-source-1024.png": 1024,
    "src-tauri/icons/icon.png": 512,
    "src-tauri/icons/128x128@2x.png": 256,
    "ui/public/favicon.png": 32,
    "ui/public/brand-mark.png": 128,
    "landing/assets/icon-128.png": 128,
    "src-tauri/icons/StoreLogo.png": 50,
}.items():
    check_png(p, s)
for n in (30, 44, 71, 89, 107, 142, 150, 284, 310):
    check_png(f"src-tauri/icons/Square{n}x{n}Logo.png", n)

ico = (ROOT / "src-tauri/icons/icon.ico").read_bytes()
assert struct.unpack_from("<HHH", ico, 0) == (0, 1, 7)
for i, n in enumerate((16, 24, 32, 48, 64, 128, 256)):
    w, h, _, _, planes, bits, size, offset = struct.unpack_from("<BBBBHHII", ico, 6 + 16 * i)
    assert (w or 256, h or 256, planes, bits) == (n, n, 1, 32)
    assert ico[offset:offset+8] == PNG_MAGIC and offset+size <= len(ico)
    icon_w, icon_h, _, color = struct.unpack_from(">IIBB", ico, offset+16)
    assert (icon_w, icon_h, color) == (n, n, 6), f"RGBA missing for ICO frame {n}"

icns = (ROOT / "src-tauri/icons/icon.icns").read_bytes()
assert icns[:4] == b"icns" and struct.unpack_from(">I", icns, 4)[0] == len(icns)
p = 8
parts = []
while p < len(icns):
    part = icns[p:p+4]
    size = struct.unpack_from(">I", icns, p+4)[0]
    assert size >= 8 and icns[p+8:p+16] == PNG_MAGIC
    parts.append(part)
    p += size
assert p == len(icns) and b"ic10" in parts and b"icp4" in parts
assert (ROOT / "branding/ad-forest.svg").read_text().count("<svg") == 1
print(f"AD icon regression checks passed: {len(parts)} macOS frames, 7 Windows ICO frames, RGBA PNGs")
