#!/usr/bin/env python3
"""OBSOS browser icon — procedural (no deps): rounded tile with a violet→teal
gradient, an open white ring ("O", gap upper-right) and an orange planet in the
gap. Renders PNGs at several sizes with 4x4 supersampling.

usage: obsos_icon.py <out_dir>
"""
import math
import struct
import sys
import zlib


def lerp(a, b, t):
    return a + (b - a) * t


def mix(c1, c2, t):
    return tuple(lerp(a, b, t) for a, b in zip(c1, c2))


def hexc(h):
    h = h.lstrip('#')
    return tuple(int(h[i:i + 2], 16) / 255 for i in (0, 2, 4))


TILE_A = hexc('#6a3cf0')   # violet, top-left
TILE_B = hexc('#10b8a8')   # teal, bottom-right
RING = hexc('#ffffff')
PLANET_A = hexc('#ffd04a')
PLANET_B = hexc('#ff5e3a')
SHADOW = (0.08, 0.04, 0.25)

GAP_FROM = -70.0   # degrees, screen coords (y down): gap from -70°…
GAP_TO = 10.0      # …to +10° (upper right)
PLANET_AT = -30.0


def sd_round_rect(x, y, cx, cy, hw, hh, r):
    qx = abs(x - cx) - (hw - r)
    qy = abs(y - cy) - (hh - r)
    ox, oy = max(qx, 0.0), max(qy, 0.0)
    return math.hypot(ox, oy) + min(max(qx, qy), 0.0) - r


def ring_cover(x, y, cx, cy, r, t):
    """Signed distance to an open ring (arc with round caps)."""
    dx, dy = x - cx, y - cy
    ang = math.degrees(math.atan2(dy, dx))
    in_gap = GAP_FROM <= ang <= GAP_TO
    if not in_gap:
        return abs(math.hypot(dx, dy) - r) - t / 2
    # nearest cap
    best = 1e9
    for a in (GAP_FROM, GAP_TO):
        px = cx + r * math.cos(math.radians(a))
        py = cy + r * math.sin(math.radians(a))
        best = min(best, math.hypot(x - px, y - py) - t / 2)
    return best


def render(size):
    S = float(size)
    ss = 4
    px = bytearray()
    cx, cy = 0.5, 0.52
    ring_r, ring_t = 0.255, 0.092 if size >= 32 else 0.11
    pr = 0.075 if size >= 32 else 0.09
    pa = math.radians(PLANET_AT)
    pcx, pcy = cx + ring_r * math.cos(pa), cy + ring_r * math.sin(pa)
    for j in range(size):
        px.append(0)  # filter byte
        for i in range(size):
            acc = [0.0, 0.0, 0.0, 0.0]
            for sj in range(ss):
                for si in range(ss):
                    x = (i + (si + 0.5) / ss) / S
                    y = (j + (sj + 0.5) / ss) / S
                    # tile
                    d = sd_round_rect(x, y, 0.5, 0.5, 0.46, 0.46, 0.2)
                    if d > 0:
                        continue
                    t = min(max((x + y) / 2, 0.0), 1.0)
                    col = mix(TILE_A, TILE_B, t)
                    # soft top highlight
                    hl = max(0.0, 1.0 - y / 0.55) * 0.10
                    col = mix(col, (1, 1, 1), hl)
                    # ring shadow (offset)
                    sd = ring_cover(x, y - 0.018, cx, cy, ring_r, ring_t)
                    if sd < 0.012:
                        a = min(1.0, (0.012 - sd) / 0.03) * 0.35
                        col = mix(col, SHADOW, a)
                    # ring
                    rd = ring_cover(x, y, cx, cy, ring_r, ring_t)
                    if rd < 0:
                        col = RING
                    # planet glow + body
                    pd = math.hypot(x - pcx, y - pcy) - pr
                    if pd < 0.05:
                        g = max(0.0, 1.0 - max(pd, 0.0) / 0.05) * 0.35
                        col = mix(col, PLANET_A, g)
                    if pd < 0:
                        u = min(max(((x - pcx) + (y - pcy)) / (2 * pr) + 0.5, 0.0), 1.0)
                        col = mix(PLANET_A, PLANET_B, u)
                        # tiny specular
                        if math.hypot(x - (pcx - pr * 0.35), y - (pcy - pr * 0.35)) < pr * 0.28:
                            col = mix(col, (1, 1, 1), 0.55)
                    acc[0] += col[0]
                    acc[1] += col[1]
                    acc[2] += col[2]
                    acc[3] += 1.0
            n = ss * ss
            a = acc[3] / n
            if acc[3] > 0:
                r, g, b = (acc[k] / acc[3] for k in range(3))
            else:
                r = g = b = 0.0
            px += bytes((int(r * 255 + 0.5), int(g * 255 + 0.5), int(b * 255 + 0.5), int(a * 255 + 0.5)))
    return bytes(px)


def png(size, raw):
    def chunk(tag, data):
        c = struct.pack('>I', len(data)) + tag + data
        return c + struct.pack('>I', zlib.crc32(tag + data) & 0xffffffff)
    ihdr = struct.pack('>IIBBBBB', size, size, 8, 6, 0, 0, 0)
    return b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', ihdr) + chunk(b'IDAT', zlib.compress(raw, 9)) + chunk(b'IEND', b'')


def main():
    out = sys.argv[1]
    for size in (16, 24, 32, 48, 64, 128, 256):
        raw = render(size)
        with open(f'{out}/obsos-{size}.png', 'wb') as f:
            f.write(png(size, raw))
        # raw RGBA too (for embedding without a PNG decoder)
        with open(f'{out}/obsos-{size}.rgba', 'wb') as f:
            f.write(bytes(b for row in range(size) for b in raw[row * (size * 4 + 1) + 1:(row + 1) * (size * 4 + 1)]))
        print('ok', size)


if __name__ == '__main__':
    main()
