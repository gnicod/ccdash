//! Session discovery: ~/.claude/sessions (live), ~/.claude/projects (transcripts), kitty remote control.

use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

pub fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

pub fn tilde(p: &str) -> String {
    let h = home();
    let h = h.to_string_lossy();
    match p.strip_prefix(h.as_ref()) {
        Some(rest) if !h.is_empty() => format!("~{rest}"),
        _ => p.to_string(),
    }
}

pub fn ago(ts: f64) -> String {
    if ts <= 0.0 {
        return "?".into();
    }
    let d = (now() - ts).max(0.0) as u64;
    match d {
        0..=59 => format!("{d}s"),
        60..=3599 => format!("{}m", d / 60),
        3600..=86399 => format!("{}h", d / 3600),
        _ => format!("{}d", d / 86400),
    }
}

fn slug(cwd: &str) -> String {
    cwd.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

fn pid_alive(pid: i64) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

/// Last `max` bytes of `s`, cut on a char boundary.
pub fn tail_str(s: &str, max: usize) -> &str {
    let mut i = s.len().saturating_sub(max);
    while !s.is_char_boundary(i) {
        i += 1;
    }
    &s[i..]
}

fn read_tail(path: &Path, nbytes: u64) -> Vec<String> {
    let Ok(mut f) = fs::File::open(path) else { return vec![] };
    let size = f.seek(SeekFrom::End(0)).unwrap_or(0);
    let start = size.saturating_sub(nbytes);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return vec![];
    }
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    let s = String::from_utf8_lossy(&buf);
    let mut lines: Vec<String> = s.lines().map(String::from).collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    lines
}

/// Visible text of a message `content` (string or block list).
fn msg_text(content: &Value) -> String {
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    let mut out = Vec::new();
    for b in content.as_array().into_iter().flatten() {
        match b.get("type").and_then(Value::as_str) {
            Some("text") => out.push(b.get("text").and_then(Value::as_str).unwrap_or("").to_string()),
            Some("tool_use") => {
                let inp = b.get("input").cloned().unwrap_or(Value::Null);
                let hint = ["description", "command", "file_path", "pattern", "url"]
                    .iter()
                    .find_map(|k| inp.get(*k).and_then(Value::as_str))
                    .unwrap_or("");
                let hint: String = hint.lines().next().unwrap_or("").chars().take(90).collect();
                out.push(format!("⚙ {} {}", b.get("name").and_then(Value::as_str).unwrap_or("?"), hint));
            }
            _ => {}
        }
    }
    out.retain(|s| !s.trim().is_empty());
    out.join("\n")
}

fn is_real_prompt(t: &str) -> bool {
    let t = t.trim();
    !t.is_empty() && !t.starts_with('<') && !t.starts_with("[Request interrupted")
}

fn ai_title(path: Option<&Path>) -> String {
    let Some(path) = path else { return String::new() };
    for line in read_tail(path, 256 * 1024).iter().rev() {
        if !line.contains("\"ai-title\"") && !line.contains("\"custom-title\"") {
            continue;
        }
        if let Ok(d) = serde_json::from_str::<Value>(line) {
            if let Some(t) = d.get("customTitle").or_else(|| d.get("aiTitle")).and_then(Value::as_str) {
                if !t.is_empty() {
                    return t.to_string();
                }
            }
        }
    }
    String::new()
}

// ───────────────────────── kitty ─────────────────────────

#[derive(Clone, Debug)]
pub struct Win {
    pub sock: String,
    pub id: u64,
    /// lives in the hidden "ccdash-sessions" window on the sway scratchpad
    pub hidden: bool,
}

/// App id of the background kitty window that hosts sessions started from ccdash.
pub const HOST_CLASS: &str = "ccdash-sessions";

fn kitty_sockets() -> Vec<String> {
    let mut socks = Vec::new();
    if let Ok(s) = std::env::var("KITTY_LISTEN_ON") {
        socks.push(s);
    }
    for e in fs::read_dir("/tmp").into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if let Some(pid) = name.strip_prefix("kitty-").and_then(|p| p.parse::<i64>().ok()) {
            let s = format!("unix:/tmp/{name}");
            if pid_alive(pid) && !socks.contains(&s) {
                socks.push(s);
            }
        }
    }
    socks
}

