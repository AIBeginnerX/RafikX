"""Generate RafikX desktop icons (PNG / ICO / ICNS) without extra deps.

Design: dark rounded-square badge, violet diagonal gradient, soft cyan orbit,
and a gold terminal-prompt motif (chevron '>' + underscore) with glow.
Rendered with signed-distance fields at high resolution, then downscaled
(box filter) for smooth anti-aliased edges.
"""
from __future__ import annotations

import math
import struct
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1] / "desktop" / "src-tauri" / "icons"

SS = 4          # supersampling factor
BASE = 512      # master render size


# ---------------------------------------------------------------- SDF helpers
def sd_round_rect(px: float, py: float, cx: float, cy: float, hx: float, hy: float, r: float) -> float:
    qx = abs(px - cx) - (hx - r)
    qy = abs(py - cy) - (hy - r)
    ox = max(qx, 0.0)
    oy = max(qy, 0.0)
    return math.hypot(ox, oy) + min(max(qx, qy), 0.0) - r


def sd_segment(px: float, py: float, ax: float, ay: float, bx: float, by: float) -> float:
    vx, vy = bx - ax, by - ay
    wx, wy = px - ax, py - ay
    t = max(0.0, min(1.0, (wx * vx + wy * vy) / (vx * vx + vy * vy + 1e-9)))
    dx, dy = wx - t * vx, wy - t * vy
    return math.hypot(dx, dy)


def sd_arc(px: float, py: float, cx: float, cy: float, radius: float, a0: float, a1: float) -> float:
    """Distance to a circular arc (angles in radians, ccw from a0 to a1)."""
    dx, dy = px - cx, py - cy
    ang = math.atan2(dy, dx)
    two_pi = 2.0 * math.pi

    def norm(a: float) -> float:
        while a < 0:
            a += two_pi
        while a >= two_pi:
            a -= two_pi
        return a

    rel = norm(ang - a0)
    span = norm(a1 - a0)
    if rel <= span:
        return abs(math.hypot(dx, dy) - radius)
    d0 = math.hypot(dx - radius * math.cos(a0), dy - radius * math.sin(a0))
    d1 = math.hypot(dx - radius * math.cos(a1), dy - radius * math.sin(a1))
    return min(d0, d1)


def mix(a: tuple[int, int, int], b: tuple[int, int, int], t: float) -> tuple[int, int, int]:
    t = max(0.0, min(1.0, t))
    return tuple(int(round(a[i] + (b[i] - a[i]) * t)) for i in range(3))


# ---------------------------------------------------------------- palette
NAVY_TOP = (16, 21, 48)
VIOLET_BOT = (44, 31, 142)
GOLD_HI = (244, 226, 178)
GOLD_LO = (212, 166, 74)
CYAN = (94, 231, 255)
WHITE_WARM = (255, 250, 236)


