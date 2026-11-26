#!/usr/bin/env python3
"""Draw the `bbs` layout's ANSI screen: assets/nirux-bbs.ans.

    python3 -I tools/bbs-art.py assets/nirux-bbs.ans

The screen is a real ANSI file of the BBS era: CP437 characters and ANSI
colour codes, 80 columns by 29 rows, with a SAUCE record at the end (iCE
colours: a blinking attribute means a bright background). It holds what
does not change: the Yggdrasil picture in its frame, the NIRUX logo, the
boxes and their labels. NIRULOCK draws the rest into it at run time (the
values, the message of the day, the tune and the login prompt), at the
places named in src/bbs.rs, and maps the 16 ANSI colours to the active
NIRUX theme.

The picture is drawn here from shapes (no source image): the world tree's
crown as overlapping circles lit from the upper right, the trunk, the roots
and Urd's well under them, the moon and the stars, painted in a few ANSI
colours with ordered dithering and then fitted cell by cell to the block and
shade characters (space, the half and full blocks and the three shades) and
two colours each, the way ANSI artists drew with them. Deterministic: the
same script gives the same bytes. Needs only Python 3.
"""
import hashlib
import math
import sys

COLS, ROWS = 80, 29
CW, CH = 8, 16

# A reference palette for the fitting: the ANSI colours as NIRULOCK uses
# them (roles, see src/bbs.rs), in a typical theme.
REF = {
    0: (0, 0, 0),
    1: (190, 80, 80),
    2: (70, 125, 70),       # leaves
    3: (110, 80, 60),       # bark, accent dark
    4: (30, 45, 75),        # deep water
    5: (32, 62, 36),        # dark leaves
    6: (70, 110, 160),      # water
    7: (175, 175, 175),
    8: (64, 64, 64),
    9: (240, 160, 160),
    10: (150, 200, 140),    # lit leaves
    11: (225, 200, 180),    # accent light
    12: (176, 141, 125),    # accent
    13: (128, 128, 128),
    14: (150, 190, 230),
    15: (255, 255, 255),
}

BAYER = [
    [0, 8, 2, 10],
    [12, 4, 14, 6],
    [3, 11, 1, 9],
    [15, 7, 13, 5],
]

# CP437 codes used here.
FULL, UPPER, LOWER, LEFT, RIGHT = 0xDB, 0xDF, 0xDC, 0xDD, 0xDE
LIGHT, MEDIUM, DARK = 0xB0, 0xB1, 0xB2
SPACE = 0x20
DOT, BULLET = 0xFA, 0xF9
BOX = {
    "═": 0xCD, "║": 0xBA, "╔": 0xC9, "╗": 0xBB, "╚": 0xC8, "╝": 0xBC,
    "─": 0xC4, "│": 0xB3, "┌": 0xDA, "┐": 0xBF, "└": 0xC0, "┘": 0xD9,
    "╡": 0xB5, "╞": 0xC6, "┤": 0xB4, "├": 0xC3, "·": 0xFA, "∙": 0xF9,
    "░": LIGHT, "▒": MEDIUM, "▓": DARK, "█": FULL, "▀": UPPER, "▄": LOWER,
    "■": 0xFE,
}


def hash01(x, y, seed=0):
    h = (x * 374761393 + y * 668265263 + seed * 2246822519) & 0xFFFFFFFF
    h = ((h ^ (h >> 13)) * 1274126177) & 0xFFFFFFFF
    return ((h ^ (h >> 16)) & 0xFFFF) / 65535.0


def value_noise(x, y, scale, seed):
    gx, gy = x / scale, y / scale
    x0, y0 = int(math.floor(gx)), int(math.floor(gy))
    fx, fy = gx - x0, gy - y0
    fx, fy = fx * fx * (3 - 2 * fx), fy * fy * (3 - 2 * fy)
    a = hash01(x0, y0, seed)
    b = hash01(x0 + 1, y0, seed)
    c = hash01(x0, y0 + 1, seed)
    d = hash01(x0 + 1, y0 + 1, seed)
    return (a * (1 - fx) + b * fx) * (1 - fy) + (c * (1 - fx) + d * fx) * fy


