#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child as OsChild, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use anyhow::{Context as AnyhowCtx, Result};
use egui::Context;
use chrono::{DateTime, Local};
use crossbeam_channel::{unbounded, Receiver, Sender};
use eframe::egui::{self, *};
use lazy_static::lazy_static;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use regex::Regex;
use serde::{Deserialize, Serialize};
use unicode_width::UnicodeWidthChar;
use vte::{Params, Parser, Perform};

const AI_RAM_LIMIT_BYTES: u64 = 1_610_612_736;
const APP_VERSION: &str = "BETA-0.1";

fn is_hyprland() -> bool {
    std::env::var("HYPRLAND_INSTANCE_SIGNATURE").is_ok()
}

fn find_icon_fonts() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let nerd_candidates = [
        "/usr/share/fonts/TTF/JetBrainsMono Nerd Font Mono Regular.ttf",
        "/usr/share/fonts/JetBrainsMono/JetBrainsMonoNerdFontMono-Regular.ttf",
        "/usr/share/fonts/nerd-fonts/JetBrainsMonoNerdFontMono-Regular.ttf",
        "/usr/share/fonts/truetype/JetBrainsMono-NF/JetBrainsMonoNerdFontMono-Regular.ttf",
        "/usr/share/fonts/OTF/BlexMonoNerdFont-Regular.otf",
    ];
    let symbol_candidates = [
        "/usr/share/fonts/TTF/SymbolsNerdFont-Regular.ttf",
        "/usr/share/fonts/nerd-fonts/SymbolsNerdFont-Regular.ttf",
    ];
    let emoji_candidates = [
        "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf",
        "/usr/share/fonts/truetype/noto/NotoEmoji-Regular.ttf",
    ];
    let fallback_candidates = [
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    ];

    for candidates in [nerd_candidates.as_slice(), symbol_candidates.as_slice(), emoji_candidates.as_slice(), fallback_candidates.as_slice()] {
        if let Some(found) = candidates.iter().map(PathBuf::from).find(|p| p.exists()) {
            out.push(found);
        }
    }
    out
}

fn image_from_path(path: &PathBuf) -> Option<ColorImage> {
    let mut img = image::open(path).ok()?;
    let max_side = 2048u32;
    if img.width() > max_side || img.height() > max_side {
        img = img.thumbnail(max_side, max_side);
    }
    let img = img.to_rgba8();
    let (w, h) = img.dimensions();
    Some(ColorImage::from_rgba_unmultiplied([w as usize, h as usize], img.as_raw()))
}

fn video_poster_path(path: &PathBuf) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    let id = hasher.finish();
    PathBuf::from(format!("/tmp/spiltixal_video_poster_{id}.png"))
}

fn extract_video_poster(path: &PathBuf) -> Option<ColorImage> {
    let out = video_poster_path(path);
    if !out.exists() {
        let status = Command::new("ffmpeg")
            .arg("-y")
            .arg("-i").arg(path)
            .arg("-frames:v").arg("1")
            .arg("-f").arg("image2")
            .arg(&out)
            .status()
            .ok()?;
        if !status.success() { return None; }
    }
    image_from_path(&out)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GradientStop {
    pub position: f32,
    pub color: [u8; 4],
}
impl GradientStop {
    pub fn to_color32(&self) -> Color32 {
        Color32::from_rgba_unmultiplied(self.color[0], self.color[1], self.color[2], self.color[3])
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Background {
    Solid([u8; 4]),
    Gradient { stops: Vec<GradientStop>, angle: f32 },
    Image { path: PathBuf, opacity: f32 },
    Video { path: PathBuf, opacity: f32 },
}
impl Default for Background {
    fn default() -> Self { Background::Solid([13, 13, 20, 255]) }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Theme {
    pub background:      Background,
    pub foreground:      [u8; 4],
    pub cursor_color:    [u8; 4],
    pub selection_color: [u8; 4],
    pub font_size:       f32,
    pub font_family:     String,
    pub black:           [u8; 4],
    pub red:             [u8; 4],
    pub green:           [u8; 4],
    pub yellow:          [u8; 4],
    pub blue:            [u8; 4],
    pub magenta:         [u8; 4],
    pub cyan:            [u8; 4],
    pub white:           [u8; 4],
    pub bright_black:    [u8; 4],
    pub bright_red:      [u8; 4],
    pub bright_green:    [u8; 4],
    pub bright_yellow:   [u8; 4],
    pub bright_blue:     [u8; 4],
    pub bright_magenta:  [u8; 4],
    pub bright_cyan:     [u8; 4],
    pub bright_white:    [u8; 4],
}
impl Default for Theme {
    fn default() -> Self {
        Self {
            background:      Background::Solid([13, 13, 20, 255]),
            foreground:      [220, 220, 230, 255],
            cursor_color:    [120, 200, 255, 255],
            selection_color: [80, 120, 180, 100],
            font_size:       14.0,
            font_family:     "monospace".into(),
            black:           [30,  30,  46,  255],
            red:             [243, 139, 168, 255],
            green:           [166, 227, 161, 255],
            yellow:          [249, 226, 175, 255],
            blue:            [137, 180, 250, 255],
            magenta:         [203, 166, 247, 255],
            cyan:            [137, 220, 235, 255],
            white:           [205, 214, 244, 255],
            bright_black:    [88,  91,  112, 255],
            bright_red:      [243, 139, 168, 255],
            bright_green:    [166, 227, 161, 255],
            bright_yellow:   [249, 226, 175, 255],
            bright_blue:     [137, 180, 250, 255],
            bright_magenta:  [203, 166, 247, 255],
            bright_cyan:     [137, 220, 235, 255],
            bright_white:    [255, 255, 255, 255],
        }
    }
}
impl Theme {
    pub fn fg(&self) -> Color32 {
        let c = self.foreground;
        Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3])
    }
    pub fn bg(&self) -> Color32 {
        match &self.background {
            Background::Solid(c) => Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]),
            Background::Gradient { stops, .. } => {
                stops.first().map(|s| s.to_color32()).unwrap_or(Color32::BLACK)
            }
            _ => Color32::from_rgba_unmultiplied(13, 13, 20, 200),
        }
    }
    pub fn bg_alpha(&self, alpha: u8) -> Color32 {
        let b = self.bg();
        Color32::from_rgba_unmultiplied(b.r(), b.g(), b.b(), alpha)
    }
    pub fn ansi_color(&self, idx: u8, bright: bool) -> Color32 {
        let c = match (idx, bright) {
            (0, false) => self.black,        (1, false) => self.red,
            (2, false) => self.green,        (3, false) => self.yellow,
            (4, false) => self.blue,         (5, false) => self.magenta,
            (6, false) => self.cyan,         (7, false) => self.white,
            (0, true)  => self.bright_black, (1, true)  => self.bright_red,
            (2, true)  => self.bright_green, (3, true)  => self.bright_yellow,
            (4, true)  => self.bright_blue,  (5, true)  => self.bright_magenta,
            (6, true)  => self.bright_cyan,  (7, true)  => self.bright_white,
            _          => self.white,
        };
        Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3])
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub theme:                Theme,
    pub shell:                String,
    pub ai_enabled:           bool,
    pub ai_endpoint:          String,
    pub ai_model:             String,
    pub ai_system_prompt:     String,
    pub mate_name:            String,
    pub scrollback_lines:     usize,
    pub opacity:              f32,
    pub custom_mate_happy:    Option<PathBuf>,
    pub custom_mate_neutral:  Option<PathBuf>,
    pub custom_mate_thinking: Option<PathBuf>,
    #[serde(default)]
    pub theme_preset:         String,
    #[serde(default)]
    pub install_prompt_done:  bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            theme:            Theme::default(),
            shell:            std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into()),
            ai_enabled:       false,
            ai_endpoint:      "http://localhost:11434/api/generate".into(),
            ai_model:         "qwen2.5:0.5b".into(),
            ai_system_prompt: "You are Bob inside a terminal app called Spiltixal. \
                               You can see what's on the terminal screen when the user asks something. \
                               You are attached to the live PTY terminal and allowed to run commands through user-approved actions. \
                               Supported direct actions are /run <command>, /ctrl c, /ctrl z, /ctrl \\\\, and /signal <INT|TSTP|QUIT>. \
                               You can analyze files, code, images and videos when given their paths or content. \
                               Keep responses short, direct, and practical. Plain text only. \
                               When analyzing code or files, \
                               be specific about what you see. When you notice terminal errors, address them directly.".into(),
            mate_name:        "Bob".into(),
            scrollback_lines: 5000,
            opacity:          if is_hyprland() { 0.70 } else { 0.97 },
            custom_mate_happy:    None,
            custom_mate_neutral:  None,
            custom_mate_thinking: None,
            theme_preset:         "Default".into(),
            install_prompt_done:  false,
        }
    }
}
impl Config {
    pub fn load() -> Self {
        let path = Self::path();
        if path.exists() {
            if let Ok(data) = std::fs::read_to_string(&path) {
                if let Ok(mut c) = serde_json::from_str::<Config>(&data) {
                    if c.theme_preset == "Cosmic Purple" {
                        c.theme_preset = "1".into();
                        c.save();
                    }
                    return c;
                }
            }
        }
        Self::default()
    }
    pub fn save(&self) {
        if let Some(dir) = Self::path().parent() { let _ = std::fs::create_dir_all(dir); }
        if let Ok(json) = serde_json::to_string_pretty(self) { let _ = std::fs::write(Self::path(), json); }
    }
    fn path() -> PathBuf {
        dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("spiltixal").join("config.json")
    }
}

