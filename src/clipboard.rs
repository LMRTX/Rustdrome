//! Tiny desktop integrations: copy to clipboard (OSC 52, works over ssh and
//! in most modern terminals) and desktop notifications via `notify-send`.

use std::io::Write;
use std::path::PathBuf;

use base64::Engine;

pub fn osc52(text: &str) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(text);
    format!("\x1b]52;c;{b64}\x07")
}

pub fn copy(text: &str) {
    let mut out = std::io::stdout();
    let _ = out.write_all(osc52(text).as_bytes());
    let _ = out.flush();
}

/// Fire-and-forget; silently does nothing if `notify-send` isn't installed.
pub fn desktop_notify(title: &str, body: &str, icon: Option<PathBuf>) {
    let (title, body) = (title.to_string(), body.to_string());
    std::thread::spawn(move || {
        let mut cmd = std::process::Command::new("notify-send");
        cmd.args(["-a", "Rustdrome", "-t", "4000"]);
        if let Some(i) = icon {
            cmd.arg("-i").arg(i);
        }
        cmd.arg(title).arg(body);
        cmd.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        let _ = cmd.status();
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn osc52_wraps_base64() {
        assert_eq!(super::osc52("hi"), "\x1b]52;c;aGk=\x07");
    }
}
