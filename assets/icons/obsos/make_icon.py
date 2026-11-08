#!/usr/bin/env python3
"""OBSOS browser icon — procedural (no deps): rounded tile with a violet→teal
gradient, an open white ring ("O", gap upper-right), the Earth inside the ring
(oceans, continents, lit from the Sun) and a small yellow Sun far away in the
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
SUN_CORE = hexc('#fff6c8')
SUN_EDGE = hexc('#ffc43a')
OCEAN_LIT = hexc('#3a9cf0')
OCEAN_DARK = hexc('#0a3f8f')
LAND_LIT = hexc('#5cc65a')
LAND_DARK = hexc('#1f6b32')
ATMO = hexc('#9fd8ff')
SHADOW = (0.08, 0.04, 0.25)

GAP_FROM = -70.0   # degrees, screen coords (y down): gap from -70°…
GAP_TO = 10.0      # …to +10° (upper right)
SUN_AT = -30.0

# Continents: ellipses in disc coords (-1..1): cx, cy, rx, ry, angle (deg).
LAND = [
    # North America
    (-0.38, -0.36, 0.36, 0.22, -20),
    (-0.58, -0.12, 0.14, 0.20, 10),
    (-0.22, -0.05, 0.10, 0.16, 35),
    # South America
    (-0.10, 0.36, 0.20, 0.30, -18),
    (-0.02, 0.68, 0.09, 0.16, -12),
    # Greenland
    (-0.02, -0.74, 0.14, 0.08, 0),
    # Europe
    (0.42, -0.48, 0.22, 0.10, 15),
    # Africa
    (0.52, -0.12, 0.26, 0.17, 0),
    (0.54, 0.22, 0.20, 0.30, 10),
]


def in_land(u, v):
    for cx, cy, rx, ry, ang in LAND:
        a = math.radians(ang)
        dx, dy = u - cx, v - cy
        x = dx * math.cos(a) + dy * math.sin(a)
        y = -dx * math.sin(a) + dy * math.cos(a)
        if (x / rx) ** 2 + (y / ry) ** 2 <= 1.0:
            return True
    return False


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
    pr = 0.05 if size >= 32 else 0.075
    pa = math.radians(SUN_AT)
    pcx, pcy = cx + ring_r * math.cos(pa), cy + ring_r * math.sin(pa)
    # Earth inside the ring.
    er = ring_r - ring_t / 2 - (0.022 if size >= 32 else 0.015)
    # light comes from the Sun direction
    lx, ly = math.cos(pa), math.sin(pa)
    lz = 0.55
    ln = math.sqrt(lx * lx + ly * ly + lz * lz)
    lx, ly, lz = lx / ln, ly / ln, lz / ln
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
                    # Earth: sphere shading, oceans and continents, atmosphere rim
                    ed = math.hypot(x - cx, y - cy)
                    if ed < er + 0.018 and ed >= er:
                        a = (1.0 - (ed - er) / 0.018) * 0.55
                        col = mix(col, ATMO, a)
                    if ed < er:
                        u, v = (x - cx) / er, (y - cy) / er
                        z = math.sqrt(max(0.0, 1.0 - u * u - v * v))
                        lam = max(0.0, u * lx + v * ly + z * lz)
                        shade = 0.35 + 0.65 * lam
                        if v < -0.86 or v > 0.9:
                            base = mix((0.62, 0.70, 0.80), (1, 1, 1), shade)  # polar ice
                        elif in_land(u, v):
                            base = mix(LAND_DARK, LAND_LIT, shade)
                        else:
                            base = mix(OCEAN_DARK, OCEAN_LIT, shade)
                        # atmosphere haze towards the limb
                        base = mix(base, ATMO, (1.0 - z) ** 3 * 0.45)
                        col = base
                    # Sun far away: glow + small bright disc
                    pd = math.hypot(x - pcx, y - pcy) - pr
                    if pd < 0.06:
                        g = max(0.0, 1.0 - max(pd, 0.0) / 0.06) ** 2 * 0.6
                        col = mix(col, SUN_EDGE, g)
                    if pd < 0:
                        k = min(1.0, math.hypot(x - pcx, y - pcy) / pr)
                        col = mix(SUN_CORE, SUN_EDGE, k * k)
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
