//! Rustdrome — a fast, animated terminal player for Navidrome.
//! A Rust/ratatui rewrite of NaviTui (Python/Textual).

mod actions;
mod anim;
mod api;
mod app;
mod art;
mod clipboard;
mod config;
mod input;
mod keys;
mod mem;
mod modal;
mod models;
mod mutations;
mod player;
mod queue;
mod sixel;
mod stats;
mod theme;
mod ui;

use std::time::Duration;

use anyhow::Result;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture, Event};
use crossterm::execute;
use tokio::sync::mpsc::unbounded_channel;

use crate::app::{App, Level, Msg};
use crate::config::{AppDirs, Settings};
use crate::player::Player;

const HELP: &str = "\
Rustdrome — a fast, animated terminal player for Navidrome

USAGE:  rustdrome [--version | --help]

Needs `mpv` on PATH for playback. Configuration lives in ~/.config/rustdrome/.
Env: RUSTDROME_ART=auto|sixel|halfcell|off chooses the cover-art renderer;
     RUSTDROME_AO=<driver> picks an mpv audio output.";

fn main() -> Result<()> {
    if let Some(arg) = std::env::args().nth(1) {
        match arg.as_str() {
            "-V" | "--version" => return Ok(println!("rustdrome {}", env!("CARGO_PKG_VERSION"))),
            "-h" | "--help" => return Ok(println!("{HELP}")),
            other => anyhow::bail!("unknown argument {other:?} (try --help)"),
        }
    }
    mem::tune();
    // Two workers is plenty for an app that spends its life waiting on a
    // socket or a keypress; the default (one per core) would also multiply
    // allocator arenas and thread stacks on a many-core machine.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(4)
        .thread_stack_size(1024 * 1024)
        .enable_all()
        .build()?;
    rt.block_on(real_main())
}

async fn real_main() -> Result<()> {
    let dirs = AppDirs::new();
    let settings = Settings::load(dirs.config_dir());
    let (tx, mut rx) = unbounded_channel::<Msg>();
    let (ptx, mut prx) = unbounded_channel();

    let mut terminal = ratatui::init(); // also installs a panic hook that restores the tty
    execute!(std::io::stdout(), EnableMouseCapture)?;

    // Ask the terminal about sixel + cell size while we own raw mode and
    // before the event reader starts. Skipped when the user chose a mode.
    let choice = config::env("ART");
    let probe = match choice.as_deref().map(str::to_ascii_lowercase).as_deref() {
        Some("off") | Some("none") | Some("halfcell") | Some("half") | Some("blocks") => sixel::Probe::default(),
        _ => sixel::probe(Duration::from_millis(400)),
    };

    let mut app = App::new(dirs, settings, tx);
    app.art_mode = art::resolve_mode(choice.as_deref(), probe.sixel);
    app.cell_px = sixel::cell_size(&probe);
    app.cell_probe_ok = probe.cell_px.is_some();
    app.win_sig = crossterm::terminal::window_size().ok().map(|w| (w.columns, w.rows, w.width, w.height));
    let ao = config::env("AO");
    match Player::spawn(&app.settings, ptx, ao.as_deref()).await {
        Ok(p) => app.player = p,
        Err(e) => {
            app.notify(format!("{e} — playback disabled"), Level::Warn);
            app.player = Player::null();
        }
    }
    app.start();

    let mut input = input::Input::spawn();
    let result = run(&mut terminal, &mut app, &mut rx, &mut prx, &mut input).await;
    let _ = std::io::Write::write_all(&mut std::io::stdout(), b"\x1b[?2026l"); // never leave the display held
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

async fn run(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<Msg>,
    prx: &mut player::PlayerEvents,
    input: &mut input::Input,
) -> Result<()> {
    // the one shared 8fps heartbeat that drives every animation
    let mut beat = tokio::time::interval(Duration::from_millis(125));
    beat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    while !app.quit {
        // one synchronized update per step: a resize's screen clear and the
        // repaint that follows are presented together, never the blank between
        sixel::begin_frame(terminal)?;
        terminal.draw(|f| ui::draw(f, app))?;
        // `true` means the screen was cleared (the art panel had moved): repaint
        // right now, in the same step, rather than leaving it blank until the next tick
        if sixel::sync(app, terminal)? {
            terminal.draw(|f| ui::draw(f, app))?;
            sixel::sync(app, terminal)?;
        }
        sixel::end_frame(terminal)?;
        tokio::select! {
            ev = input.rx.recv() => match ev {
                Some(Event::Key(k)) => app.on_key(k),
                Some(Event::Mouse(m)) => app.on_mouse(m),
                // ratatui clears the screen on resize, which wipes any sixel; the
                // cell size may have changed too (font zoom, another monitor)
                Some(Event::Resize(..)) => {
                    app.sixel.shown = None;
                    if app.art_mode == art::ArtMode::Sixel {
                        app.resize_at = Some(std::time::Instant::now());
                    }
                }
                Some(_) => {}
                None => break,
            },
            _ = beat.tick() => {
                app.on_tick();
                app.maintain_cell_size(input);
            }
            Some(m) = rx.recv() => app.on_msg(m),
            Some(e) = prx.recv() => app.on_player_event(e),
        }
        // drain anything else that's already queued so a burst costs one redraw
        while let Ok(m) = rx.try_recv() {
            app.on_msg(m);
        }
    }
    app.shutdown();
    Ok(())
}
