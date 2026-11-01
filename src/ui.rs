//! Drawing. Everything takes its colors from the pywal theme.

use crate::data::{ago, tilde, Live, Status};
use crate::ansi;
use crate::highlight::{self as hl, Palette};
use crate::theme::{Rgb, Theme};
use crate::{App, Mode, Tab};
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;

const SPIN: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn c(x: Rgb) -> Color {
    Color::Rgb(x.0, x.1, x.2)
}
fn fg(x: Rgb) -> Style {
    Style::default().fg(c(x))
}
fn on(bg: Rgb, t: &Theme) -> Rgb {
    if bg.lum() > 0.55 {
        t.bg.mix(Rgb(0, 0, 0), 0.5)
    } else {
        t.fg.mix(Rgb(255, 255, 255), 0.3)
    }
}
fn pill<'a>(label: impl Into<String>, bg: Rgb, t: &Theme) -> Span<'a> {
    Span::styled(format!(" {} ", label.into()), Style::default().bg(c(bg)).fg(c(on(bg, t))).add_modifier(Modifier::BOLD))
}
fn keycap<'a>(k: &str, t: &Theme) -> Span<'a> {
    Span::styled(format!(" {k} "), Style::default().bg(c(t.card_alt)).fg(c(t.fg)).add_modifier(Modifier::BOLD))
}

fn trunc(s: &str, w: usize) -> String {
    let n = s.chars().count();
    if n <= w {
        s.to_string()
    } else if w == 0 {
        String::new()
    } else {
        s.chars().take(w - 1).collect::<String>() + "…"
    }
}

/// Word wrap with a hanging indent matching the line's own leading whitespace.
fn wrap(s: &str, w: usize) -> Vec<String> {
    let w = w.max(4);
    let mut out = Vec::new();
    for line in s.lines() {
        if line.chars().count() <= w {
            out.push(line.to_string());
            continue;
        }
        let lead_n = (line.len() - line.trim_start().len()).min(w / 3);
        let indent = " ".repeat(lead_n);
        let room = w - lead_n;
        let mut cur = indent.clone();
        let mut cur_n = 0usize; // chars of words on the current row (excluding indent)
        for word in line.trim_start().split(' ') {
            let mut word: Vec<char> = word.chars().collect();
            if cur_n > 0 && cur_n + 1 + word.len() > room {
                out.push(std::mem::replace(&mut cur, indent.clone()));
                cur_n = 0;
            }
            while word.len() > room {
                if cur_n > 0 {
                    out.push(std::mem::replace(&mut cur, indent.clone()));
                    cur_n = 0;
                }
                out.push(format!("{indent}{}", word.drain(..room).collect::<String>()));
            }
            if cur_n > 0 {
                cur.push(' ');
                cur_n += 1;
            }
            cur_n += word.len();
            cur.extend(word);
        }
        if cur_n > 0 {
            out.push(cur);
        }
    }
    out
}

fn is_rule(l: &str) -> bool {
    let t = l.trim();
    !t.is_empty() && t.chars().all(|c| matches!(c, '─' | '━' | '═' | '-' | '▔' | '▁'))
}

/// Which lines continue the previous one. Claude Code wraps at its own terminal
/// width; undoing that lets the text re-wrap to the preview width.
fn reflow_plan(lines: &[String]) -> Vec<bool> {
    let src = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let mut plan = Vec::with_capacity(lines.len());
    let mut prev_n = 0usize;
    let mut prev_joinable = false;
    for l in lines {
        let t = l.trim_start();
        let n = l.chars().count();
        let first = t.chars().next();
        let starter = matches!(first, Some('⏺' | '⎿' | '│' | '╭' | '╰' | '❯' | '>' | '•' | '✻' | '✳' | '*' | '-' | '⚠' | '☐' | '☒'))
            || t.split_once(". ").is_some_and(|(a, _)| !a.is_empty() && a.len() <= 3 && a.chars().all(|c| c.is_ascii_digit()));
        let cont = !plan.is_empty() && prev_joinable && src > 40 && prev_n + 12 >= src && !t.is_empty() && l.starts_with(' ') && !starter && !is_rule(l);
        plan.push(cont);
        prev_n = n;
        prev_joinable = !t.is_empty() && !is_rule(l) && !t.starts_with('│');
    }
    plan
}

fn reflow(screen: &str) -> Vec<String> {
    let lines: Vec<String> = screen.lines().map(|l| l.trim_end().to_string()).collect();
    let plan = reflow_plan(&lines);
    let mut out: Vec<String> = Vec::new();
    for (l, cont) in lines.into_iter().zip(plan) {
        match out.last_mut() {
            Some(last) if cont => {
                last.push(' ');
                last.push_str(l.trim_start());
            }
            _ => out.push(l),
        }
    }
    out
}

/// Live view with the session's original terminal colours: reflowed, re-wrapped,
/// input box and status lines pulled out like in the structured view.
fn format_terminal(l: &Live, w: usize, t: &Theme, frame: u64) -> (Vec<Line<'static>>, String, String) {
    let mut rows: Vec<Vec<ansi::Cell>> = ansi::parse(&l.screen_ansi).into_iter().map(|r| ansi::trim_end(&r).to_vec()).collect();
    while rows.last().is_some_and(|r| r.is_empty()) {
        rows.pop();
    }
    let mut plain: Vec<String> = rows.iter().map(|r| ansi::plain(r)).collect();
    let approve = matches!(l.status, Status::Approve);
    let (mut status, mut draft) = (String::new(), String::new());
    if !approve {
        let rules: Vec<usize> = plain.iter().enumerate().filter(|(_, r)| is_rule(r)).map(|(i, _)| i).collect();
        if rules.len() >= 2 {
            let (a, b) = (rules[rules.len() - 2], rules[rules.len() - 1]);
            if plain.len() - b <= 6 && b - a <= 10 {
                draft = plain[a + 1..b].iter().map(|s| s.trim().trim_start_matches('❯').trim().to_string()).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
                status = plain[b + 1..].iter().map(|s| tidy_status(s)).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
                rows.truncate(a);
                plain.truncate(a);
            }
        }
    }
    let plan = reflow_plan(&plain);
    let mut joined: Vec<Vec<ansi::Cell>> = Vec::new();
    for (r, cont) in rows.into_iter().zip(plan) {
        match joined.last_mut() {
            Some(last) if cont => {
                last.push((' ', last.last().map(|c| c.1).unwrap_or_default()));
                last.extend_from_slice(ansi::trim_start(&r));
            }
            _ => joined.push(r),
        }
    }
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut blank = 0;
    for r in joined {
        let p = ansi::plain(&r);
        if p.trim().is_empty() {
            blank += 1;
            if blank == 1 && !out.is_empty() {
                out.push(Line::from(""));
            }
            continue;
        }
        blank = 0;
        if is_rule(&p) {
            let st = r.iter().find(|c| c.0 != ' ').map(|c| c.1).unwrap_or_default();
            out.push(Line::from(Span::styled("─".repeat(w), st)));
            continue;
        }
        for row in ansi::wrap(&r, w) {
            out.push(ansi::to_line(&row));
        }
    }
    if approve {
        let red = status_look(&Status::Approve, t, frame).2;
        out.push(Line::from(""));
        out.push(Line::from(vec![pill("⚠ permission needed", red, t), Span::styled("  press 1-3 here or ⏎ to jump", fg(t.dim))]));
    }
    (out, status, draft)
}

