# Rustdrome

**A lightweight, beautiful & blazing-fast terminal music client for [Navidrome](https://www.navidrome.org/) and any Subsonic-compatible server.**

Rustdrome is a Rust TUI application built with ratatui which uses mpv for playback. 

Originally started out as a rewrite of the [NaviTui](https://github.com/Gheat1/NaviTui) Project.

[![Rust 1.84+](https://img.shields.io/badge/Rust-1.84%2B-orange?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Built with Ratatui](https://img.shields.io/badge/Built%20with-Ratatui-8A2BE2?style=flat-square&logo=ratatui&logoColor=white)](https://ratatui.rs/)
[![License: GPL-3.0-or-later](https://img.shields.io/badge/License-GPL--3.0--or--later-blue?style=flat-square&logo=gnu&logoColor=white)](LICENSE)


<table>
  <tr>
    <td align="center">
      <img width="100%" src="https://github.com/user-attachments/assets/432068a2-92fa-4bd2-90c5-c157d622ff2d" alt="Rustdrome Zen Mode" />
      <br />
      <em>Rustdrome Zen Mode</em>
    </td>
    <td align="center">
      <img width="100%" src="https://github.com/user-attachments/assets/50f787e0-544c-4d39-98c3-052b6373dfc5" alt="Rustdrome Library View" />
      <br />
      <em>Rustdrome Library View</em>
    </td>
  </tr>
</table>


## Install

### Dependencies

Rustdrome requires **`mpv` on your `PATH` at runtime**. It does not require libmpv development headers to build.

Install mpv using your distribution's package manager; or on Windows download it from mpv releases:

**Arch Linux**
```sh
sudo pacman -S mpv
```

**Debian / Ubuntu**
```sh
sudo apt install mpv
```

**Fedora**
```sh
sudo dnf install mpv
```

**Windows 11**
```
https://github.com/mpv-player/mpv/releases/download/git-release/
```

Move the `mpv.exe` file to somewhere in `Path`.

For example move it to `C:\Tools\` and add that folder to the user `Path`.

### Precompiled Unix binary

The precompiled binary targets Unix systems with **glibc 2.39 or newer**.

```sh
git clone https://github.com/LMRTX/rustdrome
cd rustdrome
chmod +x ./install
./install
```
Then just launch the application with `rustdrome` in your terminal.

### Precompiled Windows executable

The precompiled binary targets **Windows 11** and **Windows 10** systems.

The executable can be ran from anywhere and will run Rustdrome in default terminal. (usually Windows Terminal)

For convenient acess, move the executable to somewhere on your `Path` for example in to the folder `C:\Tools\` mentioned earlier.

If Rustdrome is in `Path` you can run it from any terminal window with `rustdrome`.

## Building from Source

Rustdrome requires **Rust 1.91 or newer**. If Rust is not packaged newer than 1.91 for your distribution, use `rustup` to install tool-chain.

```sh
git clone https://github.com/LMRTX/rustdrome --branch source
cd rustdrome
cargo build --release
./target/release/rustdrome
```

For convenient access, move the compiled binary somewhere on your `PATH`, such as:

```sh
mkdir -p ~/.local/bin
cp ./target/release/rustdrome ~/.local/bin/
```

Make sure `~/.local/bin` is included in your `PATH`.



## Keybindings

Press `?` inside Rustdrome to see the complete in-app keybinding reference.

Most actions can be remapped in `player.toml`. List navigation, `enter`, and the `1`–`5` rating keys are fixed.

### Playback & transport

| Key | Action |
|---|---|
| `space` | Play / pause |
| `n` / `b` | Next / previous track |
| `←` / `→` | Seek 5 seconds |
| `-` / `+` | Decrease / increase volume |
| `s` | Shuffle |
| `r` | Repeat |
| `i` | Start radio |

### Library & navigation

| Key | Action |
|---|---|
| `/` | Search |
| `\` | Filter the current list |
| `v` | Select mode |
| `y` | Browse genres |
| `enter` | Activate selection |

### Queue & playlists

| Key | Action |
|---|---|
| `a` | Add to queue |
| `A` | Play next |
| `p` / `P` | Add / remove playlist |
| `f` | Star / unstar track |
| `1`–`5` | Rate track |

### Downloads & display

| Key | Action |
|---|---|
| `d` / `D` | Download |
| `O` | Toggle offline mode |
| `L` | Lyrics |
| `z` | Zen mode |
| `t` / `T` | Theme |
| `ctrl+p` | Command palette |


## Files and Compatibility

Rustdrome stores its configuration and application data in the following locations for new installations:

| Location | Purpose |
|---|---|
| `~/.config/rustdrome/` | Configuration files |
| `~/.local/state/rustdrome/` | Persistent application state |
| `~/.cache/rustdrome/` | Library caches, cover art, and offline downloads |

**Upgrading from NaviTui is supported.** If an existing `navitui` configuration is present and no Rustdrome configuration exists, Rustdrome continues using the existing NaviTui directories. This preserves your login, queue, cache, and offline downloads.

The file formats remain compatible with the original Python NaviTui.

### Important files

| File or directory | Purpose |
|---|---|
| `config.toml` | Server configuration and salted authentication token |
| `player.toml` | Settings and keybindings |
| `state.json` | Theme, volume, current view, shuffle, and resume position |
| `queue.json` | Playback queue |
| `*.json` | Cached library data |
| `mutations.json` | Pending stars, ratings, and scrobbles |
| `plays.jsonl` | Listening statistics |
| `art/` | Cover-art cache |
| `audio/` | Offline music downloads |

Configuration files containing authentication tokens should be protected with restrictive permissions, such as `chmod 600`.

## Features

### Library & discovery

- Browse all tracks, newest additions, recently played tracks, most-played songs, and starred tracks.
- Shuffle your entire library.
- Search your music library and filter the current list.
- Browse by genre and access podcasts and internet radio.
- Jump directly to an album or artist.
- Select multiple tracks for batch operations.
- Navigate lists with vim-style movement and repeat counts.

### Playback & queue

- Play, pause, skip, seek, and adjust volume.
- Add tracks to the queue or schedule them to play next.
- Edit queue order and manage upcoming tracks.
- Shuffle and repeat playback.
- Start radio from your music and discover similar tracks.
- Resume where you left off.
- Control playback speed and configure a sleep timer.
- Enable crossfade, ReplayGain, and gapless playback through mpv.
- Prefetch the next track for smoother transitions.

### Offline listening

- Download individual tracks or collections.
- Keep music available without a network connection.
- Enable offline mode to play downloaded tracks.
- Cache cover art and library data locally.
- Queue stars, ratings, and scrobbles made offline, then sync them later.
- Preserve original audio files for offline downloads.

### Lyrics & now playing

- View synced and plain lyrics.
- Use Zen mode for a distraction-free listening experience.
- Display album art using sixel or half-cell terminal graphics.
- Follow animated titles and the dynamic playback progress bar.
- Copy share links using OSC 52.

### Playlists & organization

- Create and delete playlists.
- Add or remove tracks.
- Reorder tracks and rename playlists.
- Save the current queue as a playlist.
- Star tracks and assign ratings from one to five.
- Maintain bookmarks for longer tracks.
- Review local listening statistics.

### Interface & customization

- Choose from five themes with live preview.
- Tint the interface using album-art colors.
- Use the command palette to discover available actions.
- Navigate with the keyboard or mouse.
- Customize settings and keybindings through `player.toml`.
- Receive desktop notifications when tracks change.



## How It Works

Rustdrome combines Rust, ratatui, and mpv into a compact terminal music player.

- **Interface:** [ratatui](https://ratatui.rs) provides the terminal user interface.
- **Playback:** mpv runs as a child process and communicates with Rustdrome through its JSON IPC socket.
- **Music server:** Rustdrome connects to Navidrome and other Subsonic-compatible servers.
- **Persistence:** configuration, application state, queue data, and caches are stored locally.
- **Offline synchronization:** supported mutations are queued locally and synced when the server becomes available again.

## Differences from NaviTui

Rustdrome started as a rewrite of [NaviTui](https://github.com/Gheat1/NaviTui), preserving its existing file formats and much of its functionality while changing the implementation language and terminal UI framework.

| Area | NaviTui | Rustdrome |
|---|---|---|
| Language | Python | Rust |
| Terminal UI | Textual | ratatui |
| Playback integration | Python bindings to libmpv | mpv child process over JSON IPC |
| Cover art | Multiple protocols, including Kitty graphics | Sixel or half-cell blocks |

## License

**[GPL-3.0-or-later](https://www.gnu.org/licenses/gpl-3.0.html)**

Rustdrome derives from [NaviTui](https://github.com/Gheat1/NaviTui) and retains its compatible file formats and feature lineage.
