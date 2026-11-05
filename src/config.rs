//! `~/.config/niruui/lock.toml`
//!
//! The NIRU desktop programs share one directory for their settings so they
//! can be found in one place, but each program has its own file and never
//! reads the others'. That means installing one, two or all of them is fine.

use crate::{palette, Result};
use std::path::Path;

pub const DEFAULT_PATH: &str = "lock.toml";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    Minimal,
    Terminal,
    Runic,
    Corner,
    Cava,
}

impl Layout {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "minimal" => Some(Self::Minimal),
            "terminal" => Some(Self::Terminal),
            "runic" => Some(Self::Runic),
            "corner" => Some(Self::Corner),
            "cava" => Some(Self::Cava),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Terminal => "terminal",
            Self::Runic => "runic",
            Self::Corner => "corner",
            Self::Cava => "cava",
        }
    }
}

/// How the prompt echoes the password entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Echo {
    /// One `*` per character (the default and the earlier behaviour).
    Count,
    /// A fixed-width indicator that moves on every key and never shows the
    /// length.
    Pulse,
}

impl Echo {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "count" => Some(Self::Count),
            "pulse" => Some(Self::Pulse),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Count => "count",
            Self::Pulse => "pulse",
        }
    }
}

/// The shared screensaver settings, the same file `niruwm/preferences.rs`
/// reads for the idle ladder.
///
/// niruwm draws its own lock screen when the session uses the suite's window
/// manager instead of niruidle. Reading the same numbers means changing the
/// lock delay once changes it everywhere.
pub const LADDER_PATH: &str = "screensaver.toml";
/// The appearance contract's default `warning` colour.
pub const DEFAULT_WARNING: u32 = 0xd7b36a;

#[derive(Clone, Debug)]
pub struct Config {
    pub font_path: Option<String>,
    pub rune_font_path: Option<String>,
    pub font_size: f32,
    pub layout: Layout,
    /// Draw the CAVA spectrum in the `cava` layout.
    pub visualizer: bool,
    /// What the `cava` layout's panel shows: `auto` (the spectrum while
    /// music plays, quotes otherwise), `cava`, `quotes` or `none`.
    /// `visualizer = false` without `visual` means `none`.
    pub visual: crate::visual::Mode,
    pub palette: palette::Palette,
    /// Colour of failure feedback. Kept out of `Palette`, which is shared
    /// with NIRUSCREENSAVER: the appearance contract's `warning`, or
    /// `[palette] warning` in lock.toml.
    pub warning: u32,
    pub prompt_echo: Echo,
    /// Deprecated compatibility input. Never used to dismiss a lock.
    pub lock_timeout_minutes: u64,
}

/// The built-in suite themes, for a component that does not carry the
/// theme files itself. Only the text colours are used: the lock screen keeps an
/// opaque black background so nothing behind it can show through.
fn builtin_palette(name: &str) -> Option<palette::Palette> {
    let (foreground, muted, border, focus) = match name {
        "niru-noir" => (0xe5e5e5, 0xa8a8a8, 0x2a2a2d, 0x5a5a60),
        "niru-nord" => (0xeceff4, 0x9aa5b1, 0x3b4252, 0x88c0d0),
        "niru-satie" => (0xdedad4, 0x8f8b86, 0x2a2724, 0xb08d7d),
        "niru-carl-larsson" => (0x3b3228, 0x7d6f5f, 0xd8cdbb, 0xa63d2f),
        "niru-forest-vigil" => (0xd8ded2, 0x96a292, 0x2b362c, 0x8aa87e),
        "niru-blood-of-urdr" => (0xe8d6cf, 0xb09a92, 0x3a1d1b, 0xc96050),
        "niru-root-and-stone" => (0xeadfd2, 0xb5a08c, 0x3a2a1e, 0xc19768),
        "niru-blue-horizon" => (0xdce6eb, 0x93a6b2, 0x243846, 0x86aec8),
        "niru-the-norns" => (0xece9e4, 0xa6a19b, 0x3b3936, 0xc9c2b9),
        "niru-winter-nights" => (0xead9c3, 0xb9aa9a, 0x4e5157, 0xd8c4aa),
        _ => return None,
    };
    Some(palette::Palette {
        background: 0x000000,
        foreground,
        muted,
        border,
        focus,
    })
}

