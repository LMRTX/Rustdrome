"""pty + pyte harness: runs the real binary in a pseudo-terminal and reads the screen back.
Requires: pip install pyte pillow ; mpv on PATH. Binary: $RUSTDROME_BIN or target/debug/rustdrome."""
import os, pty, sys, time, select, fcntl, termios, struct, signal, pyte, tempfile, shutil

class Term:
    def __init__(self, home, cols=150, rows=42, env=None, probe=None, cell=(10, 20), px=True):
        # probe: None = never answer (a terminal that ignores queries), "plain" = answer
        # without sixel, "sixel" = answer like a sixel-capable terminal (10x20px cells)
        self.probe, self._scan = probe, 0
        self.cell, self.px, self.cols, self.rows = cell, px, cols, rows
        self.screen = pyte.Screen(cols, rows); self.stream = pyte.ByteStream(self.screen)
        pid, fd = pty.fork()
        if pid == 0:
            e = dict(os.environ, HOME=home, TERM="xterm-256color", RUSTDROME_AO="null", COLORTERM="truecolor", XDG_RUNTIME_DIR=home)
            e.update(env or {})
            binary = os.environ.get("RUSTDROME_BIN") or os.path.join(os.path.dirname(os.path.abspath(__file__)), "../../target/debug/rustdrome")
            os.execve(binary, ["rustdrome"], e)
        self.pid, self.fd = pid, fd
        self._dcs = False  # inside a DCS (sixel) string right now?
        self._winsz()
        self.raw = b""
    def pump(self, secs=0.6):
        end = time.time() + secs
        while time.time() < end:
            r, _, _ = select.select([self.fd], [], [], 0.05)
            if r:
                try: d = os.read(self.fd, 65536)
                except OSError: return
                if not d: return
                self.raw += d; self._feed(d)
                self._answer_queries()
    def _feed(self, d):
        """Feed pyte everything except DCS strings (sixel images): a real terminal
        paints those as pixels, pyte would print the payload as text over the UI."""
        out, i = bytearray(), 0
        while i < len(d):
            if self._dcs:
                j = d.find(b"\x1b\\", i)
                if j < 0: break
                i, self._dcs = j + 2, False
            else:
                j = d.find(b"\x1bP", i)
                if j < 0: out += d[i:]; break
                out += d[i:j]; i, self._dcs = j + 2, True
        self.stream.feed(bytes(out))

    def _winsz(self):
        cw, ch = self.cell
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", self.rows, self.cols, self.cols * cw if self.px else 0, self.rows * ch if self.px else 0))

    def _answer_queries(self):
        """Answer `CSI 16 t` (+ DA1) like a terminal whose cell size is self.cell."""
        if not self.probe: return
        while True:
            i = self.raw.find(b"\x1b[16t", self._scan)
            if i < 0:
                self._scan = max(self._scan, len(self.raw) - 8); return
            self._scan = i + 5
            cw, ch = self.cell
            os.write(self.fd, b"\x1b[6;%d;%dt" % (ch, cw) + (b"\x1b[?62;4;22c" if self.probe == "sixel" else b"\x1b[?62;22c"))

    def set_size(self, cols, rows, cell=None, px=None):
        """Resize the window (and optionally change the font => cell size), like zooming."""
        self.cols, self.rows = cols, rows
        if cell: self.cell = cell
        if px is not None: self.px = px
        self._winsz(); self.screen.resize(rows, cols)
        os.kill(self.pid, signal.SIGWINCH)

    def send(self, s, wait=0.5):
        os.write(self.fd, s.encode() if isinstance(s, str) else s); self.pump(wait)
    def text(self):
        return "\n".join(l.rstrip() for l in self.screen.display)
    def show(self, title=""):
        print(f"──── {title} " + "─" * 60); print(self.text())
    def alive(self):
        try: return os.waitpid(self.pid, os.WNOHANG) == (0, 0)
        except ChildProcessError: return False
    def close(self):
        try: os.kill(self.pid, signal.SIGKILL); os.waitpid(self.pid, 0)
        except Exception: pass

KEYS = dict(UP="\x1b[A", DOWN="\x1b[B", RIGHT="\x1b[C", LEFT="\x1b[D", ENTER="\r", ESC="\x1b", TAB="\t", BS="\x7f",
            CTRL_P="\x10", CTRL_W="\x17")


def sixel_blobs(raw):
    """Every complete sixel DCS in a byte stream, with the cursor position it was drawn at."""
    import re
    out, pos = [], 0
    while True:
        i = raw.find(b"\x1bP0;1;0q", pos)
        if i < 0: return out
        j = raw.find(b"\x1b\\", i)
        if j < 0: return out
        m = None
        for m in re.finditer(rb"\x1b\[(\d+);(\d+)H", raw[max(0, i - 12):i]): pass
        out.append(((int(m.group(2)) - 1, int(m.group(1)) - 1) if m else None, raw[i:j + 2]))
        pos = j + 2

def sixel_to_image(blob):
    """Independent sixel decoder -> PIL image (the check that the bytes really are a picture)."""
    from PIL import Image
    import re
    s = blob[len(b"\x1bP0;1;0q"):-2].decode()
    m = re.match(r'"1;1;(\d+);(\d+)', s); w, h = int(m.group(1)), int(m.group(2)); i = m.end()
    img = Image.new("RGB", (w, h), (255, 0, 255)); px = img.load(); pal = {}; cur = 0; x = band = 0
    def num():
        nonlocal i
        j = i
        while i < len(s) and s[i].isdigit(): i += 1
        return int(s[j:i] or 0)
    def paint(mask, n=1):
        nonlocal x
        for _ in range(n):
            for dy in range(6):
                if mask & (1 << dy) and x < w and band * 6 + dy < h: px[x, band * 6 + dy] = pal.get(cur, (0, 0, 0))
            x += 1
    while i < len(s):
        c = s[i]
        if c == "#":
            i += 1; n = num()
            if i < len(s) and s[i] == ";":
                i += 1; num(); vals = []
                for _ in range(3): i += 1; vals.append(round(num() * 255 / 100))
                pal[n] = tuple(vals)
            else: cur = n
        elif c == "$": x = 0; i += 1
        elif c == "-": x = 0; band += 1; i += 1
        elif c == "!": i += 1; n = num(); paint(ord(s[i]) - 63, n); i += 1
        else: paint(ord(c) - 63); i += 1
    return img
