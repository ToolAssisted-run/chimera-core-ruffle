#!/usr/bin/env python3
"""Every control the package declares must have a default binding.

The frontend renders the controller the config declares and looks up its
defaults by name. A button the config names and the keybinds file does not is
one the player finds unbound with no way to guess what it was for; a binding
for a name the config does not declare is dead weight that will silently rot.
"""
import json, sys

cfg = json.load(open(sys.argv[1]))
kb = json.load(open(sys.argv[2]))
name = cfg["input"]["name"]
buttons = cfg["input"]["buttons"]
axes = [a["name"] for a in cfg["input"]["axes"]]

trollers = kb.get("AllTrollers", {}).get(name)
analog = kb.get("AllTrollersAnalog", {}).get(name)
if trollers is None or analog is None:
    sys.exit(f"keybinds declare no controller called {name!r}")

unbound = [b for b in buttons if not trollers.get(b)]
unknown = [b for b in trollers if b not in buttons]
axes_unbound = [a for a in axes if not (analog.get(a) or {}).get("Value")]
axes_unknown = [a for a in analog if a not in axes]

problems = []
if unbound: problems.append(f"{len(unbound)} buttons unbound: {unbound[:4]}")
if unknown: problems.append(f"{len(unknown)} bindings for buttons the config does not declare: {unknown[:4]}")
if axes_unbound: problems.append(f"axes unbound: {axes_unbound}")
if axes_unknown: problems.append(f"axis bindings not declared: {axes_unknown}")
if problems:
    sys.exit("; ".join(problems))
print(f"{len(buttons)} buttons and {len(axes)} axes bound for {name!r}")
