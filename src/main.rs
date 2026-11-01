//! ccdash: a dashboard for your Claude Code sessions, in your pywal colors.

mod ansi;
mod data;
mod highlight;
mod theme;
mod ui;

use data::{HistCache, Live, Past};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use std::cell::Cell;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Live,
    History,
}

pub enum Mode {
    Normal,
    Filter,
    Send(String),
    New { input: String, sel: Option<usize> },
    Confirm(i64, String),
    Remove(Past),
    Help,
}

pub struct Snap {
    live: Vec<Live>,
    hist: Option<Vec<Past>>,
}

pub struct App {
    pub theme: theme::Theme,
    pub tab: Tab,
    pub mode: Mode,
    pub live: Vec<Live>,
    pub hist: Vec<Past>,
    pub live_sel: usize,
    pub hist_sel: usize,
    pub filter: String,
    /// preview: 0 live (original colours), 1 structured, 2 conversation
    pub view: u8,
    pub show_recent: bool,
    pub frame: u64,
    pub loaded: bool,
    pub toast: Option<(String, Instant, bool)>,
    pub transcripts: std::collections::HashMap<std::path::PathBuf, data::Transcript>,
    /// formatted conversation lines, cached by (path, transcript gen, width)
    pub log_cache: std::cell::RefCell<Option<((std::path::PathBuf, u64, usize), Vec<ratatui::text::Line<'static>>)>>,
    /// preview scroll, in rows up from the bottom (0 = follow live output)
    pub scroll: Cell<usize>,
    pub scroll_total: Cell<usize>,
    pub scroll_max: Cell<usize>,
    pub page: Cell<usize>,
    shared_sel: Arc<Mutex<Option<i64>>>,
    /// mouse selection in the preview: absolute line indices (anchor, cursor)
    pub sel: Option<(usize, usize)>,
    selecting: bool,
    /// set while drawing: preview body area, index of its first line, plain text of visible lines
    pub pv_body: Cell<ratatui::layout::Rect>,
    pub pv_start: Cell<usize>,
    pub pv_text: std::cell::RefCell<Vec<String>>,
    gen: u64,
    refresh_at: Vec<Instant>,
    pending: Option<(String, f64)>,
    last_removed: Vec<(std::path::PathBuf, std::path::PathBuf)>,
    tx: Sender<()>,
}

impl App {
    pub fn cur_live(&self) -> Option<&Live> {
        self.live.get(self.live_sel)
    }

    pub fn hist_view(&self) -> Vec<&Past> {
        let q = self.filter.to_lowercase();
        self.hist
            .iter()
            .filter(|p| q.is_empty() || format!("{} {} {}", p.cwd, p.title, p.first_prompt).to_lowercase().contains(&q))
            .collect()
    }

    pub fn cur_past(&self) -> Option<Past> {
        self.hist_view().get(self.hist_sel).map(|p| (*p).clone())
    }

    pub fn dirs(&self) -> Vec<String> {
        let mut d: Vec<String> = self.hist.iter().map(|p| p.cwd.clone()).filter(|c| std::path::Path::new(c).is_dir()).collect();
        d.sort();
        d.dedup();
        d
    }

    pub fn suggestions(&self, input: &str) -> Vec<String> {
        let q = data::tilde(input.trim_end_matches('/')).to_lowercase();
        let q = q.trim_start_matches('~');
        self.dirs().into_iter().filter(|d| data::tilde(d).to_lowercase().contains(q)).take(8).collect()
    }

    fn toast(&mut self, msg: impl Into<String>, err: bool) {
        self.toast = Some((msg.into(), Instant::now(), err));
    }

    fn refresh(&mut self, delays_ms: &[u64]) {
        let _ = self.tx.send(());
        for d in delays_ms {
            self.refresh_at.push(Instant::now() + Duration::from_millis(*d));
        }
    }

