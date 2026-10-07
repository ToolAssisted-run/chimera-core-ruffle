#!/usr/bin/env python3
"""A three-frame AVM1 movie that keeps one number and says what it is.

    make-counter-swf.py <out.swf>

    frame 1:  v = 123456789;
    frame 2:  v = v + 1; trace(v);
    frame 3:  gotoAndPlay(2);

So v goes up by one every frame (a gotoAndPlay runs the frame it lands on at
once, so frames 3 and 2 are one step of the movie), and the trace says its
value each time.
It is what the gate's Heap-bus leg looks for in the core's heap (chimera#216):
a number a movie keeps is an f64 somewhere in it, a number nobody else has
(the pushed constant sits in the movie's bytes with its two halves swapped,
the SWF's way, so it is not the same eight bytes).
"""
import struct
import sys


def rect(x1, y1):
    n = max(x1.bit_length(), y1.bit_length()) + 1
    bits = format(n, "05b") + "".join(format(v, "0%db" % n) for v in (0, x1, 0, y1))
    bits += "0" * (-len(bits) % 8)
    return bytes(int(bits[i:i + 8], 2) for i in range(0, len(bits), 8))


def tag(code, body=b""):
    if len(body) < 63:
        return struct.pack("<H", (code << 6) | len(body)) + body
    return struct.pack("<HI", (code << 6) | 63, len(body)) + body


def push(*items):
    out = b""
    for item in items:
        if isinstance(item, str):
            out += b"\x00" + item.encode() + b"\0"
        else:  # a double: SWF stores its high half first
            p = struct.pack("<d", float(item))
            out += b"\x06" + p[4:] + p[:4]
    return b"\x96" + struct.pack("<H", len(out)) + out


GET, SET, ADD, TRACE, PLAY, END = b"\x1c", b"\x1d", b"\x47", b"\x26", b"\x06", b"\x00"
frame1 = push("v", 123456789) + SET + END
frame2 = push("v", "v") + GET + push(1) + ADD + SET + push("v") + GET + TRACE + END
frame3 = b"\x81" + struct.pack("<HH", 2, 1) + PLAY + END  # GotoFrame 1 (the second), then play

body = rect(2000, 2000) + bytes([0, 50]) + struct.pack("<H", 3)
body += tag(9, bytes([0, 0, 0]))
for actions in (frame1, frame2, frame3):
    body += tag(12, actions) + tag(1)
body += tag(0)
open(sys.argv[1], "wb").write(b"FWS" + bytes([8]) + struct.pack("<I", 8 + len(body)) + body)
