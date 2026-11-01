//! pywal palette (~/.cache/wal/colors.json), reloaded when the file changes.

use std::path::PathBuf;
use std::time::SystemTime;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn mix(self, o: Rgb, t: f32) -> Rgb {
        let f = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round().clamp(0.0, 255.0) as u8;
        Rgb(f(self.0, o.0), f(self.1, o.1), f(self.2, o.2))
    }
    pub fn lum(self) -> f32 {
        (0.2126 * self.0 as f32 + 0.7152 * self.1 as f32 + 0.0722 * self.2 as f32) / 255.0
    }
    fn hex(s: &str) -> Option<Rgb> {
        let s = s.trim_start_matches('#');
        if s.len() < 6 {
            return None;
        }
        let p = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).ok();
        Some(Rgb(p(0)?, p(2)?, p(4)?))
    }
}

pub struct Theme {
    pub bg: Rgb,
    pub fg: Rgb,
    pub card: Rgb,
    pub card_alt: Rgb,
    pub dim: Rgb,
    pub faint: Rgb,
    pub shadow: Rgb,
    /// six accents (pywal color1..color6), contrast-adjusted against the cards
    pub accent: [Rgb; 6],
    path: PathBuf,
    mtime: Option<SystemTime>,
}

const FALLBACK: [&str; 8] = ["#16161e", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff", "#c0caf5"];

impl Theme {
    pub fn load() -> Theme {
        let path = PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cache/wal/colors.json");
        let mut t = Theme {
            bg: Rgb(0, 0, 0),
            fg: Rgb(255, 255, 255),
            card: Rgb(0, 0, 0),
            card_alt: Rgb(0, 0, 0),
            dim: Rgb(0, 0, 0),
            faint: Rgb(0, 0, 0),
            shadow: Rgb(0, 0, 0),
            accent: [Rgb(0, 0, 0); 6],
            path,
            mtime: None,
        };
        t.read();
        t
    }

    /// Re-read the palette if pywal rewrote it. Returns true when it changed.
    pub fn refresh(&mut self) -> bool {
        let m = std::fs::metadata(&self.path).and_then(|m| m.modified()).ok();
        if m != self.mtime {
            self.read();
            return true;
        }
        false
    }

    fn read(&mut self) {
        self.mtime = std::fs::metadata(&self.path).and_then(|m| m.modified()).ok();
        let json: serde_json::Value = std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(serde_json::Value::Null);
        let get = |sect: &str, key: &str, dflt: &str| {
            json.get(sect)
                .and_then(|s| s.get(key))
                .and_then(|v| v.as_str())
                .and_then(Rgb::hex)
                .unwrap_or_else(|| Rgb::hex(dflt).unwrap())
        };
        self.bg = get("special", "background", FALLBACK[0]);
        self.fg = get("special", "foreground", FALLBACK[7]);
        let dark = self.bg.lum() < 0.5;
        // cards float slightly above the background
        self.card = self.bg.mix(self.fg, if dark { 0.075 } else { 0.06 });
        self.card_alt = self.bg.mix(self.fg, if dark { 0.11 } else { 0.09 });
        self.dim = self.card.mix(self.fg, 0.55);
        self.faint = self.card.mix(self.fg, 0.16);
        self.shadow = if dark { self.bg.mix(Rgb(0, 0, 0), 0.6) } else { self.bg.mix(Rgb(0, 0, 0), 0.2) };
        for i in 0..6 {
            let c = get("colors", &format!("color{}", i + 1), FALLBACK[i + 1]);
            self.accent[i] = self.pop(c);
        }
    }

    /// Push a color away from the card background until it reads clearly.
    fn pop(&self, c: Rgb) -> Rgb {
        let mut c = c;
        for _ in 0..6 {
            if (c.lum() - self.card.lum()).abs() >= 0.28 {
                break;
            }
            c = c.mix(self.fg, 0.3);
        }
        c
    }

    pub fn a(&self, i: usize) -> Rgb {
        self.accent[i % 6]
    }

    /// low -> high load gradient through three accents
    pub fn heat(&self, t: f32) -> Rgb {
        let t = t.clamp(0.0, 1.0);
        let (a, b, c) = (self.a(1), self.a(2), self.a(0));
        if t < 0.5 {
            a.mix(b, t * 2.0)
        } else {
            b.mix(c, (t - 0.5) * 2.0)
        }
    }
}
