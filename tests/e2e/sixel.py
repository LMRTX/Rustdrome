#!/usr/bin/env python3
"""End-to-end sixel test: the real binary in a pseudo-terminal that answers the
capability probe like a sixel terminal, checking what is written to it.

    cargo build && python3 tests/e2e/sixel.py
"""
import fcntl, os, shutil, signal, struct, subprocess, sys, tempfile, termios, time
HERE = os.path.dirname(os.path.abspath(__file__)); sys.path.insert(0, HERE)
from drive import Term, KEYS, sixel_blobs, sixel_to_image

failures = []
def check(name, ok):
    print(("PASS " if ok else "FAIL ") + name)
    if not ok: failures.append(name)

mock = subprocess.Popen([sys.executable, os.path.join(HERE, "mock_server.py"), "4599"],
                        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(1.5)
homes = []

def login_profile():
    """A profile that is already logged in, made by driving the real login form."""
    h = tempfile.mkdtemp(prefix="rustdrome-six-"); homes.append(h)
    t = Term(h); t.pump(2.0)
    t.send("http://127.0.0.1:4599"); t.send(KEYS["TAB"]); t.send("demo"); t.send(KEYS["TAB"]); t.send("sesame")
    t.send(KEYS["ENTER"], 3.0); t.close(); subprocess.run(["pkill", "-x", "mpv"])
    return h

def play(h, cols=150, rows=42, probe="sixel", env=None):
    t = Term(h, cols=cols, rows=rows, probe=probe, env=env); t.pump(2.5)
    t.send(KEYS["TAB"], .3); t.send(KEYS["ENTER"], 1.0); t.pump(3.0)
    return t

def delta(t, keys, wait=1.8):
    n = len(sixel_blobs(t.raw)); t.send(keys, wait); t.pump(0.8)
    return len(sixel_blobs(t.raw)) - n

try:
    h = login_profile()
    t = play(h)
    blobs = sixel_blobs(t.raw)
    check("sixel is chosen when the terminal advertises it", len(blobs) == 1 and "▀" not in t.text())
    pos, data = blobs[0]
    img = sixel_to_image(data)
    check("the bytes decode to a picture with a real palette", img.size[0] > 60 and len(set(img.getdata())) > 20)
    check("it is placed inside the cover panel (right-hand column)", pos is not None and pos[0] >= 150 - 36)
    n = len(sixel_blobs(t.raw)); t.pump(6.0)
    check("it is drawn once, not re-sent every animation frame", len(sixel_blobs(t.raw)) == n)
    t.close()

    t = play(h, cols=100, rows=36)       # narrow: the help modal overlaps the cover
    n = len(sixel_blobs(t.raw)); c0 = t.raw.count(b"\x1b[2J"); t.send("?", 1.0)
    check("an overlapping modal suppresses the image", len(sixel_blobs(t.raw)) == n)
    check("…without blanking the whole screen to do it", t.raw.count(b"\x1b[2J") == c0)
    check("closing the modal restores it", delta(t, KEYS["ESC"]) == 1)
    check("…and without a full-screen clear either", t.raw.count(b"\x1b[2J") == c0)
    check("changing theme re-sends it", delta(t, "t") == 1)
    check("changing track re-sends it", delta(t, "n", 2.5) == 1)
    check("a toast does not touch it", delta(t, "f", 1.0) == 0)
    check("zen mode sends a larger image", delta(t, "z", 2.5) == 1 and sixel_to_image(sixel_blobs(t.raw)[-1][1]).size[0] > 300)
    check("leaving zen restores the panel image", delta(t, "z", 2.5) == 1)
    fcntl.ioctl(t.fd, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 120, 0, 0)); os.kill(t.pid, signal.SIGWINCH)
    t.screen.resize(30, 120); n = len(sixel_blobs(t.raw)); t.pump(2.5)
    check("resizing the terminal re-sends it at the new size", len(sixel_blobs(t.raw)) == n + 1)
    t.close()

    for label, probe, env, expect in [
        ("a terminal that ignores the probe falls back to half-cells", None, {}, "half"),
        ("a terminal answering without sixel falls back to half-cells", "plain", {}, "half"),
        ("RUSTDROME_ART=halfcell overrides a sixel terminal", "sixel", {"RUSTDROME_ART": "halfcell"}, "half"),
        ("RUSTDROME_ART=off shows no art", "sixel", {"RUSTDROME_ART": "off"}, "none"),
        ("RUSTDROME_ART=sixel forces sixel without a reply", None, {"RUSTDROME_ART": "sixel"}, "sixel"),
    ]:
        t = play(h, probe=probe, env=env)
        got = "sixel" if sixel_blobs(t.raw) else ("half" if "▀" in t.text() else "none")
        check(label, got == expect); t.close()
finally:
    mock.kill(); subprocess.run(["pkill", "-x", "mpv"]); [shutil.rmtree(x, ignore_errors=True) for x in homes]
print("\n%d failure(s)" % len(failures)); sys.exit(1 if failures else 0)
