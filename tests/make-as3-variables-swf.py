#!/usr/bin/env python3
"""An ActionScript 3 movie that keeps one of everything a variable list can
show, written out byte by byte - there is no compiler to ask.

    make-as3-variables-swf.py <out.swf>

The document class, as its source would read:

    public class Vars extends flash.display.MovieClip {
        public var count:int;
        public var speed:Number;
        public var alive:Boolean;
        public var label:String;
        private var secret:int;
        public var inner:Object;

        public function Vars() {
            count = 123456789; speed = 1.5; alive = true; label = "hero";
            secret = 77;
            inner = new Object(); inner.depth = 2;
            addEventListener("enterFrame", step);
        }
        public function step(e:*):void {
            count = count + 1; speed = speed + 0.25; trace(count);
        }
    }

So the list the gate expects is known without running anything: root.count
(an int that goes up by one a frame), root.speed (a Number, a quarter more
each frame - which makes it a whole number every fourth), root.alive,
root.label (four characters), root.secret, root.inner.depth (chimera#216).
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


ints = Pool(u30)
doubles = Pool(lambda d: struct.pack("<d", d))
strings = Pool(s)
namespaces = Pool(lambda ns: bytes([ns[0]]) + u30(strings(ns[1])))
multinames = Pool(lambda mn: bytes([0x07]) + u30(namespaces(mn[0])) + u30(strings(mn[1])))

PUBLIC = (0x16, "")              # a package namespace: the unnamed package
DISPLAY = (0x16, "flash.display")
PRIVATE = (0x05, "Vars")


def q(name, ns=PUBLIC):
    # a namespace's name must be in the pool before the namespace is written
    strings(ns[1])
    strings(name)
    namespaces(ns)
    return multinames((ns, name))


VARS, MOVIECLIP = q("Vars"), q("MovieClip", DISPLAY)
COUNT, SPEED, ALIVE, LABEL, INNER = q("count"), q("speed"), q("alive"), q("label"), q("inner")
SECRET = q("secret", PRIVATE)
T_INT, T_NUMBER, T_BOOL, T_STRING, T_OBJECT = q("int"), q("Number"), q("Boolean"), q("String"), q("Object")
ADD_LISTENER, STEP, TRACE, DEPTH = q("addEventListener"), q("step"), q("trace"), q("depth")

GETLOCAL0, PUSHSCOPE, POPSCOPE, RETURNVOID = b"\xD0", b"\x30", b"\x1D", b"\x47"


def op(code, *args):
    return bytes([code]) + b"".join(u30(a) for a in args)


def setprop(mn, value):
    return GETLOCAL0 + value + op(0x61, mn)


constructor = (
    GETLOCAL0 + PUSHSCOPE
    + GETLOCAL0 + op(0x49, 0)                                        # constructsuper()
    + setprop(COUNT, op(0x2D, ints(123456789)))                      # pushint
    + setprop(SPEED, op(0x2F, doubles(1.5)))                         # pushdouble
    + setprop(ALIVE, b"\x26")                                        # pushtrue
    + setprop(LABEL, op(0x2C, strings("hero")))                      # pushstring
    + setprop(SECRET, op(0x24, 77))                                  # pushbyte
    + setprop(INNER, op(0x5D, T_OBJECT) + op(0x4A, T_OBJECT, 0))     # findpropstrict, constructprop
    + GETLOCAL0 + op(0x66, INNER) + op(0x24, 2) + op(0x61, DEPTH)    # inner.depth = 2
    + GETLOCAL0 + op(0x2C, strings("enterFrame")) + GETLOCAL0 + op(0x66, STEP) + op(0x4F, ADD_LISTENER, 2)
    + RETURNVOID
)
step = (
    GETLOCAL0 + PUSHSCOPE
    + GETLOCAL0 + GETLOCAL0 + op(0x66, COUNT) + b"\xC0" + op(0x61, COUNT)                     # increment_i
    + GETLOCAL0 + GETLOCAL0 + op(0x66, SPEED) + op(0x2F, doubles(0.25)) + b"\xA0" + op(0x61, SPEED)   # add
    + op(0x5D, TRACE) + GETLOCAL0 + op(0x66, COUNT) + op(0x4F, TRACE, 1)
    + RETURNVOID
)
class_init = GETLOCAL0 + PUSHSCOPE + RETURNVOID
script_init = (
    GETLOCAL0 + PUSHSCOPE
    + op(0x65, 0)                                # getscopeobject 0
    + op(0x60, MOVIECLIP) + PUSHSCOPE            # getlex, as a class's scope
    + op(0x60, MOVIECLIP) + op(0x58, 0)          # the base class, newclass 0
    + POPSCOPE + op(0x68, VARS)                  # initproperty
    + RETURNVOID
)


def method(params):
    # param_count, return type (any), each param's type (any), name, flags
    return u30(params) + u30(0) + u30(0) * params + u30(0) + b"\x00"


def slot(mn, type_mn):
    return u30(mn) + b"\x00" + u30(0) + u30(type_mn) + u30(0)


def body(index, code, locals_, stack=4):
    return u30(index) + u30(stack) + u30(locals_) + u30(0) + u30(8) + u30(len(code)) + code + u30(0) + u30(0)


traits = [slot(COUNT, T_INT), slot(SPEED, T_NUMBER), slot(ALIVE, T_BOOL), slot(LABEL, T_STRING),
          slot(SECRET, T_INT), slot(INNER, T_OBJECT),
          u30(STEP) + b"\x01" + u30(0) + u30(1)]                     # a method: disp id 0, method 1
instance = u30(VARS) + u30(MOVIECLIP) + b"\x01" + u30(0) + u30(0) + u30(len(traits)) + b"".join(traits)
klass = u30(2) + u30(0)
script = u30(3) + u30(1) + u30(VARS) + b"\x04" + u30(0) + u30(0)     # one trait: the class
bodies = [body(0, constructor, 1), body(1, step, 2), body(2, class_init, 1), body(3, script_init, 1)]

abc = (
    struct.pack("<HH", 16, 46)
    + ints.bytes() + u30(0) + doubles.bytes() + strings.bytes() + namespaces.bytes() + u30(0) + multinames.bytes()
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


movie = rect(4000, 4000) + bytes([0, 50]) + struct.pack("<H", 1)
movie += tag(69, struct.pack("<I", 0x08))                            # FileAttributes: ActionScript 3
movie += tag(9, bytes([0, 0, 0]))
movie += tag(82, struct.pack("<I", 0) + b"vars\0" + abc)             # DoABC
movie += tag(76, struct.pack("<HH", 1, 0) + b"Vars\0")               # SymbolClass: the document class
movie += tag(1) + tag(0)
open(sys.argv[1], "wb").write(b"FWS" + bytes([15]) + struct.pack("<I", 8 + len(movie)) + movie)
