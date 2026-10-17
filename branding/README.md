# Anything Downloader · AD brand mark (Forest edition)

## Approved artwork

**01 · 深墨绿版** is the canonical logo. The two canonical sources are:

- `branding/ad-forest.svg`: exact vector artwork, intended for rendering at native icon sizes.
- `branding/ad-logo-source.png`: the approved 1024 × 1024 PNG. The 1024px SVG render is **pixel-identical** to this PNG, checked by the asset generator.

The mark uses a dark forest-green rounded tile, a bright ivory **A**, and a sage **D**. The canvas outside the rounded tile is transparent, with no giant wallpaper-style glow or white rectangular matte. On Windows this provides clear contrast in 16–48px desktop icons.

## Where this icon is used

| Surface | Files |
| --- | --- |
| Windows shortcut, taskbar, installer | `src-tauri/icons/icon.ico` (native 16/24/32/48/64/128/256px PNG frames), `32x32.png`, `128x128.png`, Windows Square/Store logos |
| macOS app | `src-tauri/icons/icon.icns` (standard + Retina sizes) |
| Linux app | `src-tauri/icons/icon.png` (512 × 512) |
| Desktop app header, favicon | `ui/public/brand-mark.png`, `ui/public/favicon.png` |
| Browser extension | `extension/icons/16.png`, `32.png`, `48.png`, `128.png` |
| Landing page | `landing/assets/icon-128.png` |

## Regenerate and verify

All icons are committed, so normal application builds do **not** require Python image dependencies.

```bash
python -m pip install Pillow cairosvg
python scripts/generate_ad_icons.py
python scripts/test_brand_icons.py
```

The generator renders **every small-size PNG directly from the SVG**: it does not downscale a screenshot of the app icon. The regression check verifies PNG color format is 8-bit RGBA (Tauri requirement), correct dimensions, and all Windows ICO / macOS ICNS image frames. CI runs the dependency-free regression check automatically.

Windows shortcut arrows are added by the OS and aren't embedded in the icon. Users who had a previous version pinned to the Windows taskbar may need to unpin/re-pin if the shell is still showing its cached icon.
