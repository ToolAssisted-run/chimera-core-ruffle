#!/usr/bin/env python3
"""ruffle input.json -> a chimera-style moves file for run-wbx --input.

Ruffle's test protocol is a stream of EDGE events between "Wait"s; chimera's
(like every core's) is LEVELS per frame. Block k of the stream becomes frame k
of the moves file (0-based), because ruffle's runner injects block k after
frame k's step and the guest injects a frame's levels at the same point.

One line per frame: tokens "A<axis>=<value>" and "B<button>=<0|1>", only for
what changed; an empty line is a frame with nothing new. Exits 3 for a stream
the level model cannot express (a button pressed AND released inside one
frame), so a gate can skip it honestly rather than fail it.

    --split N

turns one movie frame into N of the machine's frames, which is what the core's
fps setting is for: the movie still runs at its own rate and the frames in
between carry input. A block is then cut into sub-frames - at every point where
a button would have to be pressed and released at once, and wherever the
pointer moves after a click - and each block is padded to exactly N lines, so
line (k*N + i) is sub-frame i of movie frame k. That makes a whole family of
ruffle's own tests replayable: five clicks inside one movie frame is not a
level stream at 1x and is an ordinary one at 10x. Exits 3 when N is too small
to hold a block, naming what it would take.
"""
import json, sys, os
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "waterbox"))
from importlib import util
spec = util.spec_from_file_location("it", os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "waterbox", "input-table.py"))
it = util.module_from_spec(spec); spec.loader.exec_module(it)

NAMES = [n for n, _, _ in it.T]
IDX = {n: i for i, n in enumerate(NAMES)}
MOUSE = {"Left": IDX["Mouse Left Button"], "Middle": IDX["Mouse Middle Button"], "Right": IDX["Mouse Right Button"]}
NAMED = {"Enter": "Key Enter", "Escape": "Key Escape", "Tab": "Key Tab", "Backspace": "Key Backspace",
         "Delete": "Key Delete", "Insert": "Key Insert", "Home": "Key Home", "End": "Key End",
         "PageUp": "Key Page Up", "PageDown": "Key Page Down", "ArrowUp": "Key Up", "ArrowDown": "Key Down",
         "ArrowLeft": "Key Left", "ArrowRight": "Key Right", "LeftShift": "Key Left Shift",
         "RightShift": "Key Right Shift", "LeftControl": "Key Left Control", "RightControl": "Key Right Control",
         "LeftAlt": "Key Left Alt", "CapsLock": "Key Caps Lock", "NumLock": "Key Num Lock",
         "ScrollLock": "Key Scroll Lock", "Pause": "Key Pause", "Space": "Key Space"}
for i in range(1, 13): NAMED[f"F{i}"] = f"Key F{i}"
CHARS = {}
for n, kind, p in it.T:
    if kind == "char":
        CHARS[p[0]] = (IDX[n], False); CHARS[p[1]] = (IDX[n], True)

def key_index(key):
    """(button index, needs shift) for an AutomatedKey as serialized in input.json."""
    if isinstance(key, dict):  # {"Char": "a"} / {"Numpad": "5"} forms
        if "Char" in key: key = key["Char"]
        elif "Numpad" in key: return (IDX[f"Numpad {key['Numpad']}"], False)
    if key in NAMED: return (IDX[NAMED[key]], False)
    if isinstance(key, str) and len(key) == 1 and key in CHARS: return CHARS[key]
    raise KeyError(f"unmapped key {key!r}")

def subframes(block):
    """A block's events cut into the fewest sub-frames that keep every edge.

    A sub-frame is one of the machine's frames: the levels it carries become at
    most one edge per button, and the pointer sits at one place. So a new one
    starts whenever a button would move twice (the press and the release of one
    click) and whenever the pointer moves after a button has already acted -
    otherwise a release would be delivered at the NEXT click's position, which
    is not where it happened.
    """
    subs = [[]]; used = [set()]; acted = [False]
    def cut():
        subs.append([]); used.append(set()); acted.append(False)
    for e in block:
        t = e["type"]
        if t == "MouseMove":
            if acted[-1]: cut()
            subs[-1] += [f"A0={int(round(e['pos'][0]))}", f"A1={int(round(e['pos'][1]))}"]
        elif t in ("MouseDown", "MouseUp"):
            b = MOUSE[e["btn"]]
            if b in used[-1]: cut()
            subs[-1] += [f"A0={int(round(e['pos'][0]))}", f"A1={int(round(e['pos'][1]))}"]
            subs[-1].append(f"B{b}={1 if t == 'MouseDown' else 0}")
            used[-1].add(b); acted[-1] = True
        elif t in ("KeyDown", "KeyUp"):
            b, shift = key_index(e["key"])
            if b in used[-1]: cut()
            if shift: subs[-1].append(f"B{IDX['Key Left Shift']}={1 if t == 'KeyDown' else 0}")
            subs[-1].append(f"B{b}={1 if t == 'KeyDown' else 0}")
            used[-1].add(b); acted[-1] = True
        else:
            sys.stderr.write(f"unsupported event {t}\n"); sys.exit(3)
    return subs

def main():
    args = sys.argv[1:]
    split = 0
    if "--split" in args:
        i = args.index("--split")
        split = int(args[i + 1]); del args[i:i + 2]
    events = json.load(open(args[0]))
    frames = []; cur = []
    for e in events + [{"type": "Wait"}]:
        if e["type"] == "Wait": frames.append(cur); cur = []
        else: cur.append(e)
    out = []
    for block in frames:
        subs = subframes(block)
        if not split:
            if len(subs) > 1:
                sys.stderr.write("press and release inside one frame: not expressible as levels\n")
                sys.exit(3)
            out.append(" ".join(subs[0]))
            continue
        if len(subs) > split:
            sys.stderr.write(f"a block needs {len(subs)} sub-frames and --split is {split}\n")
            sys.exit(3)
        for i in range(split):
            out.append(" ".join(subs[i]) if i < len(subs) else "")
    sys.stdout.write("\n".join(out) + "\n")

if __name__ == "__main__":
    main()
