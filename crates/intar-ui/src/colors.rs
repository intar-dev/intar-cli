use crossterm::tty::IsTty;
use ratatui::style::Color;
use std::env;
use std::io::{self, Write};
use std::time::Duration;

#[cfg(unix)]
use std::time::Instant;

#[cfg(unix)]
use std::thread::sleep;

#[cfg(unix)]
use nix::fcntl::{FcntlArg, OFlag, fcntl};
#[cfg(unix)]
use nix::unistd::read;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeMode {
    Dark,
    Light,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorLevel {
    None,
    Ansi16,
    Ansi256,
    TrueColor,
}

#[derive(Clone, Copy, Debug)]
pub struct ThemeSettings {
    pub mode: ThemeMode,
    pub color_level: ColorLevel,
}

pub struct Theme {
    pub bg: Color,
    pub fg: Color,
    pub primary: Color, // Main accent
    pub on_primary: Color,
    pub secondary: Color, // Secondary accent
    pub on_secondary: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub info: Color,
    pub dim: Color,
    pub border: Color,
    pub highlight: Color,
    pub surface: Color,
    pub color_level: ColorLevel,
}

impl Default for Theme {
    fn default() -> Self {
        Self::for_mode(ThemeMode::Dark, ColorLevel::Ansi16)
    }
}

impl ThemeSettings {
    #[must_use]
    pub fn from_env() -> Self {
        let color_level = ColorLevel::detect();
        let mode = ThemeMode::from_env_override().unwrap_or(ThemeMode::Dark);
        Self { mode, color_level }
    }

    #[must_use]
    pub fn resolve() -> Self {
        let color_level = ColorLevel::detect();
        let mode = ThemeMode::resolve(color_level);
        Self { mode, color_level }
    }
}

impl ThemeMode {
    #[must_use]
    fn from_env_override() -> Option<Self> {
        env_theme_override("INTAR_THEME").or_else(|| env_theme_override("CLITHEME"))
    }

    #[must_use]
    fn resolve(color_level: ColorLevel) -> Self {
        if let Some(mode) = Self::from_env_override() {
            return mode;
        }

        if color_level == ColorLevel::None {
            return ThemeMode::Dark;
        }

        detect_theme_mode().unwrap_or(ThemeMode::Dark)
    }

    #[must_use]
    pub fn toggle(self) -> Self {
        match self {
            ThemeMode::Dark => ThemeMode::Light,
            ThemeMode::Light => ThemeMode::Dark,
        }
    }
}

impl ColorLevel {
    #[must_use]
    pub fn detect() -> Self {
        if env::var_os("NO_COLOR").is_some() {
            return ColorLevel::None;
        }

        if !io::stdout().is_tty() {
            return ColorLevel::None;
        }

        let colorterm = env::var("COLORTERM")
            .unwrap_or_default()
            .to_ascii_lowercase();
        if colorterm.contains("truecolor") || colorterm.contains("24bit") {
            return ColorLevel::TrueColor;
        }

        let term = env::var("TERM").unwrap_or_default();
        if term.contains("256color") {
            return ColorLevel::Ansi256;
        }

        ColorLevel::Ansi16
    }
}

struct ThemePalette {
    bg: Color,
    fg: Color,
    primary: Color,
    on_primary: Color,
    secondary: Color,
    on_secondary: Color,
    success: Color,
    warning: Color,
    error: Color,
    info: Color,
    dim: Color,
    border: Color,
    highlight: Color,
    surface: Color,
}

impl ThemePalette {
    fn into_theme(self, color_level: ColorLevel) -> Theme {
        Theme {
            bg: self.bg,
            fg: self.fg,
            primary: self.primary,
            on_primary: self.on_primary,
            secondary: self.secondary,
            on_secondary: self.on_secondary,
            success: self.success,
            warning: self.warning,
            error: self.error,
            info: self.info,
            dim: self.dim,
            border: self.border,
            highlight: self.highlight,
            surface: self.surface,
            color_level,
        }
    }
}

impl Theme {
    #[must_use]
    pub fn for_mode(mode: ThemeMode, color_level: ColorLevel) -> Self {
        let palette = match (mode, color_level) {
            (ThemeMode::Dark, ColorLevel::TrueColor) => palette_dark_truecolor(),
            (ThemeMode::Dark, ColorLevel::Ansi256) => palette_dark_ansi256(),
            (ThemeMode::Dark, ColorLevel::Ansi16) => palette_dark_ansi16(),
            (ThemeMode::Light, ColorLevel::TrueColor) => palette_light_truecolor(),
            (ThemeMode::Light, ColorLevel::Ansi256) => palette_light_ansi256(),
            (ThemeMode::Light, ColorLevel::Ansi16) => palette_light_ansi16(),
            (_, ColorLevel::None) => palette_none(),
        };

        palette.into_theme(color_level)
    }

    #[must_use]
    pub fn is_monochrome(&self) -> bool {
        self.color_level == ColorLevel::None
    }
}

fn palette_none() -> ThemePalette {
    ThemePalette {
        bg: Color::Reset,
        fg: Color::Reset,
        primary: Color::Reset,
        on_primary: Color::Reset,
        secondary: Color::Reset,
        on_secondary: Color::Reset,
        success: Color::Reset,
        warning: Color::Reset,
        error: Color::Reset,
        info: Color::Reset,
        dim: Color::Reset,
        border: Color::Reset,
        highlight: Color::Reset,
        surface: Color::Reset,
    }
}

fn palette_dark_truecolor() -> ThemePalette {
    ThemePalette {
        bg: Color::Rgb(20, 20, 20),
        fg: Color::Rgb(230, 230, 230),
        primary: Color::Rgb(255, 184, 108),
        on_primary: Color::Rgb(20, 20, 20),
        secondary: Color::Rgb(98, 114, 164),
        on_secondary: Color::Rgb(230, 230, 230),
        success: Color::Rgb(80, 250, 123),
        warning: Color::Rgb(241, 250, 140),
        error: Color::Rgb(255, 85, 85),
        info: Color::Rgb(139, 233, 253),
        dim: Color::Rgb(68, 71, 90),
        border: Color::Rgb(98, 114, 164),
        highlight: Color::Rgb(68, 71, 90),
        surface: Color::Rgb(40, 42, 54),
    }
}

fn palette_dark_ansi256() -> ThemePalette {
    ThemePalette {
        bg: Color::Indexed(235),
        fg: Color::Indexed(252),
        primary: Color::Indexed(179),
        on_primary: Color::Indexed(235),
        secondary: Color::Indexed(103),
        on_secondary: Color::Indexed(252),
        success: Color::Indexed(78),
        warning: Color::Indexed(220),
        error: Color::Indexed(203),
        info: Color::Indexed(81),
        dim: Color::Indexed(241),
        border: Color::Indexed(103),
        highlight: Color::Indexed(237),
        surface: Color::Indexed(236),
    }
}

fn palette_dark_ansi16() -> ThemePalette {
    ThemePalette {
        bg: Color::Black,
        fg: Color::White,
        primary: Color::Yellow,
        on_primary: Color::Black,
        secondary: Color::Cyan,
        on_secondary: Color::Black,
        success: Color::LightGreen,
        warning: Color::Yellow,
        error: Color::LightRed,
        info: Color::LightCyan,
        dim: Color::DarkGray,
        border: Color::DarkGray,
        highlight: Color::DarkGray,
        surface: Color::Black,
    }
}

fn palette_light_truecolor() -> ThemePalette {
    ThemePalette {
        bg: Color::Rgb(250, 250, 250),
        fg: Color::Rgb(30, 30, 30),
        primary: Color::Rgb(200, 100, 0),
        on_primary: Color::Rgb(250, 250, 250),
        secondary: Color::Rgb(100, 100, 120),
        on_secondary: Color::Rgb(250, 250, 250),
        success: Color::Rgb(0, 120, 0),
        warning: Color::Rgb(200, 150, 0),
        error: Color::Rgb(200, 0, 0),
        info: Color::Rgb(0, 100, 200),
        dim: Color::Rgb(150, 150, 150),
        border: Color::Rgb(210, 210, 210),
        highlight: Color::Rgb(220, 220, 220),
        surface: Color::Rgb(245, 245, 245),
    }
}

fn palette_light_ansi256() -> ThemePalette {
    ThemePalette {
        bg: Color::Indexed(231),
        fg: Color::Indexed(235),
        primary: Color::Indexed(166),
        on_primary: Color::Indexed(231),
        secondary: Color::Indexed(245),
        on_secondary: Color::Indexed(231),
        success: Color::Indexed(28),
        warning: Color::Indexed(178),
        error: Color::Indexed(160),
        info: Color::Indexed(25),
        dim: Color::Indexed(250),
        border: Color::Indexed(252),
        highlight: Color::Indexed(254),
        surface: Color::Indexed(255),
    }
}

fn palette_light_ansi16() -> ThemePalette {
    ThemePalette {
        bg: Color::White,
        fg: Color::Black,
        primary: Color::Blue,
        on_primary: Color::White,
        secondary: Color::Magenta,
        on_secondary: Color::White,
        success: Color::Green,
        warning: Color::LightRed,
        error: Color::Red,
        info: Color::Cyan,
        dim: Color::DarkGray,
        border: Color::Gray,
        highlight: Color::Gray,
        surface: Color::White,
    }
}

fn env_theme_override(var: &str) -> Option<ThemeMode> {
    let value = env::var(var).ok()?;
    match value.trim().to_ascii_lowercase().as_str() {
        "dark" => Some(ThemeMode::Dark),
        "light" => Some(ThemeMode::Light),
        _ => None,
    }
}

fn detect_theme_mode() -> Option<ThemeMode> {
    if !io::stdin().is_tty() || !io::stdout().is_tty() {
        return None;
    }

    if let Some(rgb) =
        query_osc_11(Duration::from_millis(120)).and_then(|resp| parse_rgb_response(&resp))
    {
        let luminance =
            (0.2126 * f64::from(rgb.0) + 0.7152 * f64::from(rgb.1) + 0.0722 * f64::from(rgb.2))
                / 255.0;
        return Some(if luminance > 0.55 {
            ThemeMode::Light
        } else {
            ThemeMode::Dark
        });
    }

    theme_from_colorfgbg()
}

fn theme_from_colorfgbg() -> Option<ThemeMode> {
    let value = env::var("COLORFGBG").ok()?;
    let bg = value.split(';').next_back()?;
    let bg = bg.parse::<u8>().ok()?;
    Some(if bg <= 6 || bg == 8 {
        ThemeMode::Dark
    } else {
        ThemeMode::Light
    })
}

fn parse_rgb_response(response: &str) -> Option<(u8, u8, u8)> {
    let idx = response.find("rgb:")?;
    let rgb_part = &response[idx + 4..];
    let end = rgb_part
        .find(|c: char| !(c.is_ascii_hexdigit() || c == '/'))
        .unwrap_or(rgb_part.len());
    let rgb_part = &rgb_part[..end];
    let mut iter = rgb_part.split('/');
    let r = parse_rgb_component(iter.next()?)?;
    let g = parse_rgb_component(iter.next()?)?;
    let b = parse_rgb_component(iter.next()?)?;
    Some((r, g, b))
}

fn parse_rgb_component(hex: &str) -> Option<u8> {
    let len = hex.len();
    if len == 0 || len > 4 {
        return None;
    }
    let value = u32::from_str_radix(hex, 16).ok()?;
    let max = (1u32 << (len * 4)) - 1;
    let scaled = (value * 255 + (max / 2)) / max;
    u8::try_from(scaled).ok()
}

fn query_osc_11(timeout: Duration) -> Option<String> {
    #[cfg(unix)]
    {
        let mut stdout = io::stdout();
        stdout.write_all(b"\x1b]11;?\x07").ok()?;
        stdout.flush().ok()?;

        let stdin = io::stdin();
        let flags = fcntl(&stdin, FcntlArg::F_GETFL).ok()?;
        let old_flags = OFlag::from_bits_truncate(flags);
        let _ = fcntl(&stdin, FcntlArg::F_SETFL(old_flags | OFlag::O_NONBLOCK));

        let start = Instant::now();
        let mut buf = Vec::new();
        let mut tmp = [0u8; 128];

        while start.elapsed() < timeout && buf.len() < 1024 {
            match read(&stdin, &mut tmp) {
                Ok(n) if n > 0 => {
                    buf.extend_from_slice(&tmp[..n]);
                    if response_complete(&buf) {
                        break;
                    }
                }
                Ok(_) | Err(_) => {}
            }
            sleep(Duration::from_millis(5));
        }

        let _ = fcntl(&stdin, FcntlArg::F_SETFL(old_flags));

        if buf.is_empty() {
            return None;
        }

        String::from_utf8(buf).ok()
    }

    #[cfg(not(unix))]
    {
        let _ = timeout;
        None
    }
}

#[cfg(unix)]
fn response_complete(buf: &[u8]) -> bool {
    if buf.contains(&0x07) {
        return true;
    }
    buf.windows(2).any(|w| w == [0x1b, b'\\'])
}
