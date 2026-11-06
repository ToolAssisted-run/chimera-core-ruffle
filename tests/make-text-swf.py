#!/usr/bin/env python3
"""A one-frame movie of one text field drawn with a device font.

    make-text-swf.py <out.swf> <font name> <text>

The text field holds no font of its own: the HTML names the font, as a movie
that writes "<font face='SimSun'>" does, so the player looks the name up among
the device fonts, and _sans (or a name nobody has) goes to the default font.
The text may hold \\uXXXX escapes (make-han-font.py's letter is \\u4e2d).
It is what the gate's fonts leg draws (chimera#525).
"""
import re
import struct
import sys


def rect(x1, y1):
    n = max(x1.bit_length(), y1.bit_length()) + 1
    bits = format(n, "05b") + "".join(format(v, "0%db" % n) for v in (0, x1, 0, y1))
    bits += "0" * (-len(bits) % 8)
    return bytes(int(bits[i:i + 8], 2) for i in range(0, len(bits), 8))


def tag(code, body=b""):
    if len(body) < 63:
        return struct.pack("<H", code << 6 | len(body)) + body
    return struct.pack("<HI", code << 6 | 63, len(body)) + body


out, face = sys.argv[1], sys.argv[2]
text = re.sub(r"\\u([0-9a-fA-F]{4})", lambda m: chr(int(m.group(1), 16)), sys.argv[3])
html = '<p><font face="%s" size="60">%s</font></p>' % (face, text)
edit = struct.pack("<H", 1) + rect(6000, 1600)
edit += bytes([0x80 | 0x20 | 0x08 | 0x04, 0x02])  # text, multiline, read only, a colour; HTML
edit += bytes([0, 0, 0, 255]) + b"\0" + html.encode("utf-8") + b"\0"
place = bytes([0x06]) + struct.pack("<HH", 1, 1) + b"\0"  # character 1 at depth 1, at 0,0
tags = tag(9, bytes([255, 255, 255])) + tag(37, edit) + tag(26, place) + tag(1) + tag(0)
data = rect(6000, 1600) + struct.pack("<HH", 24 << 8, 1) + tags
open(out, "wb").write(b"FWS" + bytes([8]) + struct.pack("<I", 8 + len(data)) + data)