struct DangerRule { pattern: Regex, reason: &'static str }

lazy_static! {
    static ref DANGER_RULES: Vec<DangerRule> = vec![
        DangerRule { pattern: Regex::new(r"(?i)sudo\s+rm\s+-[a-z]*rf?\s+/").unwrap(),
            reason: "Recursively removes files from the root filesystem with elevated privileges." },
        DangerRule { pattern: Regex::new(r"(?i)rm\s+-[a-z]*rf?\s+/\*?").unwrap(),
            reason: "Recursively removes files starting from the root directory or its contents." },
        DangerRule { pattern: Regex::new(r"(?i)dd\s+.*if=/dev/zero\s+.*of=/dev/").unwrap(),
            reason: "Overwrites a block device with zeros — destroys all data on the drive." },
        DangerRule { pattern: Regex::new(r"(?i)dd\s+.*of=/dev/(sd|nvme|hd)[a-z]").unwrap(),
            reason: "Writes directly to a block device, which can destroy data irreversibly." },
        DangerRule { pattern: Regex::new(r"(?i)mv\s+/\s+/dev/null").unwrap(),
            reason: "Moves the entire root filesystem into /dev/null, destroying all data." },
        DangerRule { pattern: Regex::new(r":\(\)\{:\|:&\};:").unwrap(),
            reason: "Fork bomb — creates processes exponentially until the system crashes." },
        DangerRule { pattern: Regex::new(r"echo\s+[bBsSuU]\s*>\s*/proc/sysrq-trigger").unwrap(),
            reason: "Triggers a kernel SysRq event (reboot/poweroff/crash) immediately." },
        DangerRule { pattern: Regex::new(r"(?i)sudo\s+rm\s+-[a-z]*rf?\s+/etc/fstab").unwrap(),
            reason: "Deletes the filesystem table — the system will not boot properly." },
        DangerRule { pattern: Regex::new(r"(?i)chmod\s+-R\s+777\s+/").unwrap(),
            reason: "Grants full permissions to every file on the system — massive security hole." },
        DangerRule { pattern: Regex::new(r"(?i)mkfs\.(ext[234]|btrfs|xfs|vfat)\s+/dev/(sd|nvme|hd)[a-z]").unwrap(),
            reason: "Formats a block device, erasing all data on it." },
        DangerRule { pattern: Regex::new(r"(?i)(sudo\s+)?shred\s+-[a-z]*z?\s+/dev/(sd|nvme|hd)[a-z]").unwrap(),
            reason: "Securely erases a block device — all data is unrecoverable." },
        DangerRule { pattern: Regex::new(r"(?i)>\s*/dev/(sd|nvme|hd)[a-z]").unwrap(),
            reason: "Redirects output directly to a block device, overwriting its contents." },
    ];
}

fn check_dangerous(command: &str) -> Option<&'static str> {
    for rule in DANGER_RULES.iter() {
        if rule.pattern.is_match(command.trim()) { return Some(rule.reason); }
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TermColor { Default, Ansi(u8), Ansi256(u8), Rgb(u8, u8, u8) }
impl TermColor {
    pub fn resolve(&self, is_fg: bool, theme: &Theme) -> Color32 {
        match self {
            TermColor::Default      => if is_fg { theme.fg() } else { theme.bg() },
            TermColor::Ansi(idx)    => {
                let (base, bright) = if *idx < 8 { (*idx, false) } else { (idx - 8, true) };
                theme.ansi_color(base, bright)
            }
            TermColor::Ansi256(idx) => ansi256_to_color32(*idx),
            TermColor::Rgb(r, g, b) => Color32::from_rgb(*r, *g, *b),
        }
    }
}

fn ansi256_to_color32(idx: u8) -> Color32 {
    match idx {
        0 => Color32::from_rgb(0x00, 0x00, 0x00),
        1 => Color32::from_rgb(0xcd, 0x00, 0x00),
        2 => Color32::from_rgb(0x00, 0xcd, 0x00),
        3 => Color32::from_rgb(0xcd, 0xcd, 0x00),
        4 => Color32::from_rgb(0x00, 0x00, 0xee),
        5 => Color32::from_rgb(0xcd, 0x00, 0xcd),
        6 => Color32::from_rgb(0x00, 0xcd, 0xcd),
        7 => Color32::from_rgb(0xe5, 0xe5, 0xe5),
        8 => Color32::from_rgb(0x7f, 0x7f, 0x7f),
        9 => Color32::from_rgb(0xff, 0x00, 0x00),
        10 => Color32::from_rgb(0x00, 0xff, 0x00),
        11 => Color32::from_rgb(0xff, 0xff, 0x00),
        12 => Color32::from_rgb(0x5c, 0x5c, 0xff),
        13 => Color32::from_rgb(0xff, 0x00, 0xff),
        14 => Color32::from_rgb(0x00, 0xff, 0xff),
        15 => Color32::from_rgb(0xff, 0xff, 0xff),
        16..=231 => { let v = idx - 16; Color32::from_rgb((v / 36) * 51, ((v / 6) % 6) * 51, (v % 6) * 51) }
        232..=255 => { let g = (idx - 232) * 10 + 8; Color32::from_rgb(g, g, g) }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Attrs {
    pub bold: bool, pub dim: bool, pub italic: bool, pub underline: bool,
    pub blink: bool, pub reverse: bool, pub invisible: bool, pub strikeout: bool,
}

#[derive(Clone, Debug)]
pub struct Cell {
    pub ch: char, pub fg: TermColor, pub bg: TermColor, pub attrs: Attrs, pub width: u8,
}
impl Default for Cell {
    fn default() -> Self {
        Self { ch: ' ', fg: TermColor::Default, bg: TermColor::Default, attrs: Attrs::default(), width: 1 }
    }
}

pub struct Grid {
    pub rows: usize, pub cols: usize,
    pub cells: Vec<Vec<Cell>>,
    pub cursor_x: usize, pub cursor_y: usize,
    pub scroll_top: usize, pub scroll_bot: usize,
    pub scrollback: Vec<Vec<Cell>>,
    pub max_scrollback: usize,
    pub scroll_offset: usize,
}
impl Grid {
    pub fn new(rows: usize, cols: usize, max_scrollback: usize) -> Self {
        Self {
            rows, cols, cells: vec![vec![Cell::default(); cols]; rows],
            cursor_x: 0, cursor_y: 0, scroll_top: 0, scroll_bot: rows.saturating_sub(1),
            scrollback: Vec::new(), max_scrollback, scroll_offset: 0,
        }
    }
    pub fn resize(&mut self, new_rows: usize, new_cols: usize) {
        for row in &mut self.cells { row.resize(new_cols, Cell::default()); }
        if new_rows > self.rows {
            for _ in 0..(new_rows - self.rows) { self.cells.push(vec![Cell::default(); new_cols]); }
        } else { self.cells.truncate(new_rows); }
        self.rows = new_rows; self.cols = new_cols;
        self.scroll_bot = new_rows.saturating_sub(1);
        self.cursor_x = self.cursor_x.min(new_cols.saturating_sub(1));
        self.cursor_y = self.cursor_y.min(new_rows.saturating_sub(1));
    }
    pub fn put_char(&mut self, ch: char, fg: TermColor, bg: TermColor, attrs: Attrs) {
        if self.cursor_y >= self.rows { return; }
        if self.cursor_x >= self.cols { self.cursor_x = 0; self.newline(); }
        let width = UnicodeWidthChar::width(ch).unwrap_or(1).clamp(1, 2) as u8;
        if width == 2 && self.cursor_x + 1 >= self.cols {
            self.cursor_x = 0;
            self.newline();
            if self.cursor_y >= self.rows { return; }
        }
        self.cells[self.cursor_y][self.cursor_x] = Cell { ch, fg, bg, attrs, width };
        if width == 2 {
            let next = self.cursor_x + 1;
            if next < self.cols {
                self.cells[self.cursor_y][next] = Cell { ch: ' ', fg, bg, attrs, width: 0 };
            }
            self.cursor_x += 2;
        } else {
            self.cursor_x += 1;
        }
    }
    pub fn newline(&mut self) {
        if self.cursor_y >= self.scroll_bot { self.scroll_up(1); } else { self.cursor_y += 1; }
    }
    pub fn scroll_up(&mut self, n: usize) {
        for _ in 0..n {
            if !self.cells.is_empty() {
                let evicted = self.cells.remove(self.scroll_top);
                self.scrollback.push(evicted);
                if self.scrollback.len() > self.max_scrollback { self.scrollback.remove(0); }
                self.cells.insert(self.scroll_bot, vec![Cell::default(); self.cols]);
            }
        }
    }
    pub fn scroll_down(&mut self, n: usize) {
        for _ in 0..n {
            if self.cells.len() > self.scroll_bot { self.cells.remove(self.scroll_bot); }
            self.cells.insert(self.scroll_top, vec![Cell::default(); self.cols]);
        }
    }
    pub fn erase_line(&mut self, mode: u8) {
        if self.cursor_y >= self.rows { return; }
        let cx = self.cursor_x;
        let row = &mut self.cells[self.cursor_y];
        match mode {
            0 => { for c in row.iter_mut().skip(cx)     { *c = Cell::default(); } }
            1 => { for c in row.iter_mut().take(cx + 1) { *c = Cell::default(); } }
            2 => { for c in row.iter_mut()              { *c = Cell::default(); } }
            _ => {}
        }
    }
    pub fn erase_display(&mut self, mode: u8) {
        match mode {
            0 => {
                self.erase_line(0);
                for y in (self.cursor_y + 1)..self.rows { for c in &mut self.cells[y] { *c = Cell::default(); } }
            }
            1 => {
                for y in 0..self.cursor_y { for c in &mut self.cells[y] { *c = Cell::default(); } }
                self.erase_line(1);
            }
            2 | 3 => {
                for row in &mut self.cells { for c in row.iter_mut() { *c = Cell::default(); } }
                self.cursor_x = 0; self.cursor_y = 0;
            }
            _ => {}
        }
    }
    pub fn visible_row(&self, y: usize) -> Option<&Vec<Cell>> {
        let total = self.scrollback.len() + self.rows;
        let view_start = total.saturating_sub(self.rows + self.scroll_offset);
        let idx = view_start + y;
        if idx < self.scrollback.len() { self.scrollback.get(idx) }
        else { self.cells.get(idx - self.scrollback.len()) }
    }
}

struct Performer<'a> {
    grid: &'a mut Grid,
    current_fg: TermColor, current_bg: TermColor, current_attrs: Attrs,
    title: &'a mut String,
}
impl<'a> Perform for Performer<'a> {
    fn print(&mut self, ch: char) {
        self.grid.put_char(ch, self.current_fg, self.current_bg, self.current_attrs);
    }
    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' | 0x0B | 0x0C => self.grid.newline(),
            b'\r' => self.grid.cursor_x = 0,
            b'\t' => { self.grid.cursor_x = ((self.grid.cursor_x / 8 + 1) * 8).min(self.grid.cols - 1); }
            0x08  => { if self.grid.cursor_x > 0 { self.grid.cursor_x -= 1; } }
            _     => {}
        }
    }
    fn csi_dispatch(&mut self, params: &Params, _ints: &[u8], _ignore: bool, action: char) {
        let ps: Vec<u16> = params.iter().map(|p| p[0]).collect();
        let p0 = ps.first().copied().unwrap_or(0);
        let pn = |i: usize| -> usize { ps.get(i).copied().unwrap_or(1).max(1) as usize };
        let p1 = || -> usize { ps.first().copied().unwrap_or(1).max(1) as usize };
        match action {
            'A' => { self.grid.cursor_y = self.grid.cursor_y.saturating_sub(p1()); }
            'B' => { self.grid.cursor_y = (self.grid.cursor_y + p1()).min(self.grid.rows - 1); }
            'C' => { self.grid.cursor_x = (self.grid.cursor_x + p1()).min(self.grid.cols - 1); }
            'D' => { self.grid.cursor_x = self.grid.cursor_x.saturating_sub(p1()); }
            'H' | 'f' => {
                self.grid.cursor_y = (pn(0).saturating_sub(1)).min(self.grid.rows - 1);
                self.grid.cursor_x = (pn(1).saturating_sub(1)).min(self.grid.cols - 1);
            }
            'J' => self.grid.erase_display(p0 as u8),
            'K' => self.grid.erase_line(p0 as u8),
            'S' | 'L' => self.grid.scroll_up(p1()),
            'T' | 'M' => self.grid.scroll_down(p1()),
            'm' => self.handle_sgr(&ps),
            'r' => {
                self.grid.scroll_top = pn(0).saturating_sub(1);
                self.grid.scroll_bot = (pn(1).saturating_sub(1)).min(self.grid.rows - 1);
            }
            'd' => { self.grid.cursor_y = (p0 as usize).saturating_sub(1).min(self.grid.rows - 1); }
            'G' => { self.grid.cursor_x = (p0 as usize).saturating_sub(1).min(self.grid.cols - 1); }
            'P' => {
                let n = p1(); let y = self.grid.cursor_y; let x = self.grid.cursor_x; let cols = self.grid.cols;
                if y < self.grid.rows {
                    let row = &mut self.grid.cells[y];
                    for i in x..cols { if i + n < cols { row[i] = row[i + n].clone(); } else { row[i] = Cell::default(); } }
                }
            }
            '@' => {
                let n = p1(); let y = self.grid.cursor_y; let x = self.grid.cursor_x; let cols = self.grid.cols;
                if y < self.grid.rows {
                    let row = &mut self.grid.cells[y];
                    for i in (x..cols).rev() { if i >= x + n { row[i] = row[i - n].clone(); } else { row[i] = Cell::default(); } }
                }
            }
            _ => {}
        }
    }
    fn osc_dispatch(&mut self, params: &[&[u8]], _bell: bool) {
        if params.len() >= 2 && (params[0] == b"0" || params[0] == b"2") {
            if let Ok(t) = std::str::from_utf8(params[1]) { *self.title = t.to_string(); }
        }
    }
    fn esc_dispatch(&mut self, _ints: &[u8], _ignore: bool, byte: u8) {
        if byte == b'M' {
            if self.grid.cursor_y <= self.grid.scroll_top { self.grid.scroll_down(1); }
            else { self.grid.cursor_y = self.grid.cursor_y.saturating_sub(1); }
        }
    }
    fn hook(&mut self, _: &Params, _: &[u8], _: bool, _: char) {}
    fn put(&mut self, _: u8) {}
    fn unhook(&mut self) {}
}
impl<'a> Performer<'a> {
    fn handle_sgr(&mut self, ps: &[u16]) {
        let mut i = 0;
        if ps.is_empty() { self.reset_attrs(); return; }
        while i < ps.len() {
            match ps[i] {
                0  => self.reset_attrs(),
                1  => self.current_attrs.bold      = true,
                2  => self.current_attrs.dim       = true,
                3  => self.current_attrs.italic    = true,
                4  => self.current_attrs.underline = true,
                5  => self.current_attrs.blink     = true,
                7  => self.current_attrs.reverse   = true,
                8  => self.current_attrs.invisible = true,
                9  => self.current_attrs.strikeout = true,
                22 => { self.current_attrs.bold = false; self.current_attrs.dim = false; }
                23 => self.current_attrs.italic    = false,
                24 => self.current_attrs.underline = false,
                25 => self.current_attrs.blink     = false,
                27 => self.current_attrs.reverse   = false,
                28 => self.current_attrs.invisible = false,
                29 => self.current_attrs.strikeout = false,
                30..=37   => self.current_fg = TermColor::Ansi((ps[i] - 30) as u8),
                38        => { if let Some(c) = self.parse_ext(ps, &mut i) { self.current_fg = c; } }
                39        => self.current_fg = TermColor::Default,
                40..=47   => self.current_bg = TermColor::Ansi((ps[i] - 40) as u8),
                48        => { if let Some(c) = self.parse_ext(ps, &mut i) { self.current_bg = c; } }
                49        => self.current_bg = TermColor::Default,
                90..=97   => self.current_fg = TermColor::Ansi((ps[i] - 90 + 8) as u8),
                100..=107 => self.current_bg = TermColor::Ansi((ps[i] - 100 + 8) as u8),
                _ => {}
            }
            i += 1;
        }
    }
    fn parse_ext(&self, ps: &[u16], i: &mut usize) -> Option<TermColor> {
        match ps.get(*i + 1).copied() {
            Some(2) => {
                let r = ps.get(*i + 2).copied()? as u8;
                let g = ps.get(*i + 3).copied()? as u8;
                let b = ps.get(*i + 4).copied()? as u8;
                *i += 4; Some(TermColor::Rgb(r, g, b))
            }
            Some(5) => { let idx = ps.get(*i + 2).copied()? as u8; *i += 2; Some(TermColor::Ansi256(idx)) }
            _ => None,
        }
    }
    fn reset_attrs(&mut self) {
        self.current_fg    = TermColor::Default;
        self.current_bg    = TermColor::Default;
        self.current_attrs = Attrs::default();
    }
}

pub struct TerminalState {
    pub grid: Grid, pub title: String,
    parser: Parser,
    current_fg: TermColor, current_bg: TermColor, current_attrs: Attrs,
}
impl TerminalState {
    pub fn new(rows: usize, cols: usize, max_scrollback: usize) -> Self {
        Self {
            grid: Grid::new(rows, cols, max_scrollback), title: "Spiltixal".into(),
            parser: Parser::new(), current_fg: TermColor::Default,
            current_bg: TermColor::Default, current_attrs: Attrs::default(),
        }
    }
    pub fn process_bytes(&mut self, bytes: &[u8]) {
        let mut perf = Performer {
            grid: &mut self.grid, current_fg: self.current_fg,
            current_bg: self.current_bg, current_attrs: self.current_attrs,
            title: &mut self.title,
        };
        for &byte in bytes { self.parser.advance(&mut perf, byte); }
        self.current_fg    = perf.current_fg;
        self.current_bg    = perf.current_bg;
        self.current_attrs = perf.current_attrs;
    }
    pub fn resize(&mut self, rows: usize, cols: usize) { self.grid.resize(rows, cols); }
}

pub struct PtyHandle {
    pub master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    pub writer: Arc<Mutex<Box<dyn Write + Send>>>,
    pub child:  Box<dyn Child + Send + Sync>,
    pub rx:     Receiver<Vec<u8>>,
}
impl PtyHandle {
    pub fn spawn(shell: &str, rows: u16, cols: u16) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .context("Failed to open PTY")?;
        let master = pair.master;
        let mut cmd = CommandBuilder::new(shell);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("SPILTIXAL", "1");
        let child  = pair.slave.spawn_command(cmd).context("Failed to spawn shell")?;
        let writer = Arc::new(Mutex::new(master.take_writer().context("PTY writer")?));
        let mut reader = master.try_clone_reader().context("PTY reader")?;
        let master = Arc::new(Mutex::new(master));
        let (tx, rx) = crossbeam_channel::bounded(256);
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => { if tx.send(buf[..n].to_vec()).is_err() { break; } }
                }
            }
        });
        Ok(Self { master, writer, child, rx })
    }
    pub fn write_str(&self, s: &str) -> Result<()> {
        self.writer.lock().map_err(|_| anyhow::anyhow!("lock"))?.write_all(s.as_bytes()).context("write")
    }
    pub fn resize(&self, rows: u16, cols: u16) -> Result<()> {
        self.master.lock().map_err(|_| anyhow::anyhow!("lock"))?
            .resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .context("resize")
    }
    pub fn signal_foreground(&self, signal_name: &str) -> Result<()> {
        #[cfg(unix)]
        {
            let pgrp = self.master.lock().map_err(|_| anyhow::anyhow!("lock"))?
                .process_group_leader();
            if let Some(pgrp) = pgrp {
                let group = format!("-{}", pgrp);
                let status = Command::new("kill")
                    .arg("-s")
                    .arg(signal_name)
                    .arg(group)
                    .status()
                    .context("kill")?;
                if !status.success() {
                    anyhow::bail!("kill command failed");
                }
            }
        }
        Ok(())
    }
    pub fn is_alive(&mut self) -> bool { matches!(self.child.try_wait(), Ok(None)) }
}

#[derive(Debug, Default)]
pub struct SearchState {
    pub query: String, pub matches: Vec<SearchMatch>,
    pub current_idx: usize, pub active: bool,
}
#[derive(Debug, Clone)]
pub struct SearchMatch { pub row: usize, pub col: usize, pub len: usize }

