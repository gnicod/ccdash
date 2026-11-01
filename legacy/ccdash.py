#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["textual>=0.80"]
# ///
"""ccdash — TUI dashboard for Claude Code sessions (live + history, all folders)."""

from __future__ import annotations

import glob
import json
import os
import re
import signal
import subprocess
import time
from dataclasses import dataclass, field
from pathlib import Path

from rich.text import Text
from textual import work
from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.containers import Horizontal, Vertical, VerticalScroll
from textual.screen import ModalScreen
from textual.suggester import SuggestFromList
from textual.widgets import DataTable, Footer, Input, Label, Static, TabbedContent, TabPane

HOME = str(Path.home())
CLAUDE = Path.home() / ".claude"
SESSIONS = CLAUDE / "sessions"
PROJECTS = CLAUDE / "projects"
APPROVAL_RE = re.compile(r"Do you want to |❯ 1\. Yes|Esc to cancel")
LIVE_INTERVAL = 2.0


# ───────────────────────── helpers ─────────────────────────

def tilde(p: str) -> str:
    return "~" + p[len(HOME):] if p.startswith(HOME) else p


def slug(cwd: str) -> str:
    return re.sub(r"[^A-Za-z0-9]", "-", cwd)


def ago(ts: float | None) -> str:
    if not ts:
        return "?"
    d = max(0, time.time() - ts)
    for unit, sec in (("d", 86400), ("h", 3600), ("m", 60)):
        if d >= sec:
            return f"{int(d // sec)}{unit}"
    return f"{int(d)}s"


def pid_alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
        return True
    except PermissionError:
        return True
    except OSError:
        return False


def iso_ts(s: str | None) -> float | None:
    if not s:
        return None
    try:
        from datetime import datetime
        return datetime.fromisoformat(s.replace("Z", "+00:00")).timestamp()
    except ValueError:
        return None


def msg_text(content) -> str:
    """Extract visible text from a message content field."""
    if isinstance(content, str):
        return content
    out = []
    for b in content or []:
        if not isinstance(b, dict):
            continue
        if b.get("type") == "text":
            out.append(b.get("text", ""))
        elif b.get("type") == "tool_use":
            inp = b.get("input") or {}
            hint = inp.get("description") or inp.get("command") or inp.get("file_path") or ""
            out.append(f"⚙ {b.get('name')} {str(hint)[:80]}")
    return "\n".join(x for x in out if x)


def is_real_prompt(t: str) -> bool:
    t = t.strip()
    return bool(t) and not t.startswith("<") and not t.startswith("[Request interrupted")


def read_tail(path: Path, nbytes: int) -> list[str]:
    try:
        with open(path, "rb") as f:
            f.seek(0, 2)
            size = f.tell()
            f.seek(max(0, size - nbytes))
            data = f.read().decode("utf-8", "replace")
    except OSError:
        return []
    lines = data.splitlines()
    return lines[1:] if size > nbytes else lines


# ───────────────────────── kitty ─────────────────────────

def kitty_sockets() -> list[str]:
    socks = []
    if os.environ.get("KITTY_LISTEN_ON"):
        socks.append(os.environ["KITTY_LISTEN_ON"])
    for p in glob.glob("/tmp/kitty-*"):
        m = re.search(r"-(\d+)$", p)
        if m and pid_alive(int(m.group(1))):
            s = f"unix:{p}"
            if s not in socks:
                socks.append(s)
    return socks


def kitty(sock: str, *args: str, timeout: float = 3) -> subprocess.CompletedProcess | None:
    try:
        return subprocess.run(["kitty", "@", "--to", sock, *args],
                              capture_output=True, text=True, timeout=timeout)
    except (OSError, subprocess.TimeoutExpired):
        return None


def kitty_windows() -> dict[int, tuple[str, int]]:
    """Map every foreground pid → (socket, kitty window id)."""
    out: dict[int, tuple[str, int]] = {}
    for sock in kitty_sockets():
        r = kitty(sock, "ls")
        if not r or r.returncode:
            continue
        try:
            data = json.loads(r.stdout)
        except json.JSONDecodeError:
            continue
        for osw in data:
            for tab in osw.get("tabs", []):
                for w in tab.get("windows", []):
                    for p in w.get("foreground_processes", []):
                        out[p["pid"]] = (sock, w["id"])
    return out


# ───────────────────────── data ─────────────────────────

