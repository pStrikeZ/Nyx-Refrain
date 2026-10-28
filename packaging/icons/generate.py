#!/usr/bin/env python3
"""Generate PNG, raw RGBA and ICO assets with the Python standard library."""

import struct
import sys
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
TRAY_SIZES = (16, 22, 24, 32, 48, 64)
APP_SIZES = (16, 22, 24, 32, 48, 64, 128, 256)
# Transparent margin around the Fluent 3D emoji artwork (alpha bbox is 16..239 of 256).
TRAY_TRIM = 16
ICO_SIZES = (16, 20, 24, 32, 40, 48, 64, 256)


def paeth(left, above, corner):
    prediction = left + above - corner
    return min((left, above, corner), key=lambda value: abs(prediction - value))


def read_png(path):
    """Read the owners' non-interlaced, 8-bit RGBA PNGs, including all filters."""
    data = path.read_bytes()
    assert data[:8] == b"\x89PNG\r\n\x1a\n"
    offset, compressed = 8, bytearray()
    while offset < len(data):
        length = struct.unpack_from(">I", data, offset)[0]
        kind = data[offset + 4 : offset + 8]
        value = data[offset + 8 : offset + 8 + length]
        checksum = struct.unpack_from(">I", data, offset + 8 + length)[0]
        assert zlib.crc32(kind + value) == checksum
        if kind == b"IHDR":
            width, height, depth, color, compression, filtering, interlace = struct.unpack(
                ">IIBBBBB", value
            )
            assert (depth, color, compression, filtering, interlace) == (8, 6, 0, 0, 0)
        elif kind == b"IDAT":
            compressed.extend(value)
        offset += length + 12
    raw = zlib.decompress(compressed)
    stride = width * 4
    assert len(raw) == height * (stride + 1)
    pixels, previous = bytearray(), bytearray(stride)
    for y in range(height):
        start = y * (stride + 1)
        filter_type = raw[start]
        assert filter_type in range(5)
        row = bytearray(raw[start + 1 : start + 1 + stride])
        for x in range(stride):
            left = row[x - 4] if x >= 4 else 0
            above = previous[x]
            corner = previous[x - 4] if x >= 4 else 0
            predictor = (0, left, above, (left + above) // 2, paeth(left, above, corner))
            row[x] = (row[x] + predictor[filter_type]) & 255
        pixels.extend(row)
        previous = row
    return width, height, pixels


def chunk(kind, data):
    return (
        struct.pack(">I", len(data)) + kind + data
        + struct.pack(">I", zlib.crc32(kind + data))
    )


def png(size, pixels):
    rows = b"".join(
        b"\0" + pixels[y * size * 4 : (y + 1) * size * 4] for y in range(size)
    )
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(rows, level=9))
        + chunk(b"IEND", b"")
    )


def crop(source, edge, left, top, side):
    """Square `side`x`side` region of an `edge`x`edge` RGBA image."""
    result = bytearray()
    for y in range(top, top + side):
        start = (y * edge + left) * 4
        result.extend(source[start : start + side * 4])
    return result