pub fn kitty(sock: &str, args: &[&str]) -> Option<String> {
    let out = Command::new("kitty")
        .args(["@", "--to", sock])
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn kitty_windows() -> HashMap<i64, Win> {
    let mut map = HashMap::new();
    for sock in kitty_sockets() {
        let Some(out) = kitty(&sock, &["ls"]) else { continue };
        let Ok(data) = serde_json::from_str::<Value>(&out) else { continue };
        for osw in data.as_array().into_iter().flatten() {
            let hidden = osw["wm_class"].as_str() == Some(HOST_CLASS);
            for tab in osw["tabs"].as_array().into_iter().flatten() {
                for w in tab["windows"].as_array().into_iter().flatten() {
                    let id = w["id"].as_u64().unwrap_or(0);
                    for p in w["foreground_processes"].as_array().into_iter().flatten() {
                        if let Some(pid) = p["pid"].as_i64() {
                            map.insert(pid, Win { sock: sock.clone(), id, hidden });
                        }
                    }
                }
            }
        }
    }
    map
}

impl Win {
    fn m(&self) -> String {
        format!("id:{}", self.id)
    }
    pub fn focus(&self) {
        kitty(&self.sock, &["focus-window", "--match", &self.m()]);
        if self.hidden && !host_visible() {
            sway(&format!("[app_id=\"{HOST_CLASS}\"] scratchpad show"));
        }
    }
    pub fn text(&self, s: &str) {
        kitty(&self.sock, &["send-text", "--match", &self.m(), "--", s]);
    }
    pub fn key(&self, k: &str) {
        kitty(&self.sock, &["send-key", "--match", &self.m(), k]);
    }
    fn screen(&self, all: bool) -> String {
        let extent = if all { "all" } else { "screen" };
        kitty(&self.sock, &["get-text", "--ansi", "--extent", extent, "--match", &self.m()]).unwrap_or_default()
    }
}

// ───────────────────────── live sessions ─────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Approve,
    Working,
    Idle,
    Other(String),
}

#[derive(Clone, Debug)]
pub struct Live {
    pub pid: i64,
    pub sid: String,
    pub cwd: String,
    pub name: String,
    pub status: Status,
    pub started: f64,
    pub updated: f64,
    pub transcript: Option<PathBuf>,
    pub title: String,
    pub win: Option<Win>,
    /// visible text of the session's terminal, and the same with its SGR colours
    pub screen: String,
    pub screen_ansi: String,
}

const APPROVAL: [&str; 3] = ["Do you want to ", "❯ 1. Yes", "Esc to cancel"];

/// `full_for`: pid whose whole scrollback (not just the visible screen) is wanted.
pub fn load_live(full_for: Option<i64>) -> Vec<Live> {
    let wins = kitty_windows();
    let dir = home().join(".claude/sessions");
    let projects = home().join(".claude/projects");
    let mut out = Vec::new();
    for e in fs::read_dir(&dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Ok(d) = fs::read_to_string(&p).map_err(|_| ()).and_then(|s| serde_json::from_str::<Value>(&s).map_err(|_| ())) else {
            continue;
        };
        let Some(pid) = d["pid"].as_i64() else { continue };
        if !pid_alive(pid) {
            continue;
        }
        let s = |k: &str| d[k].as_str().unwrap_or("").to_string();
        let cwd = s("cwd");
        let sid = s("sessionId");
        let tp = projects.join(slug(&cwd)).join(format!("{sid}.jsonl"));
        let transcript = tp.exists().then_some(tp);
        let win = wins.get(&pid).cloned();
        let screen_ansi = win.as_ref().map(|w| w.screen(full_for == Some(pid))).unwrap_or_default();
        let screen = crate::ansi::strip(&screen_ansi).trim_end().to_string();
        let raw = s("status");
        let status = if !screen.is_empty() && APPROVAL.iter().any(|a| tail_str(&screen, 1500).contains(a)) {
            Status::Approve
        } else {
            match raw.as_str() {
                "busy" => Status::Working,
                "idle" => Status::Idle,
                o => Status::Other(o.to_string()),
            }
        };
        let ms = |k: &str| d[k].as_f64().unwrap_or(0.0) / 1000.0;
        out.push(Live {
            pid,
            title: ai_title(transcript.as_deref()),
            sid,
            cwd,
            name: s("name"),
            status,
            started: ms("startedAt"),
            updated: if d["statusUpdatedAt"].is_number() { ms("statusUpdatedAt") } else { ms("updatedAt") },
            transcript,
            win,
            screen,
            screen_ansi,
        });
    }
    out.sort_by(|a, b| a.cwd.cmp(&b.cwd).then(a.started.total_cmp(&b.started)));
    out
}

// ───────────────────────── history ─────────────────────────