@dataclass
class Live:
    pid: int
    sid: str
    cwd: str
    name: str
    status: str
    started: float
    updated: float
    kind: str
    transcript: Path | None
    title: str = ""
    win: tuple[str, int] | None = None
    screen: str = ""

    @property
    def needs_approval(self) -> bool:
        return bool(self.screen) and bool(APPROVAL_RE.search(self.screen[-1500:]))


@dataclass
class Past:
    sid: str
    path: Path
    cwd: str
    mtime: float
    title: str
    first_prompt: str
    size: int


@dataclass
class HistCache:
    items: dict[Path, tuple[float, Past]] = field(default_factory=dict)


def ai_title(path: Path | None) -> str:
    if not path:
        return ""
    for line in reversed(read_tail(path, 256 * 1024)):
        if '"ai-title"' in line or '"custom-title"' in line:
            try:
                d = json.loads(line)
            except json.JSONDecodeError:
                continue
            t = d.get("customTitle") or d.get("aiTitle")
            if t:
                return t
    return ""


def load_live() -> list[Live]:
    wins = kitty_windows()
    out = []
    for f in SESSIONS.glob("*.json"):
        try:
            d = json.loads(f.read_text())
        except (OSError, json.JSONDecodeError):
            continue
        pid = d.get("pid")
        if not pid or not pid_alive(pid):
            continue
        cwd = d.get("cwd", "")
        tp = PROJECTS / slug(cwd) / f"{d.get('sessionId')}.jsonl"
        s = Live(
            pid=pid, sid=d.get("sessionId", ""), cwd=cwd, name=d.get("name", ""),
            status=d.get("status", "?"), started=(d.get("startedAt") or 0) / 1000,
            updated=(d.get("statusUpdatedAt") or d.get("updatedAt") or 0) / 1000,
            kind=d.get("kind", ""), transcript=tp if tp.exists() else None,
            win=wins.get(pid),
        )
        s.title = ai_title(s.transcript)
        if s.win:
            r = kitty(s.win[0], "get-text", "--match", f"id:{s.win[1]}")
            if r and r.returncode == 0:
                s.screen = r.stdout.rstrip()
        out.append(s)
    out.sort(key=lambda s: (s.cwd, s.started))
    return out


def parse_past(path: Path) -> Past:
    cwd, first = "", ""
    try:
        with open(path, encoding="utf-8", errors="replace") as f:
            for i, line in enumerate(f):
                if i > 200 or (cwd and first):
                    break
                try:
                    d = json.loads(line)
                except json.JSONDecodeError:
                    continue
                cwd = cwd or d.get("cwd", "")
                m = d.get("message")
                if not first and d.get("type") == "user" and m and not d.get("isMeta"):
                    t = msg_text(m.get("content"))
                    if is_real_prompt(t):
                        first = " ".join(t.split())[:200]
    except OSError:
        pass
    st = path.stat()
    return Past(sid=path.stem, path=path, cwd=cwd or "?", mtime=st.st_mtime,
                title=ai_title(path), first_prompt=first, size=st.st_size)


def load_history(cache: HistCache) -> list[Past]:
    out = []
    for p in PROJECTS.glob("*/*.jsonl"):
        try:
            mt = p.stat().st_mtime
        except OSError:
            continue
        hit = cache.items.get(p)
        if not hit or hit[0] != mt:
            hit = (mt, parse_past(p))
            cache.items[p] = hit
        if hit[1].first_prompt or hit[1].title:
            out.append(hit[1])
    out.sort(key=lambda x: x.mtime, reverse=True)
    return out


def transcript_tail(path: Path | None, n: int = 14) -> Text:
    t = Text()
    if not path:
        return Text("(no transcript)", style="dim")
    msgs = []
    for line in read_tail(path, 512 * 1024):
        try:
            d = json.loads(line)
        except json.JSONDecodeError:
            continue
        m = d.get("message")
        if d.get("type") not in ("user", "assistant") or not m or d.get("isMeta"):
            continue
        body = msg_text(m.get("content")).strip()
        if not body or (d["type"] == "user" and not is_real_prompt(body)):
            continue
        msgs.append((d["type"], body))
    for role, body in msgs[-n:]:
        if role == "user":
            t.append("▶ you  ", style="bold cyan")
        else:
            t.append("◆ claude  ", style="bold magenta")
        t.append(body[:1200] + "\n\n")
    return t


def launch_claude(cwd: str, *args: str) -> None:
    cmd = "claude " + " ".join(args) + "; exec $SHELL"
    subprocess.Popen(
        ["kitty", "--single-instance", "--instance-group", "sway", "--directory", cwd,
         os.environ.get("SHELL", "zsh"), "-ic", cmd],
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        start_new_session=True,
    )