impl SearchState {
    pub fn search(&mut self, scrollback: &[Vec<Cell>], grid: &[Vec<Cell>]) {
        self.matches.clear(); self.current_idx = 0;
        if self.query.is_empty() { return; }
        let q = self.query.to_lowercase();
        for (r, row) in scrollback.iter().chain(grid.iter()).enumerate() {
            let line: String = row.iter().map(|c| c.ch).collect();
            let lower = line.to_lowercase();
            let mut start = 0;
            while let Some(pos) = lower[start..].find(&q) {
                let abs = start + pos;
                self.matches.push(SearchMatch { row: r, col: abs, len: q.len() });
                start = abs + 1;
            }
        }
    }
    pub fn next(&mut self) {
        if !self.matches.is_empty() { self.current_idx = (self.current_idx + 1) % self.matches.len(); }
    }
    pub fn prev(&mut self) {
        if self.matches.is_empty() { return; }
        if self.current_idx == 0 { self.current_idx = self.matches.len() - 1; } else { self.current_idx -= 1; }
    }
    pub fn current_match(&self) -> Option<&SearchMatch> { self.matches.get(self.current_idx) }
    pub fn is_match_at(&self, row: usize, col: usize) -> bool {
        self.matches.iter().any(|m| m.row == row && col >= m.col && col < m.col + m.len)
    }
    pub fn is_current_at(&self, row: usize, col: usize) -> bool {
        self.current_match().map_or(false, |m| m.row == row && col >= m.col && col < m.col + m.len)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedCommand {
    pub id: u64, pub label: String, pub command: String,
    pub description: String, pub created_at: DateTime<Local>, pub use_count: u32,
}
impl SavedCommand {
    pub fn new(id: u64, command: impl Into<String>, description: impl Into<String>) -> Self {
        let cmd = command.into();
        let label = cmd.chars().take(40).collect();
        Self { id, label, command: cmd, description: description.into(), created_at: Local::now(), use_count: 0 }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SavedCommandStore { pub commands: Vec<SavedCommand>, next_id: u64 }
impl SavedCommandStore {
    pub fn load() -> Self {
        let p = Self::path();
        if p.exists() {
            if let Ok(data) = std::fs::read_to_string(&p) {
                if let Ok(s) = serde_json::from_str::<SavedCommandStore>(&data) { return s; }
            }
        }
        Self::default()
    }
    pub fn save_to_disk(&self) {
        if let Some(dir) = Self::path().parent() { let _ = std::fs::create_dir_all(dir); }
        if let Ok(json) = serde_json::to_string_pretty(self) { let _ = std::fs::write(Self::path(), json); }
    }
    fn path() -> PathBuf {
        dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".")).join("spiltixal").join("saved_commands.json")
    }
    pub fn add(&mut self, command: impl Into<String>, description: impl Into<String>) -> u64 {
        let id = self.next_id; self.next_id += 1;
        self.commands.push(SavedCommand::new(id, command, description));
        self.save_to_disk(); id
    }
    pub fn remove(&mut self, id: u64) { self.commands.retain(|c| c.id != id); self.save_to_disk(); }
    pub fn increment_use(&mut self, id: u64) {
        if let Some(c) = self.commands.iter_mut().find(|c| c.id == id) { c.use_count += 1; self.save_to_disk(); }
    }
    pub fn search(&self, q: &str) -> Vec<&SavedCommand> {
        if q.is_empty() { return self.commands.iter().collect(); }
        let q = q.to_lowercase();
        self.commands.iter().filter(|c|
            c.command.to_lowercase().contains(&q) || c.description.to_lowercase().contains(&q)
        ).collect()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatMessage { pub role: String, pub content: String }

#[derive(Serialize)]
struct OllamaReq<'a> { model: &'a str, prompt: &'a str, stream: bool }

#[derive(Deserialize)]
struct OllamaResp { response: String }

pub enum AiEvent { Token(String), Done, Error(String) }

#[derive(Clone)]
pub struct AiClient { pub endpoint: String, pub model: String, pub system_prompt: String }
impl AiClient {
    pub fn new(endpoint: impl Into<String>, model: impl Into<String>, system_prompt: impl Into<String>) -> Self {
        Self { endpoint: endpoint.into(), model: model.into(), system_prompt: system_prompt.into() }
    }
    pub fn send_async(&self, history: Vec<ChatMessage>, tx: Sender<AiEvent>) {
        let endpoint = self.endpoint.clone();
        let model    = self.model.clone();
        let sys      = self.system_prompt.clone();
        thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build();
            match rt {
                Err(e) => { let _ = tx.send(AiEvent::Error(e.to_string())); }
                Ok(rt) => rt.block_on(async move {
                    match Self::call(&endpoint, &model, &sys, &history).await {
                        Ok(reply) => { let _ = tx.send(AiEvent::Token(reply)); let _ = tx.send(AiEvent::Done); }
                        Err(e)    => {
                            let msg = if e.to_string().contains("404") {
                                format!("Model not found. Run: ollama pull {}", model)
                            } else if e.to_string().contains("Connection refused") || e.to_string().contains("error sending request") {
                                "Ollama not running. Start it: ollama serve".into()
                            } else {
                                e.to_string()
                            };
                            let _ = tx.send(AiEvent::Error(msg));
                        }
                    }
                }),
            }
        });
    }
    async fn call(endpoint: &str, model: &str, sys: &str, history: &[ChatMessage]) -> Result<String> {
        let client = reqwest::Client::builder().timeout(Duration::from_secs(60)).build()?;
        let prompt = format!("{}\n\n{}",
            sys,
            history.iter().map(|m| format!("{}: {}", m.role, m.content)).collect::<Vec<_>>().join("\n")
        );
        let generate_url = if endpoint.ends_with("/api/chat") {
            endpoint.replace("/api/chat", "/api/generate")
        } else if endpoint.ends_with("/api/generate") {
            endpoint.to_string()
        } else {
            format!("{}/api/generate", endpoint.trim_end_matches('/'))
        };
        let resp = client.post(&generate_url)
            .json(&OllamaReq { model, prompt: &prompt, stream: false })
            .send().await?.error_for_status()?.json::<OllamaResp>().await?;
        Ok(resp.response.trim().to_string())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Emotion { Happy, Neutral, Thinking, Curious, Worried, Excited, Confused }

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MateView { Chat, SavedCommands }

pub struct Mate {
    pub name:           String,
    pub emotion:        Emotion,
    pub chat_history:   Vec<ChatMessage>,
    pub input_text:     String,
    pub save_box_text:  String,
    pub save_desc_text: String,
    pub reply_pending:  bool,
    pub last_message:   String,
    pub view:           MateView,
    pub commands:       SavedCommandStore,
    pub ai_client:      Option<AiClient>,
    pub event_rx:       Option<Receiver<AiEvent>>,
    pub emotion_timer:  Option<Instant>,
    pub customize_mode: bool,
    pub typing_target:  String,
    pub typing_chars:   usize,
    pub typing_tick:    Instant,
    pub attach_path:    String,
}
impl Mate {
    pub fn new(name: String, ai_client: Option<AiClient>) -> Self {
        let greeting = format!("{name} here. I am connected to your terminal. Send a file path or ask me to run a command.");
        Self {
            name, emotion: Emotion::Happy, chat_history: Vec::new(),
            input_text: String::new(), save_box_text: String::new(), save_desc_text: String::new(),
            reply_pending: false, last_message: greeting.clone(), view: MateView::Chat,
            commands: SavedCommandStore::load(), ai_client, event_rx: None,
            emotion_timer: None, customize_mode: false,
            typing_target: greeting, typing_chars: usize::MAX, typing_tick: Instant::now(),
            attach_path: String::new(),
        }
    }

    pub fn emotion_from_text(text: &str) -> Emotion {
        let t = text.to_lowercase();
        if t.contains("error") || t.contains("fail") || t.contains("crash") || t.contains("kill") || t.contains("rm -rf") {
            Emotion::Worried
        } else if t.contains("?") || t.contains("how") || t.contains("what") || t.contains("why") || t.contains("explain") {
            Emotion::Curious
        } else if t.contains("nice") || t.contains("thanks") || t.contains("thank") || t.contains("great") || t.contains("awesome") || t.contains("cool") {
            Emotion::Excited
        } else if t.contains("idk") || t.contains("not sure") || t.contains("confused") || t.contains("hm") || t.contains("hmm") {
            Emotion::Confused
        } else if t.contains("analyze") || t.contains("look at") || t.contains("check") || t.contains("review") || t.contains("read") {
            Emotion::Thinking
        } else {
            Emotion::Neutral
        }
    }

    pub fn tick_typing(&mut self) {
        if self.typing_chars >= self.typing_target.len() { return; }
        if self.typing_tick.elapsed() >= Duration::from_millis(18) {
            let remaining = &self.typing_target[self.typing_chars..];
            let next = remaining.char_indices().nth(1).map(|(i, _)| self.typing_chars + i).unwrap_or(self.typing_target.len());
            self.typing_chars = next;
            self.typing_tick = Instant::now();
        }
    }

    pub fn typed_text(&self) -> &str {
        if self.typing_chars >= self.typing_target.len() {
            &self.typing_target
        } else {
            &self.typing_target[..self.typing_chars]
        }
    }

    pub fn is_typing(&self) -> bool {
        self.typing_chars < self.typing_target.len()
    }
    pub fn poll_ai(&mut self) {
        if self.event_rx.is_none() { return; }
        let mut reply = String::new(); let mut done = false;
        while let Ok(ev) = self.event_rx.as_ref().unwrap().try_recv() {
            match ev {
                AiEvent::Token(t) => reply.push_str(&t),
                AiEvent::Done     => done = true,
                AiEvent::Error(e) => { reply = e; done = true; }
            }
        }
        if !reply.is_empty() {
            self.last_message = reply.clone();
            self.typing_target = reply.clone();
            self.typing_chars = 0;
            self.typing_tick = Instant::now();
            self.chat_history.push(ChatMessage { role: "assistant".into(), content: reply });
        }
        if done { self.reply_pending = false; self.emotion = Emotion::Happy; self.event_rx = None; }
        if let Some(t) = self.emotion_timer {
            if t.elapsed() > Duration::from_secs(30) { self.emotion = Emotion::Neutral; self.emotion_timer = None; }
        }
    }
    pub fn send_message(&mut self, msg: String) {
        if msg.trim().eq_ignore_ascii_case("customize") {
            self.last_message = "Customize mode is open.".into();
            self.typing_target = self.last_message.clone();
            self.typing_chars = 0;
            self.typing_tick = Instant::now();
            self.customize_mode = true; return;
        }
        self.emotion = Self::emotion_from_text(&msg);
        self.chat_history.push(ChatMessage { role: "user".into(), content: msg.clone() });
        let thinking_msg = "Working...".to_string();
        self.last_message = thinking_msg.clone();
        self.typing_target = thinking_msg;
        self.typing_chars = usize::MAX;
        if let Some(client) = &self.ai_client {
            let (tx, rx) = unbounded::<AiEvent>();
            client.send_async(self.chat_history.clone(), tx);
            self.event_rx = Some(rx); self.reply_pending = true;
            self.emotion = Emotion::Thinking; self.emotion_timer = Some(Instant::now());
        } else {
            let offline = "AI is disabled. Toggle AI to enable it.".to_string();
            self.last_message = offline.clone();
            self.typing_target = offline;
            self.typing_chars = 0;
            self.typing_tick = Instant::now();
        }
    }
    pub fn delete_saved(&mut self, id: u64) { self.commands.remove(id); }
    pub fn save_command(&mut self) {
        let cmd  = self.save_box_text.trim().to_string();
        let desc = self.save_desc_text.trim().to_string();
        if !cmd.is_empty() {
            self.commands.add(cmd, desc);
            self.save_box_text.clear(); self.save_desc_text.clear();
            let msg = "saved it.".to_string();
            self.last_message = msg.clone();
            self.typing_target = msg;
            self.typing_chars = 0;
            self.typing_tick = Instant::now();
            self.emotion = Emotion::Happy;
        }
    }
}

#[derive(Default)]
pub struct CustomizeState {
    pub open: bool,
    pub fg_color: [u8; 4], pub bg_solid: [u8; 4],
    pub use_gradient: bool, pub grad_a: [u8; 4], pub grad_b: [u8; 4], pub grad_angle: f32,
    pub font_size: f32, pub bg_opacity: f32,
    pub bg_image: Option<PathBuf>, pub bg_video: Option<PathBuf>,
    pub happy_path: Option<PathBuf>, pub neutral_path: Option<PathBuf>, pub thinking_path: Option<PathBuf>,
    pub bg_image_input: String, pub bg_video_input: String,
    pub happy_input: String, pub neutral_input: String, pub thinking_input: String,
    pub path_error: String,
    pub theme_preset: String,
    pub tool: CustomizeTool,
    pub layer_path_input: String,
    pub layers: Vec<OverlayLayer>,
    pub selected_layer: Option<usize>,
    pub drawing: Vec<DrawStroke>,
    pub active_stroke: Vec<Pos2>,
    pub stroke_width: f32,
    pub drag_layer: Option<usize>,
    pub drag_offset: Vec2,
    pub save_message: String,
    pub reset_confirm_step: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum CustomizeTool {
    #[default]
    AddImage,
    AddVideo,
    Draw,
    TextColor,
    BackgroundColor,
    Theme,
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum OverlayAnimation {
    #[default]
    None,
    Spin,
    Floating,
}

pub struct OverlayLayer {
    pub path: PathBuf,
    pub is_video: bool,
    pub pos: Vec2,
    pub size: Vec2,
    pub rotation_deg: f32,
    pub tint: [u8; 4],
    pub animation: OverlayAnimation,
    pub texture: Option<TextureHandle>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct DrawStroke {
    pub points: Vec<[f32; 2]>,
    pub color: [u8; 4],
    pub width: f32,
}

#[derive(Serialize, Deserialize)]
struct SavedOverlayLayer {
    path: String,
    is_video: bool,
    pos: [f32; 2],
    size: [f32; 2],
    rotation_deg: f32,
    tint: [u8; 4],
    animation: OverlayAnimation,
}

#[derive(Serialize, Deserialize)]
struct SavedCustomizeLayout {
    saved_at: String,
    text_color: [u8; 4],
    background_color: [u8; 4],
    theme_preset: String,
    layers: Vec<SavedOverlayLayer>,
    drawing: Vec<DrawStroke>,
}
impl CustomizeState {
    pub fn from_config(c: &Config) -> Self {
        let initial_bg_image = match &c.theme.background {
            Background::Image { path, .. } => Some(path.clone()),
            _ => None,
        };
        let initial_bg_video = match &c.theme.background {
            Background::Video { path, .. } => Some(path.clone()),
            _ => None,
        };
        let (bg_solid, use_gradient, grad_a, grad_b, grad_angle, bg_image_input, bg_video_input) = match &c.theme.background {
            Background::Solid(col)            => (*col, false, [30u8,30,30,255], [80u8,50,120,255], 135.0, String::new(), String::new()),
            Background::Gradient { stops, angle } => {
                let a = stops.first().map(|s| s.color).unwrap_or([30,30,30,255]);
                let b = stops.last().map(|s| s.color).unwrap_or([80,50,120,255]);
                ([0,0,0,255], true, a, b, *angle, String::new(), String::new())
            }
            Background::Image { path, .. } => (
                [13,13,20,255], false, [30,30,30,255], [80,50,120,255], 135.0,
                path.display().to_string(), String::new()
            ),
            Background::Video { path, .. } => (
                [13,13,20,255], false, [30,30,30,255], [80,50,120,255], 135.0,
                String::new(), path.display().to_string()
            ),
        };
        Self {
            open: true, fg_color: c.theme.foreground, bg_solid, use_gradient, grad_a, grad_b, grad_angle,
            font_size: c.theme.font_size, bg_opacity: c.opacity,
            bg_image: initial_bg_image,
            bg_video: initial_bg_video,
            happy_path: c.custom_mate_happy.clone(), neutral_path: c.custom_mate_neutral.clone(),
            thinking_path: c.custom_mate_thinking.clone(),
            bg_image_input,
            bg_video_input,
            happy_input: c.custom_mate_happy.as_ref().map(|p| p.display().to_string()).unwrap_or_default(),
            neutral_input: c.custom_mate_neutral.as_ref().map(|p| p.display().to_string()).unwrap_or_default(),
            thinking_input: c.custom_mate_thinking.as_ref().map(|p| p.display().to_string()).unwrap_or_default(),
            path_error: String::new(),
            theme_preset: c.theme_preset.clone(),
            tool: CustomizeTool::AddImage,
            layer_path_input: String::new(),
            layers: Vec::new(),
            selected_layer: None,
            drawing: Vec::new(),
            active_stroke: Vec::new(),
            stroke_width: 2.0,
            drag_layer: None,
            drag_offset: Vec2::ZERO,
            save_message: String::new(),
            reset_confirm_step: 0,
            ..Default::default()
        }
    }
    pub fn apply_to(&self, config: &mut Config) {
        config.theme.foreground          = self.fg_color;
        config.theme.font_size           = self.font_size;
        config.opacity                   = self.bg_opacity;
        config.custom_mate_happy         = self.happy_path.clone();
        config.custom_mate_neutral       = self.neutral_path.clone();
        config.custom_mate_thinking      = self.thinking_path.clone();
        config.theme_preset              = self.theme_preset.clone();
        config.theme.background = if let Some(p) = &self.bg_image {
            Background::Image { path: p.clone(), opacity: self.bg_opacity }
        } else if let Some(p) = &self.bg_video {
            Background::Video { path: p.clone(), opacity: self.bg_opacity }
        } else if self.use_gradient {
            Background::Gradient {
                stops: vec![
                    GradientStop { position: 0.0, color: self.grad_a },
                    GradientStop { position: 1.0, color: self.grad_b },
                ],
                angle: self.grad_angle,
            }
        } else {
            Background::Solid(self.bg_solid)
        };
    }
}

fn show_color_picker(ui: &mut Ui, rgba: &mut [u8; 4]) {
    let mut c = Color32::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], rgba[3]);
    if ui.color_edit_button_srgba(&mut c).changed() {
        rgba[0] = c.r(); rgba[1] = c.g(); rgba[2] = c.b(); rgba[3] = c.a();
    }
}

fn path_from_input(input: &str) -> Option<PathBuf> {
    let t = input.trim();
    if t.is_empty() { None } else { Some(PathBuf::from(t)) }
}

fn apply_path_input(slot: &mut Option<PathBuf>, input: &str) -> Result<()> {
    match path_from_input(input) {
        None => { *slot = None; Ok(()) }
        Some(path) => {
            if !path.exists() {
                anyhow::bail!("Path does not exist: {}", path.display());
            }
            *slot = Some(path);
            Ok(())
        }
    }
}

fn show_customize_window(ctx: &Context, state: &mut CustomizeState, config: &mut Config) -> bool {
    let mut submitted = false;
    let mut close     = false;
    egui::Window::new("Customize Spiltixal").collapsible(false).resizable(true).default_size([540.0, 600.0]).show(ctx, |ui| {
        ui.heading("Make it yours");
        ui.separator();

        egui::CollapsingHeader::new("Theme Preset").default_open(true).show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.selectable_label(state.theme_preset == "Default", "Default").clicked() {
                    state.theme_preset = "Default".into();
                    state.use_gradient = false;
                    state.bg_solid = [13, 13, 20, 255];
                    state.fg_color = [220, 220, 230, 255];
                }
                if ui.selectable_label(Spiltixal::is_theme_one_name(&state.theme_preset), "1").clicked() {
                    state.theme_preset = "1".into();
                    state.use_gradient = true;
                    state.grad_a = [18, 12, 34, 255];
                    state.grad_b = [58, 24, 88, 255];
                    state.grad_angle = 130.0;
                    state.fg_color = [236, 224, 255, 255];
                    state.bg_image = None;
                    state.bg_video = None;
                    state.bg_image_input.clear();
                    state.bg_video_input.clear();
                }
            });
        });

        egui::CollapsingHeader::new("Background").default_open(true).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut state.use_gradient, false, "Solid");
                ui.selectable_value(&mut state.use_gradient, true, "Gradient");
            });
            if state.use_gradient {
                ui.horizontal(|ui| { ui.label("Color A:"); show_color_picker(ui, &mut state.grad_a); });
                ui.horizontal(|ui| { ui.label("Color B:"); show_color_picker(ui, &mut state.grad_b); });
                ui.horizontal(|ui| { ui.label("Angle:");   ui.add(egui::Slider::new(&mut state.grad_angle, 0.0..=360.0).suffix("deg")); });
            } else {
                ui.horizontal(|ui| { ui.label("Color:"); show_color_picker(ui, &mut state.bg_solid); });
            }
            ui.add_space(6.0);
            ui.label("Set background by file path:");
            ui.horizontal(|ui| {
                ui.label("Image:");
                ui.add(egui::TextEdit::singleline(&mut state.bg_image_input).desired_width(f32::INFINITY).hint_text("/path/to/image.png"));
                if ui.small_button("Apply").clicked() {
                    match apply_path_input(&mut state.bg_image, &state.bg_image_input) {
                        Ok(()) => { if state.bg_image.is_some() { state.bg_video = None; state.path_error.clear(); } }
                        Err(e) => state.path_error = e.to_string(),
                    }
                }
                if ui.small_button("Clear").clicked() { state.bg_image = None; state.bg_image_input.clear(); state.path_error.clear(); }
            });
            ui.horizontal(|ui| {
                ui.label("Video:");
                ui.add(egui::TextEdit::singleline(&mut state.bg_video_input).desired_width(f32::INFINITY).hint_text("/path/to/video.mp4"));
                if ui.small_button("Apply").clicked() {
                    match apply_path_input(&mut state.bg_video, &state.bg_video_input) {
                        Ok(()) => { if state.bg_video.is_some() { state.bg_image = None; state.path_error.clear(); } }
                        Err(e) => state.path_error = e.to_string(),
                    }
                }
                if ui.small_button("Clear").clicked() { state.bg_video = None; state.bg_video_input.clear(); state.path_error.clear(); }
            });
            if let Some(p) = &state.bg_image { ui.label(format!("Using image: {}", p.display())); }
            if let Some(p) = &state.bg_video { ui.label(format!("Using video: {}", p.display())); }
            ui.horizontal(|ui| { ui.label("Opacity:"); ui.add(egui::Slider::new(&mut state.bg_opacity, 0.2..=1.0)); });
        });

        egui::CollapsingHeader::new("Text & Font").show(ui, |ui| {
            ui.horizontal(|ui| { ui.label("Foreground:"); show_color_picker(ui, &mut state.fg_color); });
            ui.horizontal(|ui| { ui.label("Font size:");  ui.add(egui::Slider::new(&mut state.font_size, 8.0..=32.0).suffix("px")); });
        });

        egui::CollapsingHeader::new("Bob (Mate images)").show(ui, |ui| {
            ui.label("Set a file path for each emotion image:");
            for (label, input, path_opt) in [
                ("1. Happy", &mut state.happy_input, &mut state.happy_path),
                ("2. Neutral", &mut state.neutral_input, &mut state.neutral_path),
                ("3. Thinking", &mut state.thinking_input, &mut state.thinking_path),
            ] {
                ui.horizontal(|ui| {
                    ui.label(label);
                    ui.add(egui::TextEdit::singleline(input).desired_width(260.0).hint_text("/path/to/avatar.png"));
                    if ui.small_button("Apply").clicked() {
                        match apply_path_input(path_opt, input) {
                            Ok(()) => state.path_error.clear(),
                            Err(e) => state.path_error = e.to_string(),
                        }
                    }
                    if ui.small_button("Clear").clicked() { *path_opt = None; input.clear(); state.path_error.clear(); }
                });
            }
        });
        if !state.path_error.is_empty() {
            ui.add_space(4.0);
            ui.colored_label(Color32::from_rgb(240, 110, 110), &state.path_error);
        }

        ui.add_space(8.0);
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("Submit").clicked() { state.apply_to(config); config.save(); submitted = true; close = true; }
            if ui.button("Cancel").clicked() { close = true; }
        });
    });
    if close { state.open = false; }
    submitted
}