    fn apply(&mut self, s: Snap) {
        let pid = self.cur_live().map(|l| l.pid);
        let sid = self.cur_past().map(|p| p.sid);
        self.live = s.live;
        if let Some(h) = s.hist {
            self.hist = h;
        }
        if let Some(i) = pid.and_then(|p| self.live.iter().position(|l| l.pid == p)) {
            self.live_sel = i;
        }
        if let Some(i) = sid.and_then(|s| self.hist_view().iter().position(|p| p.sid == s)) {
            self.hist_sel = i;
        }
        // jump to a session we just started once it shows up
        if let Some((cwd, since)) = self.pending.clone() {
            if let Some(i) = self.live.iter().position(|l| l.cwd == cwd && l.started >= since - 2.0) {
                self.live_sel = i;
                self.pending = None;
            }
        }
        self.clamp();
        self.loaded = true;
        self.gen += 1;
    }

    fn clamp(&mut self) {
        self.live_sel = self.live_sel.min(self.live.len().saturating_sub(1));
        self.hist_sel = self.hist_sel.min(self.hist_view().len().saturating_sub(1));
    }

    /// Transcript tail for the selected live session, cached per refresh.
    pub fn ensure_log(&mut self) {
        let Some(l) = self.cur_live() else { return };
        let Some(p) = l.transcript.clone() else { return };
        // cheap when nothing changed: one stat, then only appended bytes are parsed
        self.transcripts.entry(p.clone()).or_default().update(&p);
    }

    fn with_win(&mut self, f: impl FnOnce(&data::Win, &Live) -> String) {
        let Some(l) = self.cur_live().cloned() else { return };
        match &l.win {
            Some(w) => {
                let msg = f(w, &l);
                if !msg.is_empty() {
                    self.toast(msg, false);
                }
                self.refresh(&[600]);
            }
            None => self.toast("not in a kitty window, view only", true),
        }
    }

    /// Pasted text goes into whatever input is active (newlines become spaces,
    /// so a paste never submits the prompt by itself).
    fn paste(&mut self, p: &str) {
        let clean: String = p.replace("\r\n", " ").replace(['\n', '\r'], " ");
        let can_write = self.tab == Tab::Live && self.cur_live().is_some_and(|l| l.win.is_some());
        match &mut self.mode {
            Mode::Send(s) => s.push_str(&clean),
            Mode::New { input, .. } => input.push_str(clean.trim()),
            Mode::Filter => self.filter.push_str(&clean),
            Mode::Normal if can_write => self.mode = Mode::Send(clean),
            _ => {}
        }
    }

    fn start(&mut self, cwd: &str, args: &[&str], visible: bool) {
        let bg = data::launch_session(cwd, args, visible);
        let verb = if args.is_empty() { "started" } else { "resuming" };
        self.toast(format!("{verb} in {}{}", data::tilde(cwd), if bg { " (background · ⏎ to show)" } else { "" }), false);
        self.pending = Some((cwd.to_string(), data::now()));
        self.tab = Tab::Live;
        self.refresh(&[1500, 3000, 6000]);
    }

    fn resume(&mut self, visible: bool) {
        let Some(p) = self.cur_past() else { return };
        if let Some(i) = self.live.iter().position(|l| l.sid == p.sid) {
            self.live_sel = i;
            self.tab = Tab::Live;
            self.with_win(|w, _| {
                w.focus();
                String::new()
            });
        } else if !std::path::Path::new(&p.cwd).is_dir() {
            self.toast(format!("folder is gone: {}", p.cwd), true);
        } else {
            self.start(&p.cwd.clone(), &["--resume", &p.sid], visible);
        }
    }

    fn primary(&mut self) {
        match self.tab {
            Tab::Live => self.with_win(|w, _| {
                w.focus();
                String::new()
            }),
            Tab::History => self.resume(false),
        }
    }

    /// Line index under the mouse in the preview (clamped to the body when `clamp`).
    fn pv_hit(&self, x: u16, y: u16, clamp: bool) -> Option<usize> {
        let b = self.pv_body.get();
        if b.height == 0 {
            return None;
        }
        let inside = x >= b.x && x < b.right() && y >= b.y && y < b.bottom();
        if !inside && !clamp {
            return None;
        }
        let row = y.clamp(b.y, b.bottom() - 1) - b.y;
        Some(self.pv_start.get() + row as usize)
    }

