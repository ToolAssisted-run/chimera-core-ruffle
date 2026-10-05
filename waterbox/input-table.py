#!/usr/bin/env python3
"""The one source of truth for the core's input wire order.

Emits guest/src/input_table.rs (what the guest injects for each button) and
prints the waterbox.config "input" section (what the frontend shows), so the
two can never drift. Run it after editing the table.

Buttons are LEVELS, as in every chimera core: a key held across frames is a
key held. The guest turns level changes into Ruffle's edge events (KeyDown,
KeyUp, MouseDown, MouseUp) at the start of each frame.
"""
import json, sys, os

# (name shown to the user, kind, payload)
#   kind "mouse": payload = Ruffle MouseButton variant
#   kind "char":  payload = (unshifted char, shifted char, PhysicalKey)
#   kind "named": payload = (NamedKey, PhysicalKey, KeyLocation)
#   kind "numch": payload = (char, PhysicalKey)   - numpad character, Numpad location
T = []
T += [("Mouse Left Button", "mouse", "Left"), ("Mouse Middle Button", "mouse", "Middle"),
      ("Mouse Right Button", "mouse", "Right")]
for c in "ABCDEFGHIJKLMNOPQRSTUVWXYZ":
    T.append((f"Key {c}", "char", (c.lower(), c, f"Key{c}")))
for d, s in zip("1234567890", "!@#$%^&*()"):
    T.append((f"Key {d}", "char", (d, s, f"Digit{d}")))
T += [("Key Space", "char", (" ", " ", "Space"))]
for name, nk, pk in [("Enter","Enter","Enter"),("Escape","Escape","Escape"),("Tab","Tab","Tab"),
                     ("Backspace","Backspace","Backspace"),("Delete","Delete","Delete"),
                     ("Insert","Insert","Insert"),("Home","Home","Home"),("End","End","End"),
                     ("Page Up","PageUp","PageUp"),("Page Down","PageDown","PageDown"),
                     ("Up","ArrowUp","ArrowUp"),("Down","ArrowDown","ArrowDown"),
                     ("Left","ArrowLeft","ArrowLeft"),("Right","ArrowRight","ArrowRight")]:
    T.append((f"Key {name}", "named", (nk, pk, "Standard")))
T += [("Key Left Shift", "named", ("Shift", "ShiftLeft", "Left")),
      ("Key Right Shift", "named", ("Shift", "ShiftRight", "Right")),
      ("Key Left Control", "named", ("Control", "ControlLeft", "Left")),
      ("Key Right Control", "named", ("Control", "ControlRight", "Right")),
      ("Key Left Alt", "named", ("Alt", "AltLeft", "Left")),
      ("Key Right Alt", "named", ("Alt", "AltRight", "Right")),
      ("Key Caps Lock", "named", ("CapsLock", "CapsLock", "Standard")),
      ("Key Num Lock", "named", ("NumLock", "NumLock", "Standard")),
      ("Key Scroll Lock", "named", ("ScrollLock", "ScrollLock", "Standard")),
      ("Key Pause", "named", ("Pause", "Pause", "Standard"))]
for i in range(1, 13):
    T.append((f"Key F{i}", "named", (f"F{i}", f"F{i}", "Standard")))
for name, u, s, pk in [("Minus","-","_","Minus"),("Equals","=","+","Equal"),
                       ("Left Bracket","[","{","BracketLeft"),("Right Bracket","]","}","BracketRight"),
                       ("Backslash","\\","|","Backslash"),("Semicolon",";",":","Semicolon"),
                       ("Quote","'",'"',"Quote"),("Comma",",","<","Comma"),("Period",".",">","Period"),
                       ("Slash","/","?","Slash"),("Backquote","`","~","Backquote")]:
    T.append((f"Key {name}", "char", (u, s, pk)))
for d in "0123456789":
    T.append((f"Numpad {d}", "numch", (d, f"Numpad{d}")))
