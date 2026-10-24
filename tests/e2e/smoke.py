#!/usr/bin/env python3
"""End-to-end smoke test: mock Subsonic server + real rustdrome + real mpv (null audio).

    cargo build && python3 tests/e2e/smoke.py
"""
import json, os, shutil, subprocess, sys, tempfile, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from drive import Term, KEYS

HERE = os.path.dirname(os.path.abspath(__file__))
LOG = "/tmp/mock_requests.log"  # written by mock_server.py
failures = []

def check(name, ok):
    print(("PASS " if ok else "FAIL ") + name)
    if not ok:
        failures.append(name)

def requests(*eps):
    return [r for r in map(json.loads, open(LOG)) if r[0] in eps]

mock = subprocess.Popen([sys.executable, os.path.join(HERE, "mock_server.py"), "4599"],
                        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(1.5)
home = tempfile.mkdtemp(prefix="rustdrome-e2e-")
try:
    open(LOG, "w").close()
    t = Term(home)
    has = lambda s: any(s in l for l in t.text().split("\n"))
    for _ in range(40):                      # a cold machine can take seconds; don't race it
        t.pump(0.25)
        if has("connect to your navidrome"): break
    check("first run shows the login form", has("connect to your navidrome"))
    t.send("http://127.0.0.1:4599"); t.send(KEYS["TAB"]); t.send("demo"); t.send(KEYS["TAB"]); t.send("wrong")
    t.send(KEYS["ENTER"], 1.5)
    check("wrong password is rejected with the server's message", has("Wrong username or password"))
    for _ in range(5): t.send(KEYS["BS"], 0.05)
    t.send("sesame"); t.send(KEYS["ENTER"], 3.0)
    check("correct login loads the library", has("Track 01"))
    cfg = os.path.join(home, ".config/rustdrome/config.toml")
    check("credentials saved 0600 without the password",
          oct(os.stat(cfg).st_mode & 0o777) == "0o600" and "sesame" not in open(cfg).read())

    t.send(KEYS["TAB"]); t.send(KEYS["ENTER"], 3.0)
    check("playback starts and cover art renders", has("0:0") and "▀" in t.text())
    check("now-playing scrobble sent", any(r[1].get("submission") == ["false"] for r in requests("scrobble")))
    t.send("n", 1.0); check("next track advances", has("Track 02"))
    t.send("/", 0.5); t.send("daft", 1.8)
    check("search shows songs, albums and artists", all(has(" " + g) for g in ("songs", "albums", "artists")))
    t.send(KEYS["ESC"], 0.4)
    open(LOG, "w").close(); t.send("f", 0.8); t.send("4", 0.8)
    check("star and rating reach the server exactly once",
          len(requests("star")) == 1 and [r[1]["rating"] for r in requests("setRating")] == [["4"]])
    t.send("L", 1.5); check("synced lyrics modal", has("first lyric line")); t.send(KEYS["ESC"], 0.3)
    t.send("y", 1.0); t.send(KEYS["ENTER"], 1.2); check("genre browse", has("genre · Trip-hop"))
    t.send(KEYS["CTRL_W"], 0.6); check("stats modal", has("listening stats")); t.send(KEYS["ESC"], 0.3)
    t.send("q", 0.0); time.sleep(0.5)
    q = json.load(open(os.path.join(home, ".cache/rustdrome/queue.json")))
    check("queue persisted on quit", len(q["songs"]) > 0 and q["index"] >= 0)
    t.close()
finally:
    mock.kill(); subprocess.run(["pkill", "-x", "mpv"]); shutil.rmtree(home, ignore_errors=True)
print("\n%d failure(s)" % len(failures)); sys.exit(1 if failures else 0)