    fn mouse(&mut self, m: ratatui::crossterm::event::MouseEvent) {
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) if self.tab == Tab::Live => {
                self.sel = self.pv_hit(m.column, m.row, false).map(|i| (i, i));
                self.selecting = self.sel.is_some();
            }
            MouseEventKind::Drag(MouseButton::Left) if self.selecting => {
                let b = self.pv_body.get();
                // dragging past the edge scrolls
                if m.row < b.y {
                    self.scroll_by(1);
                } else if m.row >= b.bottom() {
                    self.scroll_by(-1);
                }
                if let (Some(i), Some(s)) = (self.pv_hit(m.column, m.row, true), self.sel.as_mut()) {
                    s.1 = i;
                }
            }
            MouseEventKind::Up(MouseButton::Left) if self.selecting => {
                self.selecting = false;
                self.copy_sel();
            }
            MouseEventKind::ScrollUp if self.tab == Tab::Live => self.scroll_by(3),
            MouseEventKind::ScrollDown if self.tab == Tab::Live => self.scroll_by(-3),
            MouseEventKind::ScrollUp => self.mv(-1),
            MouseEventKind::ScrollDown => self.mv(1),
            _ => {}
        }
    }

    fn copy_sel(&mut self) {
        let Some((a, b)) = self.sel else { return };
        let (a, b) = (a.min(b), a.max(b));
        let start = self.pv_start.get();
        let text = self.pv_text.borrow();
        let lo = a.saturating_sub(start).min(text.len());
        let hi = (b + 1).saturating_sub(start).min(text.len());
        if lo >= hi {
            return;
        }
        let out = text[lo..hi].join("\n");
        let n = hi - lo;
        drop(text);
        let ok = std::process::Command::new("wl-copy")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .and_then(|mut c| {
                use std::io::Write;
                c.stdin.take().unwrap().write_all(out.as_bytes())?;
                c.wait()
            })
            .is_ok_and(|s| s.success());
        if ok {
            self.toast(format!("copied {n} line{}", if n == 1 { "" } else { "s" }), false);
        } else {
            self.toast("copy failed (is wl-copy installed?)", true);
        }
    }

    fn scroll_by(&mut self, d: i64) {
        let max = self.scroll_max.get() as i64;
        let live_screen = self.view < 2 && self.cur_live().is_some_and(|l| !l.screen.is_empty());
        if d > 0 && live_screen && self.scroll.get() as i64 >= max {
            self.view = 2;
            self.scroll.set(0);
            self.toast("live screen has no history · showing the full conversation", false);
            return;
        }
        self.scroll.set((self.scroll.get() as i64 + d).clamp(0, max) as usize);
    }

    /// Tell the worker which session wants its full scrollback.
    pub fn sync_sel(&self) {
        let pid = if self.tab == Tab::Live { self.cur_live().map(|l| l.pid) } else { None };
        *self.shared_sel.lock().unwrap() = pid;
    }

    fn mv(&mut self, d: i64) {
        if self.tab == Tab::Live {
            self.scroll.set(0);
        }
        let len = match self.tab {
            Tab::Live => self.live.len(),
            Tab::History => self.hist_view().len(),
        };
        let cur = match self.tab {
            Tab::Live => self.live_sel,
            Tab::History => self.hist_sel,
        } as i64;
        let new = (cur + d).clamp(0, len.saturating_sub(1) as i64) as usize;
        match self.tab {
            Tab::Live => self.live_sel = new,
            Tab::History => self.hist_sel = new,
        }
    }

    /// Returns true to quit.
    fn key(&mut self, k: KeyEvent) -> bool {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            return true;
        }
        self.sel = None;
        let mode = std::mem::replace(&mut self.mode, Mode::Normal);
        self.mode = match mode {
            Mode::Normal => return self.key_normal(k),
            Mode::Help => Mode::Normal,
            Mode::Filter => match k.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Down => Mode::Normal,
                KeyCode::Backspace => {
                    self.filter.pop();
                    self.hist_sel = 0;
                    Mode::Filter
                }
                KeyCode::Char(c) => {
                    self.filter.push(c);
                    self.hist_sel = 0;
                    Mode::Filter
                }
                _ => Mode::Filter,
            },
            Mode::Remove(p) => match k.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    match data::remove_past(&p) {
                        Ok(moved) => {
                            self.last_removed = moved;
                            self.hist.retain(|h| h.sid != p.sid);
                            self.clamp();
                            self.toast("removed from history · U to undo", false);
                        }
                        Err(e) => self.toast(format!("could not remove: {e}"), true),
                    }
                    self.refresh(&[]);
                    Mode::Normal
                }
                KeyCode::Char('n') | KeyCode::Esc | KeyCode::Char('q') => Mode::Normal,
                _ => Mode::Remove(p),
            },
            Mode::Confirm(pid, name) => match k.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    if data::kill(pid) {
                        self.toast(format!("killed {name}"), false);
                    } else {
                        self.toast(format!("could not kill {pid}"), true);
                    }
                    self.refresh(&[800]);
                    Mode::Normal
                }
                KeyCode::Char('n') | KeyCode::Esc | KeyCode::Char('q') => Mode::Normal,
                _ => Mode::Confirm(pid, name),
            },
            Mode::Send(mut s) => match k.code {
                KeyCode::Esc => Mode::Normal,
                KeyCode::Enter => {
                    self.with_win(|w, l| {
                        w.text(&s);
                        std::thread::sleep(Duration::from_millis(150));
                        w.key("enter");
                        format!("sent to {}", if l.title.is_empty() { &l.name } else { &l.title })
                    });
                    Mode::Normal
                }
                KeyCode::Backspace => {
                    s.pop();
                    Mode::Send(s)
                }
                KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => Mode::Send(String::new()),
                KeyCode::Char('w') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    let trimmed = s.trim_end().to_string();
                    let cut = trimmed.rfind(' ').map(|i| i + 1).unwrap_or(0);
                    Mode::Send(trimmed[..cut].to_string())
                }
                KeyCode::Char(c) => {
                    s.push(c);
                    Mode::Send(s)
                }
                _ => Mode::Send(s),
            },
            Mode::New { mut input, mut sel } => {
                let sugg = self.suggestions(&input);
                match k.code {
                    KeyCode::Esc => Mode::Normal,
                    KeyCode::Down => {
                        sel = Some(sel.map_or(0, |i| (i + 1).min(sugg.len().saturating_sub(1))));
                        Mode::New { input, sel }
                    }
                    KeyCode::Up => {
                        sel = sel.and_then(|i| i.checked_sub(1));
                        Mode::New { input, sel }
                    }
                    KeyCode::Tab => {
                        if let Some(d) = sugg.get(sel.unwrap_or(0)) {
                            input = format!("{}/", data::tilde(d));
                        }
                        Mode::New { input, sel: None }
                    }
                    KeyCode::Enter | KeyCode::Char('t') if k.code == KeyCode::Enter || k.modifiers.contains(KeyModifiers::CONTROL) => {
                        let visible = k.code != KeyCode::Enter;
                        let chosen = match sel.and_then(|i| sugg.get(i)) {
                            Some(d) => d.clone(),
                            None => input.trim().to_string(),
                        };
                        let path = match chosen.strip_prefix('~') {
                            Some(r) => format!("{}{r}", data::home().display()),
                            None => chosen,
                        };
                        if std::path::Path::new(&path).is_dir() {
                            self.start(&path, &[], visible);
                            Mode::Normal
                        } else {
                            self.toast(format!("not a folder: {path}"), true);
                            Mode::New { input, sel }
                        }
                    }
                    KeyCode::Backspace => {
                        input.pop();
                        Mode::New { input, sel: None }
                    }
                    KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => Mode::New { input: String::new(), sel: None },
                    KeyCode::Char(c) => {
                        input.push(c);
                        Mode::New { input, sel: None }
                    }
                    _ => Mode::New { input, sel },
                }
            }
        };
        false
    }

    fn key_normal(&mut self, k: KeyEvent) -> bool {
        match k.code {
            KeyCode::Char('q') => return true,
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right | KeyCode::Char('h') | KeyCode::Char('l') => {
                self.tab = if self.tab == Tab::Live { Tab::History } else { Tab::Live };
            }
            KeyCode::Down | KeyCode::Char('j') => self.mv(1),
            KeyCode::Up | KeyCode::Char('k') => self.mv(-1),
            KeyCode::PageUp if self.tab == Tab::Live => self.scroll_by(self.page.get() as i64),
            KeyCode::PageDown if self.tab == Tab::Live => self.scroll_by(-(self.page.get() as i64)),
            KeyCode::Char('u') if self.tab == Tab::Live => self.scroll_by(self.page.get() as i64 / 2),
            KeyCode::Char('d') if self.tab == Tab::Live => self.scroll_by(-(self.page.get() as i64 / 2)),
            KeyCode::Home if self.tab == Tab::Live => self.scroll.set(self.scroll_max.get()),
            KeyCode::End if self.tab == Tab::Live => self.scroll.set(0),
            KeyCode::PageDown => self.mv(10),
            KeyCode::PageUp => self.mv(-10),
            KeyCode::Home | KeyCode::Char('g') => self.mv(-100_000),
            KeyCode::End | KeyCode::Char('G') => self.mv(100_000),
            KeyCode::Enter => self.primary(),
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('r') => {
                self.refresh(&[]);
                self.toast("refreshing", false);
            }
            KeyCode::Char('n') => {
                let start = self.cur_live().map(|l| l.cwd.clone()).unwrap_or_else(|| data::home().display().to_string());
                self.mode = Mode::New { input: format!("{}/", data::tilde(&start)), sel: None };
            }
            KeyCode::Char('/') => {
                self.tab = Tab::History;
                self.mode = Mode::Filter;
            }
            KeyCode::Esc if self.tab == Tab::History && !self.filter.is_empty() => {
                self.filter.clear();
                self.clamp();
            }
            KeyCode::Char('t') if self.tab == Tab::History => self.resume(true),
            KeyCode::Char('D') if self.tab == Tab::History => {
                if let Some(p) = self.cur_past() {
                    if self.live.iter().any(|l| l.sid == p.sid) {
                        self.toast("session is running · end it with K in Live first", true);
                    } else {
                        self.mode = Mode::Remove(p);
                    }
                }
            }
            KeyCode::Char('U') if self.tab == Tab::History => {
                if self.last_removed.is_empty() {
                    self.toast("nothing to undo", true);
                } else if data::restore(&self.last_removed) {
                    self.last_removed.clear();
                    self.toast("restored", false);
                    self.refresh(&[]);
                } else {
                    self.toast("could not restore", true);
                }
            }
            _ if self.tab == Tab::Live => self.key_live(k),
            _ => {}
        }
        false
    }

    fn key_live(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Char('i') => {
                if self.cur_live().map(|l| l.win.is_some()) == Some(true) {
                    self.mode = Mode::Send(String::new());
                } else if self.cur_live().is_some() {
                    self.toast("not in a kitty window, view only", true);
                }
            }
            KeyCode::Char(c @ '1'..='3') => self.with_win(|w, _| {
                w.text(&c.to_string());
                format!("pressed {c}")
            }),
            KeyCode::Char('x') => self.with_win(|w, _| {
                w.key("escape");
                "sent Esc".into()
            }),
            KeyCode::Char('v') => {
                self.view = (self.view + 1) % 3;
                self.scroll.set(0);
                self.toast(["live · original colours", "structured", "conversation"][self.view as usize], false);
            }
            KeyCode::Char('R') => {
                self.show_recent = !self.show_recent;
                save_state("recent", if self.show_recent { "1" } else { "0" });
                self.toast(if self.show_recent { "recent shown" } else { "recent hidden (R to show)" }, false);
            }
            KeyCode::Char('K') => {
                if let Some(l) = self.cur_live() {
                    let name = if l.title.is_empty() { l.name.clone() } else { l.title.clone() };
                    self.mode = Mode::Confirm(l.pid, name);
                }
            }
            _ => {}
        }
    }
}