struct DangerPrompt { command: String, reason: &'static str }

pub struct Spiltixal {
    config:             Config,
    term:               TerminalState,
    pty:                Option<PtyHandle>,
    input_buf:          String,
    command_history:    Vec<String>,
    history_idx:        Option<usize>,
    danger_prompt:      Option<DangerPrompt>,
    search:             SearchState,
    search_open:        bool,
    mate:               Mate,
    mate_open_target:   bool,
    mate_open_anim:     f32,
    mate_input_focused: bool,
    mate_textures:      HashMap<String, TextureHandle>,
    bg_texture:         Option<TextureHandle>,
    bg_texture_path:    Option<PathBuf>,
    customize:          Option<CustomizeState>,
    cursor_blink_timer: Instant,
    cursor_visible:     bool,
    cell_w:             f32,
    cell_h:             f32,
    nerd_font_loaded:   bool,
    anim_t:             f32,
    terminal_has_focus: bool,
    terminal_rect:      Option<Rect>,
    mate_rect:          Option<Rect>,
    install_prompt_open: bool,
    install_feedback:    String,
    install_in_progress: bool,
    install_rx:          Option<Receiver<String>>,
    last_ram_check:      Instant,
    ai_enable_prompt_open: bool,
    ai_enable_feedback:    String,
    ollama_child:         Option<OsChild>,
    term_rows:            usize,
    term_cols:            usize,
    last_metrics_update:  Instant,
    applied_layers:       Vec<OverlayLayer>,
    applied_drawing:      Vec<DrawStroke>,
    picker_in_progress:   bool,
    picker_rx:            Option<Receiver<Result<String, String>>>,
}

impl Drop for Spiltixal {
    fn drop(&mut self) {
        self.picker_rx = None;
        if let Some(mut child) = self.ollama_child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Spiltixal {
    fn launched_from_usr_bin() -> bool {
        std::env::current_exe()
            .ok()
            .is_some_and(|p| p.starts_with("/usr/bin"))
    }

    fn is_theme_one_name(name: &str) -> bool {
        name == "1" || name == "Cosmic Purple"
    }

    fn is_theme_one(&self) -> bool {
        Self::is_theme_one_name(&self.config.theme_preset)
    }

    fn layer_to_saved(layer: &OverlayLayer) -> SavedOverlayLayer {
        SavedOverlayLayer {
            path: layer.path.display().to_string(),
            is_video: layer.is_video,
            pos: [layer.pos.x, layer.pos.y],
            size: [layer.size.x, layer.size.y],
            rotation_deg: layer.rotation_deg,
            tint: layer.tint,
            animation: layer.animation,
        }
    }

    fn draw_rotated_texture(
        painter: &Painter,
        tex: TextureId,
        center: Pos2,
        size: Vec2,
        angle_deg: f32,
        tint: Color32,
    ) {
        let angle = angle_deg.to_radians();
        let (s, c) = angle.sin_cos();
        let hw = size.x * 0.5;
        let hh = size.y * 0.5;
        let corners = [vec2(-hw, -hh), vec2(hw, -hh), vec2(hw, hh), vec2(-hw, hh)];
        let mut mesh = egui::epaint::Mesh::with_texture(tex);
        let uvs = [pos2(0.0, 0.0), pos2(1.0, 0.0), pos2(1.0, 1.0), pos2(0.0, 1.0)];
        for (local, uv) in corners.into_iter().zip(uvs.into_iter()) {
            let rot = vec2(local.x * c - local.y * s, local.x * s + local.y * c);
            mesh.vertices.push(egui::epaint::Vertex { pos: center + rot, uv, color: tint });
        }
        mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
        painter.add(Shape::mesh(mesh));
    }

    fn layer_center(rect: Rect, layer: &OverlayLayer, t: f32, phase: f32) -> Pos2 {
        let mut c = pos2(
            rect.left() + rect.width() * layer.pos.x,
            rect.top() + rect.height() * layer.pos.y,
        );
        if layer.animation == OverlayAnimation::Floating {
            c.y += (t * 1.9 + phase).sin() * 12.0;
            c.x += (t * 1.3 + phase).cos() * 6.0;
        }
        c
    }

    fn layer_size_px(rect: Rect, layer: &OverlayLayer) -> Vec2 {
        let base = rect.width().min(rect.height());
        vec2(
            (layer.size.x * base).max(12.0),
            (layer.size.y * base).max(12.0),
        )
    }

    fn ensure_layer_texture(layer: &mut OverlayLayer, ctx: &Context) {
        if layer.texture.is_some() {
            return;
        }
        let img = if layer.is_video {
            extract_video_poster(&layer.path)
        } else {
            image_from_path(&layer.path)
        };
        if let Some(ci) = img {
            let name = format!("overlay-{}", layer.path.display());
            layer.texture = Some(ctx.load_texture(name, ci, TextureOptions::LINEAR));
        }
    }

    fn render_overlay_layers(&self, painter: &Painter, rect: Rect, layers: &[OverlayLayer], selected: Option<usize>) {
        for (i, layer) in layers.iter().enumerate() {
            let center = Self::layer_center(rect, layer, self.anim_t, i as f32 * 0.73);
            let size = Self::layer_size_px(rect, layer);
            let rot = if layer.animation == OverlayAnimation::Spin {
                layer.rotation_deg + self.anim_t * 45.0
            } else {
                layer.rotation_deg
            };
            let tint = Color32::from_rgba_unmultiplied(layer.tint[0], layer.tint[1], layer.tint[2], layer.tint[3]);
            if let Some(tex) = &layer.texture {
                Self::draw_rotated_texture(painter, tex.id(), center, size, rot, tint);
            }
            if selected == Some(i) {
                painter.rect_stroke(
                    Rect::from_center_size(center, size + vec2(8.0, 8.0)),
                    2.0,
                    Stroke::new(1.3, Color32::from_rgb(245, 190, 90)),
                );
            }
        }
    }

    fn render_drawing(&self, painter: &Painter, rect: Rect, strokes: &[DrawStroke]) {
        for stroke in strokes {
            if stroke.points.len() < 2 {
                continue;
            }
            let color = Color32::from_rgba_unmultiplied(stroke.color[0], stroke.color[1], stroke.color[2], stroke.color[3]);
            for w in stroke.points.windows(2) {
                let p0 = pos2(rect.left() + w[0][0] * rect.width(), rect.top() + w[0][1] * rect.height());
                let p1 = pos2(rect.left() + w[1][0] * rect.width(), rect.top() + w[1][1] * rect.height());
                painter.line_segment([p0, p1], Stroke::new(stroke.width, color));
            }
        }
    }

    fn save_customize_layout(&mut self, state: &mut CustomizeState) {
        let Some(home) = dirs::home_dir() else { return; };
        let dir = home.join(".config").join("spiltixal");
        let path = dir.join("layout.json");
        let _ = std::fs::create_dir_all(&dir);
        let layout = SavedCustomizeLayout {
            saved_at: Local::now().to_rfc3339(),
            text_color: state.fg_color,
            background_color: state.bg_solid,
            theme_preset: state.theme_preset.clone(),
            layers: state.layers.iter().map(Self::layer_to_saved).collect(),
            drawing: state.drawing.clone(),
        };
        if let Ok(json) = serde_json::to_string_pretty(&layout) {
            if std::fs::write(&path, json).is_ok() {
                state.save_message = format!("Saved at {}", path.display());
                self.mate.last_message = state.save_message.clone();
                self.mate.typing_target = state.save_message.clone();
                self.mate.typing_chars = 0;
                self.mate.typing_tick = Instant::now();
            }
        }
    }

    fn load_customize_layout() -> Option<SavedCustomizeLayout> {
        let home = dirs::home_dir()?;
        let path = home.join(".config").join("spiltixal").join("layout.json");
        let data = std::fs::read_to_string(path).ok()?;
        serde_json::from_str::<SavedCustomizeLayout>(&data).ok()
    }

    fn point_to_norm(rect: Rect, p: Pos2) -> Vec2 {
        vec2(
            ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0),
            ((p.y - rect.top()) / rect.height()).clamp(0.0, 1.0),
        )
    }

    fn hit_layer_index(rect: Rect, layers: &[OverlayLayer], p: Pos2, t: f32) -> Option<usize> {
        for i in (0..layers.len()).rev() {
            let layer = &layers[i];
            let center = Self::layer_center(rect, layer, t, i as f32 * 0.73);
            let size = Self::layer_size_px(rect, layer);
            let r = Rect::from_center_size(center, size);
            if r.contains(p) {
                return Some(i);
            }
        }
        None
    }

    fn draw_customize_editor(&mut self, ctx: &Context, term_rect: Rect) {
        let Some(mut state) = self.customize.take() else { return };
        if !state.open {
            self.customize = None;
            return;
        }

        if self.picker_in_progress {
            if let Some(rx) = &self.picker_rx {
                if let Ok(result) = rx.try_recv() {
                    match result {
                        Ok(path) => {
                            state.layer_path_input = path;
                            state.path_error.clear();
                        }
                        Err(err) => {
                            state.path_error = err;
                        }
                    }
                    self.picker_in_progress = false;
                    self.picker_rx = None;
                }
            }
        }

        for layer in &mut state.layers {
            Self::ensure_layer_texture(layer, ctx);
        }

        let term_painter = ctx.layer_painter(LayerId::new(egui::Order::Foreground, Id::new("customize_overlay")));
        self.render_overlay_layers(&term_painter, term_rect, &state.layers, state.selected_layer);
        self.render_drawing(&term_painter, term_rect, &state.drawing);
        if state.active_stroke.len() > 1 {
            let stroke_color = Color32::from_rgba_unmultiplied(state.fg_color[0], state.fg_color[1], state.fg_color[2], state.fg_color[3]);
            for pts in state.active_stroke.windows(2) {
                term_painter.line_segment([pts[0], pts[1]], Stroke::new(state.stroke_width, stroke_color));
            }
        }

        let pointer = ctx.input(|i| {
            (
                i.pointer.interact_pos(),
                i.pointer.primary_down(),
                i.pointer.any_pressed(),
                i.pointer.any_released(),
            )
        });

        if state.tool == CustomizeTool::Draw {
            if let Some(p) = pointer.0 {
                if term_rect.contains(p) && pointer.1 {
                    state.active_stroke.push(p);
                    if state.active_stroke.len() > 4000 {
                        let reduced = state
                            .active_stroke
                            .iter()
                            .enumerate()
                            .filter_map(|(i, p)| if i % 2 == 0 { Some(*p) } else { None })
                            .collect::<Vec<_>>();
                        state.active_stroke = reduced;
                    }
                }
            }
            if pointer.3 && !state.active_stroke.is_empty() {
                let points = state.active_stroke
                    .iter()
                    .map(|p| {
                        let n = Self::point_to_norm(term_rect, *p);
                        [n.x, n.y]
                    })
                    .collect::<Vec<_>>();
                if points.len() > 1 {
                    state.drawing.push(DrawStroke {
                        points,
                        color: state.fg_color,
                        width: state.stroke_width,
                    });
                    if state.drawing.len() > 300 {
                        let extra = state.drawing.len() - 300;
                        state.drawing.drain(0..extra);
                    }
                }
                state.active_stroke.clear();
            }
        } else if let Some(p) = pointer.0 {
            if pointer.2 && term_rect.contains(p) {
                if let Some(idx) = Self::hit_layer_index(term_rect, &state.layers, p, self.anim_t) {
                    state.selected_layer = Some(idx);
                    let center = Self::layer_center(term_rect, &state.layers[idx], self.anim_t, idx as f32 * 0.73);
                    state.drag_layer = Some(idx);
                    state.drag_offset = p - center;
                }
            } else if pointer.1 {
                if let Some(idx) = state.drag_layer {
                    let target = p - state.drag_offset;
                    let n = Self::point_to_norm(term_rect, target);
                    if let Some(layer) = state.layers.get_mut(idx) {
                        layer.pos = n;
                    }
                }
            } else if pointer.3 {
                state.drag_layer = None;
            }
        }

        egui::Area::new("customize_left_tools".into())
            .anchor(Align2::LEFT_TOP, vec2(10.0, 82.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let view_w = ctx.input(|i| i.screen_rect().width());
                let left_w = (view_w * 0.14).clamp(155.0, 240.0);
                egui::Frame::none()
                    .fill(Color32::from_rgba_unmultiplied(12, 14, 24, 225))
                    .rounding(10.0)
                    .stroke(Stroke::new(1.2, Color32::from_rgba_unmultiplied(85, 115, 205, 210)))
                    .inner_margin(Margin::symmetric(10.0, 10.0))
                    .show(ui, |ui| {
                        ui.set_width(left_w);
                        ui.label(RichText::new("Customize").strong().size(15.0).color(Color32::from_rgb(165, 208, 255)));
                        ui.add_space(6.0);
                        for (tool, label) in [
                            (CustomizeTool::AddImage, "1. Add Image"),
                            (CustomizeTool::AddVideo, "2. Add Video"),
                            (CustomizeTool::Draw, "3. Draw"),
                            (CustomizeTool::TextColor, "4. Text Color"),
                            (CustomizeTool::BackgroundColor, "5. Background Color"),
                            (CustomizeTool::Theme, "6. Theme"),
                        ] {
                            if ui.selectable_label(state.tool == tool, label).clicked() {
                                state.tool = tool;
                            }
                        }
                    });
            });

        egui::Area::new("customize_right_props".into())
            .anchor(Align2::RIGHT_TOP, vec2(-12.0, 82.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let view_w = ctx.input(|i| i.screen_rect().width());
                let right_w = (view_w * 0.22).clamp(270.0, 420.0);
                egui::Frame::none()
                    .fill(Color32::from_rgba_unmultiplied(12, 14, 24, 225))
                    .rounding(10.0)
                    .stroke(Stroke::new(1.2, Color32::from_rgba_unmultiplied(85, 115, 205, 210)))
                    .inner_margin(Margin::symmetric(10.0, 10.0))
                    .show(ui, |ui| {
                        ui.set_width(right_w);
                        match state.tool {
                            CustomizeTool::AddImage | CustomizeTool::AddVideo => {
                                let is_video_tool = state.tool == CustomizeTool::AddVideo;
                                let label = if !is_video_tool { "Image Path" } else { "Video Path" };
                                ui.label(label);
                                ui.add(egui::TextEdit::singleline(&mut state.layer_path_input).hint_text("/path/to/file"));
                                let pick_label = if !is_video_tool { "Select Image" } else { "Select Video" };
                                if ui.button(pick_label).clicked() {
                                    self.start_picker(is_video_tool);
                                    state.path_error = "Opening file picker...".into();
                                }
                                if ui.button("Add Layer").clicked() {
                                    let p = PathBuf::from(state.layer_path_input.trim());
                                    if p.exists() {
                                        let mut layer = OverlayLayer {
                                            path: p,
                                            is_video: is_video_tool,
                                            pos: vec2(0.5, 0.5),
                                            size: vec2(0.24, 0.24),
                                            rotation_deg: 0.0,
                                            tint: [255, 255, 255, 255],
                                            animation: OverlayAnimation::None,
                                            texture: None,
                                        };
                                        Self::ensure_layer_texture(&mut layer, ctx);
                                        if layer.texture.is_none() {
                                            state.path_error = if layer.is_video {
                                                "Could not load video poster. Check ffmpeg and file path.".into()
                                            } else {
                                                "Could not load image. Check file path/format.".into()
                                            };
                                        } else {
                                            state.layers.push(layer);
                                            state.selected_layer = Some(state.layers.len().saturating_sub(1));
                                            state.layer_path_input.clear();
                                            state.path_error.clear();
                                        }
                                    } else {
                                        state.path_error = "Path does not exist".into();
                                    }
                                }
                            }
                            CustomizeTool::Draw => {
                                ui.label("Draw over terminal");
                                ui.horizontal(|ui| {
                                    ui.label("Width");
                                    ui.add(egui::Slider::new(&mut state.stroke_width, 1.0..=10.0));
                                });
                                if ui.button("Clear Drawing").clicked() {
                                    state.drawing.clear();
                                }
                            }
                            CustomizeTool::TextColor => {
                                ui.label("Terminal text color");
                                show_color_picker(ui, &mut state.fg_color);
                            }
                            CustomizeTool::BackgroundColor => {
                                ui.label("Background color");
                                show_color_picker(ui, &mut state.bg_solid);
                            }
                            CustomizeTool::Theme => {
                                ui.label("Theme");
                                ui.horizontal(|ui| {
                                    if ui.selectable_label(state.theme_preset == "Default", "Default").clicked() {
                                        state.theme_preset = "Default".into();
                                    }
                                    if ui.selectable_label(Self::is_theme_one_name(&state.theme_preset), "1").clicked() {
                                        state.theme_preset = "1".into();
                                    }
                                });
                            }
                        }

                        if let Some(idx) = state.selected_layer {
                            if let Some(layer) = state.layers.get_mut(idx) {
                                ui.separator();
                                ui.label("Selected Layer");
                                ui.horizontal(|ui| {
                                    ui.label("Size");
                                    ui.add(egui::Slider::new(&mut layer.size.x, 0.05..=0.9).show_value(false));
                                    ui.add(egui::Slider::new(&mut layer.size.y, 0.05..=0.9).show_value(false));
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Rotation");
                                    ui.add(egui::Slider::new(&mut layer.rotation_deg, -180.0..=180.0));
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Tint");
                                    show_color_picker(ui, &mut layer.tint);
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Animation");
                                    ui.selectable_value(&mut layer.animation, OverlayAnimation::None, "None");
                                    ui.selectable_value(&mut layer.animation, OverlayAnimation::Spin, "Spin");
                                    ui.selectable_value(&mut layer.animation, OverlayAnimation::Floating, "Floating");
                                });
                                if ui.button("Remove Layer").clicked() {
                                    state.layers.remove(idx);
                                    state.selected_layer = None;
                                }
                            }
                        }

                        if !state.path_error.is_empty() {
                            ui.colored_label(Color32::from_rgb(245, 120, 120), &state.path_error);
                        }
                        ui.separator();
                        if ui.button("Reset to default").clicked() {
                            state.reset_confirm_step = 1;
                        }
                        if state.reset_confirm_step > 0 {
                            let prompts = [
                                "Are you sure?",
                                "Are you really sure?",
                                "Are you really really sure?",
                                "You are about to reset your customization to default.",
                                "Alright. Press Proceed",
                            ];
                            let idx = (state.reset_confirm_step - 1).min(prompts.len() - 1);
                            ui.add_space(4.0);
                            ui.colored_label(Color32::from_rgb(255, 185, 120), prompts[idx]);
                            ui.horizontal(|ui| {
                                if ui.button("Proceed").clicked() {
                                    if state.reset_confirm_step < 5 {
                                        state.reset_confirm_step += 1;
                                    } else {
                                        let defaults = Theme::default();
                                        state.theme_preset = "1".into();
                                        state.fg_color = defaults.foreground;
                                        state.use_gradient = true;
                                        state.grad_a = [18, 12, 34, 255];
                                        state.grad_b = [58, 24, 88, 255];
                                        state.grad_angle = 130.0;
                                        state.bg_solid = [13, 13, 20, 255];
                                        state.bg_image = None;
                                        state.bg_video = None;
                                        state.bg_image_input.clear();
                                        state.bg_video_input.clear();
                                        state.layer_path_input.clear();
                                        state.layers.clear();
                                        state.selected_layer = None;
                                        state.active_stroke.clear();
                                        state.drawing.clear();
                                        state.path_error.clear();
                                        state.reset_confirm_step = 0;
                                    }
                                }
                                if ui.button("Cancel").clicked() {
                                    state.reset_confirm_step = 0;
                                }
                            });
                        }
                    });
            });

        egui::Area::new("customize_apply".into())
            .anchor(Align2::RIGHT_BOTTOM, vec2(-12.0, -12.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                if ui.add(egui::Button::new(RichText::new("Apply").strong()).fill(Color32::from_rgb(55, 125, 220))).clicked() {
                    state.apply_to(&mut self.config);
                    self.config.save();
                    self.applied_layers = state.layers
                        .iter()
                        .map(|l| OverlayLayer {
                            path: l.path.clone(),
                            is_video: l.is_video,
                            pos: l.pos,
                            size: l.size,
                            rotation_deg: l.rotation_deg,
                            tint: l.tint,
                            animation: l.animation,
                            texture: l.texture.clone(),
                        })
                        .collect();
                    self.applied_drawing = state.drawing.clone();
                    self.save_customize_layout(&mut state);
                    state.open = false;
                }
            });

        if !state.save_message.is_empty() {
            egui::Area::new("customize_saved_msg".into())
                .anchor(Align2::CENTER_BOTTOM, vec2(0.0, -12.0))
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    egui::Frame::none()
                        .fill(Color32::from_rgba_unmultiplied(22, 34, 52, 230))
                        .rounding(8.0)
                        .inner_margin(Margin::symmetric(10.0, 8.0))
                        .show(ui, |ui| {
                            ui.label(RichText::new(&state.save_message).color(Color32::from_rgb(190, 230, 255)));
                        });
                });
        }

