//! The OS telling us to go away: the terminal was closed (SIGHUP / console close), or the
//! process was asked to stop (SIGTERM / system shutdown).
//!
//! Without a handler the default action ends the process on the spot, so no `Drop` runs and
//! the idle mpv child is left behind. Catching these lets the main loop leave normally, which
//! saves state and stops mpv. The terminal is in raw mode, so Ctrl+C is a key event and never
//! arrives as a signal.

/// Handlers are installed by `new`, so call it early: a signal that lands before the loop
/// starts waiting is remembered and seen by the first `recv`.
pub struct Hangup(Inner);

#[cfg(unix)]
struct Inner {
    hup: Option<tokio::signal::unix::Signal>,
    term: Option<tokio::signal::unix::Signal>,
}

#[cfg(windows)]
struct Inner {
    close: Option<tokio::signal::windows::CtrlClose>,
    shutdown: Option<tokio::signal::windows::CtrlShutdown>,
}

#[cfg(not(any(unix, windows)))]
struct Inner;

/// Wait on a handler that may have failed to install; a missing one never fires.
macro_rules! wait {
    ($sig:expr) => {
        async {
            if let Some(s) = $sig.as_mut() {
                if s.recv().await.is_some() {
                    return;
                }
            }
            std::future::pending::<()>().await
        }
    };
}

impl Hangup {
    #[cfg(unix)]
    pub fn new() -> Self {
        use tokio::signal::unix::{signal, SignalKind};
        Hangup(Inner {
            hup: signal(SignalKind::hangup()).ok(),
            term: signal(SignalKind::terminate()).ok(),
        })
    }

    #[cfg(windows)]
    pub fn new() -> Self {
        use tokio::signal::windows::{ctrl_close, ctrl_shutdown};
        Hangup(Inner { close: ctrl_close().ok(), shutdown: ctrl_shutdown().ok() })
    }

    #[cfg(not(any(unix, windows)))]
    pub fn new() -> Self {
        Hangup(Inner)
    }

    /// Resolves once, when we are asked to quit.
    #[cfg(unix)]
    pub async fn recv(&mut self) {
        let Inner { hup, term } = &mut self.0;
        tokio::select! { _ = wait!(hup) => {}, _ = wait!(term) => {} }
    }

    #[cfg(windows)]
    pub async fn recv(&mut self) {
        let Inner { close, shutdown } = &mut self.0;
        tokio::select! { _ = wait!(close) => {}, _ = wait!(shutdown) => {} }
    }

    #[cfg(not(any(unix, windows)))]
    pub async fn recv(&mut self) {
        std::future::pending::<()>().await
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn sighup_and_sigterm_are_caught_instead_of_killing_us() {
        let mut gone = Hangup::new();
        for sig in [libc::SIGHUP, libc::SIGTERM] {
            // the handler is installed, so raising is safe; with the default action this
            // would end the whole test process
            unsafe { libc::raise(sig) };
            tokio::time::timeout(Duration::from_secs(5), gone.recv())
                .await
                .unwrap_or_else(|_| panic!("signal {sig} was not reported"));
        }
    }

    #[tokio::test]
    async fn silence_means_no_exit() {
        let mut gone = Hangup::new();
        assert!(tokio::time::timeout(Duration::from_millis(100), gone.recv()).await.is_err());
    }
}