# ───────────────────────── modals ─────────────────────────

class Prompt(ModalScreen[str | None]):
    BINDINGS = [Binding("escape", "dismiss(None)", "Cancel")]
    DEFAULT_CSS = """
    Prompt { align: center middle; }
    Prompt > Vertical { width: 80%; max-width: 110; height: auto; border: thick $accent; padding: 1 2; background: $surface; }
    Prompt Label { margin-bottom: 1; }
    """

    def __init__(self, title: str, value: str = "", suggestions: list[str] | None = None):
        super().__init__()
        self.title_ = title
        self.value = value
        self.suggestions = suggestions or []

    def compose(self) -> ComposeResult:
        with Vertical():
            yield Label(self.title_)
            yield Input(value=self.value, suggester=SuggestFromList(self.suggestions, case_sensitive=False)
                        if self.suggestions else None)

    def on_input_submitted(self, e: Input.Submitted) -> None:
        self.dismiss(e.value)


class Confirm(ModalScreen[bool]):
    BINDINGS = [Binding("y", "dismiss(True)", "Yes"), Binding("n,escape", "dismiss(False)", "No")]
    DEFAULT_CSS = """
    Confirm { align: center middle; }
    Confirm > Label { border: thick $error; padding: 1 3; background: $surface; }
    """

    def __init__(self, msg: str):
        super().__init__()
        self.msg = msg

    def compose(self) -> ComposeResult:
        yield Label(f"{self.msg}\n\n[b]y[/b] yes   [b]n[/b] no")


# ───────────────────────── app ─────────────────────────