        self.customize = Some(state);
    }

    fn ctrl_or_cmd(modifiers: egui::Modifiers) -> bool {
        modifiers.ctrl || modifiers.command
    }

    fn key_to_ctrl_byte(key: Key) -> Option<u8> {
        match key {
            Key::A => Some(0x01), Key::B => Some(0x02), Key::C => Some(0x03), Key::D => Some(0x04),
            Key::E => Some(0x05), Key::F => Some(0x06), Key::G => Some(0x07), Key::H => Some(0x08),
            Key::I => Some(0x09), Key::J => Some(0x0a), Key::K => Some(0x0b), Key::L => Some(0x0c),
            Key::M => Some(0x0d), Key::N => Some(0x0e), Key::O => Some(0x0f), Key::P => Some(0x10),
            Key::Q => Some(0x11), Key::R => Some(0x12), Key::S => Some(0x13), Key::T => Some(0x14),
            Key::U => Some(0x15), Key::V => Some(0x16), Key::W => Some(0x17), Key::X => Some(0x18),
            Key::Y => Some(0x19), Key::Z => Some(0x1a),
            _ => None,
        }
    }

    pub fn new(cc: &eframe::CreationContext) -> Self {
        egui_extras::install_image_loaders(&cc.egui_ctx);

        let mut fonts = egui::FontDefinitions::default();
        let mut nerd_loaded = false;
        for (idx, font_path) in find_icon_fonts().into_iter().enumerate() {
            if let Ok(bytes) = std::fs::read(&font_path) {
                let key = format!("IconFont{idx}");
                if idx == 0 { nerd_loaded = true; }
                fonts.font_data.insert(key.clone(), egui::FontData::from_owned(bytes));
                fonts.families.entry(FontFamily::Monospace).or_default().insert(0, key.clone());
                fonts.families.entry(FontFamily::Proportional).or_default().insert(0, key);
            }
        }
        cc.egui_ctx.set_fonts(fonts);

        let mut style = (*cc.egui_ctx.style()).clone();
        style.visuals.window_rounding            = Rounding::same(10.0);
        style.visuals.widgets.noninteractive.rounding = Rounding::same(6.0);
        style.visuals.widgets.inactive.rounding  = Rounding::same(6.0);
        style.visuals.widgets.hovered.rounding   = Rounding::same(6.0);
        style.visuals.widgets.active.rounding    = Rounding::same(6.0);
        style.visuals.panel_fill                 = Color32::TRANSPARENT;
        style.visuals.window_fill                = Color32::from_rgba_unmultiplied(18, 18, 28, 240);
        cc.egui_ctx.set_style(style);

        let mut config = Config::load();
        if !config.ai_system_prompt.contains("attached to the live PTY terminal") {
            config.ai_system_prompt.push_str(" You are attached to the live PTY terminal and allowed to run commands through user-approved actions. Supported direct actions are /run <command>, /ctrl c, /ctrl z, /ctrl \\\\, and /signal <INT|TSTP|QUIT>.");
            config.save();
        }
        let ai_client = if config.ai_enabled {
            Some(AiClient::new(&config.ai_endpoint, &config.ai_model, &config.ai_system_prompt))
        } else { None };
        let mate = Mate::new(config.mate_name.clone(), ai_client);
        let pty  = PtyHandle::spawn(&config.shell, 24, 80).ok();
        let (applied_layers, applied_drawing) = if let Some(layout) = Self::load_customize_layout() {
            let layers = layout.layers.into_iter().map(|l| OverlayLayer {
                path: PathBuf::from(l.path),
                is_video: l.is_video,
                pos: vec2(l.pos[0], l.pos[1]),
                size: vec2(l.size[0], l.size[1]),
                rotation_deg: l.rotation_deg,
                tint: l.tint,
                animation: l.animation,
                texture: None,
            }).collect::<Vec<_>>();
            (layers, layout.drawing)
        } else {
            (Vec::new(), Vec::new())
        };

        Self {
            term: TerminalState::new(24, 80, config.scrollback_lines),
            pty, input_buf: String::new(), command_history: Vec::new(), history_idx: None,
            danger_prompt: None, search: SearchState::default(), search_open: false,
            mate, mate_open_target: true, mate_open_anim: 1.0, mate_input_focused: false,
            mate_textures: HashMap::new(), bg_texture: None, bg_texture_path: None, customize: None,
            cursor_blink_timer: Instant::now(), cursor_visible: true,
            cell_w: 8.5, cell_h: 17.0, nerd_font_loaded: nerd_loaded, anim_t: 0.0,
            terminal_has_focus: true, terminal_rect: None, mate_rect: None,
            install_prompt_open: !Self::launched_from_usr_bin(), install_feedback: String::new(),
            install_in_progress: false,
            install_rx: None,
            last_ram_check: Instant::now(),
            ai_enable_prompt_open: false,
            ai_enable_feedback: String::new(),
            ollama_child: None,
            term_rows: 24,
            term_cols: 80,
            last_metrics_update: Instant::now(),
            applied_layers,
            applied_drawing,
            picker_in_progress: false,
            picker_rx: None,
            config,
        }
    }

    fn poll_pty(&mut self) {
        if let Some(pty) = &self.pty {
            while let Ok(bytes) = pty.rx.try_recv() { self.term.process_bytes(&bytes); }
        }
    }

    fn send_input(&self, data: &str) {
        if let Some(pty) = &self.pty { let _ = pty.write_str(data); }
    }

    fn send_signal(&self, signal_name: &str) {
        if let Some(pty) = &self.pty {
            let _ = pty.signal_foreground(signal_name);
        }
    }

    fn execute_command(&mut self, cmd: String) {
        self.command_history.push(cmd.clone());
        self.history_idx = None;
        self.input_buf.clear();
        self.send_input(&format!("{}\n", cmd));
    }

    fn replace_terminal_input_line(&self, new_line: &str) {
        self.send_input("\x15");
        if !new_line.is_empty() { self.send_input(new_line); }
    }

    fn autocorrect_command(&self, cmd: &str) -> String {
        let mut parts = cmd.splitn(2, ' ');
        let head = parts.next().unwrap_or("");
        let tail = parts.next().unwrap_or("");
        let fixed = match head {
            "sl" => "ls",
            "gti" => "git",
            "grpe" => "grep",
            "pyhton" => "python",
            "pnpmn" => "pnpm",
            _ => head,
        };
        if fixed == head { cmd.to_string() }
        else if tail.is_empty() { fixed.to_string() }
        else { format!("{fixed} {tail}") }
    }

    fn finalize_typed_command(&mut self) {
        let cmd = self.input_buf.trim_end_matches('\n').to_string();
        if cmd.is_empty() {
            self.send_input("\n");
            self.input_buf.clear();
            return;
        }
        let corrected = self.autocorrect_command(&cmd);
        if corrected != cmd {
            self.replace_terminal_input_line(&corrected);
            self.input_buf = corrected.clone();
            self.mate.last_message = format!("autocorrected: {cmd} → {corrected}");
        }
        if let Some(reason) = check_dangerous(&self.input_buf) {
            self.danger_prompt = Some(DangerPrompt { command: self.input_buf.clone(), reason });
            return;
        }
        self.command_history.push(self.input_buf.clone());
        self.history_idx = None;
        self.input_buf.clear();
        self.send_input("\n");
    }

    fn terminal_context(&self) -> String {
        let total = self.term.grid.scrollback.len() + self.term.grid.rows;
        let start = total.saturating_sub(12);
        let mut lines = Vec::new();
        for idx in start..total {
            let row = if idx < self.term.grid.scrollback.len() {
                &self.term.grid.scrollback[idx]
            } else {
                &self.term.grid.cells[idx - self.term.grid.scrollback.len()]
            };
            let line: String = row.iter().map(|c| c.ch).collect::<String>().trim_end().to_string();
            if !line.is_empty() { lines.push(line); }
        }
        lines.join("\n")
    }

    fn ensure_background_texture(&mut self, ctx: &Context) {
        match &self.config.theme.background {
            Background::Image { path, .. } => {
                if self.bg_texture_path.as_ref() == Some(path) { return; }
                self.bg_texture = image_from_path(path)
                    .map(|ci| ctx.load_texture("spiltixal-bg-image", ci, TextureOptions::LINEAR));
                self.bg_texture_path = Some(path.clone());
            }
            Background::Video { path, .. } => {
                if self.bg_texture_path.as_ref() == Some(path) { return; }
                self.bg_texture = extract_video_poster(path)
                    .map(|ci| ctx.load_texture("spiltixal-bg-video-poster", ci, TextureOptions::LINEAR));
                self.bg_texture_path = Some(path.clone());
            }
            _ => {
                self.bg_texture = None;
                self.bg_texture_path = None;
            }
        }
    }

    fn current_rss_bytes() -> Option<u64> {
        let data = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = data.lines().find(|l| l.starts_with("VmRSS:"))?;
        let kb = line
            .split_whitespace()
            .nth(1)
            .and_then(|v| v.parse::<u64>().ok())?;
        Some(kb * 1024)
    }