def ramp(colours, tone, x, y):
    """`tone` 0..1 along the colour ramp, ordered dithered per 2x4 block."""
    tone = min(max(tone, 0.0), 0.9999)
    v = tone * (len(colours) - 1)
    base = int(v)
    frac = v - base
    threshold = (BAYER[(y // 4) % 4][(x // 2) % 4] + 0.5) / 16
    return colours[min(len(colours) - 1, base + (1 if frac > threshold else 0))]


# ---- the tree ---------------------------------------------------------------

TW, TH = 33 * CW, 27 * CH  # 264 x 432
LEAVES = [0, 5, 2, 10]
BARK = [0, 8, 3, 13]
LIGHT_DIR = (0.5, -0.86)  # the light comes from the upper right


def clusters():
    """The crown: balls of leaves in a wide dome, back to front."""
    out = []
    i = 0
    while len(out) < 64:
        i += 1
        x = 132 + (hash01(i, 11, 3) * 2 - 1) * 124
        y = 30 + hash01(i, 12, 3) * 220
        dome = ((x - 132) / 124) ** 2 + ((y - 168) / 140) ** 2
        if dome > 1.0 or y > 240:
            continue
        r = 14 + 13 * hash01(i, 14, 3)
        out.append((x, y, r))
    # The lower balls in front of the upper ones.
    out.sort(key=lambda c: c[1] + c[2] * 0.5)
    return out


CLUSTERS = clusters()
BRANCHES = [
    ([(132, 262), (110, 222), (70, 196)], 10, 3),
    ([(134, 262), (160, 220), (198, 194)], 10, 3),
    ([(132, 266), (130, 210), (134, 150)], 11, 3),
]
ROOTS = [
    ([(112, 352), (80, 374), (40, 390), (4, 398)], 14, 3),
    ([(154, 352), (188, 374), (228, 388), (262, 396)], 14, 3),
    ([(132, 360), (130, 388), (126, 412), (122, 432)], 11, 3),
    ([(120, 358), (98, 392), (70, 416), (44, 432)], 8, 2),
    ([(144, 358), (168, 394), (198, 416), (224, 432)], 8, 2),
]


def segment_distance(px, py, ax, ay, bx, by):
    dx, dy = bx - ax, by - ay
    t = max(0.0, min(1.0, ((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)))
    return math.hypot(px - (ax + t * dx), py - (ay + t * dy)), t


def limb(px, py, points, w0, w1):
    """Inside a tapered polyline (a branch or a root): the shade across it,
    -1 (left edge) .. 1 (right edge), or None."""
    total = len(points) - 1
    for i in range(total):
        (ax, ay), (bx, by) = points[i], points[i + 1]
        d, t = segment_distance(px, py, ax, ay, bx, by)
        u = (i + t) / total
        w = w0 + (w1 - w0) * u
        if d <= w:
            side = (bx - ax) * (py - ay) - (by - ay) * (px - ax)
            return math.copysign(d / w, -side)
    return None


def leaves(x, y):
    """The crown's colour at (x, y), or None outside it."""
    hit = None
    for cx, cy, r in CLUSTERS:
        dx, dy = (x - cx) / r, (y - cy) / r
        # A ragged edge: leaves, not a circle.
        rr = 1.0 + 0.18 * (value_noise(x, y, 5, 1) - 0.5)
        if dx * dx + dy * dy <= rr * rr:
            hit = (dx, dy)
    if hit is None:
        # Shadowed leaves deep inside the crown, between the balls.
        dome = ((x - 132) / 116) ** 2 + ((y - 165) / 128) ** 2
        if dome <= 1.0 and 40 < y < 236:
            return ramp(LEAVES, 0.2 + 0.25 * value_noise(x, y, 5, 6), x, y)
        return None
    dx, dy = hit
    light = dx * LIGHT_DIR[0] + dy * LIGHT_DIR[1]
    rim = max(0.0, math.hypot(dx, dy) - 0.7) * 2.2
    tone = 0.46 + 0.6 * light - 0.55 * rim + 0.2 * (value_noise(x, y, 4, 2) - 0.5)
    # Deeper in shadow low in the crown.
    tone -= 0.3 * max(0.0, (y - 190) / 60)
    return ramp(LEAVES, tone, x, y)


def tree_pixel(x, y):
    # The roots, the trunk, the branches, the crown, the ground, the moon.
    for points, w0, w1 in ROOTS:
        s = limb(x, y, points, w0, w1)
        if s is not None:
            return ramp(BARK, 0.5 + 0.45 * s + 0.15 * (value_noise(x, y, 6, 3) - 0.5), x, y)
    if 200 <= y <= 366:
        u = (y - 200) / 166
        half = 19 + 9 * u + 160 * max(0.0, u - 0.8) ** 2
        centre = 132 + 3 * math.sin(y / 30)
        if abs(x - centre) <= half:
            s = (x - centre) / half
            bark = value_noise(x * 4, y / 4, 6, 4) - 0.5
            return ramp(BARK, 0.48 + 0.45 * s + 0.35 * bark, x, y)
    c = leaves(x, y)
    if c is not None:
        return c
    for points, w0, w1 in BRANCHES:
        s = limb(x, y, points, w0, w1)
        if s is not None:
            return ramp(BARK, 0.45 + 0.4 * s, x, y)
    if y >= 398:
        g = value_noise(x, y, 9, 5)
        return ramp([0, 8], 0.15 + 0.55 * g * min(1.0, (y - 398) / 24), x, y)
    # The full moon at the upper left, with its seas.
    d = math.hypot(x - 28, y - 30)
    if d <= 19:
        seas = value_noise(x, y, 6, 8) > 0.62
        tone = 0.75 - 0.6 * (d / 19) ** 3 + 0.12 * ((28 - x) - (y - 30)) / 19
        return ramp([8, 7, 15], tone - (0.35 if seas else 0.0), x, y)
    return 0


# ---- fitting cells --------------------------------------------------------------

def masks():
    """Coverage of each glyph's foreground in 4x4 sub-blocks (2x4 pixels)."""
    out = {}
    for code in (SPACE, FULL, UPPER, LOWER, LEFT, RIGHT, LIGHT, MEDIUM, DARK):
        m = []
        for by in range(4):
            for bx in range(4):
                if code == SPACE:
                    v = 0.0
                elif code == FULL:
                    v = 1.0
                elif code == UPPER:
                    v = 1.0 if by < 2 else 0.0
                elif code == LOWER:
                    v = 1.0 if by >= 2 else 0.0
                elif code == LEFT:
                    v = 1.0 if bx < 2 else 0.0
                elif code == RIGHT:
                    v = 1.0 if bx >= 2 else 0.0
                else:
                    v = {LIGHT: 0.25, MEDIUM: 0.5, DARK: 0.75}[code]
                m.append(v)
        out[code] = m
    return out


MASKS = masks()
# A small cost for the shades, so a flat area stays solid.
COST = {SPACE: 0, FULL: 0, UPPER: 0, LOWER: 0, LEFT: 30, RIGHT: 30,
        LIGHT: 60, MEDIUM: 60, DARK: 60}


def fit(pixel, cols, rows, ox=0, oy=0):
    """Fit `pixel(x, y)` (ANSI indices) over `cols` x `rows` cells."""
    cells = []
    for row in range(rows):
        line = []
        for col in range(cols):
            blocks = []
            used = set()
            for by in range(4):
                for bx in range(4):
                    acc = [0, 0, 0]
                    for yy in range(4):
                        for xx in range(2):
                            x = ox + col * CW + bx * 2 + xx
                            y = oy + row * CH + by * 4 + yy
                            c = pixel(x, y)
                            used.add(c)
                            r = REF[c]
                            acc[0] += r[0]
                            acc[1] += r[1]
                            acc[2] += r[2]
                    blocks.append(tuple(a / 8 for a in acc))
            used = sorted(used)
            if len(used) == 1:
                c = used[0]
                line.append((SPACE, 7, 0) if c == 0 else (FULL, c, 0))
                continue
            best = None
            for code, m in MASKS.items():
                for fg in used:
                    for bg in used:
                        if fg == bg and code != FULL:
                            continue
                        f, b = REF[fg], REF[bg]
                        err = COST[code]
                        for k, (r, g, bl) in enumerate(blocks):
                            a = m[k]
                            err += (r - (f[0] * a + b[0] * (1 - a))) ** 2
                            err += (g - (f[1] * a + b[1] * (1 - a))) ** 2
                            err += (bl - (f[2] * a + b[2] * (1 - a))) ** 2
                        if best is None or err < best[0]:
                            best = (err, code, fg, bg)
            _, code, fg, bg = best
            if code == SPACE:
                fg = 7
            line.append((code, fg, bg))
        cells.append(line)
    return cells


# ---- the logo -----------------------------------------------------------------

LETTERS = {
    "N": [
        "##....##", "###...##", "####..##", "##.##.##", "##.##.##",
        "##..####", "##...###", "##....##", "##....##", "##....##",
    ],
    "I": ["######", "..##..", "..##..", "..##..", "..##..",
          "..##..", "..##..", "..##..", "..##..", "######"],
    "R": [
        "#######.", "##....##", "##....##", "##....##", "#######.",
        "##.##...", "##..##..", "##...##.", "##....##", "##....##",
    ],
    "U": [
        "##....##", "##....##", "##....##", "##....##", "##....##",
        "##....##", "##....##", "##....##", ".##..##.", "..####..",
    ],
    "X": [
        "##....##", "##....##", ".##..##.", "..####..", "...##...",
        "..####..", ".##..##.", "##....##", "##....##", "##....##",
    ],
}
# Top to bottom: shine, accent light, accent, accent dark.
LOGO_ROWS = [15, 11, 11, 11, 12, 12, 12, 3, 3, 3]
SHADOW = 8


def logo_cells(word):
    pixels = {}
    x = 0
    for ch in word:
        rows = LETTERS[ch]
        for y, row in enumerate(rows):
            for dx, c in enumerate(row):
                if c == "#":
                    pixels[(x + dx, y)] = LOGO_ROWS[y]
        x += len(rows[0]) + 1
    width = x
    # A drop shadow one pixel right and down.
    for (px, py), _ in list(pixels.items()):
        if (px + 1, py + 1) not in pixels:
            pixels.setdefault((px + 1, py + 1), SHADOW)
    rows = []
    for row in range(6):
        line = []
        for col in range(width + 1):
            top = pixels.get((col, row * 2), 0)
            bottom = pixels.get((col, row * 2 + 1), 0)
            if top == bottom:
                line.append((SPACE, 7, 0) if top == 0 else (FULL, top, 0))
            elif bottom == 0:
                line.append((UPPER, top, 0))
            elif top == 0:
                line.append((LOWER, bottom, 0))
            else:
                line.append((UPPER, top, bottom))
        rows.append(line)
    return rows, width + 1


# ---- the screen -----------------------------------------------------------------

def screen():
    grid = [[(SPACE, 7, 0) for _ in range(COLS)] for _ in range(ROWS)]

    def put(col, row, text, fg, bg=0):
        for i, ch in enumerate(text):
            code = BOX.get(ch, ord(ch))
            if 0 <= col + i < COLS:
                grid[row][col + i] = (code, fg, bg)

    def box(c0, r0, c1, r1, fg, double=False):
        h, v = ("═", "║") if double else ("─", "│")
        tl, tr, bl, br = ("╔", "╗", "╚", "╝") if double else ("┌", "┐", "└", "┘")
        put(c0, r0, tl + h * (c1 - c0 - 1) + tr, fg)
        put(c0, r1, bl + h * (c1 - c0 - 1) + br, fg)
        for r in range(r0 + 1, r1):
            put(c0, r, v, fg)
            put(c1, r, v, fg)

    def title(c0, row, text, fg, bracket):
        put(c0, row, "┤" if bracket == "single" else "╡", bracket_colour)
        put(c0 + 1, row, text, fg)
        put(c0 + 1 + len(text), row, "├" if bracket == "single" else "╞", bracket_colour)

    bracket_colour = 12
    # The tree in its double frame.
    box(0, 0, 34, 28, 3, double=True)
    tree = fit(tree_pixel, 33, 27)
    for r, line in enumerate(tree):
        for c, cell in enumerate(line):
            grid[1 + r][1 + c] = cell
    # Stars in the empty sky.
    for i in range(90):
        col = int(hash01(i, 1, 7) * 33)
        row = int(hash01(i, 2, 7) * 10)
        if grid[1 + row][1 + col][0] != SPACE:
            continue
        # Never right next to the crown or the moon.
        near = any(
            grid[1 + rr][1 + cc][0] != SPACE
            for rr in range(max(0, row - 1), min(27, row + 2))
            for cc in range(max(0, col - 1), min(33, col + 2))
        )
        if near:
            continue
        kind = hash01(i, 3, 7)
        if kind < 0.6:
            grid[1 + row][1 + col] = (DOT, 8, 0)
        elif kind < 0.85:
            grid[1 + row][1 + col] = (BULLET, 7, 0)
        else:
            grid[1 + row][1 + col] = (ord("*"), 15, 0)
    title(3, 0, " YGGDRASIL ", 11, "double")
    # The logo.
    cells, width = logo_cells("NIRUX")
    left = 36 + (44 - width) // 2
    for r, line in enumerate(cells):
        for c, cell in enumerate(line):
            grid[r][left + c] = cell
    put(36, 6, "▓▒░", 3)
    put(41, 6, "B U L L E T I N   B O A R D", 11)
    put(73, 6, "░▒▓", 3)
    # The system box.
    box(36, 8, 79, 15, 4)
    title(38, 8, " SYSTEM ", 14, "single")
    for i, label in enumerate(["NODE", "CONNECTION", "DATE", "TIME", "TIME ONLINE", "STATUS"]):
        put(38, 9 + i, label, 6)
        put(38 + len(label) + 1, 9 + i, "." * (14 - len(label) - 2), 8)
    # The message of the day.
    box(36, 16, 79, 21, 4)
    title(38, 16, " MESSAGE OF THE DAY ", 14, "single")
    return grid


def encode(grid):
    out = bytearray(b"\x1b[0m")
    for row in grid:
        # Trailing blank cells on black need not be sent.
        end = len(row)
        while end > 0 and row[end - 1][0] == SPACE and row[end - 1][2] == 0:
            end -= 1
        current = None
        for code, fg, bg in row[:end]:
            if code == SPACE:
                fg = current[0] if current and current[1] == bg else fg
            if current != (fg, bg):
                params = ["0"]
                if fg >= 8:
                    params.append("1")
                if bg >= 8:
                    params.append("5")
                params.append(str(30 + fg % 8))
                params.append(str(40 + bg % 8))
                out += b"\x1b[" + ";".join(params).encode() + b"m"
                current = (fg, bg)
            out.append(code)
        out += b"\x1b[0m\r\n"
        current = None
    return bytes(out)


def sauce(size):
    def field(text, n):
        return text.encode("ascii").ljust(n, b" ")[:n]

    record = bytearray(b"SAUCE00")
    record += field("NIRUX BBS", 35)
    record += field("NIRUX", 20)
    record += field("NIRULOCK", 20)
    record += b"20261126"
    record += size.to_bytes(4, "little")
    record += bytes([1, 1])  # character, ANSi
    record += (80).to_bytes(2, "little") + ROWS.to_bytes(2, "little")
    record += bytes(4)
    record += bytes([0, 0x01])  # no comments; iCE colours
    record += field("IBM VGA", 22).replace(b" ", b"\0")
    assert len(record) == 128
    return bytes(record)


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    data = encode(screen())
    data += b"\x1a" + sauce(len(data))
    with open(sys.argv[1], "wb") as f:
        f.write(data)
    print(f"{sys.argv[1]}: {len(data)} bytes, sha256 {hashlib.sha256(data).hexdigest()}")


if __name__ == "__main__":
    main()
