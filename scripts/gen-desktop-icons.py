"""Generate RafikX desktop icons (PNG / ICO / ICNS) without extra deps."""
from __future__ import annotations

import struct
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1] / "desktop" / "src-tauri" / "icons"


def pixel(x: int, y: int, size: int) -> bytes:
    cx = cy = size / 2
    dx, dy = x - cx + 0.5, y - cy + 0.5
    r = (dx * dx + dy * dy) ** 0.5 / (size * 0.46)
    # space navy
    bgr = (3, 6, 14)
    gold = (232, 213, 163)
    violet = (107, 92, 255)
    cyan = (94, 231, 255)
    if r > 1.05:
        return bytes((*bgr, 0 if r > 1.12 else 255))
    t = max(0.0, min(1.0, 1.0 - r))
    mix_v = 0.35 + 0.65 * t
    ring = 1.0 if 0.72 < r < 0.92 else 0.0
    core = 1.0 if r < 0.28 else 0.0
    rgb = [
        int(bgr[i] * (1 - mix_v) + violet[i] * mix_v * 0.55 + gold[i] * core + cyan[i] * ring * 0.45)
        for i in range(3)
    ]
    rgb = [max(0, min(255, v)) for v in rgb]
    return bytes((*rgb, 255))


def png(size: int) -> bytes:
    raw = b"".join(b"\x00" + b"".join(pixel(x, y, size) for x in range(size)) for y in range(size))

    def chunk(tag: bytes, data: bytes) -> bytes:
        crc = zlib.crc32(tag + data) & 0xFFFFFFFF
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", crc)

    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")


def ico(images: list[tuple[int, bytes]]) -> bytes:
    count = len(images)
    header = struct.pack("<HHH", 0, 1, count)
    offset = 6 + 16 * count
    entries = b""
    bodies = b""
    for size, data in images:
        w = 0 if size >= 256 else size
        entries += struct.pack("<BBBBHHII", w, w, 0, 0, 1, 32, len(data), offset)
        bodies += data
        offset += len(data)
    return header + entries + bodies


def icns(png128: bytes) -> bytes:
    # ic07 = 128×128 PNG
    inner = b"ic07" + struct.pack(">I", 8 + len(png128)) + png128
    return b"icns" + struct.pack(">I", 8 + len(inner)) + inner


def main() -> None:
    ROOT.mkdir(parents=True, exist_ok=True)
    p32 = png(32)
    p128 = png(128)
    (ROOT / "32x32.png").write_bytes(p32)
    (ROOT / "128x128.png").write_bytes(p128)
    (ROOT / "icon.ico").write_bytes(ico([(32, p32), (128, p128)]))
    (ROOT / "icon.icns").write_bytes(icns(p128))
    print(f"wrote icons in {ROOT}")


if __name__ == "__main__":
    main()
