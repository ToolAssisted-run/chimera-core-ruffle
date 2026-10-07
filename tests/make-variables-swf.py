#!/usr/bin/env python3
"""An AVM1 movie that keeps one of everything a variable list can show.

    make-variables-swf.py <out.swf> [swf version, 8 by default] [grow]

    frame 1:  v = 123456789; flag = true; who = "hero";
              o = new Object(); o.hp = 7;
              o.inner = new Object(); o.inner.depth = 2;
              a = [10, 20, 30];
              _global.lives = 3;
    frame 2:  v = v + 1; trace(v);
    frame 3:  gotoAndPlay(2);

and a clip called "ship" at (100, 50) pixels whose own first frame says
fuel = 55. So the list the gate expects is known without running anything:
_root.v (a number that goes up by one a frame), _root.flag (a boolean),
_root.who (four characters), _root.o.hp, _root.o.inner.depth, _root.a[0..2],
_root.ship._x = 2000 twips, _root.ship._y = 1000, _root.ship.fuel, and
_global.lives. Version 6 makes the same movie with names that do not tell
upper case from lower (chimera#216).

With "grow", frame 2 also makes a new variable each time it runs (n123456790,
n123456791, ...). The clip's table of properties is then outgrown again and
again, and every time it is, v is kept somewhere else: the movie the gate
uses to see that a watched variable is followed when it moves.
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
        elif isinstance(item, bool):
            out += b"\x05" + bytes([item])
        elif isinstance(item, int):  # a count, for NewObject and InitArray
            out += b"\x07" + struct.pack("<I", item)
        else:  # a double: SWF stores its high half first
            p = struct.pack("<d", item)
            out += b"\x06" + p[4:] + p[:4]
    return b"\x96" + struct.pack("<H", len(out)) + out


GET, SET, ADD, TRACE, PLAY, END = b"\x1c", b"\x1d", b"\x47", b"\x26", b"\x06", b"\x00"
NEW, ARRAY, GETMEMBER, SETMEMBER = b"\x40", b"\x42", b"\x4e", b"\x4f"
new_object = push(0, "Object") + NEW
frame1 = (
    push("v", 123456789.0) + SET
    + push("flag", True) + SET
    + push("who", "hero") + SET
    + push("o") + new_object + SET
    + push("o") + GET + push("hp", 7.0) + SETMEMBER
    + push("o") + GET + push("inner") + new_object + SETMEMBER
    + push("o") + GET + push("inner") + GETMEMBER + push("depth", 2.0) + SETMEMBER
    + push("a", 30.0, 20.0, 10.0, 3) + ARRAY + SET
    + push("_global") + GET + push("lives", 3.0) + SETMEMBER
    + END
)
frame2 = push("v", "v") + GET + push(1.0) + ADD + SET + push("v") + GET + TRACE
if "grow" in sys.argv[3:]:
    frame2 += push("n", "v") + GET + b"\x21" + push("v") + GET + SET  # set("n" + v, v)
frame2 += END
frame3 = b"\x81" + struct.pack("<HH", 2, 1) + PLAY + END  # GotoFrame 1 (the second), then play

# the clip: one frame, one variable of its own
sprite = struct.pack("<HH", 1, 1) + tag(12, push("fuel", 55.0) + SET + END) + tag(1) + tag(0)


def translate(tx, ty):
    n = max(tx.bit_length(), ty.bit_length()) + 1
    bits = "00" + format(n, "05b") + format(tx, "0%db" % n) + format(ty, "0%db" % n)
    bits += "0" * (-len(bits) % 8)
    return bytes(int(bits[i:i + 8], 2) for i in range(0, len(bits), 8))


# PlaceObject2: a character, a matrix and a name, at depth 1
place = bytes([0x26]) + struct.pack("<HH", 1, 1) + translate(2000, 1000) + b"ship\0"

version = int(sys.argv[2]) if len(sys.argv) > 2 else 8
body = rect(4000, 4000) + bytes([0, 50]) + struct.pack("<H", 3)
body += tag(9, bytes([0, 0, 0]))
body += tag(39, sprite) + tag(26, place)
for actions in (frame1, frame2, frame3):
    body += tag(12, actions) + tag(1)
body += tag(0)
open(sys.argv[1], "wb").write(b"FWS" + bytes([version]) + struct.pack("<I", 8 + len(body)) + body)