    fn process_rss_bytes(pid: u32) -> Option<u64> {
        let data = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
        let line = data.lines().find(|l| l.starts_with("VmRSS:"))?;
        let kb = line.split_whitespace().nth(1)?.parse::<u64>().ok()?;
        Some(kb * 1024)
    }

    fn enforce_ai_ram_limit(&mut self) {
        if !self.config.ai_enabled { return; }
        if self.last_ram_check.elapsed() < Duration::from_millis(750) { return; }
        self.last_ram_check = Instant::now();

        let mut rss = Self::current_rss_bytes().unwrap_or(0);
        if let Some(child) = &self.ollama_child {
            if let Some(ollama_rss) = Self::process_rss_bytes(child.id()) {
                rss = rss.saturating_add(ollama_rss);
            }
        }
        if rss > 0 {
            if rss > AI_RAM_LIMIT_BYTES {
                self.disable_ai();
                self.mate.last_message = "had to turn off AI — hit the 1.5GB RAM limit.".into();
            }
        }
    }

    fn handle_terminal_scroll(&mut self, ctx: &Context) {
        let Some(rect) = self.terminal_rect else { return; };
        let pointer_in_terminal = ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| rect.contains(p));
        if !pointer_in_terminal { return; }

        let dy = ctx.input(|i| i.smooth_scroll_delta.y);
        if dy.abs() < f32::EPSILON { return; }