def resize(source, size, edge=256):
    """Exact area weights in integer coordinates; RGB is filtered premultiplied."""
    result = bytearray()
    for y in range(size):
        top, bottom = y * edge, (y + 1) * edge
        for x in range(size):
            left, right = x * edge, (x + 1) * edge
            alpha, colors = 0, [0, 0, 0]
            for sy in range(top // size, (bottom + size - 1) // size):
                vertical = min(bottom, (sy + 1) * size) - max(top, sy * size)
                for sx in range(left // size, (right + size - 1) // size):
                    horizontal = min(right, (sx + 1) * size) - max(left, sx * size)
                    weight = vertical * horizontal
                    offset = (sy * edge + sx) * 4
                    weighted_alpha = source[offset + 3] * weight
                    alpha += weighted_alpha
                    for channel in range(3):
                        colors[channel] += source[offset + channel] * weighted_alpha
            opacity = (alpha + edge * edge // 2) // (edge * edge)
            result.extend(
                (value + alpha // 2) // alpha if opacity else 0 for value in colors
            )
            result.append(opacity)
    return result


def badge(pixels, size, color):
    """40%-diameter dot with a 1.28px white ring at 32px; 8x8 coverage sampling."""
    result = bytearray(pixels)
    center, inner, outer = 0.75 * size, (0.20 * size) ** 2, (0.24 * size) ** 2
    for y in range(size):
        for x in range(size):
            dot, ring = 0, 0
            for sy in range(8):
                for sx in range(8):
                    dx = x + (sx + 0.5) / 8 - center
                    dy = y + (sy + 0.5) / 8 - center
                    distance = dx * dx + dy * dy
                    dot += distance <= inner
                    ring += inner < distance <= outer
            outside = 64 - dot - ring
            offset = (y * size + x) * 4
            alpha = pixels[offset + 3] * outside + 255 * (dot + ring)
            for channel in range(3):
                value = pixels[offset + channel] * pixels[offset + 3] * outside
                value += 255 * (color[channel] * dot + 250 * ring)
                result[offset + channel] = (value + alpha // 2) // alpha if alpha else 0
            result[offset + 3] = (alpha + 32) // 64
    return result


def ico(source):
    images = []
    for size in ICO_SIZES:
        pixels = resize(source, size)
        if size == 256:
            images.append(png(size, pixels))
            continue
        # Smaller entries are 32-bit bottom-up BGRA DIBs, followed by the AND mask.
        bitmap, mask = bytearray(), bytearray()
        for y in reversed(range(size)):
            mask_row = bytearray(((size + 31) // 32) * 4)
            for x in range(size):
                offset = (y * size + x) * 4
                red, green, blue, alpha = pixels[offset : offset + 4]
                bitmap.extend((blue, green, red, alpha))
                if alpha == 0:
                    mask_row[x // 8] |= 128 >> (x % 8)
            mask.extend(mask_row)
        header = struct.pack(
            "<IiiHHIIiiII", 40, size, size * 2, 1, 32, 0, len(bitmap), 0, 0, 0, 0
        )
        images.append(header + bitmap + mask)
    offset = 6 + 16 * len(images)
    directory = bytearray(struct.pack("<HHH", 0, 1, len(images)))
    for size, data in zip(ICO_SIZES, images):
        directory.extend(struct.pack(
            "<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(data), offset
        ))
        offset += len(data)
    return directory + b"".join(images)


def write(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)


def generate(destination):
    sources = {}
    for name in ("milky_way", "shooting_star"):
        width, height, pixels = read_png(ROOT / "assets" / f"{name}_3d.png")
        assert (width, height) == (256, 256)
        sources[name] = pixels
    # Tray icons drop the emoji's 16px transparent margin (content is 224x224 of 256) so
    # they appear as large as other tray icons; app icons keep the original framing.
    tray = {
        name: crop(pixels, 256, TRAY_TRIM, TRAY_TRIM, 256 - 2 * TRAY_TRIM)
        for name, pixels in sources.items()
    }
    tray_edge = 256 - 2 * TRAY_TRIM
    for size in TRAY_SIZES:
        idle = resize(tray["milky_way"], size, tray_edge)
        states = {
            "idle": idle,
            "connecting": badge(idle, size, (0xF5, 0xC5, 0x18)),
            "streaming": resize(tray["shooting_star"], size, tray_edge),
            "error": badge(idle, size, (0xE5, 0x48, 0x4D)),
        }
        for state, pixels in states.items():
            write(destination / "tray" / f"{state}-{size}.png", png(size, pixels))
            write(destination / "tray" / f"{state}-{size}.rgba", pixels)
    for size in APP_SIZES:
        write(
            destination / "hicolor" / f"{size}x{size}" / "apps" / "nyx-refrain.png",
            png(size, resize(sources["shooting_star"], size)),
        )
    for name, source in (("nyx-refrain", "shooting_star"), ("nyxr", "milky_way")):
        write(destination / "windows" / f"{name}.ico", ico(sources[source]))


if __name__ == "__main__":
    generate(Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "assets" / "icons")
