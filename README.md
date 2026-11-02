# ccdash

A terminal dashboard for your [Claude Code](https://claude.com/claude-code) sessions. It shows every running session from every folder and lets you watch them, write to them, answer permission prompts, and start, resume or end sessions. Your past sessions are listed too. Colours follow your [pywal](https://github.com/dylanaraps/pywal) palette.

Built with [ratatui](https://ratatui.rs). The pywal theme comes from [pixtop](https://github.com/gnicod/pixtop).

## Features

**Live tab**
- One card per running session: status (`working`, `your turn`, `⚠ needs approval`), title, folder, uptime, last activity.
- A preview pane with three views (`v` to cycle):
  - **live**: the session's terminal in its original colours, re-wrapped to fit the pane
  - **structured**: your prompts, Claude's replies and tool calls as styled blocks, with syntax-highlighted diffs and commands
  - **conversation**: the whole transcript from the first prompt, with highlighted code blocks
- An input box at the bottom of the preview to write into the session (`i`), plus `1`–`3` to answer permission prompts and `x` to interrupt.
- Scrolling (PgUp/PgDn, mouse wheel). Scrolling past the top of the live view switches to the full conversation.
- Mouse selection in the preview copies the selected lines without the left panel.
- A "recent" panel with your latest past sessions (`R` hides it).

**Background sessions**
- New sessions (`n`) and resumed ones start without opening a terminal. They run as tabs of a kitty window kept on the sway scratchpad.
- `Enter` on a session brings its window up on the right tab. `Ctrl+T` / `t` open a visible terminal instead.

**History tab**
- Every saved session, grouped by *last 24 hours*, *this week* and *earlier*, with a filter (`/`).
- Resume with `Enter`. Remove with `D` (recoverable, `U` undoes).

## Requirements

- **Linux** with **kitty**. Remote control must be enabled in `kitty.conf`:
  ```
  allow_remote_control yes
  listen_on unix:/tmp/kitty
  ```
  Sessions running outside kitty are listed but view-only.
- **sway** for background sessions (it uses the scratchpad). Without sway, sessions open in a normal window.
- **wl-copy** (wl-clipboard) for copying selections.
- **pywal** is optional. Without `~/.cache/wal/colors.json`, a built-in palette is used.
- **Rust** to build.

## Install

```sh
git clone git@github.com:gnicod/ccdash.git
cd ccdash
cargo build --release
ln -sf "$PWD/target/release/ccdash" ~/.local/bin/ccdash
```

Run `ccdash` and press `?` for the full key list.

## Keys

| Key | Action |
|---|---|
| `Tab` `←` `→` | switch Live / History |
| `↑` `↓` `j` `k` | move (`g` / `G` top / bottom) |
| `Enter` | show session · resume past session in the background |
| `i` | write to the session (Enter sends, Esc cancels, paste works) |
| `1` `2` `3` | answer a permission prompt |
| `x` | send Esc (interrupt) |
| `v` | cycle live → structured → conversation |
| `PgUp` `PgDn` `u` `d` | scroll the preview (`Home` top, `End` follow) |
| `n` | new session (Enter: background, `Ctrl+T`: in a terminal) |
| `K` | end a live session (SIGTERM) |
| `t` | resume in a visible terminal (History) |
| `D` `U` | remove a past session · undo (History) |
| `/` | filter History (`Esc` clears) |
| `R` | show / hide the recent panel |
| `r` | refresh |
| `q` | quit |

## How it works

- **Live sessions:** reads `~/.claude/sessions/<pid>.json`, which Claude Code writes for each running session, and skips any whose process has ended.
- **Titles and conversations:** read from the transcripts in `~/.claude/projects/<folder>/<session>.jsonl`. Each transcript is read once, then only new bytes are parsed.
- **Screen, input and focus:** through kitty's remote control (`kitty @ get-text --ansi`, `send-text`, `send-key`, `focus-window`, `launch`).
- **Removing a session** moves its transcript to `~/.claude/ccdash-removed/`. While it's there, `claude --resume` won't list it either. Move it back to restore it.
- **Preferences** such as the recent panel are saved in `~/.config/ccdash/state`.

ccdash never reads your Claude credentials and has no network code. Anything sent to a session is typed into the real `claude` program, and only when you press a key.

## Limitations

- Session files, transcript format and screen layout are undocumented Claude Code internals. A Claude Code update can break parts of ccdash.
- Claude Code redraws its interface in place, so the live view only covers what is on screen. Older output is in the conversation view.
- `⚠ needs approval` is detected from the session's screen text, not reported by Claude Code.
- The first background session briefly flashes its window (about 0.15 s) before it moves to the scratchpad.
- Selection works on whole lines. Shift+drag gives the terminal's own selection.

## Layout

```
src/main.rs       app state, keys, mouse, background refresh thread
src/data.rs       sessions, transcripts, kitty and sway calls
src/ui.rs         drawing
src/ansi.rs       SGR colour parsing for the live view
src/highlight.rs  small syntax highlighter
src/theme.rs      pywal palette (from pixtop)
legacy/           first version, Python + Textual
```