fn state_path() -> std::path::PathBuf {
    data::home().join(".config/ccdash/state")
}

/// Tiny key=value store for UI preferences.
fn load_state(key: &str) -> Option<String> {
    let s = std::fs::read_to_string(state_path()).ok()?;
    s.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix('=').map(str::to_string))
}

fn save_state(key: &str, val: &str) {
    let p = state_path();
    let mut lines: Vec<String> = std::fs::read_to_string(&p).unwrap_or_default().lines().filter(|l| !l.starts_with(&format!("{key}="))).map(String::from).collect();
    lines.push(format!("{key}={val}"));
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(p, lines.join("\n") + "\n");
}

fn worker(tx: Sender<Snap>, rx: Receiver<()>, sel: Arc<Mutex<Option<i64>>>) {
    let mut cache = HistCache::default();
    let mut last_hist: Option<Instant> = None;
    loop {
        let forced = last_hist.is_none()
            || match rx.recv_timeout(Duration::from_secs(2)) {
                Ok(()) => true,
                Err(RecvTimeoutError::Timeout) => false,
                Err(RecvTimeoutError::Disconnected) => return,
            };
        while rx.try_recv().is_ok() {}
        let full_for = *sel.lock().unwrap();
        let live = data::load_live(full_for);
        let hist = if forced || last_hist.map_or(true, |t| t.elapsed() > Duration::from_secs(30)) {
            last_hist = Some(Instant::now());
            Some(data::load_history(&mut cache))
        } else {
            None
        };
        if tx.send(Snap { live, hist }).is_err() {
            return;
        }
    }
}

