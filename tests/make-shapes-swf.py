#!/usr/bin/env python3
"""A movie that shows a different shape on every frame, each one only once.

    make-shapes-swf.py <out.swf> [frames]

Frame k shows shape k alone - a rectangle of its own size, place and colour -
and the movie stops on its last frame. No shape is drawn twice, so a shape
keeps whatever the renderer gave it on the frame it was drawn.

It is what run-frontend.sh's engine:rebuild-frees test loads before every
frame: each load builds a new renderer, and a shape drawn under the old one
and never drawn again is exactly what used to keep the old renderer alive
(Corporate Climber, 2026-10-22).
"""
import struct
import sys


class Bits:
    def __init__(self):
        self.bits = ""

    def u(self, value, n):
        self.bits += format(value, "0%db" % n)

    def s(self, value, n):
        self.u(value & ((1 << n) - 1), n)

    def bytes(self):
        b = self.bits + "0" * (-len(self.bits) % 8)
        return bytes(int(b[i:i + 8], 2) for i in range(0, len(b), 8))


def signed_bits(*values):
    return max(max(abs(v) for v in values).bit_length() + 1, 2)


def rect(x0, x1, y0, y1):
    n = signed_bits(x0, x1, y0, y1)
    w = Bits()
    w.u(n, 5)
    for v in (x0, x1, y0, y1):
        w.s(v, n)
    return w.bytes()


def tag(code, body=b""):
    if len(body) < 63:
        return struct.pack("<H", (code << 6) | len(body)) + body
    return struct.pack("<HI", (code << 6) | 63, len(body)) + body


def shape(shape_id, x, y, width, height, rgb):
    """DefineShape: one solid rectangle, in twips."""
    w = Bits()
    w.u(1, 4)  # fill style bits
    w.u(0, 4)  # line style bits
    # style change: move to the corner, fill style 1 on the right of the edges
    n = signed_bits(x, y)
    w.u(0, 1)
    w.u(0b00101, 5)  # new styles 0, line style 0, fill style 1, fill style 0 no, move to
    w.u(n, 5)
    w.s(x, n)
    w.s(y, n)
    w.u(1, 1)
    for dx, dy in ((width, 0), (0, height), (-width, 0), (0, -height)):
        n = signed_bits(dx, dy)
        w.u(1, 1)  # edge
        w.u(1, 1)  # straight
        w.u(n - 2, 4)
        w.u(1, 1)  # general line
        w.s(dx, n)
        w.s(dy, n)
    w.u(0, 6)  # end of shape
    styles = bytes([1, 0x00]) + bytes(rgb) + bytes([0])  # one solid fill, no lines
    body = struct.pack("<H", shape_id) + rect(x, x + width, y, y + height) + styles + w.bytes()
    return tag(2, body)


def place(shape_id, first):
    flags = 0x02 if first else 0x03  # has character, and move (replace) after the first
    return tag(26, bytes([flags]) + struct.pack("<HH", 1, shape_id))


frames = int(sys.argv[2]) if len(sys.argv) > 2 else 120
stage = 4000  # 200 pixels
body = rect(0, stage, 0, stage) + bytes([0, 30]) + struct.pack("<H", frames)
body += tag(9, bytes([255, 255, 255]))
for k in range(frames):
    side = 400 + (k * 37) % 2400
    x = (k * 131) % (stage - side)
    y = (k * 197) % (stage - side)
    rgb = ((k * 53) % 256, (k * 101) % 256, (k * 197) % 256)
    body += shape(k + 1, x, y, side, side, rgb) + place(k + 1, k == 0)
    if k == frames - 1:
        body += tag(12, b"\x07\x00")  # stop
    body += tag(1)
body += tag(0)
open(sys.argv[1], "wb").write(b"FWS" + bytes([8]) + struct.pack("<I", 8 + len(body)) + body)
