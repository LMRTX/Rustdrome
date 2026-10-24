//! Terminal input on a dedicated thread that can be *paused*.
//!
//! The terminal answers capability queries (cell size in pixels, sixel
//! support) on the same channel keystrokes arrive on. At startup nothing is
//! reading yet, so a query can read its own reply — but cell size changes at
//! runtime (font zoom, moving the window to a monitor with a different DPI),
//! when crossterm's reader is running and would race us for those bytes.
//! Pausing the reader for the ~50ms a query takes avoids that.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

#[derive(Default)]
struct State {
    pause: bool,
    parked: bool,
    quit: bool,
}

struct Ctl {
    m: Mutex<State>,
    cv: Condvar,
}

pub struct Input {
    pub rx: UnboundedReceiver<Event>,
    ctl: Arc<Ctl>,
}

impl Input {
    pub fn spawn() -> Self {
        let (tx, rx) = unbounded_channel();
        let ctl = Arc::new(Ctl { m: Mutex::new(State::default()), cv: Condvar::new() });
        let c = ctl.clone();
        std::thread::Builder::new()
            .name("input".into())
            .spawn(move || loop {
                {
                    let mut s = c.m.lock().unwrap();
                    while s.pause && !s.quit {
                        s.parked = true;
                        c.cv.notify_all();
                        s = c.cv.wait(s).unwrap();
                    }
                    s.parked = false;
                    if s.quit {
                        return;
                    }
                }
                // Poll in slices so a pause request is honoured promptly. Input
                // itself wakes the poll immediately, so this only bounds how
                // long a pause can wait (`paused` allows 300ms) — 100ms keeps
                // the idle wakeups to ten a second.
                match event::poll(Duration::from_millis(100)) {
                    Ok(true) => match event::read() {
                        Ok(ev) => {
                            if tx.send(ev).is_err() {
                                return;
                            }
                        }
                        Err(_) => return,
                    },
                    Ok(false) => {}
                    Err(_) => return,
                }
            })
            .expect("spawn input thread");
        Self { rx, ctl }
    }

    /// Run `f` with the reader parked, so it can read the terminal's replies
    /// to its own queries without anything else consuming them.
    pub fn paused<R>(&self, f: impl FnOnce() -> R) -> R {
        {
            let mut s = self.ctl.m.lock().unwrap();
            s.pause = true;
            let deadline = Instant::now() + Duration::from_millis(300);
            while !s.parked {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                s = self.ctl.cv.wait_timeout(s, left).unwrap().0;
            }
        }
        let r = f();
        self.ctl.m.lock().unwrap().pause = false;
        self.ctl.cv.notify_all();
        r
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        self.ctl.m.lock().unwrap().quit = true;
        self.ctl.cv.notify_all();
    }
}
