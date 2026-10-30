#!/usr/bin/env python3
"""mpv must never outlive rustdrome: closing the terminal (SIGHUP), SIGTERM and
a hard SIGKILL of the app all have to take the mpv child with them.

    RUSTDROME_BIN=target/debug/rustdrome python3 tests/e2e/mpv_cleanup.py
"""
import os, pty, signal, sys, time, tempfile, fcntl, termios

BIN = os.environ.get("RUSTDROME_BIN", "target/debug/rustdrome")


def mpv_pids(app_pid):
    """pids of mpv processes started by `app_pid` (found by their IPC socket name)."""
    out = []
    for p in os.listdir("/proc"):
        if not p.isdigit():
            continue
        try:
            cmd = open(f"/proc/{p}/cmdline", "rb").read().split(b"\0")
        except OSError:
            continue
        if cmd and os.path.basename(cmd[0].decode(errors="replace")) == "mpv" \
                and any(f"rustdrome-mpv-{app_pid}".encode() in a for a in cmd):
            if not is_zombie(int(p)):
                out.append(int(p))
    return out


def is_zombie(pid):
    try:
        return open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()[0] == "Z"
    except OSError:
        return True


def alive(pid):
    return os.path.exists(f"/proc/{pid}") and not is_zombie(pid)


def wait_for(cond, secs):
    end = time.time() + secs
    while time.time() < end:
        if cond():
            return True
        time.sleep(0.05)
    return cond()


def launch():
    home = tempfile.mkdtemp(prefix="rd-cleanup-")
    env = dict(os.environ, HOME=home, XDG_CONFIG_HOME=home + "/c", XDG_CACHE_HOME=home + "/k",
               XDG_DATA_HOME=home + "/d", XDG_RUNTIME_DIR=home, TERM="xterm-256color",
               RUSTDROME_ART="off")
    pidfile = home + "/app.pid"
    leader, master = pty.fork()       # `leader` plays the shell: session leader of the pty
    if leader == 0:
        app = os.fork()
        if app == 0:
            os.execve(BIN, [BIN], env)
        open(pidfile, "w").write(str(app))
        os.waitpid(app, 0)            # a shell outlives its job, so killing only the app sends no SIGHUP
        time.sleep(30)
        os._exit(0)
    wait_for(lambda: os.path.exists(pidfile) and open(pidfile).read(), 5)
    pid = int(open(pidfile).read())
    fcntl.ioctl(master, termios.TIOCSWINSZ, b"\x28\x00\x78\x00\x00\x00\x00\x00")  # 40x120
    # keep draining the pty so the app never blocks on a full buffer
    os.set_blocking(master, False)
    return pid, master, leader


def drain(master):
    if master is None:
        return
    try:
        while os.read(master, 65536):
            pass
    except (BlockingIOError, OSError):
        pass


def scenario(name, hit):
    pid, master, leader = launch()
    ok = wait_for(lambda: (drain(master), mpv_pids(pid))[1], 15)
    mpvs = mpv_pids(pid)
    if not ok or not mpvs:
        print(f"FAIL {name}: mpv never started"); os.kill(pid, signal.SIGKILL); return False
    hit(pid, master)
    # keep playing terminal while we wait: an undrained pty would stall the app's redraw writes
    def all_gone():
        try:
            drain(master)
        except OSError:
            pass
        return all(not alive(m) for m in mpvs)
    gone = wait_for(all_gone, 5)
    for m in mpvs:                      # never leave strays behind, pass or fail
        if alive(m):
            os.kill(m, signal.SIGKILL)
    for p in (pid, leader):
        try:
            os.kill(p, signal.SIGKILL)
        except OSError:
            pass
    try:
        os.waitpid(leader, 0)
    except OSError:
        pass
    print(("ok   " if gone else "FAIL ") + f"{name}: mpv {'exited' if gone else 'SURVIVED'}")
    return gone


def close_terminal(pid, master):
    os.close(master)                    # what closing the terminal window does: the tty hangs up


results = [
    scenario("terminal closed", close_terminal),
    scenario("SIGHUP to the app only", lambda pid, m: os.kill(pid, signal.SIGHUP)),
    scenario("SIGTERM to the app only", lambda pid, m: os.kill(pid, signal.SIGTERM)),
    scenario("SIGKILL to the app only", lambda pid, m: os.kill(pid, signal.SIGKILL)),
]
sys.exit(0 if all(results) else 1)
