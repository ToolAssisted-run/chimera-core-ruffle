#!/usr/bin/env python3
"""An ActionScript 3 movie that changes its own frame rate, written out byte
by byte - there is no compiler to ask.

    make-frame-rate-swf.py <out.swf>

The SWF header says 24 frames a second. The document class, as its source
would read:

    public class Rate extends flash.display.MovieClip {
        public var count:int;
        public function Rate() {
            stage.frameRate = 60;
            addEventListener("enterFrame", step);
        }
        public function step(e:*):void {
            count = count + 1; trace(count);
        }
    }

Flash Player runs it at 60 frames a second from the second frame on. So
does the core: by default each frame is then a 60th of a second long (the
engine is told 60, and a frame holds a 60th of a second of sound); with the
fps setting raised, the movie's frames come 60 times a second among the
machine's. The trace says how many movie frames ran: one line each, from
the second (the first enterFrame comes after the constructor's frame).
"""
import struct
import sys


def u30(v):
    out = b""
    while True:
        b = v & 0x7F
        v >>= 7
        if v:
            out += bytes([b | 0x80])
        else:
            return out + bytes([b])


def s(text):
    data = text.encode("utf-8")
    return u30(len(data)) + data


class Pool:
    """A constant pool: entry 0 is implied, and the count written is one more."""

    def __init__(self, encode):
        self.items, self.encode = [], encode

    def __call__(self, item):
        if item not in self.items:
            self.items.append(item)
        return self.items.index(item) + 1

    def bytes(self):
        return u30(len(self.items) + 1 if self.items else 0) + b"".join(self.encode(i) for i in self.items)


strings = Pool(s)
namespaces = Pool(lambda ns: bytes([ns[0]]) + u30(strings(ns[1])))
multinames = Pool(lambda mn: bytes([0x07]) + u30(namespaces(mn[0])) + u30(strings(mn[1])))

PUBLIC = (0x16, "")              # a package namespace: the unnamed package
DISPLAY = (0x16, "flash.display")


def q(name, ns=PUBLIC):
    # a namespace's name must be in the pool before the namespace is written
    strings(ns[1])
    strings(name)
    namespaces(ns)
    return multinames((ns, name))


RATE, MOVIECLIP = q("Rate"), q("MovieClip", DISPLAY)
COUNT, T_INT = q("count"), q("int")
STAGE, FRAMERATE = q("stage"), q("frameRate")
ADD_LISTENER, STEP, TRACE = q("addEventListener"), q("step"), q("trace")

GETLOCAL0, PUSHSCOPE, POPSCOPE, RETURNVOID = b"\xD0", b"\x30", b"\x1D", b"\x47"


def op(code, *args):
    return bytes([code]) + b"".join(u30(a) for a in args)


constructor = (
    GETLOCAL0 + PUSHSCOPE
    + GETLOCAL0 + op(0x49, 0)                                         # constructsuper()
    + GETLOCAL0 + op(0x66, STAGE) + op(0x24, 60) + op(0x61, FRAMERATE)  # stage.frameRate = 60
    + GETLOCAL0 + op(0x2C, strings("enterFrame")) + GETLOCAL0 + op(0x66, STEP) + op(0x4F, ADD_LISTENER, 2)
    + RETURNVOID
)
step = (
    GETLOCAL0 + PUSHSCOPE
    + GETLOCAL0 + GETLOCAL0 + op(0x66, COUNT) + b"\xC0" + op(0x61, COUNT)   # increment_i
    + op(0x5D, TRACE) + GETLOCAL0 + op(0x66, COUNT) + op(0x4F, TRACE, 1)
    + RETURNVOID
)
class_init = GETLOCAL0 + PUSHSCOPE + RETURNVOID
script_init = (
    GETLOCAL0 + PUSHSCOPE
    + op(0x65, 0)                                # getscopeobject 0
    + op(0x60, MOVIECLIP) + PUSHSCOPE            # getlex, as a class's scope
    + op(0x60, MOVIECLIP) + op(0x58, 0)          # the base class, newclass 0
    + POPSCOPE + op(0x68, RATE)                  # initproperty
    + RETURNVOID
)


def method(params):
    # param_count, return type (any), each param's type (any), name, flags
    return u30(params) + u30(0) + u30(0) * params + u30(0) + b"\x00"


def body(index, code, locals_, stack=4):
    return u30(index) + u30(stack) + u30(locals_) + u30(0) + u30(8) + u30(len(code)) + code + u30(0) + u30(0)


traits = [u30(COUNT) + b"\x00" + u30(0) + u30(T_INT) + u30(0),     # a slot: count:int
          u30(STEP) + b"\x01" + u30(0) + u30(1)]                     # a method: disp id 0, method 1
instance = u30(RATE) + u30(MOVIECLIP) + b"\x01" + u30(0) + u30(0) + u30(len(traits)) + b"".join(traits)
klass = u30(2) + u30(0)
script = u30(3) + u30(1) + u30(RATE) + b"\x04" + u30(0) + u30(0)     # one trait: the class
bodies = [body(0, constructor, 1), body(1, step, 2), body(2, class_init, 1), body(3, script_init, 1)]

abc = (
    struct.pack("<HH", 16, 46)
    + u30(0) + u30(0) + u30(0) + strings.bytes() + namespaces.bytes() + u30(0) + multinames.bytes()
    + u30(4) + method(0) + method(1) + method(0) + method(0)
    + u30(0)                                                         # no metadata
    + u30(1) + instance + klass
    + u30(1) + script
    + u30(len(bodies)) + b"".join(bodies)
)


def rect(x1, y1):
    n = max(x1.bit_length(), y1.bit_length()) + 1
    bits = format(n, "05b") + "".join(format(v, "0%db" % n) for v in (0, x1, 0, y1))
    bits += "0" * (-len(bits) % 8)
    return bytes(int(bits[i:i + 8], 2) for i in range(0, len(bits), 8))


def tag(code, data=b""):
    if len(data) < 63:
        return struct.pack("<H", (code << 6) | len(data)) + data
    return struct.pack("<HI", (code << 6) | 63, len(data)) + data


movie = rect(4000, 4000) + bytes([0, 24]) + struct.pack("<H", 1)    # 24 frames a second, 8.8
movie += tag(69, struct.pack("<I", 0x08))                            # FileAttributes: ActionScript 3
movie += tag(9, bytes([0, 0, 0]))
movie += tag(82, struct.pack("<I", 0) + b"rate\0" + abc)             # DoABC
movie += tag(76, struct.pack("<HH", 1, 0) + b"Rate\0")               # SymbolClass: the document class
movie += tag(1) + tag(0)
open(sys.argv[1], "wb").write(b"FWS" + bytes([15]) + struct.pack("<I", 8 + len(movie)) + movie)