for name, ch, pk in [("Add","+","NumpadAdd"),("Subtract","-","NumpadSubtract"),
                     ("Multiply","*","NumpadMultiply"),("Divide","/","NumpadDivide"),
                     ("Decimal",".","NumpadDecimal")]:
    T.append((f"Numpad {name}", "numch", (ch, pk)))
T += [("Numpad Enter", "named", ("Enter", "NumpadEnter", "Numpad"))]

AXES = [{"name": "Mouse X", "min": 0, "max": 8191, "neutral": 0},
        {"name": "Mouse Y", "min": 0, "max": 8191, "neutral": 0}]

# ---- what the controls and the system are called ----
# The frontend keeps no table of these: a core says what its own are called.
# MNEMONICS is the letter each button writes into a movie's text and heads its
# input column with, by the button's name - whole, or without its player ("P2
# Up" is found under "Up"), so one line serves every pad. AXIS_HEADERS is the
# short header of each axis's column. (An entry is read by position: a letter
# may change and no movie made before it is harmed.)
MNEMONICS = {
    "Mouse Left Button": "B", "Mouse Middle Button": "B", "Mouse Right Button": "B", "Key A": "A",
    "Key B": "B", "Key C": "C", "Key D": "D", "Key E": "E", "Key F": "F", "Key G": "G",
    "Key H": "H", "Key I": "I", "Key J": "J", "Key K": "K", "Key L": "l", "Key M": "M",
    "Key N": "N", "Key O": "O", "Key P": "P", "Key Q": "Q", "Key R": "r", "Key S": "S",
    "Key T": "T", "Key U": "U", "Key V": "V", "Key W": "W", "Key X": "X", "Key Y": "Y",
    "Key Z": "Z", "Key 1": "1", "Key 2": "2", "Key 3": "3", "Key 4": "4", "Key 5": "5",
    "Key 6": "6", "Key 7": "7", "Key 8": "8", "Key 9": "9", "Key 0": "0", "Key Space": "S",
    "Key Enter": "E", "Key Escape": "E", "Key Tab": "T", "Key Backspace": "B", "Key Delete": "D",
    "Key Insert": "I", "Key Home": "H", "Key End": "E", "Key Page Up": "U", "Key Page Down": "D",
    "Key Up": "U", "Key Down": "D", "Key Left": "L", "Key Right": "R", "Key Left Shift": "S",
    "Key Right Shift": "S", "Key Left Control": "C", "Key Right Control": "C", "Key Left Alt": "A",
    "Key Right Alt": "A", "Key Caps Lock": "L", "Key Num Lock": "L", "Key Scroll Lock": "L",
    "Key Pause": "p", "Key F1": "1", "Key F2": "2", "Key F3": "3", "Key F4": "4", "Key F5": "5",
    "Key F6": "6", "Key F7": "7", "Key F8": "8", "Key F9": "9", "Key F10": "0", "Key F11": "F",
    "Key F12": "F", "Key Minus": "M", "Key Equals": "E", "Key Left Bracket": "B",
    "Key Right Bracket": "B", "Key Backslash": "B", "Key Semicolon": "S", "Key Quote": "Q",
    "Key Comma": "C", "Key Period": "P", "Key Slash": "S", "Key Backquote": "B", "Numpad 0": "0",
    "Numpad 1": "1", "Numpad 2": "2", "Numpad 3": "3", "Numpad 4": "4", "Numpad 5": "5",
    "Numpad 6": "6", "Numpad 7": "7", "Numpad 8": "8", "Numpad 9": "9", "Numpad Add": "A",
    "Numpad Subtract": "S", "Numpad Multiply": "M", "Numpad Divide": "D", "Numpad Decimal": "D",
    "Numpad Enter": "E",
}
AXIS_HEADERS = {
    "Mouse X": "mX", "Mouse Y": "mY",
}
SYSTEM_NAMES = {
    "Flash": "Flash",
}