/// Strip markdown bold and colour `code` spans. `code` state carries across rows.
fn md_line<'a>(seg: &str, base: Style, code: Style, in_code: &mut bool) -> Vec<Span<'a>> {
    let mut spans = Vec::new();
    for (i, part) in seg.replace("**", "").split('`').enumerate() {
        if i > 0 {
            *in_code = !*in_code;
        }
        if !part.is_empty() {
            spans.push(Span::styled(part.to_string(), if *in_code { code } else { base }));
        }
    }
    spans
}

/// Stable per-folder color (never the "alert" accent).
fn proj_color(cwd: &str, t: &Theme) -> Rgb {
    let h = cwd.bytes().fold(5381u32, |h, b| h.wrapping_mul(33) ^ b as u32);
    t.a([3, 4, 5, 1, 2][(h % 5) as usize])
}

fn status_look(s: &Status, t: &Theme, frame: u64) -> (String, String, Rgb) {
    match s {
        Status::Approve => {
            let col = if (frame / 6) % 2 == 0 { t.a(0) } else { t.a(0).mix(t.card, 0.45) };
            ("⚠".into(), "needs approval".into(), col)
        }
        Status::Working => (SPIN[(frame as usize) % SPIN.len()].into(), "working".into(), t.a(2)),
        Status::Idle => ("●".into(), "your turn".into(), t.a(1)),
        Status::Other(o) => ("·".into(), o.clone(), t.dim),
    }
}

fn display_title(l: &Live) -> &str {
    if l.title.is_empty() {
        &l.name
    } else {
        &l.title
    }
}

pub fn draw(app: &mut App, f: &mut Frame) {
    let area = f.area();
    let t = &app.theme;
    f.render_widget(Block::default().style(Style::default().bg(c(t.bg)).fg(c(t.fg))), area);
    if area.width < 60 || area.height < 14 {
        f.render_widget(Paragraph::new("ccdash needs at least 60×14"), area);
        return;
    }
    let [head, _, body, foot] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)]).areas(area);
    let body = body.inner(ratatui::layout::Margin { horizontal: 1, vertical: 0 });

    header(app, f, head);
    if app.tab == Tab::Live {
        app.ensure_log();
    }
    match app.tab {
        Tab::Live => live(app, f, body),
        Tab::History => history(app, f, body),
    }
    footer(app, f, foot);
    modal(app, f, area);
}

fn header(app: &App, f: &mut Frame, r: Rect) {
    let t = &app.theme;
    let tab = |label: String, active: bool| {
        if active {
            pill(label, t.a(3), t)
        } else {
            Span::styled(format!(" {label} "), fg(t.dim))
        }
    };
    let mut left = vec![
        Span::raw(" "),
        pill("◆ ccdash", t.a(4), t),
        Span::raw("  "),
        tab(format!("Live {}", app.live.len()), app.tab == Tab::Live),
        Span::raw(" "),
        tab(format!("History {}", app.hist.len()), app.tab == Tab::History),
    ];
    if !app.loaded {
        left.push(Span::styled(format!("  {} loading", SPIN[(app.frame as usize) % 10]), fg(t.dim)));
    }
    f.render_widget(Paragraph::new(Line::from(left)), r);

    let n = |s: Status| app.live.iter().filter(|l| l.status == s).count();
    let (appr, work, idle) = (n(Status::Approve), n(Status::Working), n(Status::Idle));
    let mut right = vec![];
    if appr > 0 {
        let (_, _, col) = status_look(&Status::Approve, t, app.frame);
        right.push(pill(format!("⚠ {appr} approve"), col, t));
        right.push(Span::raw("  "));
    }
    right.push(Span::styled(format!("{} {work} working", if work > 0 { SPIN[(app.frame as usize) % 10] } else { "○" }), fg(t.a(2))));
    right.push(Span::styled("   ", fg(t.dim)));
    right.push(Span::styled(format!("● {idle} your turn "), fg(t.a(1))));
    f.render_widget(Paragraph::new(Line::from(right).right_aligned()), r);
}