        let lines = ((dy.abs() / self.cell_h).ceil() as usize).max(1);
        let max_offset = self.term.grid.scrollback.len();
        if dy > 0.0 {
            self.term.grid.scroll_offset = (self.term.grid.scroll_offset + lines).min(max_offset);
        } else {
            self.term.grid.scroll_offset = self.term.grid.scroll_offset.saturating_sub(lines);
        }
    }

    fn sync_terminal_size(&mut self, rect: Rect) {
        let rows = ((rect.height() / self.cell_h).floor() as usize).max(2);
        let cols = ((rect.width() / self.cell_w).floor() as usize).max(8);
        if rows == self.term_rows && cols == self.term_cols { return; }
        self.term_rows = rows;
        self.term_cols = cols;
        self.term.resize(rows, cols);
        if let Some(pty) = &self.pty {
            let _ = pty.resize(rows as u16, cols as u16);
        }
    }

    fn update_cell_metrics(&mut self, ctx: &Context) {
        let font_id = FontId::new(self.config.theme.font_size, FontFamily::Monospace);
        let size = ctx.fonts(|f| f.layout_no_wrap("W".to_owned(), font_id, Color32::WHITE).size());
        if size.x.is_finite() && size.y.is_finite() && size.x > 0.0 && size.y > 0.0 {
            let w = (size.x * 10.0).round() / 10.0;
            let h = ((size.y + 2.0) * 10.0).round() / 10.0;
            self.cell_w = w.max(6.0);
            self.cell_h = h.max(10.0);
        }
    }

    fn endpoint_is_local_ollama(&self) -> bool {
        self.config.ai_endpoint.contains("localhost") || self.config.ai_endpoint.contains("127.0.0.1")
    }

    fn ollama_listening(&self) -> bool {
        TcpStream::connect("127.0.0.1:11434").is_ok()
    }

    fn start_ollama_serve_if_needed(&mut self) -> Result<()> {
        if !self.endpoint_is_local_ollama() || self.ollama_listening() {
            return Ok(());
        }
        let child = Command::new("ollama")
            .arg("serve")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("Failed to start `ollama serve`")?;
        self.ollama_child = Some(child);
        Ok(())
    }

    fn enable_ai(&mut self) {
        self.config.ai_enabled = true;
        self.mate.ai_client = Some(AiClient::new(&self.config.ai_endpoint, &self.config.ai_model, &self.config.ai_system_prompt));
        if let Err(e) = self.start_ollama_serve_if_needed() {
            self.ai_enable_feedback = format!("AI enabled, but couldn't start Ollama: {}", e);
            self.mate.last_message = self.ai_enable_feedback.clone();
        } else {
            self.ai_enable_feedback.clear();
        }
        self.enforce_ai_ram_limit();
        self.config.save();
    }

    fn disable_ai(&mut self) {
        self.config.ai_enabled = false;
        self.mate.ai_client = None;
        if let Some(mut child) = self.ollama_child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.config.save();
    }

    fn draw_ai_enable_prompt(&mut self, ctx: &Context) -> bool {
        if !self.ai_enable_prompt_open { return false; }
        let mut accept = false;
        let mut decline = false;
        egui::Window::new("Enable Local AI")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(RichText::new("Warning").strong().size(17.0).color(Color32::from_rgb(255, 160, 120)));
                ui.add_space(6.0);
                ui.label("This uses local AI and can use a lot of RAM.");
                ui.label("Continuing will start `ollama serve` now.");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("[y] Enable AI").clicked() { accept = true; }
                    if ui.button("[n] Cancel").clicked() { decline = true; }
                });
            });

        if accept {
            self.enable_ai();
            self.ai_enable_prompt_open = false;
        }
        if decline {
            self.ai_enable_prompt_open = false;
        }
        true
    }

    fn set_mate_open(&mut self, open: bool) {
        self.mate_open_target = open;
    }

    fn animate_mate_panel(&mut self) {
        let target = if self.mate_open_target { 1.0 } else { 0.0 };
        self.mate_open_anim = egui::emath::lerp(self.mate_open_anim..=target, 0.18);
        if (self.mate_open_anim - target).abs() < 0.01 {
            self.mate_open_anim = target;
        }
    }

    fn shell_escape_single(s: &str) -> String {
        s.replace('\'', "'\"'\"'")
    }

    fn command_exists(bin: &str) -> bool {
        Command::new("sh")
            .arg("-lc")
            .arg(format!("command -v {} >/dev/null 2>&1", bin))
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn run_picker(program: &str, args: &[&str]) -> Option<PathBuf> {
        let output = Command::new(program).args(args).output().ok()?;
        if !output.status.success() {
            return None;
        }
        let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if raw.is_empty() {
            return None;
        }
        let p = PathBuf::from(raw);
        if p.exists() { Some(p) } else { None }
    }

    fn pick_file_via_system(_is_video: bool) -> Result<PathBuf> {
        let kde_filter = "All Files (*)";

        if Self::command_exists("kdialog") {
            if let Some(p) = Self::run_picker("kdialog", &["--getopenfilename", "", kde_filter]) {
                return Ok(p);
            }
        }
        if Self::command_exists("zenity") {
            if let Some(p) = Self::run_picker("zenity", &["--file-selection", "--title=Select file"]) {
                return Ok(p);
            }
        }
        if Self::command_exists("yad") {
            if let Some(p) = Self::run_picker("yad", &["--file-selection", "--title=Select file"]) {
                return Ok(p);
            }
        }
        if Self::command_exists("qarma") {
            if let Some(p) = Self::run_picker("qarma", &["--file-selection", "--title=Select file"]) {
                return Ok(p);
            }
        }

        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        if Self::command_exists("dolphin") {
            let _ = Command::new("dolphin").arg(&home).spawn();
            anyhow::bail!("Opened Dolphin. Copy the file path and paste it into the path box.");
        }
        if Self::command_exists("nautilus") {
            let _ = Command::new("nautilus").arg(&home).spawn();
            anyhow::bail!("Opened Nautilus. Copy the file path and paste it into the path box.");
        }
        if Self::command_exists("thunar") {
            let _ = Command::new("thunar").arg(&home).spawn();
            anyhow::bail!("Opened Thunar. Copy the file path and paste it into the path box.");
        }
        if Self::command_exists("pcmanfm") {
            let _ = Command::new("pcmanfm").arg(&home).spawn();
            anyhow::bail!("Opened PCManFM. Copy the file path and paste it into the path box.");
        }
        if Self::command_exists("xdg-open") {
            let _ = Command::new("xdg-open").arg(&home).spawn();
            anyhow::bail!("Opened your file manager. Copy the file path and paste it into the path box.");
        }
        anyhow::bail!("No file picker detected. Paste the full path manually.");
    }

    fn start_picker(&mut self, is_video: bool) {
        if self.picker_in_progress {
            return;
        }
        let (tx, rx) = unbounded::<Result<String, String>>();
        self.picker_in_progress = true;
        self.picker_rx = Some(rx);
        thread::spawn(move || {
            let res = match Spiltixal::pick_file_via_system(is_video) {
                Ok(p) => Ok(p.display().to_string()),
                Err(e) => Err(e.to_string()),
            };
            let _ = tx.send(res);
        });
    }

    fn try_install_to_usr_bin(exe: PathBuf) -> Result<String> {
        let mut log = String::new();
        let target = PathBuf::from("/usr/bin/spiltixal");
        let helper = PathBuf::from("/usr/bin/makebuild");
        let update_mode = target.exists();
        if update_mode {
            log.push_str("Update process:\n");
        } else {
            log.push_str("Install process:\n");
        }
        log.push_str(&format!("Version: {}\n", APP_VERSION));
        log.push_str(&format!("Source binary: {}\n", exe.display()));
        log.push_str("Target binary: /usr/bin/spiltixal\n");
        log.push_str("Helper script: /usr/bin/makebuild\n");

        let direct = || -> Result<()> {
            std::fs::copy(&exe, &target).with_context(|| format!("Failed to copy {} to {}", exe.display(), target.display()))?;
            std::fs::write(&helper, "#!/bin/sh\nexec /usr/bin/spiltixal \"$@\"\n")
                .with_context(|| format!("Failed to write {}", helper.display()))?;
            #[cfg(unix)]
            {
                let perms = std::fs::Permissions::from_mode(0o755);
                std::fs::set_permissions(&target, perms.clone()).context("Failed to set executable permissions")?;
                std::fs::set_permissions(&helper, perms).context("Failed to set helper permissions")?;
            }
            Ok(())
        };

        match direct() {
            Ok(()) => {
                if update_mode {
                    log.push_str("Updated directly with current permissions.\n");
                } else {
                    log.push_str("Installed directly with current permissions.\n");
                }
                return Ok(log);
            }
            Err(e) => {
                log.push_str(&format!("Direct install failed: {e}\n"));
            }
        }

        let exe_esc = Self::shell_escape_single(&exe.display().to_string());
        let script = format!(
            "set -e\ninstall -Dm755 '{exe}' '/usr/bin/spiltixal'\ncat > '/usr/bin/makebuild' <<'EOF'\n#!/bin/sh\nexec /usr/bin/spiltixal \"$@\"\nEOF\nchmod 755 '/usr/bin/makebuild'\n",
            exe = exe_esc
        );

        let run_privileged = |launcher: &str| -> Result<String> {
            let output = Command::new(launcher)
                .arg("sh")
                .arg("-c")
                .arg(&script)
                .output()
                .with_context(|| format!("Failed to launch {launcher}"))?;
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let mut out = String::new();
            if !stdout.trim().is_empty() {
                out.push_str("stdout:\n");
                out.push_str(&stdout);
                out.push('\n');
            }
            if !stderr.trim().is_empty() {
                out.push_str("stderr:\n");
                out.push_str(&stderr);
                out.push('\n');
            }
            if !output.status.success() {
                anyhow::bail!("{} returned non-zero status", launcher);
            }
            Ok(out)
        };

        if Self::command_exists("pkexec") {
            log.push_str("Trying privileged step with pkexec...\n");
            let out = run_privileged("pkexec")?;
            log.push_str(&out);
            if update_mode {
                log.push_str("Privileged update completed.\n");
            } else {
                log.push_str("Privileged install completed.\n");
            }
            return Ok(log);
        }
        if Self::command_exists("sudo") {
            log.push_str("Trying privileged step with sudo...\n");
            let out = run_privileged("sudo")?;
            log.push_str(&out);
            if update_mode {
                log.push_str("Privileged update completed.\n");
            } else {
                log.push_str("Privileged install completed.\n");
            }
            return Ok(log);
        }
        anyhow::bail!("Need elevated privileges. Install pkexec or sudo, then try again.")
    }

    fn draw_first_launch_prompt(&mut self, ctx: &Context) -> bool {
        if !self.install_prompt_open { return false; }
        let update_mode = PathBuf::from("/usr/bin/spiltixal").exists();
        if self.install_in_progress {
            if let Some(rx) = &self.install_rx {
                if let Ok(msg) = rx.try_recv() {
                    self.install_feedback = msg;
                    self.install_in_progress = false;
                    self.install_rx = None;
                }
            }
        }
        let mut accept = false;
        let mut decline = false;
        let mut close = false;
        egui::Window::new("First Launch Setup")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                let title = if update_mode {
                    "Update Spiltixal in /usr/bin?"
                } else {
                    "Do you want to install Spiltixal?"
                };
                ui.label(RichText::new(title).strong().size(17.0));
                ui.add_space(6.0);
                ui.label(RichText::new(format!("Version: {}", APP_VERSION)).color(Color32::from_gray(180)));
                ui.label("[y] yes");
                ui.label("[n] no");
                ui.label(RichText::new("Install target: /usr/bin/spiltixal and /usr/bin/makebuild").color(Color32::from_gray(180)));
                ui.add_space(8.0);
                if self.install_in_progress {
                    ui.label("Installing... please wait.");
                } else if self.install_feedback.is_empty() {
                    ui.horizontal(|ui| {
                        if ui.button("[y] yes").clicked() { accept = true; }
                        if ui.button("[n] no").clicked() { decline = true; }
                    });
                } else if ui.button("Continue").clicked() {
                    close = true;
                }
                if !self.install_feedback.is_empty() {
                    ui.add_space(8.0);
                    egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                        ui.label(RichText::new(&self.install_feedback).color(Color32::from_gray(190)));
                    });
                }
            });

        if accept {
            match std::env::current_exe() {
                Ok(exe) => {
                    let was_installed = update_mode;
                    let (tx, rx) = unbounded::<String>();
                    self.install_in_progress = true;
                    self.install_feedback = "Starting install...".into();
                    self.install_rx = Some(rx);
                    thread::spawn(move || {
                        let out = match Spiltixal::try_install_to_usr_bin(exe) {
                            Ok(log) => {
                                if was_installed {
                                    format!("{log}\nUpdated /usr/bin/spiltixal and /usr/bin/makebuild.\nRun: spiltixal")
                                } else {
                                    format!("{log}\nInstalled to /usr/bin/spiltixal and /usr/bin/makebuild.\nRun: spiltixal")
                                }
                            }
                            Err(e) => format!("Install failed:\n{}\n", e),
                        };
                        let _ = tx.send(out);
                    });
                }
                Err(e) => {
                    self.install_feedback = format!("Install failed:\nUnable to resolve current executable path: {e}");
                }
            }
        }
        if decline {
            self.install_feedback = "Install skipped for this run.".into();
            self.install_prompt_open = false;
        }
        if close {
            self.install_prompt_open = false;
        }
        true
    }

    fn update_cursor_blink(&mut self) {
        if self.cursor_blink_timer.elapsed() >= Duration::from_millis(530) {
            self.cursor_visible = !self.cursor_visible;
            self.cursor_blink_timer = Instant::now();
        }
    }

    fn mate_texture(&mut self, ctx: &Context, emotion: Emotion) -> Option<TextureId> {
        let key = match emotion {
            Emotion::Happy | Emotion::Excited                => "happy",
            Emotion::Neutral | Emotion::Confused             => "neutral",
            Emotion::Thinking | Emotion::Curious             => "thinking",
            Emotion::Worried                                 => "neutral",
        };
        if !self.mate_textures.contains_key(key) {
            let base_emotion = match key {
                "happy"    => Emotion::Happy,
                "thinking" => Emotion::Thinking,
                _          => Emotion::Neutral,
            };
            let custom = match base_emotion {
                Emotion::Happy    => self.config.custom_mate_happy.clone(),
                Emotion::Neutral  => self.config.custom_mate_neutral.clone(),
                Emotion::Thinking => self.config.custom_mate_thinking.clone(),
                _                 => None,
            };
            let default_files: &[&str] = match base_emotion {
                Emotion::Happy    => &["MateHappy.png"],
                Emotion::Neutral  => &["MateNeutral.png", "MateNetural.png"],
                Emotion::Thinking => &["MateThinking.png"],
                _                 => &["MateNeutral.png"],
            };
            let path = custom.unwrap_or_else(|| {
                for file in default_files {
                    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src").join("Mate").join(file);
                    if p.exists() { return p; }
                }
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src").join("Mate").join(default_files[0])
            });
            if path.exists() {
                if let Some(ci) = image_from_path(&path) {
                    let handle = ctx.load_texture(key, ci, TextureOptions::LINEAR);
                    self.mate_textures.insert(key.to_string(), handle);
                }
            }
        }
        self.mate_textures.get(key).map(|t| t.id())
    }

    fn handle_keys(&mut self, ctx: &Context) {
        if self.mate_input_focused { return; }
        ctx.input(|i| {
            let suppress_text = i.modifiers.ctrl || i.modifiers.command || i.modifiers.alt;
            for event in &i.events {
                match event {
                    Event::Copy => {
                        self.send_signal("INT");
                        self.send_input("\x03");
                        self.input_buf.clear();
                    }
                    Event::Cut => {
                        self.send_input("\x18");
                        self.input_buf.clear();
                    }
                    Event::Paste(text) => {
                        self.input_buf.push_str(text);
                        self.send_input(text);
                    }
                    Event::Key { key: Key::F, pressed: true, modifiers, .. } if modifiers.alt => {
                        self.search_open = !self.search_open;
                        if !self.search_open { self.search.query.clear(); self.search.matches.clear(); }
                    }
                    Event::Key { key: Key::M, pressed: true, modifiers, .. } if modifiers.alt => {
                        self.set_mate_open(!self.mate_open_target);
                    }
                    Event::Text(t) if !suppress_text => {
                        self.input_buf.push_str(&t);
                        self.send_input(t);
                    }
                    Event::Text(t) if i.modifiers.alt => { self.send_input(&format!("\x1b{t}")); }
                    Event::Key { key: Key::Enter, pressed: true, .. } => { self.send_input("\r"); self.input_buf.clear(); }
                    Event::Key { key: Key::Backspace, pressed: true, .. } => {
                        if !self.input_buf.is_empty() { self.input_buf.pop(); self.send_input("\x7f"); }
                    }
                    Event::Key { key: Key::C, pressed: true, modifiers, .. } if Self::ctrl_or_cmd(*modifiers) && !modifiers.alt => {
                        self.send_signal("INT");
                        self.send_input("\x03");
                        self.input_buf.clear();
                    }
                    Event::Key { key: Key::Z, pressed: true, modifiers, .. } if Self::ctrl_or_cmd(*modifiers) && !modifiers.alt => {
                        self.send_signal("TSTP");
                        self.send_input("\x1a");
                        self.input_buf.clear();
                    }
                    Event::Key { key: Key::Backslash, pressed: true, modifiers, .. } if Self::ctrl_or_cmd(*modifiers) && !modifiers.alt => {
                        self.send_signal("QUIT");
                        self.send_input("\x1c");
                        self.input_buf.clear();
                    }
                    Event::Key { key, pressed: true, modifiers, .. } if Self::ctrl_or_cmd(*modifiers) && !modifiers.alt && !modifiers.shift => {
                        if let Some(code) = Self::key_to_ctrl_byte(*key) {
                            let ch = (code as char).to_string();
                            self.send_input(&ch);
                            if code == 0x03 || code == 0x15 { self.input_buf.clear(); }
                        }
                    }
                    Event::Key { key: Key::Tab,        pressed: true, modifiers, .. } if modifiers.shift => { self.send_input("\x1b[Z"); }
                    Event::Key { key: Key::Tab,        pressed: true, .. } => { self.send_input("\t"); }
                    Event::Key { key: Key::Escape,     pressed: true, .. } => { self.send_input("\x1b"); }
                    Event::Key { key: Key::ArrowUp,    pressed: true, modifiers, .. } if modifiers.ctrl => { self.send_input("\x1b[1;5A"); }
                    Event::Key { key: Key::ArrowDown,  pressed: true, modifiers, .. } if modifiers.ctrl => { self.send_input("\x1b[1;5B"); }
                    Event::Key { key: Key::ArrowRight, pressed: true, modifiers, .. } if modifiers.ctrl => { self.send_input("\x1b[1;5C"); }
                    Event::Key { key: Key::ArrowLeft,  pressed: true, modifiers, .. } if modifiers.ctrl => { self.send_input("\x1b[1;5D"); }
                    Event::Key { key: Key::ArrowUp,    pressed: true, .. } => { self.send_input("\x1b[A"); }
                    Event::Key { key: Key::ArrowDown,  pressed: true, .. } => { self.send_input("\x1b[B"); }
                    Event::Key { key: Key::ArrowLeft,  pressed: true, .. } => { self.send_input("\x1b[D"); }
                    Event::Key { key: Key::ArrowRight, pressed: true, .. } => { self.send_input("\x1b[C"); }
                    Event::Key { key: Key::Home,       pressed: true, .. } => { self.send_input("\x1b[H"); }
                    Event::Key { key: Key::End,        pressed: true, .. } => { self.send_input("\x1b[F"); }
                    Event::Key { key: Key::Delete,     pressed: true, .. } => { self.send_input("\x1b[3~"); }
                    Event::Key { key: Key::PageUp,     pressed: true, .. } => { self.send_input("\x1b[5~"); }
                    Event::Key { key: Key::PageDown,   pressed: true, .. } => { self.send_input("\x1b[6~"); }
                    _ => {}
                }
            }
        });
    }

    fn draw_danger_prompt(&mut self, ctx: &Context) -> bool {
        let Some(dp) = &self.danger_prompt else { return false };
        let (command, reason) = (dp.command.clone(), dp.reason);
        let mut confirmed = false; let mut cancelled = false;
        egui::Window::new("Dangerous Command Detected")
            .collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("WARNING").color(Color32::from_rgb(255, 80, 80)).size(18.0).strong());
                    ui.label(RichText::new("This command seems dangerous...").color(Color32::from_rgb(255, 160, 100)).size(14.0));
                });
                ui.add_space(6.0);
                egui::Frame::none().fill(Color32::from_rgba_unmultiplied(60,15,15,200)).rounding(4.0)
                    .inner_margin(Margin::symmetric(10.0, 8.0)).show(ui, |ui| {
                    ui.label(RichText::new(&command).code().color(Color32::from_rgb(255, 200, 100)));
                });
                ui.add_space(6.0);
                ui.label(RichText::new(reason).color(Color32::from_gray(200)));
                ui.add_space(8.0);
                ui.label(RichText::new("Are you sure you want to run this?").strong());
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.add(egui::Button::new(RichText::new("[y] YES, I KNOW WHAT I'M DOING").color(Color32::from_rgb(255,80,80)).strong())
                        .fill(Color32::from_rgba_unmultiplied(80,20,20,200))).clicked() { confirmed = true; }
                    ui.add_space(8.0);
                    if ui.add(egui::Button::new(RichText::new("[n] No, cancel").color(Color32::WHITE))
                        .fill(Color32::from_rgba_unmultiplied(40,80,40,200))).clicked() { cancelled = true; }
                });
            });
        if confirmed {
            self.command_history.push(command.clone());
            self.history_idx = None;
            self.input_buf.clear();
            self.danger_prompt = None;
            self.send_input("\n");
        }
        else if cancelled {
            self.danger_prompt = None;
            self.input_buf.clear();
            self.replace_terminal_input_line("");
        }
        true
    }

    fn draw_animated_border(&self, painter: &Painter, rect: Rect, t: f32) {
        let c1 = Color32::from(egui::ecolor::Hsva::new(t % 1.0,         0.65, 0.85, 1.0));
        let c2 = Color32::from(egui::ecolor::Hsva::new((t + 0.33) % 1.0, 0.65, 0.85, 1.0));
        let c3 = Color32::from(egui::ecolor::Hsva::new((t + 0.66) % 1.0, 0.65, 0.85, 1.0));
        painter.line_segment([rect.left_top(),     rect.right_top()],    Stroke::new(1.5, c1));
        painter.line_segment([rect.right_top(),    rect.right_bottom()], Stroke::new(1.5, c2));
        painter.line_segment([rect.right_bottom(), rect.left_bottom()],  Stroke::new(1.5, c3));
        painter.line_segment([rect.left_bottom(),  rect.left_top()],     Stroke::new(1.5, c2));
    }

    fn draw_terminal(&mut self, ui: &mut Ui, rect: Rect) {
        let painter = ui.painter_at(rect);
        let bg = if is_hyprland() {
            self.config.theme.bg_alpha((self.config.opacity * 255.0) as u8)
        } else {
            self.config.theme.bg()
        };
        painter.rect_filled(rect, 4.0, bg);
        let border = if self.is_theme_one() {
            Color32::from_rgba_unmultiplied(170, 120, 240, 140)
        } else {
            Color32::from_rgba_unmultiplied(110, 140, 220, 120)
        };
        painter.rect_stroke(rect, 4.0, Stroke::new(1.0, border));
        let glow = if self.is_theme_one() {
            Color32::from_rgba_unmultiplied(160, 90, 240, 30)
        } else {
            Color32::from_rgba_unmultiplied(90, 120, 210, 20)
        };
        painter.rect_filled(Rect::from_min_max(rect.min, pos2(rect.max.x, rect.min.y + 20.0)), 4.0, glow);

        if let Some(tex) = &self.bg_texture {
            let tint = match &self.config.theme.background {
                Background::Image { opacity, .. } | Background::Video { opacity, .. } => {
                    Color32::from_rgba_unmultiplied(255, 255, 255, (opacity * 255.0) as u8)
                }
                _ => Color32::WHITE,
            };
            painter.image(tex.id(), rect, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), tint);
        }

        if self.is_theme_one() {
            painter.rect_filled(rect, 4.0, Color32::from_rgba_unmultiplied(8, 6, 14, 170));
            let star_count = ((rect.width() * rect.height()) / 2600.0).clamp(36.0, 150.0) as usize;
            for i in 0..star_count {
                let fi = i as f32;
                let sx = ((fi * 12.9898 + 78.233).sin() * 43758.547).fract().abs();
                let sy = ((fi * 91.731 + 13.170).sin() * 13579.113).fract().abs();
                let spd = 2.0 + (i % 9) as f32 * 0.35;
                let drift = (self.anim_t * (0.7 + (i % 7) as f32 * 0.08) + fi * 0.31).sin() * 1.6;
                let x = rect.left() + sx * rect.width() + drift;
                let y = rect.top() + (sy * rect.height() + self.anim_t * spd).rem_euclid(rect.height());
                let twinkle = 0.35 + 0.65 * (self.anim_t * (2.1 + (i % 5) as f32 * 0.3) + fi).sin().abs();
                let radius = 0.35 + twinkle * 0.6;
                let alpha = (45.0 + twinkle * 130.0) as u8;
                painter.circle_filled(
                    pos2(x, y),
                    radius,
                    Color32::from_rgba_unmultiplied(212, 194, 252, alpha),
                );
            }
        }

        let theme   = &self.config.theme;
        let font_id = FontId::new(theme.font_size, FontFamily::Monospace);
        let (cw, ch, cx, cy) = (self.cell_w, self.cell_h, self.term.grid.cursor_x, self.term.grid.cursor_y);

        for row_idx in 0..self.term.grid.rows {
            let Some(row) = self.term.grid.visible_row(row_idx) else { continue };
            for col_idx in 0..self.term.grid.cols {
                let Some(cell) = row.get(col_idx) else { continue };
                let x = rect.left() + col_idx as f32 * cw;
                let y = rect.top()  + row_idx  as f32 * ch;
                let cell_rect = Rect::from_min_size(pos2(x, y), vec2(cw, ch));

                let is_match   = self.search.is_match_at(row_idx, col_idx);
                let is_current = self.search.is_current_at(row_idx, col_idx);

                let (mut fg, mut bg_cell) = if cell.attrs.reverse {
                    (cell.bg.resolve(false, theme), cell.fg.resolve(true, theme))
                } else {
                    (cell.fg.resolve(true, theme), cell.bg.resolve(false, theme))
                };

                if is_current     { bg_cell = Color32::from_rgb(255, 200, 0); fg = Color32::BLACK; }
                else if is_match  { bg_cell = Color32::from_rgb(70, 155, 50); fg = Color32::WHITE; }

                if bg_cell != theme.bg() || is_match || is_current {
                    painter.rect_filled(cell_rect, 0.0, bg_cell);
                }

                if row_idx == cy && col_idx == cx && self.cursor_visible {
                    let cc = theme.cursor_color;
                    painter.rect_filled(cell_rect, 2.0, Color32::from_rgba_unmultiplied(cc[0], cc[1], cc[2], 200));
                    painter.rect_stroke(cell_rect, 2.0, Stroke::new(1.0, Color32::from_rgba_unmultiplied(cc[0], cc[1], cc[2], 100)));
                }

                if cell.width == 0 {
                    continue;
                }

                if cell.ch != ' ' && !cell.attrs.invisible {
                    let mut job = text::LayoutJob::default();
                    let mut fmt = TextFormat { font_id: font_id.clone(), color: fg, ..Default::default() };
                    if cell.attrs.underline { fmt.underline     = Stroke::new(1.0, fg); }
                    if cell.attrs.strikeout { fmt.strikethrough = Stroke::new(1.0, fg); }
                    job.append(&cell.ch.to_string(), 0.0, fmt);
                    let galley = ui.ctx().fonts(|f| f.layout_job(job));
                    let y_off = ((ch - galley.size().y) * 0.5).max(0.0);
                    painter.galley(pos2(x, y + y_off), galley, fg);
                }
            }
        }

        if self.terminal_has_focus && !self.input_buf.is_empty() {
            let hint = format!("Typing: {}", self.input_buf);
            painter.text(
                rect.left_bottom() - vec2(0.0, 6.0),
                Align2::LEFT_BOTTOM,
                hint,
                FontId::new(11.0, FontFamily::Proportional),
                Color32::from_rgba_unmultiplied(200, 220, 255, 150),
            );
        }

        for layer in &mut self.applied_layers {
            Self::ensure_layer_texture(layer, ui.ctx());
        }
        self.render_overlay_layers(&painter, rect, &self.applied_layers, None);
        self.render_drawing(&painter, rect, &self.applied_drawing);
    }

    fn draw_search_bar(&mut self, ui: &mut Ui) {
        if !self.search_open { return; }
        egui::Frame::none()
            .fill(Color32::from_rgba_unmultiplied(16, 16, 28, 240))
            .rounding(8.0)
            .stroke(Stroke::new(1.0, Color32::from_rgb(70, 100, 170)))
            .inner_margin(Margin::symmetric(10.0, 6.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Search").color(Color32::from_rgb(130, 160, 230)).size(13.0));
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut self.search.query)
                            .desired_width(220.0)
                            .hint_text("type to search...")
                    );
                    if r.changed() { self.search.search(&self.term.grid.scrollback, &self.term.grid.cells); }
                    let label = if self.search.matches.is_empty() { "no matches".into() }
                                else { format!("{} / {}", self.search.current_idx + 1, self.search.matches.len()) };
                    ui.label(RichText::new(label).color(Color32::from_gray(150)).size(11.0));
                    if ui.small_button("Prev").clicked() { self.search.prev(); }
                    if ui.small_button("Next").clicked() { self.search.next(); }
                    if ui.small_button("X").clicked() {
                        self.search_open = false;
                        self.search.query.clear();
                        self.search.matches.clear();
                    }
                });
            });
    }

    fn draw_floating_bob(&mut self, ctx: &Context) {
        let emotion  = self.mate.emotion;
        let is_open  = self.mate_open_target;
        let anim     = self.mate_open_anim;

        let dot_color = match emotion {
            Emotion::Happy    => Color32::from_rgb(100, 220, 120),
            Emotion::Neutral  => Color32::from_rgb(200, 190, 100),
            Emotion::Thinking => Color32::from_rgb(100, 160, 255),
            Emotion::Curious  => Color32::from_rgb(200, 160, 255),
            Emotion::Worried  => Color32::from_rgb(255, 130, 80),
            Emotion::Excited  => Color32::from_rgb(255, 220, 60),
            Emotion::Confused => Color32::from_rgb(180, 180, 180),
        };

        let bob_w = 430.0 * anim;

        egui::Area::new("bob_float".into())
            .anchor(Align2::RIGHT_TOP, vec2(-10.0, 42.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::none()
                    .fill(Color32::from_rgba_unmultiplied(0, 0, 0, 0))
                    .rounding(16.0)
                    .stroke(Stroke::new(1.8, Color32::from_rgba_unmultiplied(85, 110, 210, 220)))
                    .inner_margin(Margin::symmetric(14.0, 12.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let pulse = (self.anim_t * 3.0).sin() * 0.15 + 0.85;
                            let (r, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                            ui.painter().circle_filled(r.center(), 6.0, dot_color.linear_multiply(pulse));
                            ui.label(RichText::new(&self.config.mate_name).strong().size(14.0).color(Color32::from_rgb(140, 200, 255)));
                            ui.add_space(8.0);
                            let (ai_label, ai_color) = if self.config.ai_enabled {
                                ("AI:ON",  Color32::from_rgb(90, 210, 120))
                            } else {
                                ("AI:OFF", Color32::from_rgb(210, 80, 80))
                            };
                            if ui.add_sized(
                                vec2(78.0, 24.0),
                                egui::Button::new(RichText::new(ai_label).color(ai_color).size(11.0))
                            ).clicked() {
                                if self.config.ai_enabled { self.disable_ai(); } else { self.ai_enable_prompt_open = true; }
                            }
                            let toggle_label = if is_open { "X" } else { "+" };
                            if ui.add_sized(vec2(30.0, 24.0), egui::Button::new(toggle_label)).clicked() {
                                self.set_mate_open(!is_open);
                            }
                        });

                        if anim > 0.1 {
                            ui.add_space(4.0);
                            ui.set_min_width(bob_w.max(340.0));
                            ui.set_max_width(bob_w.max(340.0));

                            let texture_id = self.mate_texture(ctx, emotion);

                            ui.horizontal(|ui| {
                                if let Some(tid) = texture_id {
                                    let side = (bob_w * 0.30).clamp(70.0, 110.0);
                                    let (resp, painter) = ui.allocate_painter(vec2(side, side), Sense::hover());
                                    painter.image(tid, resp.rect, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
                                }
                                ui.vertical(|ui| {
                                    egui::Frame::none()
                                        .fill(Color32::from_rgba_unmultiplied(24, 28, 52, 230))
                                        .rounding(Rounding { nw: 2.0, ne: 10.0, sw: 10.0, se: 10.0 })
                                        .inner_margin(Margin::symmetric(8.0, 6.0))
                                        .show(ui, |ui| {
                                            ui.set_max_width((bob_w * 0.60).max(150.0));
                                            let typed = self.mate.typed_text().to_string();
                                            let display = if self.mate.is_typing() {
                                                format!("{typed}▍")
                                            } else {
                                                typed
                                            };
                                            ui.label(RichText::new(display).color(Color32::from_gray(225)).size(11.5));
                                        });
                                });
                            });

                            ui.add_space(4.0);

                            ui.horizontal(|ui| {
                                let chat = self.mate.view == MateView::Chat;
                                if ui.selectable_label(chat,  "Chat").clicked()  { self.mate.view = MateView::Chat; }
                                if ui.selectable_label(!chat, "Saved").clicked() { self.mate.view = MateView::SavedCommands; }
                            });
                            ui.add_space(4.0);

                            match self.mate.view {
                                MateView::Chat          => self.draw_bob_chat(ui, ctx, bob_w),
                                MateView::SavedCommands => self.draw_saved_commands(ui),
                            }
                        }
                    });
            });
    }

    fn draw_bob_chat(&mut self, ui: &mut Ui, _ctx: &Context, _panel_w: f32) {
        let mut any_focused = false;

        egui::ScrollArea::vertical()
            .id_source("bob_chat_hist")
            .max_height(120.0)
            .stick_to_bottom(true)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for msg in &self.mate.chat_history {
                    let (prefix, color) = if msg.role == "user" {
                        ("you", Color32::from_rgb(130, 210, 130))
                    } else {
                        ("bob", Color32::from_rgb(120, 170, 255))
                    };
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(prefix).strong().color(color).size(11.0));
                        ui.label(RichText::new(&msg.content).color(Color32::from_gray(200)).size(11.0));
                    });
                    ui.add_space(2.0);
                }
            });

        ui.add_space(4.0);

        ui.label(RichText::new("file path:").size(10.0).color(Color32::from_gray(120)));
        let attach_resp = ui.add(
            egui::TextEdit::singleline(&mut self.mate.attach_path)
                .desired_width(f32::INFINITY)
                .hint_text("/path/to/file")
                .font(FontId::proportional(11.0))
        );
        if attach_resp.has_focus() { any_focused = true; }

        ui.add_space(3.0);
        ui.separator();
        ui.add_space(2.0);

        let text_resp = ui.add(
            egui::TextEdit::multiline(&mut self.mate.input_text)
                .desired_rows(2)
                .desired_width(f32::INFINITY)
                .hint_text("message Bob... or type \"customize\"")
                .font(FontId::proportional(11.5))
        );
        if text_resp.has_focus() { any_focused = true; }

        let send_clicked = ui.button("send").clicked();
        let enter_pressed = text_resp.has_focus() && ui.input(|i| i.key_pressed(Key::Enter) && !i.modifiers.shift);

        if send_clicked || enter_pressed {
            let msg = self.mate.input_text.trim().to_string();
            if !msg.is_empty() {
                self.mate.input_text.clear();

                let mut full_msg = msg.clone();

                let attach = self.mate.attach_path.trim().to_string();
                if !attach.is_empty() {
                    let path = std::path::Path::new(&attach);
                    if path.exists() {
                        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
                        let is_image = matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp");
                        let is_video = matches!(ext.as_str(), "mp4" | "mkv" | "avi" | "mov" | "webm");
                        if is_image || is_video {
                            full_msg.push_str(&format!("\n\n[attached {} file: {}]", if is_video { "video" } else { "image" }, attach));
                            if is_video {
                                full_msg.push_str("\n(video attached - analyze based on filename, extension, and any metadata you can infer)");
                            } else {
                                full_msg.push_str("\n(image attached - describe what you know about this file type and what the user might want)");
                            }
                        } else {
                            match std::fs::read_to_string(path) {
                                Ok(content) => {
                                    let preview: String = content.chars().take(4000).collect();
                                    full_msg.push_str(&format!("\n\n[file: {}]\n```\n{}\n```", attach, preview));
                                    if content.len() > 4000 {
                                        full_msg.push_str(&format!("\n(file truncated, {} total chars)", content.len()));
                                    }
                                }
                                Err(e) => {
                                    full_msg.push_str(&format!("\n\n[could not read file {}: {}]", attach, e));
                                }
                            }
                        }
                        self.mate.attach_path.clear();
                    } else {
                        full_msg.push_str(&format!("\n\n[file not found: {}]", attach));
                        self.mate.attach_path.clear();
                    }
                }

                if let Some(cmd) = msg.strip_prefix("/run ").map(str::trim).filter(|c| !c.is_empty()) {
                    self.execute_command(cmd.to_string());
                    let ran = format!("ran: {cmd}");
                    self.mate.last_message = ran.clone();
                    self.mate.typing_target = ran;
                    self.mate.typing_chars = 0;
                    self.mate.typing_tick = Instant::now();
                } else if let Some(ctrl) = msg.strip_prefix("/ctrl ").map(str::trim) {
                    let normalized = ctrl.to_ascii_lowercase();
                    let out = match normalized.as_str() {
                        "c" | "+c" | "ctrl+c" => {
                            self.send_signal("INT");
                            self.send_input("\x03");
                            "sent Ctrl+C (SIGINT)".to_string()
                        }
                        "z" | "+z" | "ctrl+z" => {
                            self.send_signal("TSTP");
                            self.send_input("\x1a");
                            "sent Ctrl+Z (SIGTSTP)".to_string()
                        }
                        "\\" | "+\\" | "ctrl+\\" => {
                            self.send_signal("QUIT");
                            self.send_input("\x1c");
                            "sent Ctrl+\\ (SIGQUIT)".to_string()
                        }
                        _ => "unknown /ctrl action. use: /ctrl c, /ctrl z, /ctrl \\".to_string(),
                    };
                    self.mate.last_message = out.clone();
                    self.mate.typing_target = out;
                    self.mate.typing_chars = 0;
                    self.mate.typing_tick = Instant::now();
                } else if let Some(sig) = msg.strip_prefix("/signal ").map(str::trim) {
                    let signal = sig.to_ascii_uppercase();
                    let out = match signal.as_str() {
                        "INT" | "SIGINT" => {
                            self.send_signal("INT");
                            self.send_input("\x03");
                            "sent SIGINT".to_string()
                        }
                        "TSTP" | "SIGTSTP" => {
                            self.send_signal("TSTP");
                            self.send_input("\x1a");
                            "sent SIGTSTP".to_string()
                        }
                        "QUIT" | "SIGQUIT" => {
                            self.send_signal("QUIT");
                            self.send_input("\x1c");
                            "sent SIGQUIT".to_string()
                        }
                        _ => "unknown signal. use: INT, TSTP, QUIT".to_string(),
                    };
                    self.mate.last_message = out.clone();
                    self.mate.typing_target = out;
                    self.mate.typing_chars = 0;
                    self.mate.typing_tick = Instant::now();
                } else {
                    let is_customize = msg.trim().eq_ignore_ascii_case("customize");
                    let term = self.terminal_context();
                    if !term.is_empty() {
                        full_msg.push_str("\n\n[terminal context]\n");
                        full_msg.push_str(&term);
                    }
                    self.mate.send_message(full_msg);
                    if is_customize { self.customize = Some(CustomizeState::from_config(&self.config)); }
                }
            }
        }

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(2.0);

        ui.label(RichText::new("save a command:").size(10.0).color(Color32::from_gray(120)));
        let r1 = ui.add(
            egui::TextEdit::singleline(&mut self.mate.save_box_text)
                .desired_width(f32::INFINITY)
                .hint_text("command...")
                .font(FontId::proportional(11.0))
        );
        let r2 = ui.add(
            egui::TextEdit::singleline(&mut self.mate.save_desc_text)
                .desired_width(f32::INFINITY)
                .hint_text("description (optional)")
                .font(FontId::proportional(11.0))
        );
        if r1.has_focus() || r2.has_focus() { any_focused = true; }
        if ui.button("save").clicked() { self.mate.save_command(); }

        self.mate_input_focused = any_focused;
        if any_focused { self.terminal_has_focus = false; }
    }

    fn draw_mate_panel(&mut self, ui: &mut Ui, ctx: &Context, panel_w: f32) {
        let emotion    = self.mate.emotion;
        let texture_id = self.mate_texture(ctx, emotion);

        egui::Frame::none()
            .fill(Color32::from_rgba_unmultiplied(13, 13, 22, 248))
            .rounding(Rounding::same(12.0))
            .stroke(Stroke::new(1.5, Color32::from_rgba_unmultiplied(65, 85, 155, 210)))
            .inner_margin(Margin::symmetric(10.0, 10.0))
            .show(ui, |ui| {
                ui.set_min_width(panel_w.max(280.0));
                ui.set_max_width(panel_w);
                self.draw_bob_chat(ui, ctx, panel_w);
                let _ = texture_id;
            });
    }

    fn draw_saved_commands(&mut self, ui: &mut Ui) {
        let mut filter = String::new();
        let fr = ui.add(
            egui::TextEdit::singleline(&mut filter)
                .desired_width(f32::INFINITY)
                .hint_text("filter commands...")
                .font(FontId::proportional(12.0))
        );
        if fr.has_focus() { self.mate_input_focused = true; }

        let cmds: Vec<_> = self.mate.commands.search(&filter)
            .iter().map(|c| (c.id, c.command.clone(), c.description.clone())).collect();

        egui::ScrollArea::vertical().max_height(340.0).show(ui, |ui| {
            for (id, cmd, desc) in &cmds {
                egui::Frame::none()
                    .fill(Color32::from_rgba_unmultiplied(20, 26, 46, 220))
                    .rounding(6.0)
                    .stroke(Stroke::new(1.0, Color32::from_rgba_unmultiplied(50, 60, 100, 130)))
                    .inner_margin(Margin::symmetric(8.0, 5.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui.small_button("Run").clicked() {
                                let c = cmd.clone();
                                self.mate.commands.increment_use(*id);
                                self.execute_command(c);
                            }
                            if ui.small_button("Del").clicked() { self.mate.delete_saved(*id); }
                            ui.label(RichText::new(cmd).code().color(Color32::from_rgb(165, 220, 125)).size(11.0));
                        });
                        if !desc.is_empty() {
                            ui.label(RichText::new(desc).color(Color32::from_gray(140)).size(10.0));
                        }
                    });
                ui.add_space(3.0);
            }
        });
    }

    fn draw_title_bar(&self, ui: &mut Ui, t: f32) {
        let accent = if self.is_theme_one() {
            Color32::from_rgb(200, 145, 255)
        } else {
            Color32::from(egui::ecolor::Hsva::new((t * 0.06) % 1.0, 0.5, 0.9, 1.0))
        };

        egui::Frame::none()
            .fill(if self.is_theme_one() {
                Color32::from_rgba_unmultiplied(22, 16, 36, 255)
            } else {
                Color32::from_rgba_unmultiplied(10, 12, 20, 255)
            })
            .stroke(Stroke::new(1.0, if self.is_theme_one() {
                Color32::from_rgba_unmultiplied(170, 120, 240, 120)
            } else {
                Color32::from_rgba_unmultiplied(90, 120, 200, 90)
            }))
            .inner_margin(Margin::symmetric(12.0, 6.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for color in [
                        Color32::from_rgb(255, 95, 86),
                        Color32::from_rgb(255, 189, 46),
                        Color32::from_rgb(39, 201, 63),
                    ] {
                        let (rect, _) = ui.allocate_exact_size(Vec2::splat(13.0), Sense::hover());
                        ui.painter().circle_filled(rect.center(), 6.5, color.linear_multiply(0.85));
                        ui.add_space(3.0);
                    }

                    ui.add_space(8.0);
                    ui.label(RichText::new(&self.term.title).color(Color32::from_gray(195)).size(13.0));

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new("Spiltixal").color(accent).size(12.0).strong());
                        ui.add_space(6.0);
                        ui.label(RichText::new(APP_VERSION).color(Color32::from_gray(160)).size(10.0));
                        ui.add_space(8.0);
                        if self.nerd_font_loaded {
                            ui.add_space(6.0);
                            ui.label(RichText::new("NF").color(Color32::from_rgb(80, 170, 80)).size(10.0));
                        }
                        if is_hyprland() {
                            ui.add_space(6.0);
                            ui.label(RichText::new("Hyprland").color(Color32::from_rgb(90, 175, 220)).size(10.0));
                        }
                    });
                });
            });
    }
}

