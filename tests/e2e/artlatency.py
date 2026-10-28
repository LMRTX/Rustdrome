#!/usr/bin/env python3
"""How long from pressing "next" until the new cover's sixel image is on the wire?

    MOCK_COVERS=300 MOCK_LATENCY_MS=80 python3 tests/e2e/artlatency.py [binary] [changes]
"""
import os, select, statistics, subprocess, sys, tempfile, time, shutil
HERE = os.path.dirname(os.path.abspath(__file__)); sys.path.insert(0, HERE)
from drive import Term, KEYS, sixel_blobs
if len(sys.argv) > 1: os.environ["RUSTDROME_BIN"] = os.path.abspath(sys.argv[1])
N = int(sys.argv[2]) if len(sys.argv) > 2 else 12
COLD = len(sys.argv) > 3 and sys.argv[3] == "cold"   # jump to an arbitrary track: nothing could have been prefetched
mock = subprocess.Popen([sys.executable, os.path.join(HERE, "mock_server.py"), "4599"], env=dict(os.environ), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(9.0); home = tempfile.mkdtemp(prefix="rd-lat-")
def wait_image(t, n, limit=6.0):
    t0 = time.time()
    while time.time() - t0 < limit:
        r, _, _ = select.select([t.fd], [], [], 0.004)
        if r:
            try: d = os.read(t.fd, 1 << 20)
            except OSError: break
            t.raw += d; t._feed(d); t._answer_queries()
            if len(sixel_blobs(t.raw)) > n: return time.time() - t0
    return None
try:
    t = Term(home, probe="sixel"); t.pump(2.0)
    t.send("http://127.0.0.1:4599"); t.send(KEYS["TAB"]); t.send("demo"); t.send(KEYS["TAB"]); t.send("sesame"); t.send(KEYS["ENTER"], 4.0)
    t.send(KEYS["TAB"]); t.send(KEYS["ENTER"], 3.0)
    times = []
    for i in range(N):
        t.pump(1.5)                                   # time for any look-ahead work to finish
        if COLD: t.send("g", 0.2); t.send("j" * (3 + 2 * (i % 17)), 0.3)   # a track whose cover was never prefetched
        n = len(sixel_blobs(t.raw)); os.write(t.fd, b"\r" if COLD else b"n"); dt = wait_image(t, n)
        times.append(dt)
    ok = [x for x in times if x is not None]
    print("latency from 'next' to image (ms):", " ".join(f"{x*1000:.0f}" for x in ok))
    print(f"median {statistics.median(ok)*1000:.0f} ms   worst {max(ok)*1000:.0f} ms   missed {len(times)-len(ok)}")
    t.close()
finally:
    mock.kill(); subprocess.run(["pkill", "-x", "mpv"]); shutil.rmtree(home, ignore_errors=True)