fn footer(app: &App, f: &mut Frame, r: Rect) {
    let t = &app.theme;
    let keys: &[(&str, &str)] = match app.tab {
        Tab::Live => &[("⏎", "show"), ("i", "write"), ("PgUp/⇳", "scroll"), ("1-3", "answer"), ("x", "esc"), ("v", "view"), ("n", "new"), ("K", "kill"), ("⇥", "history"), ("?", "help")],
        Tab::History => &[("⏎", "resume"), ("t", "in terminal"), ("D", "remove"), ("/", "filter"), ("esc", "clear"), ("n", "new"), ("⇥", "live"), ("?", "help"), ("q", "quit")],
    };
    let mut spans = vec![Span::raw(" ")];
    for (k, d) in keys {
        spans.push(keycap(k, t));
        spans.push(Span::styled(format!(" {d}  "), fg(t.dim)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), r);
    if let Some((msg, _, err)) = &app.toast {
        let col = if *err { t.a(0) } else { t.a(1) };
        f.render_widget(Paragraph::new(Line::from(vec![pill(msg.clone(), col, t), Span::raw(" ")]).right_aligned()), r);
    }
}

// ───────────────────────── live ─────────────────────────

fn live(app: &App, f: &mut Frame, r: Rect) {
    let t = &app.theme;
    if app.live.is_empty() {
        let card = centered(r, 46, 7);
        let b = Block::bordered().border_type(BorderType::Rounded).border_style(fg(t.faint)).style(Style::default().bg(c(t.card)));
        let msg = vec![
            Line::from(""),
            Line::from(Span::styled(if app.loaded { "no live Claude sessions" } else { "looking for sessions…" }, fg(t.fg).add_modifier(Modifier::BOLD))).centered(),
            Line::from(""),
            Line::from(vec![keycap("n", t), Span::styled(" start one   ", fg(t.dim)), keycap("⇥", t), Span::styled(" history", fg(t.dim))]).centered(),
        ];
        f.render_widget(Paragraph::new(msg).block(b), card);
        return;
    }
    let (list, prev) = if r.width >= 110 {
        let [a, _, b] = Layout::horizontal([Constraint::Percentage(40), Constraint::Length(1), Constraint::Min(30)]).areas(r);
        (a, b)
    } else {
        let cards = (app.live.len() as u16 * 4).min(r.height * 2 / 5).max(4);
        let [a, b] = Layout::vertical([Constraint::Length(cards), Constraint::Min(5)]).areas(r);
        (a, b)
    };

    // session cards
    let card_h = 4u16;
    let per = (list.height / card_h).max(1) as usize;
    let off = app.live_sel.saturating_sub(per - 1);
    for (i, l) in app.live.iter().enumerate().skip(off).take(per) {
        let y = list.y + ((i - off) as u16) * card_h;
        let rect = Rect { x: list.x, y, width: list.width, height: card_h };
        session_card(app, f, rect, l, i == app.live_sel);
    }
    if app.live.len() > per {
        let s = format!(" {}/{} ", app.live_sel + 1, app.live.len());
        let w = s.len() as u16;
        f.render_widget(Paragraph::new(Span::styled(s, fg(t.dim))), Rect { x: list.right().saturating_sub(w + 1), y: list.bottom() - 1, width: w, height: 1 });
    }

    // recent (not live) sessions fill the space under the cards
    let used = (app.live.len().min(per) as u16) * card_h;
    if app.show_recent && r.width >= 110 && list.height > used + 7 {
        recent(app, f, Rect { y: list.y + used + 1, height: list.height - used - 1, ..list });
    }

    preview(app, f, prev);
}

fn recent(app: &App, f: &mut Frame, r: Rect) {
    let t = &app.theme;
    let live: Vec<&str> = app.live.iter().map(|l| l.sid.as_str()).collect();
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(fg(t.faint))
        .title(Line::from(vec![Span::raw(" "), Span::styled("recent", fg(t.a(3)).add_modifier(Modifier::BOLD)), Span::raw(" ")]))
        .title_bottom(Line::from(vec![keycap("R", t), Span::styled(" hide ", fg(t.dim)), keycap("⇥", t), Span::styled(" all history ", fg(t.dim))]).right_aligned());
    let inner = block.inner(r).inner(ratatui::layout::Margin { horizontal: 1, vertical: 0 });
    f.render_widget(block, r);
    let w = inner.width as usize;
    let fw = (w / 3).clamp(6, 22);
    let lines: Vec<Line> = app
        .hist
        .iter()
        .filter(|p| !live.contains(&p.sid.as_str()))
        .take(inner.height as usize)
        .map(|p| {
            let label = if p.title.is_empty() { &p.first_prompt } else { &p.title };
            Line::from(vec![
                Span::styled(format!("{:>4}  ", ago(p.mtime)), fg(t.dim)),
                Span::styled(format!("{:<fw$}  ", trunc(&tilde(&p.cwd), fw)), fg(proj_color(&p.cwd, t))),
                Span::styled(trunc(label, w.saturating_sub(fw + 8)), fg(t.fg.mix(t.card, 0.2))),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

fn session_card(app: &App, f: &mut Frame, r: Rect, l: &Live, sel: bool) {
    let t = &app.theme;
    let (glyph, label, col) = status_look(&l.status, t, app.frame);
    let bg = if sel { t.card_alt } else { t.card };
    let border = if sel { col } else { t.faint };
    let block = Block::bordered()
        .border_type(if sel { BorderType::Thick } else { BorderType::Rounded })
        .border_style(fg(border))
        .style(Style::default().bg(c(bg)))
        .title(Line::from(vec![Span::raw(" "), Span::styled(format!("{glyph} {label}"), fg(col).add_modifier(Modifier::BOLD)), Span::raw(" ")]))
        .title(Line::from(Span::styled(format!(" up {} ", ago(l.started)), fg(t.dim))).right_aligned());
    let inner = block.inner(r);
    f.render_widget(block, r);
    let w = inner.width as usize;
    let title = Line::from(Span::styled(
        format!(" {}", trunc(display_title(l), w.saturating_sub(2))),
        fg(t.fg).add_modifier(if sel { Modifier::BOLD } else { Modifier::empty() }),
    ));
    let folder = tilde(&l.cwd);
    let tag = match &l.win {
        None => "⊘ view-only · ",
        Some(w) if w.hidden => "◌ background · ",
        _ => "",
    };
    let meta = format!("  {tag}{} · seen {}", l.pid, ago(l.updated));
    let fw = w.saturating_sub(meta.chars().count() + 3).max(8);
    let sub = Line::from(vec![
        Span::raw(" "),
        Span::styled(format!("▍{}", trunc(&folder, fw)), fg(proj_color(&l.cwd, t))),
        Span::styled(meta, fg(t.dim)),
    ]);
    f.render_widget(Paragraph::new(vec![title, sub]), inner);
}

fn preview(app: &App, f: &mut Frame, r: Rect) {
    let t = &app.theme;
    let Some(l) = app.cur_live() else { return };
    let (_, _, col) = status_look(&l.status, t, app.frame);
    let screen_mode = app.view < 2 && !l.screen.is_empty();
    let mode = match (screen_mode, app.view) {
        (true, 0) => "live",
        (true, _) => "structured",
        _ => "conversation",
    };
    // inner width is known before drawing: borders (2) + margin (2)
    let w = r.width.saturating_sub(4) as usize;
    let h = r.height.saturating_sub(2) as usize;

    let (mut lines, status, draft) = match (screen_mode, app.view) {
        (true, 0) => format_terminal(l, w, t, app.frame),
        (true, _) => format_screen(l, w, t, app.frame),
        _ => (Vec::new(), String::new(), String::new()),
    };
    // conversation: whole transcript, formatted once per change and cached
    let mut cached = false;
    if !screen_mode {
        let tr = l.transcript.as_ref().and_then(|p| app.transcripts.get(p).map(|tr| (p.clone(), tr)));
        match tr {
            Some((p, tr)) => {
                let key = (p, tr.gen, w);
                let mut cache = app.log_cache.borrow_mut();
                if cache.as_ref().map(|c| &c.0) != Some(&key) {
                    *cache = Some((key, format_log(t, &tr.msgs, w)));
                }
                cached = true;
            }
            None => lines.push(Line::from(Span::styled("(no transcript yet)", fg(t.dim)))),
        }
    }

    let foot_left = if status.is_empty() { format!(" {} · {} ", tilde(&l.cwd), &l.sid[..l.sid.len().min(8)]) } else { format!(" {status} ") };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(fg(t.faint))
        .style(Style::default().bg(c(t.bg.mix(t.card, 0.5))))
        .title(Line::from(vec![Span::raw(" "), pill(mode, col, t), Span::styled(format!(" {} ", trunc(display_title(l), 50)), fg(t.fg))]))
        .title_bottom(Line::from(Span::styled(trunc(&foot_left, w.saturating_sub(12)), fg(t.dim))))
        .title_bottom(Line::from(vec![keycap("v", t), Span::styled(" toggle ", fg(t.dim))]).right_aligned());
    let inner = block.inner(r).inner(ratatui::layout::Margin { horizontal: 1, vertical: 0 });
    f.render_widget(block, r);

    // composer pinned at the bottom: write into the session from here
    let mut body_h = h;
    if h > 8 {
        body_h = h - 3;
        let d = Rect { y: inner.bottom() - 3, height: 3, ..inner };
        composer(app, f, d, l, &draft);
    }
    let cache_ref = app.log_cache.borrow();
    let all: &[Line] = if cached { cache_ref.as_ref().map(|c| c.1.as_slice()).unwrap_or(&[]) } else { &lines };
    let max = all.len().saturating_sub(body_h);
    app.scroll_max.set(max);
    app.page.set(body_h.max(2) - 1);
    // keep the text you're reading in place while new output arrives below it
    let total = all.len();
    if app.scroll.get() > 0 && total > app.scroll_total.get() {
        app.scroll.set(app.scroll.get() + total - app.scroll_total.get());
    }
    app.scroll_total.set(total);
    let scroll = app.scroll.get().min(max);
    let start = max - scroll;
    let mut view: Vec<Line> = all[start..(start + body_h).min(all.len())].to_vec();
    // remember what's on screen so a mouse selection can be copied as plain text
    app.pv_start.set(start);
    app.pv_body.set(Rect { height: (view.len() as u16).min(body_h as u16), ..inner });
    *app.pv_text.borrow_mut() = view.iter().map(plain_text).collect();
    if let Some((a, b)) = app.sel {
        let (a, b) = (a.min(b), a.max(b));
        let bg = c(t.a(3).mix(t.bg, 0.62));
        for (i, line) in view.iter_mut().enumerate() {
            if (a..=b).contains(&(start + i)) {
                let pad = w.saturating_sub(line.width());
                line.spans.push(Span::raw(" ".repeat(pad)));
                line.style = line.style.bg(bg);
            }
        }
    }
    let at_top = scroll == max && max > 0;
    let body_r = Rect { height: body_h as u16, ..inner };
    f.render_widget(Paragraph::new(view), body_r);
    if at_top {
        let msg = if screen_mode { " top of live screen · keep scrolling for the full conversation " } else { " start of session " };
        f.render_widget(Paragraph::new(Line::from(Span::styled(msg, fg(t.dim).add_modifier(Modifier::ITALIC))).centered()), Rect { height: 1, ..body_r });
    } else if scroll > 0 {
        let tag = Line::from(vec![pill(format!("↑ {scroll} lines up"), t.a(3), t), Span::raw(" "), keycap("End", t), Span::styled(" follow ", fg(t.dim))]);
        let wdt = tag.width() as u16;
        f.render_widget(Paragraph::new(tag), Rect { x: body_r.right().saturating_sub(wdt), y: body_r.y, width: wdt.min(body_r.width), height: 1 });
    }
}

fn composer(app: &App, f: &mut Frame, r: Rect, l: &Live, draft: &str) {
    let t = &app.theme;
    let cyan = t.a(5);
    let w = r.width.saturating_sub(4) as usize;
    let (border, line, hints) = match &app.mode {
        Mode::Send(s) => {
            let cursor = if (app.frame / 6) % 2 == 0 { "▏" } else { " " };
            let room = w.saturating_sub(3);
            let n = s.chars().count();
            let shown: String = s.chars().skip(n.saturating_sub(room)).collect();
            (
                cyan,
                Line::from(vec![Span::styled("› ", fg(cyan).add_modifier(Modifier::BOLD)), Span::styled(shown, fg(t.fg)), Span::styled(cursor, fg(cyan))]),
                vec![keycap("⏎", t), Span::styled(" send ", fg(t.dim)), keycap("esc", t), Span::styled(" cancel ", fg(t.dim))],
            )
        }
        _ if l.win.is_none() => (t.faint, Line::from(Span::styled("not in a kitty window, view only", fg(t.dim))), vec![]),
        _ if !draft.is_empty() => (
            t.faint,
            Line::from(vec![Span::styled("typed in session › ", fg(t.dim)), Span::styled(trunc(draft, w.saturating_sub(20)), fg(t.fg))]),
            vec![keycap("i", t), Span::styled(" write ", fg(t.dim))],
        ),
        _ => (
            t.faint,
            Line::from(vec![Span::styled("› ", fg(t.faint.mix(t.fg, 0.3))), Span::styled("write to this session…", fg(t.faint.mix(t.fg, 0.35)))]),
            vec![keycap("i", t), Span::styled(" write ", fg(t.dim))],
        ),
    };
    let b = Block::bordered().border_type(BorderType::Rounded).border_style(fg(border)).padding(ratatui::widgets::Padding::horizontal(1));
    let inner = b.inner(r);
    f.render_widget(b, r);
    let hw: u16 = hints.iter().map(|s| s.width() as u16).sum();
    let [txt, _, keys] = Layout::horizontal([Constraint::Min(5), Constraint::Length(1), Constraint::Length(hw)]).areas(inner);
    f.render_widget(Paragraph::new(line), txt);
    f.render_widget(Paragraph::new(Line::from(hints)), keys);
}

/// A preview line as plain text, without the decorative gutter (│ ◆ ▶ ⚙ ▌ ⎿).
fn plain_text(l: &Line) -> String {
    let mut spans = l.spans.iter().peekable();
    if let Some(first) = spans.peek() {
        let g = first.content.trim();
        if matches!(g, "│" | "◆" | "▶" | "▌" | "⚙" | "⎿") {
            spans.next();
        }
    }
    let s: String = spans.map(|s| s.content.as_ref()).collect();
    let s = s.trim_end();
    s.strip_prefix("  ⎿ ").or_else(|| s.strip_prefix("⎿ ")).unwrap_or(s).to_string()
}

#[derive(Clone, Copy, PartialEq)]
enum Blk {
    None,
    User,
    Claude,
    Tool,
    Meta,
}

/// `Name(args)` tool-call header after the `●` bullet.
fn tool_header(rest: &str) -> Option<(String, String)> {
    let p = rest.find('(')?;
    let name = rest[..p].trim_end();
    if name.is_empty() || name.chars().count() > 48 || !name.chars().all(|c| c.is_alphanumeric() || "_:- ".contains(c)) {
        return None;
    }
    let first_up = name.chars().next().is_some_and(char::is_uppercase);
    if !((first_up && !name.contains(' ')) || name.contains("__") || name.contains(" - ")) {
        return None;
    }
    let args = rest[p + 1..].trim_end();
    let args = args.strip_suffix(')').unwrap_or(args);
    Some((name.to_string(), args.trim_start_matches("MCP)(").to_string()))
}

fn tidy_status(s: &str) -> String {
    let s = s.replace(['│', '⏵'], " ").replace("(shift+tab to cycle)", "").replace(" · ", "  ");
    let parts: Vec<&str> = s.split("  ").map(str::trim).filter(|p| !p.is_empty() && *p != "--" && !p.ends_with(" --")).collect();
    parts.join(" · ")
}

fn is_summary(t: &str) -> bool {
    let mut it = t.split_whitespace();
    matches!(it.next(), Some("Ran" | "Read" | "Searched" | "Listed" | "Wrote" | "Edited" | "Updated" | "Fetched" | "Called" | "Found"))
        && it.next().is_some_and(|n| n.chars().all(|c| c.is_ascii_digit()) || n == "for")
}

/// Turn the raw kitty screen of a Claude Code session into styled blocks.
/// Returns (lines, status line, draft input).
fn format_screen<'a>(l: &Live, w: usize, t: &Theme, frame: u64) -> (Vec<Line<'a>>, String, String) {
    let mut rows = reflow(&l.screen);
    let (mut status, mut draft) = (String::new(), String::new());
    let approve = matches!(l.status, Status::Approve);

    // approval dialog: everything from the box/rule above "Do you want…" down
    let mut dialog: Vec<String> = Vec::new();
    if approve {
        if let Some(q) = rows.iter().rposition(|r| r.contains("Do you want")) {
            let start = rows[..q].iter().rposition(|r| is_rule(r) || r.trim_start().starts_with('╭')).filter(|&i| q - i <= 30).unwrap_or(q.saturating_sub(6));
            dialog = rows.split_off(start);
        }
    } else {
        // input box between the last two rules, status lines after
        let rules: Vec<usize> = rows.iter().enumerate().filter(|(_, r)| is_rule(r)).map(|(i, _)| i).collect();
        if rules.len() >= 2 {
            let (a, b) = (rules[rules.len() - 2], rules[rules.len() - 1]);
            if rows.len() - b <= 6 && b - a <= 10 {
                draft = rows[a + 1..b].iter().map(|s| s.trim().trim_start_matches('❯').trim().to_string()).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
                status = rows[b + 1..].iter().map(|s| tidy_status(s)).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
                rows.truncate(a);
            }
        }
    }

    let magenta = t.a(4);
    let cyan = t.a(5);
    let tool_c = t.a(3);
    let text = t.fg.mix(t.card, 0.08);
    let meta = t.dim;
    let gw = 2usize; // gutter width
    let tw = w.saturating_sub(gw).max(8);

    let mut out: Vec<Line> = Vec::new();
    let mut kind = Blk::None;
    let mut tool_rows = 0usize;
    let mut tool_more = false;
    let mut tool_name = String::new();
    let mut gap = false;
    let pal = Palette::new(t, text);

    let push = |out: &mut Vec<Line<'a>>, gutter: Span<'a>, body: Vec<Span<'a>>| {
        let mut v = vec![gutter];
        v.extend(body);
        out.push(Line::from(v));
    };
    let sep = |out: &mut Vec<Line<'a>>| {
        if out.last().is_some_and(|l| l.width() > 0) {
            out.push(Line::from(""));
        }
    };

    for raw in rows {
        let tt = raw.trim();
        if tt.is_empty() {
            gap = true;
            continue;
        }
        if is_rule(&raw) {
            continue;
        }
        let first = tt.chars().next().unwrap();
        let rest = tt[first.len_utf8()..].trim_start();
        let was_gap = std::mem::replace(&mut gap, false);

        match first {
            '●' | '⏺' => {
                sep(&mut out);
                if let Some((name, args)) = tool_header(rest) {
                    kind = Blk::Tool;
                    tool_rows = 0;
                    tool_more = false;
                    let room = tw.saturating_sub(name.chars().count() + 1);
                    let mut v = vec![Span::styled(name.clone(), fg(tool_c).add_modifier(Modifier::BOLD)), Span::raw(" ")];
                    if name == "Bash" {
                        v.extend(hl::spans(&trunc(&args, room), &pal));
                    } else {
                        v.push(Span::styled(trunc(&args, room), fg(meta)));
                    }
                    tool_name = name;
                    push(&mut out, Span::styled("⚙ ", fg(tool_c)), v);
                } else {
                    kind = Blk::Claude;
                    for (i, seg) in wrap(rest, tw).into_iter().enumerate() {
                        let g = if i == 0 { Span::styled("◆ ", fg(magenta).add_modifier(Modifier::BOLD)) } else { Span::styled("│ ", fg(magenta.mix(t.card, 0.55))) };
                        push(&mut out, g, vec![Span::styled(seg, fg(text))]);
                    }
                }
            }
            '⎿' => {
                if kind != Blk::Tool {
                    kind = Blk::Tool;
                    tool_rows = 0;
                    tool_more = false;
                }
                tool_line(&mut out, rest, tw, &mut tool_rows, &mut tool_more, t, &tool_name, &pal);
            }
            '❯' | '>' => {
                sep(&mut out);
                kind = Blk::User;
                for (i, seg) in wrap(rest, tw).into_iter().enumerate() {
                    let g = if i == 0 { Span::styled("▶ ", fg(cyan).add_modifier(Modifier::BOLD)) } else { Span::styled("│ ", fg(cyan.mix(t.card, 0.55))) };
                    push(&mut out, g, vec![Span::styled(seg, fg(cyan.mix(t.fg, 0.35)))]);
                }
            }
            '✻' | '✳' | '✶' | '✽' | '✢' | '※' | '·' | '*' | '✦' => {
                sep(&mut out);
                kind = Blk::Meta;
                let live = rest.contains('…') && !rest.contains("done");
                let g = if live { SPIN[(frame as usize) % SPIN.len()].to_string() + " " } else { format!("{first} ") };
                let st = if live { fg(t.a(2)) } else { fg(meta).add_modifier(Modifier::ITALIC) };
                for (i, seg) in wrap(rest, tw).into_iter().enumerate() {
                    push(&mut out, Span::styled(if i == 0 { g.clone() } else { "  ".into() }, st), vec![Span::styled(seg, st)]);
                }
            }
            _ if is_summary(tt) => {
                sep(&mut out);
                kind = Blk::Tool;
                tool_rows = 0;
                tool_more = false;
                tool_name.clear();
                push(&mut out, Span::styled("⚙ ", fg(tool_c)), vec![Span::styled(tt.to_string(), fg(meta))]);
            }
            _ => match kind {
                Blk::Tool => tool_line(&mut out, raw.trim_end(), tw, &mut tool_rows, &mut tool_more, t, &tool_name, &pal),
                Blk::Claude => {
                    if was_gap {
                        push(&mut out, Span::styled("│ ", fg(magenta.mix(t.card, 0.55))), vec![]);
                    }
                    // keep relative indentation beyond Claude's own 2-space margin
                    let body = raw.strip_prefix("  ").unwrap_or(&raw).trim_end();
                    let (bullet, body) = match body.trim_start().strip_prefix("- ") {
                        Some(b) => (Some(body.len() - body.trim_start().len()), b),
                        None => (None, body),
                    };
                    let code = bullet.is_none() && hl::looks_like_code(body);
                    let segs = match bullet {
                        Some(ind) => wrap(&format!("{}  {}", " ".repeat(ind), body), tw),
                        None => wrap(body, tw),
                    };
                    for (i, seg) in segs.into_iter().enumerate() {
                        let mut spans = vec![];
                        if let (Some(ind), 0) = (bullet, i) {
                            spans.push(Span::raw(" ".repeat(ind)));
                            spans.push(Span::styled("• ", fg(magenta)));
                            spans.push(Span::styled(seg.trim_start().to_string(), fg(text)));
                        } else if code {
                            spans.extend(hl::spans(&seg, &pal));
                        } else {
                            spans.push(Span::styled(seg, fg(text)));
                        }
                        push(&mut out, Span::styled("│ ", fg(magenta.mix(t.card, 0.55))), spans);
                    }
                }
                Blk::User => {
                    for seg in wrap(tt, tw) {
                        push(&mut out, Span::styled("│ ", fg(cyan.mix(t.card, 0.55))), vec![Span::styled(seg, fg(cyan.mix(t.fg, 0.35)))]);
                    }
                }
                Blk::Meta | Blk::None => {
                    for seg in wrap(tt, tw) {
                        push(&mut out, Span::raw("  "), vec![Span::styled(seg, fg(meta))]);
                    }
                }
            },
        }
    }

    // approval dialog as a red card
    if !dialog.is_empty() {
        let red = status_look(&Status::Approve, t, frame).2;
        sep(&mut out);
        out.push(Line::from(vec![Span::styled("▌ ", fg(red)), pill("⚠ permission needed", red, t), Span::styled("  press 1-3 here or ⏎ to jump", fg(meta))]));
        for raw in dialog {
            let tt = raw.trim().trim_matches(|c| c == '│' || c == '╭' || c == '╰' || c == '╮' || c == '╯').trim();
            if tt.is_empty() || is_rule(tt) || tt.chars().all(|c| c == '─') {
                continue;
            }
            let (st, txt) = if tt.contains("Do you want") {
                (fg(t.fg).add_modifier(Modifier::BOLD), tt.to_string())
            } else if let Some(o) = tt.strip_prefix('❯') {
                (fg(red).add_modifier(Modifier::BOLD), format!("❯ {}", o.trim()))
            } else if tt.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                (fg(cyan.mix(t.fg, 0.3)), format!("  {tt}"))
            } else if tt.starts_with("Esc") {
                (fg(meta).add_modifier(Modifier::ITALIC), tt.to_string())
            } else {
                (fg(text), tt.to_string())
            };
            for seg in wrap(&txt, tw) {
                out.push(Line::from(vec![Span::styled("▌ ", fg(red)), Span::styled(seg, st)]));
            }
        }
    }
    (out, status, draft)
}

#[allow(clippy::too_many_arguments)]
fn tool_line<'a>(out: &mut Vec<Line<'a>>, s: &str, tw: usize, rows: &mut usize, more: &mut bool, t: &Theme, tool: &str, pal: &Palette) {
    let edits = matches!(tool, "Update" | "Edit" | "Write" | "MultiEdit" | "Create" | "NotebookEdit");
    let max = if edits { 14 } else { 3 };
    let gut = fg(t.faint.mix(t.fg, 0.3));
    if *rows >= max {
        if !*more {
            out.push(Line::from(Span::styled("    ⋯", gut)));
            *more = true;
        }
        return;
    }
    let lead = if *rows == 0 { "  ⎿ " } else { "    " };
    // diff rows from edit tools: line number, +/- marker, highlighted code
    if let Some((num, sign, code)) = hl::diff_row(s) {
        let (mark, tint) = match sign {
            '+' => (Span::styled("+ ", fg(t.a(1)).add_modifier(Modifier::BOLD)), Some(t.a(1))),
            '-' => (Span::styled("- ", fg(t.a(0)).add_modifier(Modifier::BOLD)), Some(t.a(0))),
            _ => (Span::raw("  "), None),
        };
        let room = tw.saturating_sub(4 + num.len() + 3);
        let mut v = vec![Span::styled(lead, gut), Span::styled(format!("{num:>4} "), fg(t.faint.mix(t.fg, 0.25))), mark];
        let mut body = hl::spans(&trunc(&code, room), pal);
        if let Some(c) = tint {
            let bg = Color::Rgb(t.bg.mix(c, 0.16).0, t.bg.mix(c, 0.16).1, t.bg.mix(c, 0.16).2);
            for sp in body.iter_mut() {
                sp.style = sp.style.bg(bg);
            }
            let used: usize = body.iter().map(|s| s.width()).sum();
            body.push(Span::styled(" ".repeat(room.saturating_sub(used)), Style::default().bg(bg)));
        }
        v.extend(body);
        out.push(Line::from(v));
        *rows += 1;
        return;
    }
    let s = s.trim();
    let code = edits || hl::looks_like_code(s);
    for seg in wrap(s, tw.saturating_sub(2)).into_iter().take(max - *rows) {
        let g = if *rows == 0 { "  ⎿ " } else { "    " };
        let mut v = vec![Span::styled(g, gut)];
        if code {
            let dim = Palette::new(t, t.dim);
            v.extend(hl::spans(&seg, &dim));
        } else {
            v.push(Span::styled(seg, fg(t.dim)));
        }
        out.push(Line::from(v));
        *rows += 1;
    }
}

fn format_log(t: &Theme, msgs: &[(bool, String)], w: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    if msgs.is_empty() {
        lines.push(Line::from(Span::styled("(no messages yet)", fg(t.dim))));
        return lines;
    }
    let mut prev: Option<bool> = None;
    for (user, body) in msgs {
        let same = prev == Some(*user);
        let tool_only = body.lines().all(|l| l.starts_with('⚙'));
        if same && tool_only {
            // consecutive tool calls: drop the blank separator, keep them grouped
            if lines.last().is_some_and(|l| l.width() == 0) {
                lines.pop();
            }
        }
        prev = Some(*user);
        if !same {
            lines.push(if *user {
            Line::from(vec![Span::styled("▶ you", fg(t.a(5)).add_modifier(Modifier::BOLD))])
        } else {
            Line::from(vec![Span::styled("◆ claude", fg(t.a(4)).add_modifier(Modifier::BOLD))])
            });
        }
        let bar = fg(if *user { t.a(5) } else { t.a(4) }.mix(t.card, 0.4));
        let mut in_code = false;
        let mut fence: Option<String> = None;
        let pal = Palette::new(t, t.fg);
        let code_bg = Style::default().bg(c(t.card));
        for raw in body.lines().take(400) {
            if let Some(lang) = raw.trim_start().strip_prefix("```") {
                if fence.is_some() {
                    fence = None;
                } else {
                    fence = Some(lang.trim().to_string());
                    if !lang.trim().is_empty() {
                        lines.push(Line::from(vec![Span::styled("│ ", bar), Span::styled(format!(" {} ", lang.trim()), fg(t.dim).bg(c(t.card_alt)))]));
                    }
                }
                continue;
            }
            if fence.is_some() {
                let cw = w.saturating_sub(4);
                for seg in wrap(raw.trim_end(), cw) {
                    let mut spans = vec![Span::styled("│ ", bar), Span::styled(" ", code_bg)];
                    let mut body = hl::spans(&seg, &pal);
                    let used: usize = body.iter().map(|s| s.width()).sum();
                    for sp in body.iter_mut() {
                        sp.style = sp.style.bg(c(t.card));
                    }
                    spans.append(&mut body);
                    spans.push(Span::styled(" ".repeat(cw.saturating_sub(used) + 1), code_bg));
                    lines.push(Line::from(spans));
                }
                continue;
            }
            let style = if raw.starts_with('⚙') { fg(t.dim) } else { fg(t.fg) };
            let style = if raw.starts_with('#') { style.add_modifier(Modifier::BOLD) } else { style };
            for seg in wrap(raw.trim_start_matches('#').trim_start_matches(' ').trim_end(), w.saturating_sub(2)) {
                let mut spans = vec![Span::styled("│ ", bar)];
                spans.extend(md_line(&seg, style, fg(t.a(5)), &mut in_code));
                lines.push(Line::from(spans));
            }
            in_code = false;
        }
        lines.push(Line::from(""));
    }
    lines
}

// ───────────────────────── history ─────────────────────────

fn bucket(mtime: f64) -> &'static str {
    let d = crate::data::now() - mtime;
    if d < 86400.0 {
        "last 24 hours"
    } else if d < 7.0 * 86400.0 {
        "this week"
    } else {
        "earlier"
    }
}