impl eframe::App for Spiltixal {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        let high_motion = self.is_theme_one() || self.mate.is_typing() || (self.mate_open_anim > 0.0 && self.mate_open_anim < 1.0);
        ctx.request_repaint_after(if high_motion { Duration::from_millis(33) } else { Duration::from_millis(90) });

        if ctx.input(|i| i.pointer.primary_clicked()) {
            if let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) {
                if self.terminal_rect.is_some_and(|r| r.contains(pos)) {
                    self.terminal_has_focus = true;
                } else if self.mate_rect.is_some_and(|r| r.contains(pos)) {
                    self.terminal_has_focus = false;
                }
            }
        }

        self.poll_pty();
        if let Some(pty) = &mut self.pty {
            if !pty.is_alive() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
        }
        self.mate.poll_ai();
        self.mate.tick_typing();
        self.update_cursor_blink();
        self.animate_mate_panel();
        self.enforce_ai_ram_limit();
        self.ensure_background_texture(ctx);
        if self.last_metrics_update.elapsed() >= Duration::from_millis(220) {
            self.update_cell_metrics(ctx);
            self.last_metrics_update = Instant::now();
        }

        self.anim_t = ctx.input(|i| i.time) as f32;

        let bg = if is_hyprland() {
            self.config.theme.bg_alpha((self.config.opacity * 255.0) as u8)
        } else {
            self.config.theme.bg()
        };

        if self.draw_danger_prompt(ctx) { return; }
        if self.draw_first_launch_prompt(ctx) { return; }
        if self.draw_ai_enable_prompt(ctx) { return; }

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(Color32::TRANSPARENT))
            .show(ctx, |ui| {
                let full_rect = ui.max_rect();
                ui.painter().rect_filled(full_rect, 0.0, bg);
                if self.is_theme_one() {
                    self.draw_animated_border(ui.painter(), full_rect, self.anim_t * 0.04);
                } else {
                    ui.painter().rect_stroke(full_rect, 0.0, Stroke::new(1.0, Color32::from_rgba_unmultiplied(70, 95, 170, 70)));
                }

                ui.vertical(|ui| {
                    self.draw_title_bar(ui, self.anim_t);

                    self.draw_search_bar(ui);
                    let term_rect = ui.available_rect_before_wrap();
                    self.terminal_rect = Some(term_rect);
                    self.sync_terminal_size(term_rect);
                    self.handle_terminal_scroll(ctx);
                    self.draw_terminal(ui, term_rect);
                    let term_resp = ui.allocate_rect(term_rect, Sense::click());
                    if term_resp.clicked() { self.terminal_has_focus = true; }
                    if self.customize.as_ref().is_some_and(|s| s.open) {
                        self.draw_customize_editor(ctx, term_rect);
                    }
                });
            });

        self.draw_floating_bob(ctx);
        self.handle_keys(ctx);
    }
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    let hyprland = is_hyprland();

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Spiltixal")
            .with_inner_size([1280.0, 780.0])
            .with_min_inner_size([640.0, 420.0])
            .with_transparent(hyprland),
        ..Default::default()
    };

    eframe::run_native("Spiltixal", native_options, Box::new(|cc| Box::new(Spiltixal::new(cc))))
        .map_err(|e| anyhow::anyhow!("eframe: {e}"))
}
