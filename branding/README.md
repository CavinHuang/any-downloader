# Anything Downloader — official AD PNG brand icon

Source of truth: `branding/ad-logo-source.png`, the **original 1254 × 1254 PNG** supplied in the design conversation. Do not approximate or redraw it as an SVG.

The source contains the pale cream tile and the softly shaded blue/sage green interwoven AD monogram. Pixel-level artwork is preserved; derivative app sizes crop only the surrounding presentation margin (80px on all sides), then use LANCZOS scaling.

| Use | Paths |
| --- | --- |
| Desktop (Tauri / Windows) | `src-tauri/icons/32x32.png`, `src-tauri/icons/128x128.png`, `src-tauri/icons/icon.ico` |
| App header and favicon | `ui/public/brand-mark.png`, `ui/public/favicon.png` |
| Browser extension | `extension/icons/{16,32,48,128}.png` |
| Landing | `landing/assets/icon-128.png` |

Generate all assets from the original PNG:

```shell
python -m pip install Pillow
python scripts/generate_ad_icons.py
```

The generated files are committed so normal builds need no image toolchain. ICO embeds 16, 24, 32, 48, 64, 128 and 256px. No raster-to-vector tracing is involved.