/// Versioned appearance contract. Invalid input is ignored; the working theme
/// stays in effect. The lock always renders on opaque black.
fn appearance_value(base: &Path) -> Option<toml::Value> {
    let path = base.join("niruwm/appearance.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            eprintln!("nirulock: {}: {e}", path.display());
            return None;
        }
    };
    let value: toml::Value = match text.parse() {
        Ok(value) => value,
        Err(e) => {
            eprintln!("nirulock: {}: {e}", path.display());
            return None;
        }
    };
    if value.get("schema").and_then(|v| v.as_integer()) != Some(1) {
        eprintln!(
            "nirulock: {}: unsupported appearance schema",
            path.display()
        );
        return None;
    }
    if let Some(colors) = value.get("colors") {
        let Some(colors) = colors.as_table() else {
            eprintln!("nirulock: {}: colors must be a table", path.display());
            return None;
        };
        for colour in colors.values() {
            let Some(text) = colour.as_str() else {
                eprintln!("nirulock: {}: colours must be strings", path.display());
                return None;
            };
            if palette::parse_hex(text).is_none() {
                eprintln!("nirulock: {}: invalid colour {text:?}", path.display());
                return None;
            }
        }
    }
    if let Some(size) = value.get("font").and_then(|font| font.get("size")) {
        if !size
            .as_float()
            .is_some_and(|size| size.is_finite() && size > 0.0)
        {
            eprintln!("nirulock: {}: font.size must be positive", path.display());
            return None;
        }
    }
    Some(value)
}

fn appearance_colour(appearance: &toml::Value, key: &str) -> Option<String> {
    appearance
        .get("colors")?
        .get(key)?
        .as_str()
        .map(str::to_string)
}

/// Read the suite's selected theme without requiring it to exist.
///
/// A user theme in `~/.config/niruwm/themes/NAME.toml` wins over the built-in
/// palette, exactly as it does in NIRUWM and NIRUBAR. The background stays
/// black for the locker; only the content colours follow the theme.
fn suite_palette_from(base: &Path) -> Option<palette::Palette> {
    let config = std::fs::read_to_string(base.join("niruwm/config.toml")).ok()?;
    let value: toml::Value = config.parse().ok()?;
    let name = value
        .get("theme")
        .and_then(|v| v.as_str())
        .unwrap_or("niru-noir");
    let file = base.join("niruwm/themes").join(format!("{name}.toml"));
    if !file.exists() {
        if let Some(appearance) = appearance_value(base) {
            let mut result = builtin_palette(name).unwrap_or_default();
            result.background = 0x000000;
            for (field, key) in [
                (&mut result.foreground, "foreground"),
                (&mut result.muted, "muted"),
                (&mut result.border, "border"),
                (&mut result.focus, "focus"),
            ] {
                if let Some(colour) =
                    appearance_colour(&appearance, key).and_then(|s| palette::parse_hex(&s))
                {
                    *field = colour;
                }
            }
            return Some(result);
        }
    }
    if let Ok(text) = std::fs::read_to_string(&file) {
        if let Ok(theme) = text.parse::<toml::Value>() {
            let mut result = builtin_palette(name).unwrap_or_default();
            result.background = 0x000000;
            let get = |field: &str, fallback: u32| {
                theme
                    .get(field)
                    .and_then(|v| v.as_str())
                    .and_then(palette::parse_hex)
                    .unwrap_or(fallback)
            };
            result.foreground = get("foreground", result.foreground);
            result.muted = get("muted", result.muted);
            result.border = get("border", result.border);
            result.focus = get("focus", result.focus);
            return Some(result);
        }
    }
    builtin_palette(name)
}

fn config_home() -> std::path::PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        })
}

fn suite_palette() -> Option<palette::Palette> {
    suite_palette_from(&config_home())
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font_path: None,
            rune_font_path: None,
            font_size: 22.0,
            layout: Layout::Cava,
            visualizer: true,
            visual: crate::visual::Mode::Auto,
            palette: palette::Palette::default(),
            warning: DEFAULT_WARNING,
            prompt_echo: Echo::Count,
            lock_timeout_minutes: 0,
        }
    }
}

fn config_file() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        });
    base.join("niruui").join(DEFAULT_PATH)
}

fn ladder_file() -> std::path::PathBuf {
    config_file().with_file_name(LADDER_PATH)
}

