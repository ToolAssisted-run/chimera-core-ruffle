#!/usr/bin/env python3
"""Apply a ruffle test.toml's [audio_assertions.*] to a per-frame peaks file.

Mirrors the framework's test_audio exactly: for each frame in the selection
(1-based, inclusive), the frame's max |sample| must not exceed max_amplitude
and must reach min_max_amplitude; known_failure inverts the verdict for that
assertion. Peaks come from the i16 stream on both the native and sandbox
sides, so the two are judged by the same numbers.

Usage: audio-assert.py <test.toml> <peaks file>   -> exit 0 pass, 1 fail
"""
import sys, tomllib

opts = tomllib.load(open(sys.argv[1], "rb"))
peaks = [float(l) for l in open(sys.argv[2]) if l.strip()]
bad = 0
for name, a in opts.get("audio_assertions", {}).items():
    # the framework's FrameSelection: a single frame, a list of frames, or {from, to}
    fr = a.get("frames", {})
    if isinstance(fr, int): sel = [fr]
    elif isinstance(fr, list): sel = list(fr)
    else: sel = list(range(fr.get("from", 1), fr.get("to", len(peaks)) + 1))
    failed_frame = None
    for frame in sel:
        if frame - 1 >= len(peaks): break
        m = peaks[frame - 1]
        if "max_amplitude" in a and m > a["max_amplitude"]:
            failed_frame = (frame, f"max {m:.4f} > {a['max_amplitude']}"); break
        if "min_max_amplitude" in a and m < a["min_max_amplitude"]:
            failed_frame = (frame, f"max {m:.4f} < {a['min_max_amplitude']}"); break
    known = a.get("known_failure", False)
    if failed_frame and not known:
        print(f"audio assertion '{name}' failed at frame {failed_frame[0]}: {failed_frame[1]}"); bad += 1
    elif not failed_frame and known:
        print(f"audio assertion '{name}' is marked known_failure but passed"); bad += 1
sys.exit(1 if bad else 0)
