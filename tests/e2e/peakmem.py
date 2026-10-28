#!/usr/bin/env python3
"""Peak memory with realistic (large) tracks: does prefetching/downloading spike the app's RSS?

    MOCK_WAV_SECONDS=180 python3 tests/e2e/peakmem.py [binary]      # 180 s ≈ 31 MB per track
"""
import os, subprocess, sys, tempfile, time, shutil
HERE = os.path.dirname(os.path.abspath(__file__)); sys.path.insert(0, HERE)
from drive import Term, KEYS
if len(sys.argv) > 1: os.environ["RUSTDROME_BIN"] = os.path.abspath(sys.argv[1])
def status(pid, key):
    for l in open(f"/proc/{pid}/status"):
        if l.startswith(key): return int(l.split()[1]) / 1024
mock = subprocess.Popen([sys.executable, os.path.join(HERE, "mock_server.py"), "4599"], env=dict(os.environ), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(4.0); home = tempfile.mkdtemp(prefix="rd-peak-")
try:
    t = Term(home, probe="sixel"); t.pump(2.0)
    t.send("http://127.0.0.1:4599"); t.send(KEYS["TAB"]); t.send("demo"); t.send(KEYS["TAB"]); t.send("sesame"); t.send(KEYS["ENTER"], 4.0)
    t.send(KEYS["TAB"]); t.send(KEYS["ENTER"], 2.0)
    size = os.path.getsize
    print(f"{'after':22s} {'now':>8s} {'PEAK':>8s}   (app RSS, MB)")
    for i in range(1, 13):
        t.send("n", 1.2); t.pump(1.0)
        cache = os.path.join(home, ".cache/rustdrome")
        count = lambda d: len([f for f in os.listdir(os.path.join(cache, d)) if not f.endswith(".part")]) if os.path.isdir(os.path.join(cache, d)) else 0
        kept = count("audio-cache"); mb = sum(os.path.getsize(os.path.join(cache, "audio-cache", f)) for f in os.listdir(os.path.join(cache, "audio-cache"))) / 1e6 if kept else 0
        print(f"track change {i:<10d} {status(t.pid,'VmRSS'):8.1f} {status(t.pid,'VmHWM'):8.1f}   look-ahead cache: {kept} files, {mb:5.0f} MB   your downloads: {count('audio')}")
    t.close()
finally:
    mock.kill(); subprocess.run(["pkill", "-x", "mpv"]); shutil.rmtree(home, ignore_errors=True)