/// `lock_timeout_minutes` from the shared ladder, when it is readable.
fn ladder_timeout() -> Option<u64> {
    let text = std::fs::read_to_string(ladder_file()).ok()?;
    let v: toml::Value = text.parse().ok()?;
    v.get("lock_timeout_minutes")?
        .as_integer()
        .filter(|n| *n >= 0)
        .map(|n| n as u64)
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_file();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let empty = toml::Value::Table(Default::default());
                return Self::from_value_with(&empty, suite_palette());
            }
            // An unreadable or damaged settings file must never keep the screen
            // unlocked. Defaults are good enough to show the lock screen, so
            // say what happened and carry on with them.
            Err(e) => {
                eprintln!("nirulock: {}: {e}; using defaults", path.display());
                return Ok(Self::default());
            }
        };
        let value: toml::Value = match text.parse() {
            Ok(v) => v,
            Err(e) => {
                eprintln!("nirulock: {}: {e}; using defaults", path.display());
                return Ok(Self::default());
            }
        };
        match Self::from_value_with(&value, suite_palette()) {
            Ok(cfg) => Ok(cfg),
            Err(e) => {
                eprintln!("nirulock: {}: {e}; using defaults", path.display());
                Ok(Self::default())
            }
        }
    }

    #[cfg(test)]
    fn from_value(v: &toml::Value) -> Result<Self> {
        Self::from_value_with(v, None)
    }

    fn from_value_with(v: &toml::Value, suite: Option<palette::Palette>) -> Result<Self> {
        let mut cfg = Self::default();
        if let Some(suite) = suite {
            cfg.palette = suite;
        }
        // The shared ladder wins over the built-in five minutes; lock.toml may
        // still override it below.
        if let Some(n) = ladder_timeout() {
            cfg.lock_timeout_minutes = n;
        }
        if let Some(n) = v
            .get("lock_timeout_minutes")
            .and_then(|x| x.as_integer())
            .filter(|n| *n >= 0)
        {
            cfg.lock_timeout_minutes = n as u64;
        }
        if let Some(t) = v.get("font_path").and_then(|x| x.as_str()) {
            cfg.font_path = Some(t.to_string());
        }
        if let Some(layout) = v.get("layout").and_then(|x| x.as_str()) {
            cfg.layout = Layout::parse(layout).ok_or_else(|| {
                "layout must be minimal, terminal, runic, corner or cava".to_string()
            })?;
        }
        if let Some(echo) = v.get("prompt_echo").and_then(|x| x.as_str()) {
            cfg.prompt_echo = Echo::parse(echo)
                .ok_or_else(|| "prompt_echo must be count or pulse".to_string())?;
        }
        if let Some(enabled) = v.get("visualizer").and_then(|x| x.as_bool()) {
            cfg.visualizer = enabled;
            if !enabled {
                cfg.visual = crate::visual::Mode::None;
            }
        }
        if let Some(visual) = v.get("visual").and_then(|x| x.as_str()) {
            cfg.visual = crate::visual::Mode::parse(visual)
                .ok_or_else(|| "visual must be auto, cava, quotes or none".to_string())?;
        }
        if let Some(t) = v.get("rune_font_path").and_then(|x| x.as_str()) {
            cfg.rune_font_path = Some(t.to_string());
        }
        if let Some(n) = v.get("font_size").and_then(|x| x.as_float()) {
            cfg.font_size = n as f32;
        }
        if let Some(t) = v.get("palette").and_then(|x| x.as_table()) {
            let get = |name: &str, fallback: u32| -> u32 {
                t.get(name)
                    .and_then(|x| x.as_str())
                    .and_then(palette::parse_hex)
                    .unwrap_or(fallback)
            };
            cfg.warning = get("warning", cfg.warning);
            let d = cfg.palette.clone();
            cfg.palette = palette::Palette {
                background: get("background", d.background),
                foreground: get("foreground", d.foreground),
                muted: get("muted", d.muted),
                border: get("border", d.border),
                focus: get("focus", d.focus),
            };
        }
        if let Some(appearance) = appearance_value(&config_home()) {
            let explicit = v
                .get("palette")
                .and_then(|p| p.get("warning"))
                .and_then(|w| w.as_str())
                .and_then(palette::parse_hex);
            if explicit.is_none() {
                if let Some(colour) =
                    appearance_colour(&appearance, "warning").and_then(|s| palette::parse_hex(&s))
                {
                    cfg.warning = colour;
                }
            }
            if v.get("font_size").is_none() {
                if let Some(size) = appearance
                    .get("font")
                    .and_then(|font| font.get("size"))
                    .and_then(|size| size.as_float())
                {
                    cfg.font_size = size as f32;
                }
            }
            if v.get("font_path").is_none() {
                if let Some(path) = appearance
                    .get("font")
                    .and_then(|font| font.get("path"))
                    .and_then(|path| path.as_str())
                {
                    cfg.font_path = Some(path.to_string());
                }
            }
        }
        if !(8.0..=400.0).contains(&cfg.font_size) {
            return Err("font_size out of range".into());
        }
        Ok(cfg)
    }

    pub fn describe(&self) -> String {
        format!(
            "config: {}\nfont size: {}\nfont: {}\nlayout: {}\nprompt echo: {}\nvisualizer: {}\nvisual: {}\nrune font: {}\nautomatic unlock: never\nlegacy lock_timeout_minutes: {} (ignored)",
            config_file().display(),
            self.font_size,
            self.font_path.as_deref().unwrap_or("auto"),
            self.layout.as_str(),
            self.prompt_echo.as_str(),
            if self.visualizer { "on" } else { "off" },
            self.visual.as_str(),
            self.rune_font_path.as_deref().unwrap_or("auto"),
            self.lock_timeout_minutes,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let c = Config::default();
        assert_eq!(c.font_size, 22.0);
        assert_eq!(c.layout, Layout::Cava);
        assert!(c.visualizer);
    }

    #[test]
    fn visualizer_can_be_turned_off() {
        let v: toml::Value = "visualizer = false".parse().unwrap();
        let c = Config::from_value(&v).unwrap();
        assert!(!c.visualizer);
    }

    #[test]
    fn visual_defaults_to_auto_and_follows_the_old_switch() {
        use crate::visual::Mode;
        assert_eq!(Config::default().visual, Mode::Auto);
        let c = |text: &str| Config::from_value(&text.parse().unwrap());
        assert_eq!(c("visual = \"cava\"").unwrap().visual, Mode::Cava);
        assert_eq!(c("visual = \"quotes\"").unwrap().visual, Mode::Quotes);
        assert_eq!(c("visual = \"none\"").unwrap().visual, Mode::None);
        assert_eq!(c("visualizer = false").unwrap().visual, Mode::None);
        assert_eq!(c("visualizer = true").unwrap().visual, Mode::Auto);
        // An explicit visual wins over the old switch.
        let both = c("visualizer = false\nvisual = \"quotes\"").unwrap();
        assert_eq!(both.visual, Mode::Quotes);
        assert!(c("visual = \"loud\"").is_err());
    }

    #[test]
    fn partial_file_keeps_defaults() {
        let v: toml::Value = "font_size = 18.0".parse().unwrap();
        let c = Config::from_value(&v).unwrap();
        assert_eq!(c.font_size, 18.0);
    }

    #[test]
    fn warning_colour_defaults_and_overrides() {
        let c = Config::from_value(&toml::Value::Table(Default::default())).unwrap();
        assert_eq!(c.warning, DEFAULT_WARNING);
        let v: toml::Value = "[palette]\nwarning = \"ff0000\"".parse().unwrap();
        assert_eq!(Config::from_value(&v).unwrap().warning, 0xff0000);
    }

    #[test]
    fn prompt_echo_defaults_to_count_and_accepts_pulse() {
        assert_eq!(Config::default().prompt_echo, Echo::Count);
        let v: toml::Value = "prompt_echo = \"pulse\"".parse().unwrap();
        assert_eq!(Config::from_value(&v).unwrap().prompt_echo, Echo::Pulse);
        let v: toml::Value = "prompt_echo = \"stars\"".parse().unwrap();
        assert!(Config::from_value(&v).is_err());
    }

    #[test]
    fn palette_overrides() {
        let v: toml::Value = "[palette]\nbackground = \"101010\"".parse().unwrap();
        let c = Config::from_value(&v).unwrap();
        assert_eq!(c.palette.background, 0x101010);
        assert_eq!(c.palette.foreground, 0xe5e5e5);
    }

    #[test]
    fn layouts_parse_and_unknown_is_rejected() {
        assert_eq!(Layout::parse("minimal"), Some(Layout::Minimal));
        assert_eq!(Layout::parse("TERMINAL"), Some(Layout::Terminal));
        assert_eq!(Layout::parse("runic"), Some(Layout::Runic));
        assert_eq!(Layout::parse("corner"), Some(Layout::Corner));
        assert_eq!(Layout::parse("CAVA"), Some(Layout::Cava));
        assert!(Layout::parse("noir-ish").is_none());
    }

    #[test]
    fn suite_theme_supplies_content_colours_but_never_the_background() {
        let dir = std::env::temp_dir().join(format!("lock-suite-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("niruwm/themes")).unwrap();
        std::fs::write(dir.join("niruwm/config.toml"), "theme = \"niru-satie\"\n").unwrap();
        let p = suite_palette_from(&dir).expect("built-in suite theme");
        assert_eq!(p.background, 0x000000, "the locker must stay opaque");
        assert_eq!(p.foreground, 0xdedad4);
        assert_eq!(p.focus, 0xb08d7d);
        std::fs::write(
            dir.join("niruwm/themes/niru-satie.toml"),
            "foreground = \"010203\"\nfocus = \"040506\"\n",
        )
        .unwrap();
        let p = suite_palette_from(&dir).expect("user suite theme");
        assert_eq!(p.foreground, 0x010203);
        assert_eq!(p.focus, 0x040506);
        assert_eq!(p.background, 0x000000);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
