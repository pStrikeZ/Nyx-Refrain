#!/usr/bin/env python3
"""Renders assets/artwork/placeholder-cover.jpg, the AirPlay artwork sent when no
now-playing information is available.

Source: assets/shooting_star_color.svg (Fluent Emoji "Color" style, vector). The viewBox
is cropped to inside the tile's rounded corners so the art is full-bleed (receivers round
the corners themselves). Needs Python Playwright with Chromium and ImageMagick:

    python3 packaging/icons/render_cover.py
"""

import subprocess
import sys
import tempfile
from pathlib import Path

from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parents[2]
SIZE = 1000
# The tile spans 2..30 of the 32-unit viewBox with corner radius 3.67; insetting by
# r * (1 - 1/sqrt(2)) ≈ 1.1 keeps the rounded corners out of frame.
VIEWBOX = "3.1 3.1 25.8 25.8"


def main(out: Path) -> None:
    svg = (ROOT / "assets" / "shooting_star_color.svg").read_text()
    svg = svg.replace(
        'width="32" height="32" viewBox="0 0 32 32"',
        f'width="{SIZE}" height="{SIZE}" viewBox="{VIEWBOX}"',
        1,
    )
    with tempfile.TemporaryDirectory() as tmp:
        png = Path(tmp) / "cover.png"
        with sync_playwright() as p:
            browser = p.chromium.launch()
            page = browser.new_page(viewport={"width": SIZE, "height": SIZE})
            page.set_content(f'<html><body style="margin:0;background:#000">{svg}</body></html>')
            page.locator("svg").screenshot(path=str(png))
            browser.close()
        out.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run(
            ["magick", str(png), "-strip", "-quality", "90", "-sampling-factor", "4:2:0", str(out)],
            check=True,
        )


if __name__ == "__main__":
    main(Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "assets" / "artwork" / "placeholder-cover.jpg")
