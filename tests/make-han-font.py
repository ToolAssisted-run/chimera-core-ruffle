#!/usr/bin/env python3
"""A TrueType font of one Chinese letter, for the fonts leg of the gate.

    make-han-font.py <out.ttf>

The font is called "ChimeraTestHan". It has one letter, U+4E2D (the "middle"
of Chinese text), drawn as a filled square that fills most of the em. The
font Ruffle carries (Noto Sans, a subset) has no Chinese letters at all, so
text with this letter shows it only when the font is a device font, as a
project's "fonts" slot makes it (chimera#525). It is made here, rather than
taken from a real Chinese font, because the repository may hold only what it
may give away, and a generated square is that.
"""
import struct
import sys

HAN = 0x4E2D
UPEM = 1000


def name_table(family):
    records, strings = [], b""
    for name_id, text in ((1, family), (2, "Regular"), (4, family), (6, family)):
        data = text.encode("utf-16-be")
        records.append(struct.pack(">HHHHHH", 3, 1, 0x409, name_id, len(data), len(strings)))
        strings += data
    head = struct.pack(">HHH", 0, len(records), 6 + 12 * len(records))
    return head + b"".join(records) + strings


def cmap_table():
    # format 4: one segment for the letter, and the 0xFFFF one every table ends with
    starts, ends = [HAN, 0xFFFF], [HAN, 0xFFFF]
    deltas = [(1 - HAN) & 0xFFFF, 1]  # the letter is glyph 1
    seg = len(starts)
    body = struct.pack(">HHHH", seg * 2, 2, 0, 0)  # searchRange etc.: readers do not need them
    body += struct.pack(">%dH" % seg, *ends) + b"\0\0" + struct.pack(">%dH" % seg, *starts)
    body += struct.pack(">%dH" % seg, *deltas) + struct.pack(">%dH" % seg, 0, 0)
    sub = struct.pack(">HHH", 4, 6 + len(body), 0) + body
    return struct.pack(">HHHHI", 0, 1, 3, 1, 12) + sub


def square_glyph(x0, y0, x1, y1):
    pts = [(x0, y0), (x0, y1), (x1, y1), (x1, y0)]
    g = struct.pack(">hhhhh", 1, x0, y0, x1, y1) + struct.pack(">HH", 3, 0)
    g += bytes([0x01] * 4)  # on the curve, two-byte deltas
    px = py = 0
    xs, ys = b"", b""
    for x, y in pts:
        xs += struct.pack(">h", x - px)
        ys += struct.pack(">h", y - py)
        px, py = x, y
    g += xs + ys
    return g + b"\0" * (-len(g) % 4)


def font(family):
    glyf = square_glyph(100, -50, 900, 750)
    loca = struct.pack(">HHH", 0, 0, len(glyf) // 2)  # glyph 0 (.notdef) is empty
    tables = {
        b"cmap": cmap_table(),
        b"glyf": glyf,
        b"head": struct.pack(">IIIIHHQQhhhhHHhhh", 0x10000, 0x10000, 0, 0x5F0F3CF5, 0x000B, UPEM,
                             0, 0, 100, -50, 900, 750, 0, 8, 2, 0, 0),
        b"hhea": struct.pack(">IhhhHhhhhhhhhhhhH", 0x10000, 880, -120, 0, 1000, 100, 100, 900,
                             1, 0, 0, 0, 0, 0, 0, 0, 2),
        b"hmtx": struct.pack(">HhHh", 500, 0, 1000, 100),
        b"loca": loca,
        b"maxp": struct.pack(">IHHHHHHHHHHHHHH", 0x10000, 2, 4, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0),
        b"name": name_table(family),
        b"OS/2": struct.pack(">HhHHHhhhhhhhhhhh10sIIIIIHHHhhhHHII", 4, 1000, 400, 5, 0,
                             650, 700, 0, 140, 650, 700, 0, 480, 50, 250, 0, b"\0" * 10,
                             0, 1 << 17, 0, 0, 0, 0x40, HAN, HAN, 880, -120, 0, 880, 120, 1 << 18, 0)
                 + struct.pack(">hhHHH", 0, 700, 0, 0, 0),
        b"post": struct.pack(">IIhhIIIII", 0x30000, 0, -100, 50, 0, 0, 0, 0, 0),
    }
    tags = sorted(tables)
    out = struct.pack(">IHHHH", 0x10000, len(tags), 128, 3, len(tags) * 16 - 128)
    off = 12 + 16 * len(tags)
    body = b""
    for t in tags:
        data = tables[t]
        out += struct.pack(">4sIII", t, 0, off + len(body), len(data))
        body += data + b"\0" * (-len(data) % 4)
    return out + body


open(sys.argv[1], "wb").write(font("ChimeraTestHan"))
