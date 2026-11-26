#!/usr/bin/env python3
"""Convert the Erik Satie portrait to the `satie` layout's C64 picture.

    python3 -I tools/satie-picture.py SOURCE.jpg assets/satie.bin [preview.png]

SOURCE is "Ericsatie.jpg" from Wikimedia Commons (Henri Manuel, about 1920,
public domain; see docs/CREDITS.md). The result is a 160x200 multicolour
bitmap like a C64 loading screen: every pixel twice as wide as tall, six
brightness levels (0 black to 5 white) after 4x4 ordered (Bayer) dithering,
packed two pixels to a byte (high nibble first), 16,000 bytes. NIRULOCK maps
the levels to colours from the active theme at run time.

Needs Pillow. The conversion is deterministic: the same source gives the
same bytes.
"""
import hashlib
import sys

from PIL import Image, ImageFilter, ImageOps

W, H, LEVELS = 160, 200, 6
# The crop in the source (2651x3711): head and shoulders, 1.6 times as wide
# as tall (the picture shows 320x200 square pixels), padded with black on
# the left where the photo ends.
CROP = (-300, 330, 2740, 2230)
BAYER = [
    [0, 8, 2, 10],
    [12, 4, 14, 6],
    [3, 11, 1, 9],
    [15, 7, 13, 5],
]


def convert(source):
    image = Image.open(source).convert("L")
    image = image.crop(CROP)
    # Wide pixels: 3040 source pixels become 160, 1900 become 200.
    image = image.resize((W, H), Image.LANCZOS)
    image = ImageOps.autocontrast(image, cutoff=1)
    image = image.filter(ImageFilter.UnsharpMask(radius=1, percent=80, threshold=2))
    pixels = image.load()
    out = []
    for y in range(H):
        for x in range(W):
            v = pixels[x, y] / 255 * (LEVELS - 1)
            base = int(v)
            frac = v - base
            threshold = (BAYER[y % 4][x % 4] + 0.5) / 16
            level = min(LEVELS - 1, base + (1 if frac > threshold else 0))
            out.append(level)
    return out


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    levels = convert(sys.argv[1])
    packed = bytes((levels[i] << 4) | levels[i + 1] for i in range(0, len(levels), 2))
    with open(sys.argv[2], "wb") as f:
        f.write(packed)
    print(f"{sys.argv[2]}: {len(packed)} bytes, sha256 {hashlib.sha256(packed).hexdigest()}")
    if len(sys.argv) > 3:
        grey = [round(level * 255 / (LEVELS - 1)) for level in levels]
        preview = Image.new("L", (W, H))
        preview.putdata(grey)
        preview.resize((W * 4, H * 2), Image.NEAREST).save(sys.argv[3])


if __name__ == "__main__":
    main()
