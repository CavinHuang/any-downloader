# Anything Downloader — AD brand mark

Canonical, resolution-independent vector: `branding/ad-mark.svg` (1024 × 1024 viewBox).

The new combined **AD** monogram matches the Anything Studio design:
- paper tile `#fbfaf6`; forest-ink A `#293c34`
- sage D `#8da994`; soft-lime crossbar `#c1d39a`

All product surfaces now reference the same design:
- Desktop: Tauri `src-tauri/icons/{32x32.png,128x128.png,icon.ico}`; tray icon follows Tauri's default window icon.
- Browser extension: `extension/icons/mark.svg` and four PNG files referenced by `extension/manifest.json`.
- Web app: `ui/public/brand-mark.svg`, displayed in the header and as favicon.
- Product site: `landing/assets/mark.svg` and `landing/assets/icon-128.png`.

To regenerate all assets from the canonical SVG:

```sh
python -m pip install cairosvg Pillow
python scripts/generate_ad_icons.py
```

ICO contains 16/24/32/48/64/128/256px frames for Windows shell sizes. Extension icons are 16, 32, 48 and 128px. All raster assets are committed so normal application builds do not need Python dependencies.