def render(size: int) -> bytes:
    n = size * SS
    scale = BASE / size
    buf = bytearray(n * n * 4)

    # geometry in BASE coordinate space
    half = BASE / 2
    corner_r = BASE * 0.235
    # chevron '>' : two strokes meeting at right vertex
    ch_ax, ch_ay = BASE * 0.30, BASE * 0.26
    ch_vx, ch_vy = BASE * 0.52, half
    ch_bx, ch_by = BASE * 0.30, BASE * 0.74
    stroke_w = BASE * 0.075
    # underscore
    us_y = BASE * 0.795
    us_x0, us_x1 = BASE * 0.55, BASE * 0.76
    us_w = BASE * 0.055

    for y in range(n):
        fy = (y + 0.5) / SS * scale
        row = y * n * 4
        for x in range(n):
            fx = (x + 0.5) / SS * scale

            # --- background: rounded square + diagonal gradient + vignette ---
            d_bg = sd_round_rect(fx, fy, half, half, half - 2, half - 2, corner_r)
            if d_bg > 1.5:
                alpha = 0
            else:
                alpha = 255 if d_bg > -1.5 else int(255 * (d_bg + 1.5) / 1.0 + 127)
                alpha = max(0, min(255, alpha))
                g = (fx + fy) / (2 * BASE)
                col = mix(NAVY_TOP, VIOLET_BOT, g ** 1.25)

                # vignette
                nx, ny = (fx - half) / half, (fy - half) / half
                rr = math.sqrt(nx * nx + ny * ny)
                vig = max(0.0, 1.0 - 0.42 * rr ** 2.4)
                col = tuple(int(c * (0.72 + 0.28 * vig)) for c in col)

                # inner top sheen
                sheen = max(0.0, 1.0 - abs(fy - BASE * 0.06) / (BASE * 0.30))
                col = mix(col, WHITE_WARM, 0.05 * sheen)

                # --- cyan orbit arc (behind glyph) ---
                d_orbit = sd_arc(fx, fy, half, half, BASE * 0.405, math.radians(-70), math.radians(150))
                orbit_glow = math.exp(-max(0.0, d_orbit) / (BASE * 0.055))
                orbit_line = 1.0 - min(1.0, abs(d_orbit) / (BASE * 0.008))
                oc = mix(col, CYAN, min(1.0, 0.20 * orbit_glow + 0.75 * orbit_line))
                col = oc

                # --- gold glyph with glow ---
                d_ch = min(
                    sd_segment(fx, fy, ch_ax, ch_ay, ch_vx, ch_vy),
                    sd_segment(fx, fy, ch_vx, ch_vy, ch_bx, ch_by),
                ) - stroke_w / 2
                d_us = sd_round_rect(fx, fy, (us_x0 + us_x1) / 2, us_y,
                                     (us_x1 - us_x0) / 2, us_w / 2, us_w / 2)
                d_glyph = min(d_ch, d_us)

                glow = math.exp(-max(0.0, d_glyph) / (BASE * 0.085))
                col = mix(col, GOLD_LO, 0.34 * glow)

                edge = 1.6
                if d_glyph < 0:
                    tt = max(0.0, min(1.0, (fx / BASE + 0.35)))
                    core = mix(GOLD_HI, GOLD_LO, tt)
                    # specular kiss on upper-left of strokes
                    spec = math.exp(-((fx - BASE * 0.36) ** 2 + (fy - BASE * 0.30) ** 2) / (2 * (BASE * 0.10) ** 2))
                    core = mix(core, WHITE_WARM, 0.45 * spec)
                    cov = min(1.0, (-d_glyph) / edge)
                    col = mix(col, core, cov)
                elif d_glyph < edge:
                    tt = max(0.0, min(1.0, (fx / BASE + 0.35)))
                    core = mix(GOLD_HI, GOLD_LO, tt)
                    col = mix(col, core, 1.0 - d_glyph / edge)

                rgb = [max(0, min(255, c)) for c in col]
            if d_bg > 1.5:
                vals = bytes((0, 0, 0, 0))
            else:
                vals = bytes((*rgb, alpha))
            buf[row + x * 4: row + x * 4 + 4] = vals

    return buf, n


def downscale(buf: bytearray, n: int, size: int) -> list[list[tuple[int, int, int, int]]]:
    f = n // size
    out: list[list[tuple[int, int, int, int]]] = []
    for y in range(size):
        row = []
        sy = y * f
        for x in range(size):
            sx = x * f
            r = g = b = a = 0
            for dy in range(f):
                base = ((sy + dy) * n + sx) * 4
                for dx in range(f):
                    i = base + dx * 4
                    r += buf[i]
                    g += buf[i + 1]
                    b += buf[i + 2]
                    a += buf[i + 3]
            cnt = f * f
            row.append((r // cnt, g // cnt, b // cnt, a // cnt))
        out.append(row)
    return out


def encode_png(pixels: list[list[tuple[int, int, int, int]]]) -> bytes:
    size = len(pixels)
    raw = b"".join(
        b"\x00" + b"".join(bytes(px) for px in row) for row in pixels
    )

    def chunk(tag: bytes, data: bytes) -> bytes:
        crc = zlib.crc32(tag + data) & 0xFFFFFFFF
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", crc)

    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")


def png(size: int) -> bytes:
    buf, n = render(size)
    return encode_png(downscale(buf, n, size))


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


def icns(png256: bytes) -> bytes:
    # ic09 = 256×256 PNG (modern macOS)
    inner = b"ic09" + struct.pack(">I", 8 + len(png256)) + png256
    return b"icns" + struct.pack(">I", 8 + len(inner)) + inner


def main() -> None:
    ROOT.mkdir(parents=True, exist_ok=True)
    p32 = png(32)
    p128 = png(128)
    p256 = png(256)
    (ROOT / "32x32.png").write_bytes(p32)
    (ROOT / "128x128.png").write_bytes(p128)
    (ROOT / "icon.ico").write_bytes(ico([(32, p32), (128, p128), (256, p256)]))
    (ROOT / "icon.icns").write_bytes(icns(p256))
    print(f"wrote refined icons in {ROOT}")


if __name__ == "__main__":
    main()
