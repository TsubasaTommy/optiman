#!/usr/bin/env python3
"""Renders the app icon: assets/optiman.icns (macOS) and assets/optiman.ico (Windows), and the
menu bar icon assets/menubar.png (macOS, a 36 px template image shown at 18 pt).

The icon is the brand mark (the site's favicon.svg: six colour patches, 3 x 2) on a rounded tile
in the site's card grey. Small sizes snap the patches to whole pixels so they stay crisp.

Usage: python3 scripts/render_icon.py [OUT_DIR]   (default: assets)
"""

import math
import struct
import sys
import zlib
from pathlib import Path

TILE = (0x76, 0x76, 0x75)
PATCHES = [
    [(0x73, 0x52, 0x44), (0xC2, 0x96, 0x82), (0x62, 0x7A, 0x9D)],
    [(0x57, 0x6C, 0x43), (0x85, 0x80, 0xB1), (0x67, 0xBD, 0xAA)],
]
# Mark width as a share of the tile (the same proportion as site/apple-touch-icon.png).
MARK_SHARE = 0.6
WINDOWS_SIZES = [16, 20, 24, 32, 40, 48, 64, 256]
# .icns entry types and their pixel sizes (the set `iconutil` writes: 16-512 pt at 1x and 2x).
MAC_TYPES = [
    (b"icp4", 16), (b"ic11", 32), (b"icp5", 32), (b"ic12", 64), (b"ic07", 128),
    (b"ic13", 256), (b"ic08", 256), (b"ic14", 512), (b"ic09", 512), (b"ic10", 1024),
]


def render(size, inset, radius, squircle=False, shadow=False):
    """RGBA rows: a rounded tile `inset` px from each edge, with the mark centred on it.

    `squircle` draws macOS's continuous-curvature outline (a superellipse) instead of circular
    corners; `shadow` adds the soft drop shadow macOS icons carry below the tile."""
    tile = size - 2 * inset
    cell = patch_size(tile)
    x0 = inset + (tile - 3 * cell) // 2
    y0 = inset + (tile - 2 * cell) // 2
    half = tile / 2
    centre = size / 2
    blur = size * 0.02
    rows = []
    for y in range(size):
        row = bytearray()
        for x in range(size):
            px, py = x + 0.5 - centre, y + 0.5 - centre
            d = squircle_distance(px, py, half) if squircle else rounded_distance(px, py, half, radius)
            alpha = min(max(0.5 - d, 0), 1)
            col, line = (x - x0) // cell, (y - y0) // cell
            if x >= x0 and y >= y0 and col < 3 and line < 2:
                colour = PATCHES[line][col]
            else:
                # A faint top-to-bottom shade, as on the system's own icons.
                shade = 6 - 12 * (y - inset) / tile if squircle else 0
                colour = tuple(round(min(max(c + shade, 0), 255)) for c in TILE)
            if shadow and alpha < 1:
                sd = squircle_distance(px, py - size * 0.012, half)
                under = 0.3 * min(max(0.5 - sd / blur, 0), 1)
                total = alpha + under * (1 - alpha)
                colour = tuple(round(c * alpha / total) for c in colour) if total else (0, 0, 0)
                alpha = total
            row += bytes(colour) + bytes([round(255 * alpha)])
        rows.append(bytes(row))
    return rows


def rounded_distance(px, py, half, radius):
    """Signed distance to a square of half-size `half` with circular corners (negative inside)."""
    qx = abs(px) - (half - radius)
    qy = abs(py) - (half - radius)
    return math.hypot(max(qx, 0), max(qy, 0)) + min(max(qx, qy), 0) - radius


def squircle_distance(px, py, half, corner=0.64, n=4.0):
    """Approximate signed distance to a square whose corners are superellipse arcs of exponent
    `n` spanning `corner` of the half-size: straight sides that blend smoothly into the corners."""
    radius = half * corner
    qx = abs(px) - (half - radius)
    qy = abs(py) - (half - radius)
    if qx <= 0 or qy <= 0:
        return max(qx, qy) - radius
    r = (qx**n + qy**n) ** (1 / n)
    grad = math.hypot(qx ** (n - 1), qy ** (n - 1)) / r ** (n - 1)
    return (r - radius) / grad


def patch_size(tile):
    """Whole-pixel patch size near MARK_SHARE whose mark centres exactly (even margins)."""
    ideal = tile * MARK_SHARE / 3
    candidates = [c for c in range(1, tile // 3 + 1) if (tile - 3 * c) % 2 == 0]
    return min(candidates, key=lambda c: (abs(c - ideal), -c))


def png(rows):
    size = len(rows)

    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    raw = b"".join(b"\0" + r for r in rows)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def dib(rows):
    """32-bit BGRA bitmap with its AND mask, as stored in .ico files (bottom-up rows)."""
    size = len(rows)
    header = struct.pack("<IiiHHIIiiII", 40, size, size * 2, 1, 32, 0, 0, 0, 0, 0, 0)
    pixels = bytearray()
    mask = bytearray()
    stride = (size + 31) // 32 * 4
    for r in reversed(rows):
        bits = bytearray(stride)
        for x in range(size):
            red, green, blue, alpha = r[4 * x : 4 * x + 4]
            pixels += bytes([blue, green, red, alpha])
            if alpha == 0:
                bits[x // 8] |= 0x80 >> (x % 8)
        mask += bits
    return header + bytes(pixels) + bytes(mask)


def ico(images):
    """images: (size, data) pairs; PNG data for 256 px, DIB below (what every reader accepts)."""
    out = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    for size, data in images:
        out += struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    return out + b"".join(data for _, data in images)


def menu_bar_rows(size=36, cell=10, gap=2):
    """The mark as black patches with gaps (a template image: macOS tints it for the menu bar)."""
    width, height = 3 * cell + 2 * gap, 2 * cell + gap
    x0, y0 = (size - width) // 2, (size - height) // 2
    rows = []
    for y in range(size):
        row = bytearray()
        for x in range(size):
            dx, dy = x - x0, y - y0
            inside = 0 <= dx < width and 0 <= dy < height and dx % (cell + gap) < cell and dy % (cell + gap) < cell
            row += bytes([0, 0, 0, 255 if inside else 0])
        rows.append(bytes(row))
    return rows


def icns(entries):
    """entries: (type, PNG data) pairs."""
    body = b"".join(kind + struct.pack(">I", 8 + len(data)) + data for kind, data in entries)
    return b"icns" + struct.pack(">I", 8 + len(body)) + body


def mac_rows(size):
    """Apple's icon grid: an 824/1024 tile, centred, with the system's outline and shadow."""
    return render(size, round(size * 100 / 1024), 0, squircle=True, shadow=True)


def windows_rows(size):
    inset = 0 if size <= 24 else round(size / 16)
    return render(size, inset, (size - 2 * inset) * 0.18)


def main():
    out = Path(sys.argv[1] if len(sys.argv) > 1 else "assets")
    out.mkdir(parents=True, exist_ok=True)
    mac = {size: png(mac_rows(size)) for size in sorted({size for _, size in MAC_TYPES})}
    (out / "optiman.icns").write_bytes(icns([(kind, mac[size]) for kind, size in MAC_TYPES]))
    images = []
    for size in WINDOWS_SIZES:
        rows = windows_rows(size)
        images.append((size, png(rows) if size >= 256 else dib(rows)))
    (out / "optiman.ico").write_bytes(ico(images))
    (out / "menubar.png").write_bytes(png(menu_bar_rows()))


if __name__ == "__main__":
    main()
