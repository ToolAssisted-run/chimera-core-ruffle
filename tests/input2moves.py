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

def main():
    events = json.load(open(sys.argv[1]))
    frames = []; cur = []
    for e in events + [{"type": "Wait"}]:
        if e["type"] == "Wait": frames.append(cur); cur = []
        else: cur.append(e)
    out = []
    for block in frames:
        toks = []; downs = set(); ups = set()
        for e in block:
            t = e["type"]
            if t == "MouseMove":
                toks += [f"A0={int(round(e['pos'][0]))}", f"A1={int(round(e['pos'][1]))}"]
            elif t in ("MouseDown", "MouseUp"):
                toks += [f"A0={int(round(e['pos'][0]))}", f"A1={int(round(e['pos'][1]))}"]
                b = MOUSE[e["btn"]]
                (downs if t == "MouseDown" else ups).add(b)
                toks.append(f"B{b}={1 if t == 'MouseDown' else 0}")
            elif t in ("KeyDown", "KeyUp"):
                b, shift = key_index(e["key"])
                (downs if t == "KeyDown" else ups).add(b)
                if shift: toks.append(f"B{IDX['Key Left Shift']}={1 if t == 'KeyDown' else 0}")
                toks.append(f"B{b}={1 if t == 'KeyDown' else 0}")
            else:
                sys.stderr.write(f"unsupported event {t}\n"); sys.exit(3)
        if downs & ups:
            sys.stderr.write("press and release inside one frame: not expressible as levels\n"); sys.exit(3)
        out.append(" ".join(toks))
    sys.stdout.write("\n".join(out) + "\n")

if __name__ == "__main__":
    main()