class CCDash(App):
    TITLE = "ccdash"
    CSS = """
    #live-body { height: 1fr; }
    #live-table { width: 45%; min-width: 60; }
    #preview-box { width: 1fr; border-left: solid $panel-lighten-2; padding: 0 1; }
    #preview-head { color: $text-muted; height: 1; }
    #hfilter { dock: top; }
    #hist-table { height: 1fr; }
    #hist-preview { height: 12; border-top: solid $panel-lighten-2; padding: 0 1; }
    DataTable { scrollbar-size-vertical: 1; }
    """
    BINDINGS = [
        Binding("enter", "primary", "Focus/Resume", show=True, priority=False),
        Binding("i", "send", "Type into"),
        Binding("1", "key('1')", "1", show=False),
        Binding("2", "key('2')", "2", show=False),
        Binding("3", "key('3')", "3", show=False),
        Binding("x", "interrupt", "Esc/Interrupt"),
        Binding("v", "toggle_view", "Screen/Log"),
        Binding("n", "new", "New session"),
        Binding("K", "kill", "Kill"),
        Binding("slash", "filter", "Filter"),
        Binding("r", "reload", "Refresh"),
        Binding("tab", "switch_tab", "Live/History", show=True, priority=True),
        Binding("q", "quit", "Quit"),
    ]

    def __init__(self):
        super().__init__()
        self.live: list[Live] = []
        self.hist: list[Past] = []
        self.hcache = HistCache()
        self.view = "screen"
        self.hfilter = ""

    # ---------- layout ----------
    def compose(self) -> ComposeResult:
        with TabbedContent(id="tabs"):
            with TabPane("Live", id="live"):
                with Horizontal(id="live-body"):
                    yield DataTable(id="live-table", cursor_type="row", zebra_stripes=True)
                    with Vertical(id="preview-box"):
                        yield Static("", id="preview-head")
                        with VerticalScroll(id="preview-scroll"):
                            yield Static("", id="preview")
            with TabPane("History", id="history"):
                yield Input(placeholder="filter by folder / title / prompt…  (esc to leave)", id="hfilter")
                yield DataTable(id="hist-table", cursor_type="row", zebra_stripes=True)
                with VerticalScroll(id="hist-preview"):
                    yield Static("", id="hist-text")
        yield Footer()

    def on_mount(self) -> None:
        lt = self.query_one("#live-table", DataTable)
        lt.add_columns("status", "folder", "title", "pid", "up", "seen")
        ht = self.query_one("#hist-table", DataTable)
        ht.add_columns("when", "folder", "title / first prompt", "size", "")
        lt.focus()
        self.refresh_live()
        self.refresh_history()
        self.set_interval(LIVE_INTERVAL, self.refresh_live)
        self.set_interval(30, self.refresh_history)

    # ---------- helpers ----------
    @property
    def tab(self) -> str:
        return self.query_one("#tabs", TabbedContent).active

    def current_live(self) -> Live | None:
        t = self.query_one("#live-table", DataTable)
        if not self.live or t.cursor_row is None or t.row_count == 0:
            return None
        try:
            key = t.coordinate_to_cell_key((t.cursor_row, 0)).row_key.value
        except Exception:
            return None
        return next((s for s in self.live if str(s.pid) == key), None)

    def current_past(self) -> Past | None:
        t = self.query_one("#hist-table", DataTable)
        if t.row_count == 0 or t.cursor_row is None:
            return None
        try:
            key = t.coordinate_to_cell_key((t.cursor_row, 0)).row_key.value
        except Exception:
            return None
        return next((p for p in self.hist if p.sid == key), None)

    # ---------- live ----------
    @work(thread=True, exclusive=True, group="live")
    def refresh_live(self) -> None:
        data = load_live()
        self.call_from_thread(self.apply_live, data)

    def apply_live(self, data: list[Live]) -> None:
        self.live = data
        t = self.query_one("#live-table", DataTable)
        prev = self.current_live()
        prev_key = str(prev.pid) if prev else None
        t.clear()
        for s in data:
            if s.needs_approval:
                st = Text("⚠ approve", style="bold red")
            elif s.status == "busy":
                st = Text("● working", style="yellow")
            elif s.status == "idle":
                st = Text("○ your turn", style="green")
            else:
                st = Text(f"? {s.status}", style="dim")
            if not s.win:
                st.append(" ⊘", style="dim")
            title = s.title or s.name
            t.add_row(st, tilde(s.cwd), title[:60], str(s.pid), ago(s.started), ago(s.updated),
                      key=str(s.pid))
        if prev_key:
            try:
                t.move_cursor(row=t.get_row_index(prev_key))
            except Exception:
                pass
        n_appr = sum(s.needs_approval for s in data)
        n_idle = sum(s.status == "idle" and not s.needs_approval for s in data)
        self.sub_title = f"{len(data)} live · {n_appr} need approval · {n_idle} waiting on you"
        self.update_preview()

    def update_preview(self) -> None:
        s = self.current_live()
        head = self.query_one("#preview-head", Static)
        body = self.query_one("#preview", Static)
        if not s:
            head.update("")
            body.update(Text("No live Claude Code sessions.\n\nPress n to start one.", style="dim"))
            return
        where = "kitty" if s.win else "not in kitty — read-only"
        head.update(Text(f"{self.view} · {s.name} · {s.sid[:8]} · {where}"))
        if self.view == "screen" and s.screen:
            body.update(Text(s.screen))
        else:
            body.update(transcript_tail(s.transcript))
        self.query_one("#preview-scroll", VerticalScroll).scroll_end(animate=False)

    # ---------- history ----------
    @work(thread=True, exclusive=True, group="hist")
    def refresh_history(self) -> None:
        data = load_history(self.hcache)
        self.call_from_thread(self.apply_history, data)

    def apply_history(self, data: list[Past]) -> None:
        self.hist = data
        self.render_history()

    def render_history(self) -> None:
        t = self.query_one("#hist-table", DataTable)
        prev = self.current_past()
        t.clear()
        live_sids = {s.sid for s in self.live}
        q = self.hfilter.lower()
        for p in self.hist:
            label = p.title or p.first_prompt
            if q and q not in f"{p.cwd} {p.title} {p.first_prompt}".lower():
                continue
            mark = Text("● live", style="green") if p.sid in live_sids else Text("")
            t.add_row(ago(p.mtime), tilde(p.cwd), label[:90], f"{p.size // 1024}k", mark, key=p.sid)
        if prev:
            try:
                t.move_cursor(row=t.get_row_index(prev.sid))
            except Exception:
                pass
        self.update_hist_preview()

    def update_hist_preview(self) -> None:
        p = self.current_past()
        w = self.query_one("#hist-text", Static)
        if not p:
            w.update("")
            return
        txt = Text()
        txt.append(f"{tilde(p.cwd)}  ·  {p.sid}\n", style="dim")
        if p.title:
            txt.append(p.title + "\n", style="bold")
        txt.append(p.first_prompt, style="cyan")
        w.update(txt)

    # ---------- events ----------
    def on_data_table_row_highlighted(self, e: DataTable.RowHighlighted) -> None:
        if e.data_table.id == "live-table":
            self.update_preview()
        else:
            self.update_hist_preview()

    def on_data_table_row_selected(self, e: DataTable.RowSelected) -> None:
        self.action_primary()

    def on_input_changed(self, e: Input.Changed) -> None:
        if e.input.id == "hfilter":
            self.hfilter = e.value
            self.render_history()

    def on_input_submitted(self, e: Input.Submitted) -> None:
        if e.input.id == "hfilter":
            self.query_one("#hist-table", DataTable).focus()

    def on_key(self, e) -> None:
        if e.key == "escape" and isinstance(self.focused, Input) and self.focused.id == "hfilter":
            self.query_one("#hist-table", DataTable).focus()
            e.stop()

    def on_tabbed_content_tab_activated(self, e: TabbedContent.TabActivated) -> None:
        self.query_one("#live-table" if self.tab == "live" else "#hist-table", DataTable).focus()

    # ---------- actions ----------
    def action_switch_tab(self) -> None:
        tabs = self.query_one("#tabs", TabbedContent)
        tabs.active = "history" if tabs.active == "live" else "live"

    def _need_win(self, s: Live | None) -> bool:
        if not s:
            return False
        if not s.win:
            self.notify("Session isn't in a reachable kitty window", severity="warning")
            return False
        return True

    def focus_live(self, s: Live) -> None:
        if self._need_win(s):
            kitty(s.win[0], "focus-window", "--match", f"id:{s.win[1]}")

    def action_primary(self) -> None:
        if self.tab == "live":
            s = self.current_live()
            if s:
                self.focus_live(s)
            return
        p = self.current_past()
        if not p:
            return
        live = next((s for s in self.live if s.sid == p.sid), None)
        if live:
            self.focus_live(live)
        elif not os.path.isdir(p.cwd):
            self.notify(f"Folder no longer exists: {p.cwd}", severity="error")
        else:
            launch_claude(p.cwd, "--resume", p.sid)
            self.notify(f"Resuming in {tilde(p.cwd)}")

    def action_send(self) -> None:
        s = self.current_live() if self.tab == "live" else None
        if not self._need_win(s):
            return

        def done(text: str | None) -> None:
            if text is None:
                return
            kitty(s.win[0], "send-text", "--match", f"id:{s.win[1]}", "--", text)
            time.sleep(0.15)
            kitty(s.win[0], "send-key", "--match", f"id:{s.win[1]}", "enter")
            self.notify(f"Sent to {s.name}")
            self.set_timer(0.6, self.refresh_live)

        self.push_screen(Prompt(f"Type into [b]{s.name}[/b] ({tilde(s.cwd)}) — Enter sends"), done)

    def action_key(self, k: str) -> None:
        s = self.current_live() if self.tab == "live" else None
        if self._need_win(s):
            kitty(s.win[0], "send-text", "--match", f"id:{s.win[1]}", "--", k)
            self.set_timer(0.6, self.refresh_live)

    def action_interrupt(self) -> None:
        s = self.current_live() if self.tab == "live" else None
        if self._need_win(s):
            kitty(s.win[0], "send-key", "--match", f"id:{s.win[1]}", "escape")
            self.notify(f"Sent Esc to {s.name}")
            self.set_timer(0.6, self.refresh_live)

    def action_toggle_view(self) -> None:
        self.view = "log" if self.view == "screen" else "screen"
        self.update_preview()

    def action_new(self) -> None:
        dirs = sorted({p.cwd for p in self.hist if os.path.isdir(p.cwd)})
        s = self.current_live()
        start = (s.cwd if s else HOME).rstrip("/") + "/"

        def done(path: str | None) -> None:
            if not path:
                return
            path = os.path.expanduser(path.strip())
            if not os.path.isdir(path):
                self.notify(f"Not a folder: {path}", severity="error")
                return
            launch_claude(path)
            self.notify(f"Started claude in {tilde(path)}")
            self.set_timer(2.5, self.refresh_live)

        self.push_screen(Prompt("New Claude session in folder (→ accepts suggestion)", start, dirs), done)

    def action_kill(self) -> None:
        s = self.current_live() if self.tab == "live" else None
        if not s:
            return

        def done(ok: bool | None) -> None:
            if ok:
                try:
                    os.kill(s.pid, signal.SIGTERM)
                    self.notify(f"SIGTERM → {s.pid}")
                except OSError as ex:
                    self.notify(str(ex), severity="error")
                self.set_timer(1, self.refresh_live)

        self.push_screen(Confirm(f"Kill session [b]{s.title or s.name}[/b] (pid {s.pid})?"), done)

    def action_filter(self) -> None:
        self.query_one("#tabs", TabbedContent).active = "history"
        self.query_one("#hfilter", Input).focus()

    def action_reload(self) -> None:
        self.refresh_live()
        self.refresh_history()


if __name__ == "__main__":
    CCDash().run()
