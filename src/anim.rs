//! Animation primitives — pure functions the widgets call every tick.
//!
//! One shared 8fps heartbeat drives everything (logo shimmer, progress pulse,
//! marquee, spinners). Under the `system` theme colors can't
//! be blended, so gradients collapse to flat styled text.

use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::theme::{blend, Theme};

pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn spinner(tick: usize) -> &'static str {
    SPINNER[tick % SPINNER.len()]
}

// ── the shimmer ───────────────────────────────────────────────────────

const SHIMMER_TAIL: f64 = 10.0;

/// A soft highlight that sweeps across `text`. Flat text under ANSI palettes.
pub fn shimmer(text: &str, phase: f64, base: ratatui::style::Color, glow: ratatui::style::Color, th: &Theme) -> Vec<Span<'static>> {
    if !th.can_blend() {
        return vec![Span::styled(
            text.to_string(),
            Style::default().fg(base).add_modifier(Modifier::BOLD),
        )];
    }
    let n = text.chars().count() as f64;
    let center = (phase % (n + SHIMMER_TAIL)) - SHIMMER_TAIL / 2.0;
    text.chars()
        .enumerate()
        .map(|(i, ch)| {
            let d = (i as f64 - center).abs();
            let intensity = (1.0 - d / 4.0).max(0.0).powi(2);
            let mut style = Style::default().fg(blend(base, glow, intensity));
            if intensity > 0.55 {
                style = style.add_modifier(Modifier::BOLD);
            }
            Span::styled(ch.to_string(), style)
        })
        .collect()
}

// ── smooth progress bar (sub-cell precision) ──────────────────────────

const EIGHTHS: [char; 8] = ['▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];

/// A progress bar with 1/8-cell resolution and a gently pulsing head.
/// `head_pulse` in [0,1] brightens the leading edge.
pub fn smooth_bar(fraction: f64, width: usize, head_pulse: f64, th: &Theme) -> Vec<Span<'static>> {
    let fraction = fraction.clamp(0.0, 1.0);
    let eighths = (fraction * width as f64 * 8.0).round() as usize;
    let (full, rem) = (eighths / 8, eighths % 8);
    let fill = blend(th.blue, th.lav, 0.3);
    let head = blend(fill, th.text, 0.55 * head_pulse);
    let mut spans = Vec::new();
    if full > 0 {
        if full > 1 {
            spans.push(Span::styled("█".repeat(full - 1), Style::default().fg(fill)));
        }
        let c = if rem == 0 { head } else { fill };
        spans.push(Span::styled("█", Style::default().fg(c)));
    }
    if rem > 0 && full < width {
        spans.push(Span::styled(EIGHTHS[rem - 1].to_string(), Style::default().fg(head)));
    }
    let used = full + usize::from(rem > 0);
    if used < width {
        spans.push(Span::styled("╌".repeat(width - used), Style::default().fg(th.vfaint)));
    }
    spans
}

/// Small blocky gauge for volume.
pub fn mini_gauge(fraction: f64, width: usize, th: &Theme) -> Vec<Span<'static>> {
    let lit = (fraction.clamp(0.0, 1.0) * width as f64).round() as usize;
    vec![
        Span::styled("▮".repeat(lit), Style::default().fg(th.lav)),
        Span::styled("▯".repeat(width - lit), Style::default().fg(th.vfaint)),
    ]
}

// ── marquee ───────────────────────────────────────────────────────────

const MARQUEE_GAP: &str = "   ·   ";
const MARQUEE_DWELL: usize = 10;

/// Slide text that doesn't fit; text that fits is returned untouched.
/// Width-aware so CJK titles don't overflow the cell budget.
pub fn marquee(text: &str, width: usize, tick: usize) -> String {
    if text.width() <= width {
        return text.to_string();
    }
    let looped: Vec<char> = format!("{text}{MARQUEE_GAP}").chars().collect();
    let span = looped.len();
    let pos = tick % (span + MARQUEE_DWELL);
    let offset = pos.saturating_sub(MARQUEE_DWELL);
    let mut out = String::new();
    let mut used = 0;
    for i in 0..span * 2 {
        let ch = looped[(offset + i) % span];
        let w = ch.width().unwrap_or(0);
        if used + w > width {
            break;
        }
        used += w;
        out.push(ch);
    }
    out
}

/// Truncate to a display width with an ellipsis.
pub fn ellipsize(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        used += w;
        out.push(ch);
    }
    out.push('…');
    out
}

// ── time ──────────────────────────────────────────────────────────────

pub fn fmt_time(seconds: f64) -> String {
    if seconds.is_nan() || seconds <= 0.0 {
        return "0:00".into();
    }
    let s = seconds as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_time_formats() {
        assert_eq!(fmt_time(0.0), "0:00");
        assert_eq!(fmt_time(65.9), "1:05");
        assert_eq!(fmt_time(3725.0), "1:02:05");
        assert_eq!(fmt_time(-3.0), "0:00");
    }

    #[test]
    fn marquee_fits_untouched_and_slides_when_long() {
        assert_eq!(marquee("short", 10, 99), "short");
        let long = "a very long title that cannot possibly fit";
        // dwell at the start of each loop
        assert_eq!(marquee(long, 8, 0), "a very l");
        assert_eq!(marquee(long, 8, 5), "a very l");
        // then it moves
        assert_eq!(marquee(long, 8, 12), "very lon");
        for t in 0..200 {
            assert!(marquee(long, 8, t).width() <= 8);
        }
    }

    #[test]
    fn ellipsize_respects_width() {
        assert_eq!(ellipsize("hello", 10), "hello");
        let e = ellipsize("hello world", 6);
        assert_eq!(e, "hello…");
        assert!(e.width() <= 6);
    }

    #[test]
    fn smooth_bar_has_exact_width() {
        let th = crate::theme::by_name("mocha");
        for pct in [0.0, 0.013, 0.5, 0.999, 1.0] {
            let w: usize = smooth_bar(pct, 30, 0.0, &th)
                .iter()
                .map(|s| s.content.width())
                .sum();
            assert_eq!(w, 30, "at {pct}");
        }
    }
}