#[derive(Clone, Debug)]
pub struct Past {
    pub path: PathBuf,
    pub sid: String,
    pub cwd: String,
    pub mtime: f64,
    pub title: String,
    pub first_prompt: String,
    pub size: u64,
}

#[derive(Default)]
pub struct HistCache(HashMap<PathBuf, (SystemTime, Past)>);

fn parse_past(path: &Path, meta: &fs::Metadata) -> Past {
    let (mut cwd, mut first) = (String::new(), String::new());
    if let Ok(f) = fs::File::open(path) {
        for line in BufReader::new(f).lines().take(200).map_while(Result::ok) {
            if !cwd.is_empty() && !first.is_empty() {
                break;
            }
            let Ok(d) = serde_json::from_str::<Value>(&line) else { continue };
            if cwd.is_empty() {
                cwd = d["cwd"].as_str().unwrap_or("").to_string();
            }
            if first.is_empty() && d["type"] == "user" && d["isMeta"] != true {
                let t = msg_text(&d["message"]["content"]);
                if is_real_prompt(&t) {
                    first = t.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(240).collect();
                }
            }
        }
    }
    let mtime = meta.modified().ok().and_then(|m| m.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs_f64()).unwrap_or(0.0);
    Past {
        path: path.to_path_buf(),
        sid: path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(),
        cwd: if cwd.is_empty() { "?".into() } else { cwd },
        mtime,
        title: ai_title(Some(path)),
        first_prompt: first,
        size: meta.len(),
    }
}

pub fn load_history(cache: &mut HistCache) -> Vec<Past> {
    let mut out = Vec::new();
    for proj in fs::read_dir(home().join(".claude/projects")).into_iter().flatten().flatten() {
        for e in fs::read_dir(proj.path()).into_iter().flatten().flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(meta) = e.metadata() else { continue };
            let Ok(mt) = meta.modified() else { continue };
            let fresh = matches!(cache.0.get(&p), Some((m, _)) if *m == mt);
            if !fresh {
                let past = parse_past(&p, &meta);
                cache.0.insert(p.clone(), (mt, past));
            }
            let past = &cache.0[&p].1;
            if !past.first_prompt.is_empty() || !past.title.is_empty() {
                out.push(past.clone());
            }
        }
    }
    out.sort_by(|a, b| b.mtime.total_cmp(&a.mtime));
    out
}

// ───────────────────────── actions ─────────────────────────

fn sway(cmd: &str) {
    let _ = Command::new("swaymsg").arg(cmd).stdout(Stdio::null()).stderr(Stdio::null()).status();
}

/// Is the background session window currently shown (pulled out of the scratchpad)?
fn host_visible() -> bool {
    fn walk(n: &Value) -> Option<bool> {
        if n["app_id"].as_str() == Some(HOST_CLASS) {
            return Some(n["visible"].as_bool().unwrap_or(false));
        }
        n["nodes"].as_array().into_iter().chain(n["floating_nodes"].as_array()).flatten().find_map(walk)
    }
    let Ok(out) = Command::new("swaymsg").args(["-t", "get_tree"]).output() else { return false };
    serde_json::from_slice::<Value>(&out.stdout).ok().as_ref().and_then(walk).unwrap_or(false)
}

/// Start a session without opening a visible terminal: it runs as a tab in a kitty
/// window that lives on the sway scratchpad. Returns false if kitty isn't reachable.
fn launch_hidden(cwd: &str, args: &[&str]) -> bool {
    static RULE: std::sync::Once = std::sync::Once::new();
    let Some(sock) = kitty_sockets().into_iter().next() else { return false };
    RULE.call_once(|| {
        // runtime-only rule: the host window goes straight to the scratchpad, sized for later
        sway(&format!("for_window [app_id=\"{HOST_CLASS}\"] floating enable, resize set width 85 ppt height 85 ppt, move scratchpad"));
    });
    let host_tab = kitty(&sock, &["ls"]).and_then(|o| serde_json::from_str::<Value>(&o).ok()).and_then(|d| {
        d.as_array()?.iter().find(|w| w["wm_class"].as_str() == Some(HOST_CLASS)).and_then(|w| w["tabs"][0]["id"].as_u64())
    });
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "zsh".into());
    let cmd = format!("claude {}", args.join(" "));
    let title = Path::new(cwd).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "claude".into());
    let mut a: Vec<String> = vec!["launch".into(), "--keep-focus".into(), "--cwd".into(), cwd.into(), "--tab-title".into(), title];
    match host_tab {
        Some(id) => a.extend(["--type=tab".into(), "--match".into(), format!("id:{id}")]),
        None => a.extend(["--type=os-window".into(), "--os-window-class".into(), HOST_CLASS.into(), "--os-window-title".into(), "claude sessions".into()]),
    }
    a.extend([shell, "-ic".into(), cmd]);
    let a: Vec<&str> = a.iter().map(String::as_str).collect();
    let ok = kitty(&sock, &a).is_some();
    if ok && host_tab.is_none() {
        // a fresh host window: make sure it ends up in the scratchpad even if the
        // for_window rule missed it (kitty sets the app id just after mapping)
        for _ in 0..30 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            sway(&format!("[app_id=\"{HOST_CLASS}\"] move scratchpad"));
            if !host_visible() && host_exists() {
                break;
            }
        }
    }
    ok
}

