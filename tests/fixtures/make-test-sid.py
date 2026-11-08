#!/usr/bin/env python3
"""Write a tiny, original PSID test tune (MIT, like NIRULOCK).

    python3 make-test-sid.py OUT.sid [TITLE] [NOTE-OFFSET]

A sawtooth arpeggio on voice 1 at 50 Hz, with its own 6502 init and play
routines. It exists only to test [pause_music]; it is not a copyrighted tune.
"""
import struct
import sys

LOAD = 0x1000
out = sys.argv[1]
title = sys.argv[2] if len(sys.argv) > 2 else "NIRULOCK test tune"
shift = int(sys.argv[3]) if len(sys.argv) > 3 else 0

# PAL: register value = Hz * 16777216 / 985248.
def reg(hz):
    return round(hz * 16777216 / 985248)

notes = [261.63, 329.63, 392.00, 523.25, 392.00, 329.63, 293.66, 349.23]
notes = [n * 2 ** (shift / 12) for n in notes]
lo = bytes(reg(n) & 0xFF for n in notes)
hi = bytes(reg(n) >> 8 for n in notes)

INIT, PLAY = LOAD, LOAD + 0x20
IDX, CNT, LO, HI = LOAD + 0x60, LOAD + 0x61, LOAD + 0x70, LOAD + 0x78
w = lambda a: struct.pack("<H", a)
init = (b"\xa9\x0f\x8d" + w(0xD418) + b"\xa9\x09\x8d" + w(0xD405)
        + b"\xa9\x00\x8d" + w(0xD406) + b"\xa9\x00\x8d" + w(IDX)
        + b"\x8d" + w(CNT) + b"\x60")
play = (b"\xce" + w(CNT) + b"\x10\x00"            # DEC cnt; BPL done
        + b"\xa9\x0b\x8d" + w(CNT)                # LDA #11; STA cnt
        + b"\xae" + w(IDX)                        # LDX idx
        + b"\xbd" + w(LO) + b"\x8d" + w(0xD400)   # LDA lo,X; STA $D400
        + b"\xbd" + w(HI) + b"\x8d" + w(0xD401)   # LDA hi,X; STA $D401
        + b"\xa9\x20\x8d" + w(0xD404)             # gate off
        + b"\xa9\x21\x8d" + w(0xD404)             # saw + gate
        + b"\xe8\xe0\x08\xd0\x02\xa2\x00"         # INX; CPX #8; BNE +2; LDX #0
        + b"\x8e" + w(IDX)                        # STX idx
        + b"\x60")                                # done: RTS
# The BPL offset lands on the final RTS.
play = play[:4] + bytes([len(play) - 6]) + play[5:]
image = bytearray(0x80)
image[0:len(init)] = init
image[0x20:0x20 + len(play)] = play
image[0x70:0x78] = lo
image[0x78:0x80] = hi

def field(text):
    data = text.encode("latin-1")[:31]
    return data + b"\0" * (32 - len(data))

header = (b"PSID" + struct.pack(">HHHHHHHI", 2, 0x7C, LOAD, INIT, PLAY, 1, 1, 0)
          + field(title) + field("NIRUX") + field("2026 NIRUX (MIT)")
          + struct.pack(">HBBH", 0x14, 0, 0, 0))
assert len(header) == 0x7C
with open(out, "wb") as f:
    f.write(header + bytes(image))
