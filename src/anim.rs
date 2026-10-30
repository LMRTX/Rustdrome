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

// ── thin line bar (progress + volume) ─────────────────────────────────

/// Cell (0-based) the dot sits in for `fraction` of a bar `width` cells wide:
/// the first cell is 0 %, the last is 100 %.
pub fn dot_index(fraction: f64, width: usize) -> usize {
    if width <= 1 {
        0
    } else {
        (fraction.clamp(0.0, 1.0) * (width - 1) as f64).round() as usize
    }
}

/// The inverse of [`dot_index`]: what fraction a click on cell `col` means.
/// Clicking the first/last cell gives exactly 0 / 1, so 0 % and 100 % are reachable.
pub fn fraction_at(col: usize, width: usize) -> f64 {
    if width <= 1 {
        0.0
    } else {
        col.min(width - 1) as f64 / (width - 1) as f64
    }
}

/// A thin line with a dot at the current point: `━━━━●────`. Exactly `width`
/// cells. `pulse` in [0,1] brightens the dot.
pub fn line_bar(fraction: f64, width: usize, fill: ratatui::style::Color, pulse: f64, th: &Theme) -> Vec<Span<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let head = dot_index(fraction, width);
    let dot = blend(fill, th.text, 0.55 * pulse.clamp(0.0, 1.0));
    let mut spans = Vec::with_capacity(3);
    if head > 0 {
        spans.push(Span::styled("━".repeat(head), Style::default().fg(fill)));
    }
    spans.push(Span::styled("●", Style::default().fg(dot)));
    if head + 1 < width {
        spans.push(Span::styled("─".repeat(width - head - 1), Style::default().fg(th.vfaint)));
    }
    spans
}

// ── playhead ──────────────────────────────────────────────────────────

/// How far past mpv's last report the playhead may be extrapolated. Reports
/// stop while mpv stalls (buffering); without a cap the dot would run ahead.
pub const MAX_EXTRAPOLATE: f64 = 0.75;

/// Where the playhead is *now*. mpv's position reaches the UI only when it has
/// moved ≥ ¼ s, but the screen redraws every ⅛ s, so drawing the report as it
/// is makes the dot hold still for a frame and then jump two. Instead advance
/// from the last report (`age` seconds old) at the playback `speed`.
///
/// `prev` is what was drawn last frame: a report that lands a hair behind the
/// extrapolation (< ½ s) doesn't pull the dot backwards; a real seek back does.
/// Not running (paused / stopped) shows the report exactly.
pub fn playhead(reported: f64, age: f64, speed: f64, running: bool, duration: f64, prev: f64) -> f64 {
    if !running {
        return reported;
    }
    let mut p = reported + age.clamp(0.0, MAX_EXTRAPOLATE) * speed.max(0.0);
    if duration > 0.0 {
        p = p.min(duration);
    }
    if p < prev && prev - p < 0.5 {
        p = prev;
    }
    p
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
    fn line_bar_has_exact_width_and_one_dot() {
        let th = crate::theme::by_name("mocha");
        for width in [0usize, 1, 2, 3, 30] {
            for pct in [0.0, 0.013, 0.5, 0.999, 1.0, 7.0, -1.0] {
                let spans = line_bar(pct, width, th.lav, 0.0, &th);
                let w: usize = spans.iter().map(|s| s.content.width()).sum();
                assert_eq!(w, width, "width {width} at {pct}");
                let dots: usize = spans.iter().map(|s| s.content.matches('●').count()).sum();
                assert_eq!(dots, usize::from(width > 0), "width {width} at {pct}");
            }
        }
    }

    #[test]
    fn playhead_moves_at_a_constant_rate_between_quarter_second_reports() {
        // mpv reports at 0.25 s steps of *its* position; the UI draws every 0.125 s
        let (mut prev, mut last_frame) = (0.0, 0.0);
        let mut t = 0.0;
        while t < 20.0 {
            let reported = (t / 0.25_f64).floor() * 0.25;
            let shown = playhead(reported, t - reported, 1.0, true, 0.0, prev);
            assert!((shown - t).abs() < 1e-9, "t={t}: shown {shown}");
            if t > 0.0 {
                assert!((shown - last_frame - 0.125).abs() < 1e-9, "uneven step at t={t}");
            }
            (prev, last_frame) = (shown, shown);
            t += 0.125;
        }
        // …whereas drawing the raw report alternates between standing still and jumping
        let raw: Vec<f64> = (0..8).map(|i| ((i as f64 * 0.125) / 0.25).floor() * 0.25).collect();
        let steps: Vec<f64> = raw.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(steps.iter().any(|s| *s == 0.0) && steps.iter().any(|s| *s == 0.25));
    }

    #[test]
    fn playhead_follows_speed_stops_when_paused_and_never_runs_away() {
        assert_eq!(playhead(10.0, 0.2, 2.0, true, 0.0, 0.0), 10.4);
        assert_eq!(playhead(10.0, 0.2, 1.0, false, 0.0, 0.0), 10.0, "paused shows the report exactly");
        assert_eq!(playhead(10.0, 0.2, 1.0, false, 0.0, 10.3), 10.0, "…even if the last frame was ahead");
        assert_eq!(playhead(10.0, 30.0, 1.0, true, 0.0, 0.0), 10.0 + MAX_EXTRAPOLATE, "a stalled mpv can't be outrun");
        assert_eq!(playhead(99.9, 0.7, 1.0, true, 100.0, 0.0), 100.0, "never past the end");
        assert_eq!(playhead(10.0, -1.0, 1.0, true, 0.0, 0.0), 10.0, "a clock hiccup can't go negative");
    }

    #[test]
    fn playhead_ignores_small_regressions_but_not_seeks() {
        assert_eq!(playhead(10.0, 0.0, 1.0, true, 0.0, 10.3), 10.3, "report a hair behind the extrapolation");
        assert_eq!(playhead(5.0, 0.0, 1.0, true, 0.0, 10.3), 5.0, "a real seek back wins");
        assert_eq!(playhead(10.0, 0.1, 1.0, true, 0.0, 9.0), 10.1, "forward is always accepted");
    }

    #[test]
    fn dot_sits_at_the_ends_and_clicks_invert_it() {
        assert_eq!(dot_index(0.0, 30), 0);
        assert_eq!(dot_index(1.0, 30), 29);
        assert_eq!(dot_index(0.5, 11), 5);
        assert_eq!(fraction_at(0, 30), 0.0);
        assert_eq!(fraction_at(29, 30), 1.0);
        assert_eq!(fraction_at(99, 30), 1.0); // past the end stays in range
        for width in 2..60 {
            for col in 0..width {
                assert_eq!(dot_index(fraction_at(col, width), width), col, "width {width} col {col}");
            }
        }
    }

}
