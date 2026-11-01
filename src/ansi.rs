//! Parse the SGR-coloured text kitty returns from `get-text --ansi` into styled cells,
//! so the preview can show a session with its original terminal colours.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

pub type Cell = (char, Style);

/// Text without escape sequences.
pub fn strip(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            skip_escape(&mut it);
        } else {
            out.push(c);
        }
    }
    out
}

/// Consume one escape sequence (after ESC). Returns the CSI parameters and final byte if it was a CSI.
fn skip_escape(it: &mut std::iter::Peekable<std::str::Chars>) -> Option<(String, char)> {
    match it.next()? {
        '[' => {
            let mut params = String::new();
            for c in it.by_ref() {
                if ('\x40'..='\x7e').contains(&c) {
                    return Some((params, c));
                }
                params.push(c);
            }
            None
        }
        ']' => {
            // OSC: until BEL or ST (ESC \)
            while let Some(c) = it.next() {
                if c == '\x07' {
                    break;
                }
                if c == '\x1b' && it.peek() == Some(&'\\') {
                    it.next();
                    break;
                }
            }
            None
        }
        _ => None,
    }
}

fn indexed(n: u8) -> Color {
    match n {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        7 => Color::Gray,
        8 => Color::DarkGray,
        9 => Color::LightRed,
        10 => Color::LightGreen,
        11 => Color::LightYellow,
        12 => Color::LightBlue,
        13 => Color::LightMagenta,
        14 => Color::LightCyan,
        15 => Color::White,
        n => Color::Indexed(n),
    }
}

/// Extended colour from `38;5;n`, `38;2;r;g;b` or colon forms `38:2:r:g:b` / `38:2::r:g:b`.
fn ext_color(subs: &[u16]) -> Option<Color> {
    match subs.first()? {
        5 => subs.get(1).map(|n| indexed(*n as u8)),
        2 => {
            let rgb: Vec<u16> = subs[1..].to_vec();
            let rgb = if rgb.len() >= 4 { &rgb[rgb.len() - 3..] } else { &rgb[..] };
            (rgb.len() == 3).then(|| Color::Rgb(rgb[0] as u8, rgb[1] as u8, rgb[2] as u8))
        }
        _ => None,
    }
}

fn apply_sgr(params: &str, st: &mut Style) {
    if params.is_empty() {
        *st = Style::default();
        return;
    }
    let parts: Vec<&str> = params.split(';').collect();
    let mut i = 0;
    while i < parts.len() {
        let p = parts[i];
        let sub: Vec<u16> = p.split(':').map(|x| x.parse().unwrap_or(0)).collect();
        let code = sub[0];
        // colon form keeps colour args inside one param; semicolon form spreads them
        let mut ext = |base: u16| -> Option<Color> {
            if sub.len() > 1 {
                ext_color(&sub[1..])
            } else {
                let mode: u16 = parts.get(i + 1)?.parse().ok()?;
                let n = if mode == 5 { 1 } else { 3 };
                let args: Vec<u16> = std::iter::once(mode).chain(parts[i + 2..(i + 2 + n).min(parts.len())].iter().map(|x| x.parse().unwrap_or(0))).collect();
                i += 1 + n;
                let _ = base;
                ext_color(&args)
            }
        };
        match code {
            0 => *st = Style::default(),
            1 => *st = st.add_modifier(Modifier::BOLD),
            2 => *st = st.add_modifier(Modifier::DIM),
            3 => *st = st.add_modifier(Modifier::ITALIC),
            4 => *st = st.add_modifier(Modifier::UNDERLINED),
            7 => *st = st.add_modifier(Modifier::REVERSED),
            9 => *st = st.add_modifier(Modifier::CROSSED_OUT),
            22 => *st = st.remove_modifier(Modifier::BOLD | Modifier::DIM),
            23 => *st = st.remove_modifier(Modifier::ITALIC),
            24 => *st = st.remove_modifier(Modifier::UNDERLINED),
            27 => *st = st.remove_modifier(Modifier::REVERSED),
            29 => *st = st.remove_modifier(Modifier::CROSSED_OUT),
            30..=37 => st.fg = Some(indexed((code - 30) as u8)),
            90..=97 => st.fg = Some(indexed((code - 90 + 8) as u8)),
            40..=47 => st.bg = Some(indexed((code - 40) as u8)),
            100..=107 => st.bg = Some(indexed((code - 100 + 8) as u8)),
            38 => {
                if let Some(c) = ext(38) {
                    st.fg = Some(c);
                }
            }
            48 => {
                if let Some(c) = ext(48) {
                    st.bg = Some(c);
                }
            }
            // default colours: leave unset so the pane's own background/foreground show
            39 => st.fg = None,
            49 => st.bg = None,
            _ => {}
        }
        i += 1;
    }
}