fn history(app: &App, f: &mut Frame, r: Rect) {
    let t = &app.theme;
    let view = app.hist_view();
    let [bar, _, list, detail] = Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(3), Constraint::Length(7)]).areas(r);

    // filter bar
    let typing = matches!(app.mode, Mode::Filter);
    let cursor = if typing && (app.frame / 6) % 2 == 0 { "▏" } else { " " };
    let mut fb = vec![keycap("/", t), Span::raw(" ")];
    if app.filter.is_empty() && !typing {
        fb.push(Span::styled("filter by folder, title or prompt", fg(t.faint.mix(t.fg, 0.3))));
    } else {
        fb.push(Span::styled(app.filter.clone(), fg(t.fg).add_modifier(Modifier::BOLD)));
        fb.push(Span::styled(cursor, fg(t.a(3))));
    }
    f.render_widget(Paragraph::new(Line::from(fb)), bar);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(format!("{} of {} sessions ", view.len(), app.hist.len()), fg(t.dim))).right_aligned()),
        bar,
    );

    // rows with bucket headers
    enum Row {
        Head(&'static str),
        Item(usize),
    }
    let mut rows = Vec::new();
    let mut last = "";
    for (i, p) in view.iter().enumerate() {
        let b = bucket(p.mtime);
        if b != last {
            rows.push(Row::Head(b));
            last = b;
        }
        rows.push(Row::Item(i));
    }
    let sel_row = rows.iter().position(|r| matches!(r, Row::Item(i) if *i == app.hist_sel)).unwrap_or(0);
    let h = list.height as usize;
    let off = if sel_row + 2 > h { sel_row + 2 - h } else { 0 };
    let live_sids: Vec<&str> = app.live.iter().map(|l| l.sid.as_str()).collect();
    let w = list.width as usize;
    let fw = (w / 4).clamp(14, 30);

    for (k, row) in rows.iter().enumerate().skip(off).take(h) {
        let y = list.y + (k - off) as u16;
        let line_r = Rect { x: list.x, y, width: list.width, height: 1 };
        match row {
            Row::Head(b) => {
                let rule = "─".repeat(w.saturating_sub(b.len() + 3));
                f.render_widget(
                    Paragraph::new(Line::from(vec![Span::styled(format!(" {b} "), fg(t.a(3)).add_modifier(Modifier::BOLD)), Span::styled(rule, fg(t.faint))])),
                    line_r,
                );
            }
            Row::Item(i) => {
                let p = view[*i];
                let sel = *i == app.hist_sel;
                let is_live = live_sids.contains(&p.sid.as_str());
                let pc = proj_color(&p.cwd, t);
                let label = if p.title.is_empty() { &p.first_prompt } else { &p.title };
                let badge = if is_live { "● live " } else { "" };
                let size = format!("{:>5} ", human(p.size));
                let tw = w.saturating_sub(2 + 5 + 2 + fw + 2 + badge.chars().count() + size.len());
                let base = if sel { Style::default().bg(c(t.card_alt)) } else { Style::default() };
                let spans = vec![
                    Span::styled(if sel { "▌ " } else { "  " }, fg(t.a(3))),
                    Span::styled(format!("{:>4} ", ago(p.mtime)), fg(t.dim)),
                    Span::raw("  "),
                    Span::styled(format!("{:<fw$}", trunc(&tilde(&p.cwd), fw)), fg(pc)),
                    Span::raw("  "),
                    Span::styled(format!("{:<tw$}", trunc(label, tw)), fg(t.fg).add_modifier(if sel { Modifier::BOLD } else { Modifier::empty() })),
                    Span::styled(badge, fg(t.a(1)).add_modifier(Modifier::BOLD)),
                    Span::styled(size, fg(t.faint.mix(t.fg, 0.35))),
                ];
                f.render_widget(Paragraph::new(Line::from(spans)).style(base), line_r);
            }
        }
    }
    if view.is_empty() {
        f.render_widget(Paragraph::new(Line::from(Span::styled("nothing matches", fg(t.dim))).centered()), Rect { y: list.y + 1, height: 1, ..list });
    }

    // detail card
    let Some(p) = view.get(app.hist_sel) else { return };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(fg(t.faint))
        .style(Style::default().bg(c(t.card)))
        .title(Line::from(vec![Span::raw(" "), Span::styled(trunc(if p.title.is_empty() { "untitled" } else { &p.title }, 70), fg(t.fg).add_modifier(Modifier::BOLD)), Span::raw(" ")]))
        .title_bottom(Line::from(Span::styled(format!(" {} · {} ", tilde(&p.cwd), p.sid), fg(t.dim))))
        .title_bottom(Line::from(vec![keycap("⏎", t), Span::styled(if live_sids.contains(&p.sid.as_str()) { " focus " } else { " resume " }, fg(t.dim))]).right_aligned());
    let inner = block.inner(detail).inner(ratatui::layout::Margin { horizontal: 1, vertical: 0 });
    f.render_widget(block, detail);
    let mut lines = vec![];
    for seg in wrap(&format!("▶ {}", p.first_prompt), inner.width as usize).into_iter().take(inner.height as usize) {
        lines.push(Line::from(Span::styled(seg, fg(t.a(5)))));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn human(b: u64) -> String {
    match b {
        0..=1023 => format!("{b}B"),
        1024..=1048575 => format!("{}k", b / 1024),
        _ => format!("{:.1}M", b as f64 / 1048576.0),
    }
}

// ───────────────────────── modals ─────────────────────────

fn centered(r: Rect, w: u16, h: u16) -> Rect {
    let [v] = Layout::vertical([Constraint::Length(h.min(r.height))]).flex(Flex::Center).areas(r);
    let [x] = Layout::horizontal([Constraint::Length(w.min(r.width))]).flex(Flex::Center).areas(v);
    x
}

fn dialog<'a>(t: &Theme, title: Vec<Span<'a>>, accent: Rgb, hints: Vec<Span<'a>>) -> Block<'a> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(fg(accent))
        .style(Style::default().bg(c(t.card)).fg(c(t.fg)))
        .title(Line::from(title))
        .title_bottom(Line::from(hints).right_aligned())
        .padding(ratatui::widgets::Padding::horizontal(1))
}

fn hint<'a>(t: &Theme, pairs: &[(&str, &str)]) -> Vec<Span<'a>> {
    let mut v = vec![];
    for (k, d) in pairs {
        v.push(keycap(k, t));
        v.push(Span::styled(format!(" {d} "), fg(t.dim)));
    }
    v
}

fn modal(app: &App, f: &mut Frame, area: Rect) {
    let t = &app.theme;
    let cursor = Span::styled(if (app.frame / 6) % 2 == 0 { "▏" } else { " " }, fg(t.a(3)));
    let w = (area.width * 7 / 10).clamp(50, 110);
    match &app.mode {
        Mode::New { input, sel } => {
            let sugg = app.suggestions(input);
            let r = centered(area, w, 6 + sugg.len().max(1) as u16);
            f.render_widget(Clear, r);
            let b = dialog(t, vec![Span::raw(" "), pill("+ new session", t.a(1), t), Span::raw(" ")], t.a(1), hint(t, &[("↑↓", "pick"), ("⇥", "complete"), ("⏎", "start"), ("^T", "in terminal"), ("esc", "cancel")]));
            let inner = b.inner(r);
            f.render_widget(b, r);
            let mut lines = vec![
                Line::from(""),
                Line::from(vec![Span::styled("folder ", fg(t.dim)), Span::styled(input.clone(), fg(t.fg).add_modifier(Modifier::BOLD)), cursor]),
                Line::from(""),
            ];
            if sugg.is_empty() {
                lines.push(Line::from(Span::styled("  no known project matches; ⏎ uses the path as typed", fg(t.dim))));
            }
            for (i, d) in sugg.iter().enumerate() {
                let s = Some(i) == *sel;
                lines.push(Line::from(vec![
                    Span::styled(if s { "▌ " } else { "  " }, fg(t.a(1))),
                    Span::styled(tilde(d), fg(proj_color(d, t)).add_modifier(if s { Modifier::BOLD } else { Modifier::empty() })),
                ]));
            }
            f.render_widget(Paragraph::new(lines), inner);
        }
        Mode::Confirm(pid, name) => {
            let r = centered(area, 56, 7);
            f.render_widget(Clear, r);
            let b = dialog(t, vec![Span::raw(" "), pill("kill session?", t.a(0), t), Span::raw(" ")], t.a(0), hint(t, &[("y", "kill"), ("n", "keep")]));
            let inner = b.inner(r);
            f.render_widget(b, r);
            f.render_widget(
                Paragraph::new(vec![
                    Line::from(""),
                    Line::from(Span::styled(trunc(name, inner.width as usize), fg(t.fg).add_modifier(Modifier::BOLD))).centered(),
                    Line::from(Span::styled(format!("pid {pid} gets SIGTERM"), fg(t.dim))).centered(),
                ]),
                inner,
            );
        }
        Mode::Remove(p) => {
            let r = centered(area, 64, 8);
            f.render_widget(Clear, r);
            let b = dialog(t, vec![Span::raw(" "), pill("remove from history?", t.a(0), t), Span::raw(" ")], t.a(0), hint(t, &[("y", "remove"), ("n", "keep")]));
            let inner = b.inner(r);
            f.render_widget(b, r);
            let name = if p.title.is_empty() { &p.first_prompt } else { &p.title };
            f.render_widget(
                Paragraph::new(vec![
                    Line::from(""),
                    Line::from(Span::styled(trunc(name, inner.width as usize), fg(t.fg).add_modifier(Modifier::BOLD))).centered(),
                    Line::from(Span::styled(tilde(&p.cwd), fg(proj_color(&p.cwd, t)))).centered(),
                    Line::from(Span::styled("moved to ~/.claude/ccdash-removed (U undoes)", fg(t.dim))).centered(),
                ]),
                inner,
            );
        }
        Mode::Help => {
            let keys: &[(&str, &str)] = &[
                ("⇥ ← →", "switch Live / History"),
                ("↑↓ j k", "move   (g/G top/bottom)"),
                ("⏎", "show session · resume past one in background"),
                ("t", "resume in a visible terminal (history)"),
                ("i", "type a prompt into the session"),
                ("1 2 3", "answer a permission prompt"),
                ("x", "send Esc (interrupt)"),
                ("v", "live (original colours) → structured → conversation"),
                ("PgUp PgDn", "scroll preview (wheel, u/d half page)"),
                ("Home End", "top of preview · back to live"),
                ("drag", "select preview lines → copied to clipboard"),
                ("R", "show / hide recent panel"),
                ("n", "new session (background · ^T in terminal)"),
                ("K", "kill session (SIGTERM)"),
                ("D U", "remove past session from history · undo"),
                ("/", "filter history  (esc clears)"),
                ("r", "refresh now"),
                ("q", "quit"),
            ];
            let r = centered(area, 60, keys.len() as u16 + 4);
            f.render_widget(Clear, r);
            let b = dialog(t, vec![Span::raw(" "), pill("keys", t.a(4), t), Span::raw(" ")], t.a(4), hint(t, &[("any key", "close")]));
            let inner = b.inner(r);
            f.render_widget(b, r);
            let mut lines = vec![Line::from("")];
            for (k, d) in keys {
                lines.push(Line::from(vec![Span::styled(format!("{k:>10}  "), fg(t.a(3)).add_modifier(Modifier::BOLD)), Span::styled(*d, fg(t.fg))]));
            }
            f.render_widget(Paragraph::new(lines), inner);
        }
        _ => {}
    }
}