fn main() -> std::io::Result<()> {
    if std::env::args().any(|a| a == "-h" || a == "--help") {
        println!("ccdash: dashboard for Claude Code sessions (pywal colors)\n\nRun it and press ? for keys.");
        return Ok(());
    }
    let (snap_tx, snap_rx) = mpsc::channel();
    let (req_tx, req_rx) = mpsc::channel();
    let shared_sel: Arc<Mutex<Option<i64>>> = Arc::new(Mutex::new(None));
    let ws = shared_sel.clone();
    std::thread::spawn(move || worker(snap_tx, req_rx, ws));

    let mut app = App {
        theme: theme::Theme::load(),
        tab: Tab::Live,
        mode: Mode::Normal,
        live: vec![],
        hist: vec![],
        live_sel: 0,
        hist_sel: 0,
        filter: String::new(),
        view: 0,
        show_recent: load_state("recent").map_or(true, |v| v != "0"),
        frame: 0,
        loaded: false,
        toast: None,
        transcripts: Default::default(),
        log_cache: Default::default(),
        scroll: Cell::new(0),
        scroll_total: Cell::new(0),
        scroll_max: Cell::new(0),
        page: Cell::new(20),
        shared_sel: shared_sel.clone(),
        sel: None,
        selecting: false,
        pv_body: Cell::new(ratatui::layout::Rect::default()),
        pv_start: Cell::new(0),
        pv_text: Default::default(),
        gen: 0,
        refresh_at: vec![],
        pending: None,
        last_removed: vec![],
        tx: req_tx,
    };

    let mut term = ratatui::init();
    let _ = ratatui::crossterm::execute!(std::io::stdout(), ratatui::crossterm::event::EnableBracketedPaste, ratatui::crossterm::event::EnableMouseCapture);
    let res = (|| -> std::io::Result<()> {
        let mut last_theme = Instant::now();
        loop {
            while let Ok(s) = snap_rx.try_recv() {
                app.apply(s);
            }
            let now = Instant::now();
            if app.refresh_at.iter().any(|t| *t <= now) {
                app.refresh_at.retain(|t| *t > now);
                let _ = app.tx.send(());
            }
            if last_theme.elapsed() > Duration::from_secs(1) {
                app.theme.refresh();
                last_theme = Instant::now();
            }
            if app.toast.as_ref().is_some_and(|t| t.1.elapsed() > Duration::from_secs(3)) {
                app.toast = None;
            }
            app.sync_sel();
            term.draw(|f| ui::draw(&mut app, f))?;
            if event::poll(Duration::from_millis(80))? {
                match event::read()? {
                    Event::Key(k) if k.kind == KeyEventKind::Press => {
                        if app.key(k) {
                            return Ok(());
                        }
                    }
                    Event::Paste(p) => app.paste(&p),
                    Event::Mouse(m) => app.mouse(m),
                    _ => {}
                }
            }
            app.frame += 1;
        }
    })();
    let _ = ratatui::crossterm::execute!(std::io::stdout(), ratatui::crossterm::event::DisableBracketedPaste, ratatui::crossterm::event::DisableMouseCapture);
    ratatui::restore();
    res
}