/// Split ANSI text into lines of styled cells.
pub fn parse(s: &str) -> Vec<Vec<Cell>> {
    let mut lines = vec![Vec::new()];
    let mut st = Style::default();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\x1b' => {
                if let Some((params, 'm')) = skip_escape(&mut it) {
                    apply_sgr(&params, &mut st);
                }
            }
            '\n' => lines.push(Vec::new()),
            '\r' => {}
            c => lines.last_mut().unwrap().push((c, st)),
        }
    }
    lines
}

pub fn plain(cells: &[Cell]) -> String {
    cells.iter().map(|c| c.0).collect()
}

pub fn trim_end(cells: &[Cell]) -> &[Cell] {
    let n = cells.iter().rposition(|c| !c.0.is_whitespace()).map_or(0, |i| i + 1);
    &cells[..n]
}

pub fn trim_start(cells: &[Cell]) -> &[Cell] {
    let n = cells.iter().position(|c| !c.0.is_whitespace()).unwrap_or(cells.len());
    &cells[n..]
}

/// Word wrap styled cells with a hanging indent equal to the line's leading spaces.
pub fn wrap(cells: &[Cell], w: usize) -> Vec<Vec<Cell>> {
    let w = w.max(4);
    let cells = trim_end(cells);
    if cells.len() <= w {
        return vec![cells.to_vec()];
    }
    let lead = (cells.len() - trim_start(cells).len()).min(w / 3);
    let indent: Vec<Cell> = vec![(' ', Style::default()); lead];
    let room = w - lead;
    let body = trim_start(cells);
    // words = runs of non-space cells
    let mut words: Vec<&[Cell]> = Vec::new();
    let mut i = 0;
    while i < body.len() {
        while i < body.len() && body[i].0 == ' ' {
            i += 1;
        }
        let s = i;
        while i < body.len() && body[i].0 != ' ' {
            i += 1;
        }
        if i > s {
            words.push(&body[s..i]);
        }
    }
    let mut out: Vec<Vec<Cell>> = Vec::new();
    let mut cur: Vec<Cell> = cells[..lead].to_vec();
    let mut n = 0usize;
    for mut word in words {
        if n > 0 && n + 1 + word.len() > room {
            out.push(std::mem::replace(&mut cur, indent.clone()));
            n = 0;
        }
        while word.len() > room {
            if n > 0 {
                out.push(std::mem::replace(&mut cur, indent.clone()));
                n = 0;
            }
            let mut row = indent.clone();
            row.extend_from_slice(&word[..room]);
            out.push(row);
            word = &word[room..];
        }
        if n > 0 {
            // keep the space's style (matters inside background-coloured runs)
            cur.push((' ', word.first().map(|c| c.1).unwrap_or_default()));
            n += 1;
        }
        cur.extend_from_slice(word);
        n += word.len();
    }
    if n > 0 {
        out.push(cur);
    }
    out
}

/// Group runs of equal style into spans.
pub fn to_line(cells: &[Cell]) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut cur: Option<Style> = None;
    for (ch, st) in cells {
        if cur != Some(*st) {
            if let Some(s) = cur {
                spans.push(Span::styled(std::mem::take(&mut buf), s));
            }
            cur = Some(*st);
        }
        buf.push(*ch);
    }
    if let Some(s) = cur {
        spans.push(Span::styled(buf, s));
    }
    Line::from(spans)
}