fn host_exists() -> bool {
    Command::new("swaymsg").args(["-t", "get_tree"]).output().map(|o| String::from_utf8_lossy(&o.stdout).contains(HOST_CLASS)).unwrap_or(false)
}

/// Start (or resume) a session. In the background unless `visible`; returns true if it went to the background.
pub fn launch_session(cwd: &str, args: &[&str], visible: bool) -> bool {
    if !visible && launch_hidden(cwd, args) {
        return true;
    }
    launch_claude(cwd, args);
    false
}

pub fn launch_claude(cwd: &str, args: &[&str]) {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "zsh".into());
    let cmd = format!("claude {}; exec {shell}", args.join(" "));
    let _ = Command::new("kitty")
        .args(["--single-instance", "--instance-group", "sway", "--directory", cwd, &shell, "-ic", &cmd])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
}

/// Move a past session's transcript (and its sibling `<sid>/` folder, if any) out of
/// ~/.claude/projects into ~/.claude/ccdash-removed. Returns (from, to) pairs for undo.
pub fn remove_past(p: &Past) -> std::io::Result<Vec<(PathBuf, PathBuf)>> {
    let proj = p.path.parent().ok_or_else(|| std::io::Error::other("no project dir"))?;
    let dest = home().join(".claude/ccdash-removed").join(proj.file_name().unwrap_or_default());
    fs::create_dir_all(&dest)?;
    let mut moved = Vec::new();
    for src in [p.path.clone(), proj.join(&p.sid)] {
        if src.exists() {
            let to = dest.join(src.file_name().unwrap_or_default());
            fs::rename(&src, &to)?;
            moved.push((src, to));
        }
    }
    Ok(moved)
}

pub fn restore(moved: &[(PathBuf, PathBuf)]) -> bool {
    moved.iter().rev().all(|(from, to)| fs::rename(to, from).is_ok())
}

pub fn kill(pid: i64) -> bool {
    Command::new("kill").args(["-TERM", &pid.to_string()]).status().map(|s| s.success()).unwrap_or(false)
}

/// Whole-transcript reader that only parses the bytes appended since the last update.
#[derive(Default)]
pub struct Transcript {
    offset: u64,
    partial: String,
    pub msgs: Vec<(bool, String)>,
    /// bumps whenever `msgs` changes (used to cache the formatted view)
    pub gen: u64,
}

impl Transcript {
    pub fn update(&mut self, path: &Path) {
        let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        if size < self.offset {
            *self = Transcript { gen: self.gen + 1, ..Default::default() };
        }
        if size == self.offset {
            return;
        }
        let Ok(mut f) = fs::File::open(path) else { return };
        if f.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut buf = Vec::new();
        if f.read_to_end(&mut buf).is_err() {
            return;
        }
        self.offset += buf.len() as u64;
        let mut text = std::mem::take(&mut self.partial);
        text.push_str(&String::from_utf8_lossy(&buf));
        let complete = text.ends_with('\n');
        let mut lines: Vec<&str> = text.split('\n').collect();
        if !complete {
            self.partial = lines.pop().unwrap_or("").to_string();
        }
        for line in lines {
            if let Some(m) = parse_msg(line) {
                self.msgs.push(m);
            }
        }
        self.gen += 1;
    }
}

fn parse_msg(line: &str) -> Option<(bool, String)> {
    if line.is_empty() || !(line.contains("\"type\":\"user\"") || line.contains("\"type\":\"assistant\"")) {
        return None;
    }
    let d = serde_json::from_str::<Value>(line).ok()?;
    let ty = d["type"].as_str().unwrap_or("");
    if (ty != "user" && ty != "assistant") || d["isMeta"] == true {
        return None;
    }
    let body = msg_text(&d["message"]["content"]).trim().to_string();
    if body.is_empty() || (ty == "user" && !is_real_prompt(&body)) {
        return None;
    }
    Some((ty == "user", body))
}
