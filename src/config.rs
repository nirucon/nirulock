//! `~/.config/niruui/lock.toml`
//!
//! The NIRU desktop programs share one directory for their settings so they
//! can be found in one place, but each program has its own file and never
//! reads the others'. That means installing one, two or all of them is fine.

use crate::{palette, Result};

pub const DEFAULT_PATH: &str = "lock.toml";

/// The shared screensaver settings, the same file `niruwm/preferences.rs`
/// reads for the idle ladder.
///
/// niruwm draws its own lock screen when the session uses the suite's window
/// manager instead of niruidle. Reading the same numbers means changing the
/// lock delay once changes it everywhere.
pub const LADDER_PATH: &str = "screensaver.toml";

#[derive(Clone, Debug)]
pub struct Config {
    pub font_path: Option<String>,
    pub font_size: f32,
    pub palette: palette::Palette,
    /// Deprecated compatibility input. Never used to dismiss a lock.
    pub lock_timeout_minutes: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font_path: None,
            font_size: 22.0,
            palette: palette::Palette::default(),
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
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
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
        match Self::from_value(&value) {
            Ok(cfg) => Ok(cfg),
            Err(e) => {
                eprintln!("nirulock: {}: {e}; using defaults", path.display());
                Ok(Self::default())
            }
        }
    }

    fn from_value(v: &toml::Value) -> Result<Self> {
        let mut cfg = Self::default();
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
            let d = palette::Palette::default();
            cfg.palette = palette::Palette {
                background: get("background", d.background),
                foreground: get("foreground", d.foreground),
                muted: get("muted", d.muted),
                border: get("border", d.border),
                focus: get("focus", d.focus),
            };
        }
        if !(8.0..=400.0).contains(&cfg.font_size) {
            return Err("font_size out of range".into());
        }
        Ok(cfg)
    }

    pub fn describe(&self) -> String {
        format!(
            "config: {}\nfont size: {}\nfont: {}\nautomatic unlock: never\nlegacy lock_timeout_minutes: {} (ignored)",
            config_file().display(),
            self.font_size,
            self.font_path.as_deref().unwrap_or("auto"),
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
    }

    #[test]
    fn partial_file_keeps_defaults() {
        let v: toml::Value = "font_size = 18.0".parse().unwrap();
        let c = Config::from_value(&v).unwrap();
        assert_eq!(c.font_size, 18.0);
    }

    #[test]
    fn palette_overrides() {
        let v: toml::Value = "[palette]\nbackground = \"101010\"".parse().unwrap();
        let c = Config::from_value(&v).unwrap();
        assert_eq!(c.palette.background, 0x101010);
        assert_eq!(c.palette.foreground, 0xe5e5e5);
    }
}
