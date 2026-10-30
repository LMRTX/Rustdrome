#!/usr/bin/env python3
"""End-to-end: the now-playing strip (elapsed ── line ● ── total, shuffle/repeat/speaker/slider/percent),
driven with a real mouse (SGR reports) against real mpv.

    cargo build && python3 tests/e2e/strip.py
"""
import os, re, shutil, subprocess, sys, tempfile, time
HERE = os.path.dirname(os.path.abspath(__file__)); sys.path.insert(0, HERE)
from drive import Term, KEYS

failures = []
def check(name, ok, detail=""):
    print(("PASS " if ok else "FAIL ") + name + ("" if ok or not detail else f"   [{detail}]"))
    if not ok: failures.append(name)

SPEAKERS = {"\U000f075f": "mute", "\U000f057f": "low", "\U000f0580": "medium", "\U000f057e": "high"}

mock = subprocess.Popen([sys.executable, os.path.join(HERE, "mock_server.py"), "4599"],
                        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(1.5)
home = tempfile.mkdtemp(prefix="rustdrome-strip-")
try:
    t = Term(home); t.pump(2.0)
    t.send("http://127.0.0.1:4599"); t.send(KEYS["TAB"]); t.send("demo"); t.send(KEYS["TAB"]); t.send("sesame")
    t.send(KEYS["ENTER"], 3.0)
    t.send(KEYS["TAB"]); t.send(KEYS["ENTER"], 3.0)          # play the first track
    t.send(" ", 0.6)                                          # pause: the clock holds still while we poke it

    def rows():
        L = t.screen.display
        top = next((y for y, l in enumerate(L) if "now playing" in l), None)
        return (top, L[top + 1], L[top + 2]) if top is not None else (None, "", "")
    def mouse(code, x, y, up=False):                          # x, y are 0-based screen cells
        t.send(f"\x1b[<{code};{x + 1};{y + 1}{'m' if up else 'M'}", 0.5)
    def elapsed():
        m = re.match(r"[│\s]*(\d+):(\d\d)", rows()[2]); return int(m[1]) * 60 + int(m[2]) if m else None
    def total():
        m = re.search(r"(\d+):(\d\d)[│\s]*$", rows()[2]); return int(m[1]) * 60 + int(m[2]) if m else None
    def volume():
        m = re.search(r"(\d+)%[│\s]*$", rows()[1]); return int(m[1]) if m else None
    def speaker():
        return next((n for g, n in SPEAKERS.items() if g in rows()[1]), None)

    top, title, line = rows()
    check("box is still four rows directly above the footer, bottom border intact", top == t.rows - 5 and any(c in t.screen.display[top + 3] for c in "╰└"), f"label row {top}, screen {t.rows} rows")
    check("elapsed left, total right, one dot on the line between", elapsed() is not None and total() == 20 and line.count("●") == 1, repr(line))
    check("no blocky bar glyphs on the line", not any(c in line for c in "█▌▏"), repr(line))
    check("controls: shuffle, repeat, speaker, slider, percent in that order",
          "\uf074" in title and title.index("\uf074") < title.index("\uf01e") < min(title.find(g) for g in SPEAKERS if g in title) and volume() is not None, repr(title))

    # the bar occupies the cells between the two time labels; find it from the drawn row
    bar0 = line.index(" ", line.index(":")) + 1; bar1 = line.rstrip("│ ").rindex(" ")
    bw = bar1 - bar0; y_line = top + 2

    # ── seeking ──
    mouse(0, bar0 + (bw - 1) // 2, y_line); mouse(0, bar0 + (bw - 1) // 2, y_line, up=True)
    t.pump(0.8)
    check("clicking the middle of the line seeks to about half", 8 <= (elapsed() or -1) <= 12, f"elapsed {elapsed()}s")
    mouse(0, bar0, y_line); mouse(0, bar0, y_line, up=True); t.pump(0.8)
    check("clicking the first cell seeks to the start", elapsed() is not None and elapsed() <= 1, f"elapsed {elapsed()}s")
    base = elapsed()
    for _ in range(2): mouse(64, bar0 + 3, y_line)                       # wheel up over the line
    t.pump(0.8)
    near = lambda got, want: got is not None and abs(got - want) <= 1      # mpv reports just under; the label floors
    check("wheel up seeks forward 5 s per notch", near(elapsed(), base + 10), f"{base}s -> {elapsed()}s")
    up2 = elapsed()
    mouse(65, bar0 + 3, y_line); t.pump(0.8)
    check("wheel down seeks back 5 s", near(elapsed(), up2 - 5), f"{up2}s -> {elapsed()}s")
    mouse(64, 2, y_line); t.pump(0.8)                                    # over the elapsed label counts too
    check("the whole row responds, not just the line", near(elapsed(), up2), f"-> {elapsed()}s")
    # one wheel notch must equal one press of the keyboard's seek key from the same spot
    mouse(0, bar0, y_line); mouse(0, bar0, y_line, up=True); t.pump(0.8); mouse(64, bar0 + 3, y_line); t.pump(0.8); wheel = elapsed()
    mouse(0, bar0, y_line); mouse(0, bar0, y_line, up=True); t.pump(0.8); t.send(KEYS["RIGHT"], 0.8); key = elapsed()
    check("one wheel notch seeks exactly as far as the keyboard's seek key", wheel == key and 3 <= (key or 0) <= 5, f"wheel {wheel}s, key {key}s")
    for _ in range(3): mouse(65, bar0 + 3, y_line)
    t.pump(0.8)
    check("seeking back past the start stops at 0:00", elapsed() == 0, f"-> {elapsed()}s")

    # ── volume ──
    y_vol = top + 1
    pc = rows()[1].rindex("%"); sl_x = pc - 4                              # a cell of the percentage text
    v0 = volume()
    mouse(64, pc - 1, y_vol); t.pump(0.4)
    check("wheel up over the percentage adds 5%", volume() == min(v0 + 5, 130), f"{v0} -> {volume()}")
    mouse(65, pc - 1, y_vol); mouse(65, pc - 1, y_vol); t.pump(0.4)
    check("wheel down takes 5% per notch", volume() == v0 - 5, f"-> {volume()}")
    # the slider: from the dot character's row, its cells are the ━ ─ ● run before the percentage
    row = rows()[1]
    cells = [i for i, c in enumerate(row) if c in "━─●" and i > row.index(next(g for g in SPEAKERS if g in row))]
    s0, s1 = cells[0], cells[-1]
    mouse(0, s1, y_vol); mouse(0, s1, y_vol, up=True); t.pump(0.4)
    check("clicking the slider's last cell gives 100%", volume() == 100 and speaker() == "high", f"{volume()}% {speaker()}")
    mouse(0, s0 + (s1 - s0) // 2, y_vol); mouse(0, s0 + (s1 - s0) // 2, y_vol, up=True); t.pump(0.4)
    check("clicking the middle gives about half and the medium speaker", 40 <= (volume() or 0) <= 60 and speaker() == "medium", f"{volume()}% {speaker()}")
    mouse(0, s0 + 1, y_vol); mouse(0, s0 + 1, y_vol, up=True); t.pump(0.4)
    check("clicking near the start gives the low speaker", 0 < (volume() or 0) <= 33 and speaker() == "low", f"{volume()}% {speaker()}")
    mouse(0, s0, y_vol); mouse(0, s0, y_vol, up=True); t.pump(0.4)
    check("clicking the first cell gives 0% and the muted speaker", volume() == 0 and speaker() == "mute", f"{volume()}% {speaker()}")
    # ── dragging (button held, pointer moving; it drifts off the row on purpose) ──
    mouse(0, bar0, y_line)                                                # press on the first cell, hold
    for pct in (25, 50, 75, 100):
        mouse(32, bar0 + round(pct / 100 * (bw - 1)), y_line + 2); t.pump(0.4)
        want = round(pct / 100 * 20)
        check(f"dragging the line: the time follows the pointer ({pct}%)", near(elapsed(), want), f"want ~{want}s, got {elapsed()}s")
    mouse(32, bar0 + (bw - 1) // 4, y_line - 1); t.pump(0.4)
    check("…and it can come back", near(elapsed(), 5), f"elapsed {elapsed()}s")
    mouse(0, bar0 + (bw - 1) // 4, y_line - 1, up=True); t.pump(1.0)
    check("releasing leaves mpv at that spot", near(elapsed(), 5), f"elapsed {elapsed()}s")
    mouse(32, bar0 + (bw - 1) * 3 // 4, y_line); t.pump(0.4)
    check("after release, further motion does nothing", near(elapsed(), 5), f"elapsed {elapsed()}s")

    mouse(0, s0, y_vol)                                                   # press on the slider's first cell, hold
    seen = []
    for c in range(s0, s1 + 1):
        mouse(32, c, y_vol + (3 if c % 2 else 0)); t.pump(0.15); seen.append(volume())
    check("dragging the slider slides the volume up smoothly to 100%",
          seen == sorted(seen) and seen[0] <= 5 and seen[-1] == 100 and len(set(seen)) >= (s1 - s0) // 2, str(seen))
    for c in range(s1, s0 - 1, -1):
        mouse(32, c, y_vol); t.pump(0.1)
    check("…and back down to 0%", volume() == 0 and speaker() == "mute", f"{volume()}% {speaker()}")
    mouse(32, s1 + 30, y_vol); t.pump(0.3)
    check("pointer far past the end still means 100%", volume() == 100, f"{volume()}%")
    mouse(0, s1 + 30, y_vol, up=True); t.pump(0.3)
    mouse(32, s0, y_vol); t.pump(0.3)
    check("after release the slider stays put", volume() == 100, f"{volume()}%")

    t.send("m", 0.5)                                                     # keyboard mute keeps working
    check("keyboard mute still works and shows 'mute'", "mute" in rows()[1], repr(rows()[1]))
    t.send("m", 0.3)
    before = volume(); t.send("+", 0.4)
    check("keyboard volume keys keep their 5% step", volume() == before + 5, f"{before} -> {volume()}")

    # ── playing: the dot only ever moves forward, also right after a click-seek ──
    mouse(0, bar0 + (bw - 1) // 10, y_line); mouse(0, bar0 + (bw - 1) // 10, y_line, up=True)
    t.send(" ", 0.0)                                                      # resume
    cols = []
    for _ in range(40):
        t.pump(0.1)
        ln = rows()[2]
        if "●" in ln: cols.append(ln.index("●"))
    check("while playing the dot never steps backwards (incl. just after a seek)", len(cols) >= 30 and cols == sorted(cols), str(cols))
    check("…and it keeps moving the whole time", cols[-1] - cols[0] >= (bw - 1) * 3 // 20, f"{cols[0]} -> {cols[-1]} on a {bw}-cell line")
    steps = [b - a for a, b in zip(cols, cols[1:])]
    check("…without leaps (no more than 2 cells between 0.1 s looks)", max(steps) <= 2, str(steps))

    t.send("q", 0.0); time.sleep(0.5); t.close()
finally:
    mock.kill(); subprocess.run(["pkill", "-x", "mpv"]); shutil.rmtree(home, ignore_errors=True)
print("\n%d failure(s)" % len(failures)); sys.exit(1 if failures else 0)