def _bare(name):
    """A control's name without its player: "P2 Up" -> "Up"."""
    head, _, rest = name.partition(" ")
    return rest if rest and head[:1] == "P" and head[1:].isdigit() else name


def mnemonics_for(buttons):
    """The "mnemonics" of an input declaration: a letter for every one of its
    buttons, and for nothing else. A button nobody gave a letter stops the
    build - the engine would give it its rule's guess, and two columns of one
    pad would share a letter with nobody having decided it."""
    out = {}
    for b in buttons:
        key = b if b in MNEMONICS else _bare(b)
        if key not in MNEMONICS:
            raise SystemExit("no mnemonic for the button %r (MNEMONICS in %s)" % (b, __file__))
        out[key] = MNEMONICS[key]
    return out


def with_headers(axes):
    """The axes with their column headers; an axis nobody named stops the build."""
    missing = [a["name"] for a in axes if a["name"] not in AXIS_HEADERS]
    if missing:
        raise SystemExit("no header for the axes %s (AXIS_HEADERS in %s)" % (missing, __file__))
    return [dict(a, header=AXIS_HEADERS[a["name"]]) for a in axes]


def rs_char(c):
    return "'" + c.replace("\\", "\\\\").replace("'", "\\'") + "'"

def emit_rs(path):
    lines = ["// GENERATED by waterbox/input-table.py - do not edit; edit the table there.",
             "// One entry per button, in waterbox.config's wire order.",
             "use ruffle_core::events::{KeyLocation, MouseButton, NamedKey, PhysicalKey};",
             "",
             "pub enum Btn {",
             "    Mouse(MouseButton),",
             "    /// unshifted char, shifted char, physical key",
             "    Char(char, char, PhysicalKey),",
             "    Named(NamedKey, PhysicalKey, KeyLocation),",
             "    /// a numpad key that types a character",
             "    NumChar(char, PhysicalKey),",
             "}",
             "",
             f"pub const BUTTON_COUNT: usize = {len(T)};",
             "pub const SHIFT_LEFT: usize = %d;" % [i for i,(n,_,_) in enumerate(T) if n=="Key Left Shift"][0],
             "pub const SHIFT_RIGHT: usize = %d;" % [i for i,(n,_,_) in enumerate(T) if n=="Key Right Shift"][0],
             "",
             "pub static BUTTONS: [Btn; BUTTON_COUNT] = ["]
    for name, kind, p in T:
        if kind == "mouse":
            lines.append(f"    Btn::Mouse(MouseButton::{p}), // {name}")
        elif kind == "char":
            lines.append(f"    Btn::Char({rs_char(p[0])}, {rs_char(p[1])}, PhysicalKey::{p[2]}), // {name}")
        elif kind == "named":
            lines.append(f"    Btn::Named(NamedKey::{p[0]}, PhysicalKey::{p[1]}, KeyLocation::{p[2]}), // {name}")
        elif kind == "numch":
            lines.append(f"    Btn::NumChar({rs_char(p[0])}, PhysicalKey::{p[1]}), // {name}")
    lines.append("];")
    open(path, "w").write("\n".join(lines) + "\n")

if __name__ == "__main__":
    here = os.path.dirname(os.path.abspath(__file__))
    emit_rs(os.path.join(here, "guest", "src", "input_table.rs"))
    inp = {"name": "Keyboard and Mouse",
           "_comment": "Mouse Left/Middle/Right, then the keyboard. The two axes are the pointer in stage pixels. Buttons are levels; the core turns level changes into Ruffle's key/mouse events at the start of each frame. Wire order is generated by waterbox/input-table.py.",
           "buttons": [n for n, _, _ in T], "mnemonics": mnemonics_for([n for n, _, _ in T]),
           "axes": with_headers(AXES)}
    if "--json" in sys.argv:
        print(json.dumps(inp, indent=2))
    else:
        print(f"{len(T)} buttons, {len(AXES)} axes -> guest/src/input_table.rs")
