#!/usr/bin/env python3
"""Measure resident memory (app + its mpv child) in a few realistic states.

    MOCK_SONGS=5000 python3 tests/e2e/memory.py [path/to/binary]
    MEM_PROBE=sixel ...   # pretend to be a sixel terminal (default: half-cell art)
"""
import os, subprocess, sys, tempfile, time, shutil, re
HERE = os.path.dirname(os.path.abspath(__file__)); sys.path.insert(0, HERE)
from drive import Term, KEYS

def rss_kb(pid):
    try:
        for l in open(f"/proc/{pid}/status"):
            if l.startswith("VmRSS"): return int(l.split()[1])
    except FileNotFoundError: pass
    return 0
def children(pid):
    out=[]
    for p in os.listdir("/proc"):
        if p.isdigit():
            try:
                st=open(f"/proc/{p}/stat").read(); ppid=int(st.rsplit(")",1)[1].split()[1])
                if ppid==pid: out.append(int(p))
            except Exception: pass
    return out
def snap(t, label):
    app=rss_kb(t.pid); mpv=sum(rss_kb(c) for c in children(t.pid))
    thr=len(os.listdir(f"/proc/{t.pid}/task"))
    print(f"{label:34s} app {app/1024:6.1f} MB | mpv {mpv/1024:6.1f} MB | total {(app+mpv)/1024:6.1f} MB | threads {thr}")
    return app, mpv

binary = sys.argv[1] if len(sys.argv)>1 else None
if binary: os.environ["RUSTDROME_BIN"]=os.path.abspath(binary)
env=dict(os.environ); mock=subprocess.Popen([sys.executable,os.path.join(HERE,"mock_server.py"),"4599"],env=env,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL); time.sleep(2.0)
home=tempfile.mkdtemp(prefix="rd-mem-")
try:
    t=Term(home, probe=os.environ.get('MEM_PROBE') or None); t.pump(2.0)
    t.send("http://127.0.0.1:4599"); t.send(KEYS["TAB"]); t.send("demo"); t.send(KEYS["TAB"]); t.send("sesame"); t.send(KEYS["ENTER"],6.0)
    snap(t,"idle, library loaded")
    t.send(KEYS["TAB"]); t.send(KEYS["ENTER"],3.0); t.pump(2.0)
    snap(t,"playing (art + viz animating)")
    t.send("/",0.4); t.send("track",2.5); snap(t,"search modal open"); t.send(KEYS["ESC"],0.4)
    t.send("\\",0.3); t.send("al",0.8); snap(t,"filter active over library"); t.send(KEYS["ESC"],0.4)
    t.send("v",0.3)
    for _ in range(3): t.send("G",0.2); t.send("g",0.2)
    t.send(KEYS["ESC"],0.3)
    for _ in range(25): t.send("n",0.15)
    t.pump(3.0); snap(t,"after 25 track changes")
    t.send("z",1.5); snap(t,"zen mode (large art)")
    t.close()
finally:
    mock.kill(); subprocess.run(["pkill","-x","mpv"]); shutil.rmtree(home,ignore_errors=True)
