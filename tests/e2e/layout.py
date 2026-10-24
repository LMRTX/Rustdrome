#!/usr/bin/env python3
"""End-to-end: font zoom / DPI changes, zen centring, drag-resizable panels, square mode.

    cargo build && python3 tests/e2e/layout.py
"""
import json, os, shutil, subprocess, sys, tempfile, time
HERE = os.path.dirname(os.path.abspath(__file__)); sys.path.insert(0, HERE)
from drive import Term, KEYS, sixel_blobs, sixel_to_image

failures = []
def check(name, ok, detail=""):
    print(("PASS " if ok else "FAIL ") + name + ("" if ok or not detail else f"   [{detail}]"))
    if not ok: failures.append(name)

mock = subprocess.Popen([sys.executable, os.path.join(HERE, "mock_server.py"), "4599"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(1.5)
homes = []

def login_profile():
    h = tempfile.mkdtemp(prefix="rustdrome-lay-"); homes.append(h)
    t = Term(h); t.pump(2.0)
    t.send("http://127.0.0.1:4599"); t.send(KEYS["TAB"]); t.send("demo"); t.send(KEYS["TAB"]); t.send("sesame")
    t.send(KEYS["ENTER"], 3.0); t.close(); subprocess.run(["pkill", "-x", "mpv"])
    return h

def start(h, cols=150, rows=42, cell=(10, 20), px=True):
    t = Term(h, cols=cols, rows=rows, probe="sixel", cell=cell, px=px); t.pump(2.5)
    t.send(KEYS["TAB"], .3); t.send(KEYS["ENTER"], 1.0); t.pump(3.0)
    return t

def cover_box(t):
    """Inner cells (x, y, w, h) of the cover panel, read off the drawn borders."""
    L = t.screen.display
    for y, line in enumerate(L):
        i = line.find(" cover ")
        if i < 0: continue
        xl = max(line.rfind("╭", 0, i), line.rfind("┌", 0, i))
        xr = min(p for p in (line.find("╮", i), line.find("┐", i)) if p >= 0)
        yb = next(yy for yy in range(y + 1, len(L)) if L[yy][xl] in "╰└")
        return xl + 1, y + 1, xr - xl - 1, yb - y - 1
    return None

def last_image(t):
    b = sixel_blobs(t.raw)
    return (b[-1][0], sixel_to_image(b[-1][1]).size, len(b)) if b else (None, None, 0)

def fits(t, label):
    """The picture fills its panel without spilling out, is cell-aligned, and is centred."""
    box = cover_box(t); pos, (W, H), n = last_image(t); cw, ch = t.cell
    if not (box and pos): return check(label, False, "no panel or no image")
    x0, y0, w, h = box; cols, rows = W // cw, H // ch
    ok_fit = W <= w * cw and H <= h * ch
    ok_fill = max(W, H) >= min(w * cw, h * ch) - 1
    ok_align = W % cw == 0 and H % ch == 0
    ok_center = (pos[0] - x0) == (x0 + w) - (pos[0] + cols) and (pos[1] - y0) == (y0 + h) - (pos[1] + rows)
    check(label, ok_fit and ok_fill and ok_align and ok_center,
          f"image {W}x{H}px @ {pos}, panel {w}x{h} cells = {w*cw}x{h*ch}px, cell {t.cell}; fit={ok_fit} fill={ok_fill} align={ok_align} centre={ok_center}")

def settle(t, n, limit=20.0):
    """Wait for a new image to arrive (debug builds encode ~30x slower than release),
    then a moment longer so any *extra* image would show up too."""
    t0 = time.time()
    while time.time() - t0 < limit and len(sixel_blobs(t.raw)) <= n: t.pump(0.25)
    t.pump(1.0)

def bracketed(raw):
    """After a resize ratatui clears the screen. Is that clear *and* the repaint that
    follows inside one synchronized-update bracket (so the blank is never presented)?"""
    i = raw.find(b"\x1b[2J")
    if i < 0: return None
    h, l = raw.rfind(b"\x1b[?2026h", 0, i), raw.find(b"\x1b[?2026l", i)
    return h >= 0 and l > i and b"library" in raw[i:l]

def resized(t, cols, rows, cell=None, px=None):
    n = len(sixel_blobs(t.raw)); t.set_size(cols, rows, cell, px); settle(t, n)
    return len(sixel_blobs(t.raw)) - n

try:
    h = login_profile()

    # ── A. zoom the font in and out (the reported bug) ────────────────────
    for px in (True, False):
        mode = "window reports pixels" if px else "window reports NO pixels (probe only)"
        t = start(h, px=px)
        fits(t, f"[{mode}] initial 10x20 cells")
        mark = len(t.raw); n = resized(t, 107, 30, (14, 28))
        if px: check("a real resize's screen clear and repaint share one synchronized update (no blank frame)", bracketed(t.raw[mark:]) is True, str(bracketed(t.raw[mark:])))
        fits(t, f"[{mode}] font bigger  (14x28): image shrinks WITH the panel"); check(f"[{mode}] …a new image was sent", n >= 1)
        resized(t, 214, 60, (7, 14));      fits(t, f"[{mode}] font smaller  (7x14): image no longer overflows")
        resized(t, 150, 42, (10, 20));     fits(t, f"[{mode}] back to 10x20")
        t.close()

    t = start(h); t.send("?", 0.5); t.send(KEYS["ESC"], 0.5); t.pump(2.0)
    r = t.raw
    check("every synchronized update that is opened is closed", abs(r.count(b"\x1b[?2026h") - r.count(b"\x1b[?2026l")) <= 1, f"{r.count(b'\x1b[?2026h')} open / {r.count(b'\x1b[?2026l')} close")
    t.close()

    # ── B. DPI change: moved to another monitor, rows/cols unchanged ──────
    t = start(h)
    n = resized(t, 150, 42, (13, 26), px=True)
    fits(t, "same rows×cols, cells 10x20 → 13x26 (other monitor)"); check("…detected without any resize of the grid", n >= 1)
    t.close()

    # ── C. zen: centred on the screen, with the right spacing ─────────────
    t = start(h); n = len(sixel_blobs(t.raw)); t.send("z", 0.5); settle(t, n)
    for cols, rows, cell in [(150, 42, (10, 20)), (121, 37, (13, 26)), (200, 56, (8, 16)), (111, 34, (16, 34))]:
        if (cols, rows, cell) != (150, 42, (10, 20)): resized(t, cols, rows, cell)
        pos, (W, H), _ = last_image(t); cw, ch = cell
        if pos is None: check(f"zen {cols}x{rows} cell {cell}: image", False, "no image"); continue
        title = next((y for y, l in enumerate(t.screen.display) if "Track" in l and y > pos[1] + H // ch - 1), None)
        centre_ok = pos[0] * 2 + W // cw == cols
        blank_ok = title == pos[1] + H // ch + 1
        check(f"zen {cols}x{rows} cell {cell}: image centred exactly, one blank row before the title", centre_ok and blank_ok,
              f"image x={pos[0]} cols={W//cw} of {cols}; bottom row {pos[1]+H//ch}, title row {title}")
    L = t.screen.display; ti = next(y for y, l in enumerate(L) if "Track" in l and " — " not in l and "·" not in l and "now playing" not in l)
    al = next((y for y in range(ti + 1, len(L)) if "Daft" in L[y] or "Nina" in L[y] or "Radiohead" in L[y] or "Bonobo" in L[y]), None)
    check("zen: album line directly under the title, then a blank row, then the lyrics", al == ti + 1 and not L[al + 1].strip(), f"title {ti} album {al}")
    t.close()

    # ── C2. shrinking the window: the "too small" screen, no crash, full recovery ──
    t = start(h)
    seq = [(100, 30), (100, 16), (100, 15), (100, 12), (100, 6), (60, 12), (63, 40), (40, 8), (30, 5), (10, 3), (1, 1)]
    base = None                                      # images sent as of the moment it first became too small
    for cols, rows in seq:
        too_small = cols < 64 or rows < 16
        if too_small and base is None: base = len(sixel_blobs(t.raw))
        if too_small: t.set_size(cols, rows); t.pump(0.7)
        else: n0 = len(sixel_blobs(t.raw)); t.set_size(cols, rows); settle(t, n0)   # a valid size gets its image
        txt = "\n".join(t.screen.display)
        if cols >= 26 and rows >= 6:     # the message needs room of its own to be readable
            shows = "Terminal size too small" in txt
            right = f"Width = {cols} Height = {rows}" in txt
            check(f"{cols}x{rows}: " + ("shows 'Terminal size too small' with the live size" if too_small else "shows the normal UI, no message"),
                  (shows and right and "Needed for current config" in txt and "Width = 64 Height = 16" in txt) if too_small else (not shows and "library" in txt))
    check("no image is drawn while the terminal is too small", len(sixel_blobs(t.raw)) == base)
    n = len(sixel_blobs(t.raw)); t.set_size(150, 42); settle(t, n)
    box_ok = cover_box(t) is not None and "Terminal size too small" not in "\n".join(t.screen.display)
    check("growing the window brings the whole UI back", box_ok)
    fits(t, "…and the cover image comes back, fitted to its panel")
    t.send("?", 0.8); check("…and the app is still responsive (a keypress opens help)", "keys" in "\n".join(t.screen.display)); t.send(KEYS["ESC"], 0.4)
    t.close()

    # ── D. drag-resize the panels ──────────────────────────────────────────
    def press(t, x, y): t.send(f"\x1b[<0;{x+1};{y+1}M", 0.25)
    def drag(t, x, y):  t.send(f"\x1b[<32;{x+1};{y+1}M", 0.25)
    def release(t, x, y): t.send(f"\x1b[<0;{x+1};{y+1}m", 0.6)
    def state(): return json.load(open(os.path.join(h, ".local/state/rustdrome/state.json"))).get("ui_layout", {})
    def corner(t, x): return t.screen.display[1][x] if x < len(t.screen.display[1]) else " "

    t = start(h)
    n0 = len(sixel_blobs(t.raw)); x0, y0, w0, h0 = cover_box(t)
    check("default library panel is 26 wide (corner at column 25)", corner(t, 25) in "╮┐")
    clears0 = t.raw.count(b"\x1b[2J")                      # full-screen clears so far (the black flash)
    press(t, 25, 12); n_mid = len(sixel_blobs(t.raw))
    for x in (30, 36, 42, 44): drag(t, x, 12)
    check("no image traffic while dragging", len(sixel_blobs(t.raw)) == n_mid)
    check("no full-screen clear when a drag starts or moves (that was the black flash)", t.raw.count(b"\x1b[2J") == clears0, f"{t.raw.count(b'\x1b[2J') - clears0} clears")
    check("library panel follows the mouse live (corner at column 44)", corner(t, 44) in "╮┐")
    release(t, 44, 12); settle(t, n_mid)
    check("…and exactly one image is sent once it settles", len(sixel_blobs(t.raw)) == n_mid + 1)
    fits(t, "image fits the cover panel after the library panel was widened")
    check("layout saved to state.json", state().get("sidebar") == 45, str(state()))

    xl = cover_box(t)[0] - 1                                # the cover panel's left border column
    clears1 = t.raw.count(b"\x1b[2J")
    press(t, xl, 12)
    for x in (xl - 6, xl - 12): drag(t, x, 12)
    nn = len(sixel_blobs(t.raw)); release(t, xl - 12, 12); settle(t, nn)
    check("…nor when dragging the right column's border", t.raw.count(b"\x1b[2J") == clears1)
    nb = cover_box(t); check("right column widened by dragging the tracks|cover border left", nb[2] > w0 + 8, f"{w0} → {nb[2]}")
    fits(t, "image fits after the right column was widened")

    yb = cover_box(t)[1] + cover_box(t)[3]                  # the row of the cover panel's bottom border
    before = cover_box(t)[3]; clears2 = t.raw.count(b"\x1b[2J")
    press(t, cover_box(t)[0] + 3, yb)
    for y in (yb - 3, yb - 6): drag(t, cover_box(t)[0] + 3, y)
    nn = len(sixel_blobs(t.raw)); release(t, cover_box(t)[0] + 3, yb - 6); settle(t, nn)
    check("…nor when dragging the cover/queue border", t.raw.count(b"\x1b[2J") == clears2)
    check("cover made shorter by dragging the cover/queue border up", cover_box(t)[3] < before, f"{before} → {cover_box(t)[3]}")
    fits(t, "image fits after the cover panel was made shorter")
    t.close()

    t = Term(h, probe="sixel"); t.pump(2.5)
    check("layout survives a restart", corner(t, 44) in "╮┐")
    click = "\x1b[<0;45;13M\x1b[<0;45;13m"            # press + release at column 44, row 12 (1-based on the wire)
    t.send(click + click, 1.0)                           # both clicks inside the 400ms double-click window
    check("double-clicking a border resets it to the default", corner(t, 25) in "╮┐" and state().get("sidebar") == 26, str(state()))
    t.close()

    # ── E. square mode ─────────────────────────────────────────────────────
    t = start(h)
    scr = lambda: "".join(t.screen.display)
    check("rounded corners by default", "╭" in scr() and "┌" not in scr())
    t.send("B", 0.8)
    check("B switches every box to square corners", "┌" in scr() and "╭" not in scr() and "╮" not in scr())
    fits(t, "the image still fits in square mode")
    t.send("?", 0.6); check("modals are square too", "┌" in scr() and "╭" not in scr()); t.send(KEYS["ESC"], 0.4)
    check("choice saved", json.load(open(os.path.join(h, ".local/state/rustdrome/state.json"))).get("border_style") == "square")
    t.close()
    t = Term(h, probe="sixel"); t.pump(2.5)
    check("square mode survives a restart", "┌" in "".join(t.screen.display) and "╭" not in "".join(t.screen.display))
    t.send("B", 0.6); check("B toggles back to rounded", "╭" in "".join(t.screen.display) and "┌" not in "".join(t.screen.display))
    t.close()
finally:
    mock.kill(); subprocess.run(["pkill", "-x", "mpv"]); [shutil.rmtree(x, ignore_errors=True) for x in homes]
print("\n%d failure(s)" % len(failures)); sys.exit(1 if failures else 0)
