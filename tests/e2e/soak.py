#!/usr/bin/env python3
"""Soak test: does memory keep climbing with use?

Drives a long, varied workload and samples the resident memory of the app and of its
mpv child separately, then fits a straight line to each (after a warm-up) so a slow leak
shows up as a slope instead of hiding in noise.

    MOCK_SONGS=5000 MOCK_COVERS=300 python3 tests/e2e/soak.py [binary] [ops]
"""
import os, subprocess, sys, tempfile, time, shutil
HERE = os.path.dirname(os.path.abspath(__file__)); sys.path.insert(0, HERE)
from drive import Term, KEYS

binary = sys.argv[1] if len(sys.argv) > 1 else None
OPS = int(sys.argv[2]) if len(sys.argv) > 2 else 400
if binary: os.environ["RUSTDROME_BIN"] = os.path.abspath(binary)

def rss_kb(pid):
    try:
        for l in open(f"/proc/{pid}/status"):
            if l.startswith("VmRSS"): return int(l.split()[1])
    except FileNotFoundError: pass
    return 0
def kids(pid):
    out = []
    for p in os.listdir("/proc"):
        if p.isdigit():
            try:
                if int(open(f"/proc/{p}/stat").read().rsplit(")", 1)[1].split()[1]) == pid: out.append(int(p))
            except Exception: pass
    return out
def slope(xs, ys):
    n = len(xs); mx, my = sum(xs) / n, sum(ys) / n
    den = sum((x - mx) ** 2 for x in xs)
    return sum((x - mx) * (y - my) for x, y in zip(xs, ys)) / den if den else 0.0

mock = subprocess.Popen([sys.executable, os.path.join(HERE, "mock_server.py"), "4599"], env=dict(os.environ), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(2.5)
home = tempfile.mkdtemp(prefix="rd-soak-")
try:
    t = Term(home, probe="sixel"); t.pump(2.0)
    t.send("http://127.0.0.1:4599"); t.send(KEYS["TAB"]); t.send("demo"); t.send(KEYS["TAB"]); t.send("sesame"); t.send(KEYS["ENTER"], 6.0)
    t.send(KEYS["TAB"]); t.send(KEYS["ENTER"], 2.0)
    samples = []
    def snap(i):
        a = rss_kb(t.pid); m = sum(rss_kb(k) for k in kids(t.pid))
        samples.append((i, a / 1024, m / 1024)); print(f"ops {i:4d}   app {a/1024:6.1f} MB   mpv {m/1024:6.1f} MB   total {(a+m)/1024:6.1f} MB", flush=True)
    snap(0)
    sizes = [(150, 42), (140, 40)]; si = 0
    for i in range(1, OPS + 1):
        t.send("n", 0.22)
        if i % 7 == 0:  t.send("/", 0.2); t.send("tra", 0.35); t.send(KEYS["ESC"], 0.2)
        if i % 11 == 0: t.send("?", 0.2); t.send(KEYS["ESC"], 0.2)
        if i % 13 == 0: t.send("t", 0.15)
        if i % 17 == 0: si ^= 1; t.set_size(*sizes[si]); t.pump(0.5)
        if i % 23 == 0: t.send("z", 0.4); t.send("z", 0.4)
        if i % 29 == 0: t.send("\\", 0.2); t.send("al", 0.3); t.send(KEYS["ESC"], 0.2)
        if i % 31 == 0: t.send("\x1b[<0;26;13M", 0.1); t.send("\x1b[<32;34;13M", 0.1); t.send("\x1b[<0;34;13m", 0.3)
        if i % 50 == 0: snap(i)
    pts = [s for s in samples if s[0] >= 100]       # after warm-up
    xs = [p[0] for p in pts]
    print("\nslope after warm-up (per 100 operations):")
    print(f"  app  {slope(xs,[p[1] for p in pts])*100:+7.2f} MB    mpv  {slope(xs,[p[2] for p in pts])*100:+7.2f} MB")
    t.close()
finally:
    mock.kill(); subprocess.run(["pkill", "-x", "mpv"]); shutil.rmtree(home, ignore_errors=True)
