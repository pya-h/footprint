//! `gui.json` in `$XDG_CONFIG_HOME/footprint/` (versioned, mode 0600). Holds UI
//! preferences only — nothing derived from evidence ever lands here.

use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::theme::ThemeName;

pub const SETTINGS_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Density {
    Compact,
    Comfortable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    pub theme: ThemeName,
    pub density: Density,
    /// 0.8–1.6; clamped on load
    pub ui_scale: f32,
    /// `local`, `UTC` or a `±HH:MM` offset, parsed by `fp_query::parse_tz` at use
    pub tz: String,
    pub panels: Panels,
    pub table: Table,
    pub last_view: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Panels {
    pub facets_width: f32,
    pub inspector_width: f32,
    pub facets_collapsed: bool,
    pub inspector_collapsed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Table {
    pub columns: Vec<String>,
}

const UI_SCALE_MIN: f32 = 0.8;
const UI_SCALE_MAX: f32 = 1.6;

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            theme: ThemeName::Dark,
            density: Density::Compact,
            ui_scale: 1.0,
            tz: "UTC".to_string(),
            panels: Panels::default(),
            table: Table::default(),
            last_view: None,
        }
    }
}

impl Default for Panels {
    fn default() -> Self {
        Self {
            facets_width: 220.0,
            inspector_width: 320.0,
            facets_collapsed: false,
            inspector_collapsed: false,
        }
    }
}

impl Default for Table {
    fn default() -> Self {
        Self {
            columns: ["time", "source", "kind", "summary"]
                .map(str::to_string)
                .to_vec(),
        }
    }
}

#[derive(Deserialize)]
struct VersionProbe {
    version: u32,
}

impl Settings {
    /// `(settings, warning)`: a corrupt or newer-version file yields the defaults
    /// plus a message the shell shows as a toast.
    pub fn load() -> (Self, Option<String>) {
        match Self::path() {
            Some(path) => Self::load_from(&path),
            None => (Self::default(), None),
        }
    }

    pub fn load_from(path: &Path) -> (Self, Option<String>) {
        let corrupt = || {
            (
                Self::default(),
                Some(format!("{} is corrupt; using defaults", path.display())),
            )
        };
        let Ok(bytes) = fs::read(path) else {
            return (Self::default(), None);
        };
        let Ok(probe) = serde_json::from_slice::<VersionProbe>(&bytes) else {
            return corrupt();
        };
        if probe.version > SETTINGS_VERSION {
            return (
                Self::default(),
                Some(format!(
                    "{} was written by a newer footprint; using defaults",
                    path.display()
                )),
            );
        }
        let Ok(mut settings) = serde_json::from_slice::<Self>(&bytes) else {
            return corrupt();
        };
        settings.ui_scale = settings.ui_scale.clamp(UI_SCALE_MIN, UI_SCALE_MAX);
        (settings, None)
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let Some(path) = Self::path() else {
            anyhow::bail!("no config directory (XDG_CONFIG_HOME or HOME is unset)");
        };
        self.save_to(&path)
    }

    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let bytes = serde_json::to_vec_pretty(self)?;
        // mode on creation only; a pre-existing file keeps its permissions
        fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?
            .write_all(&bytes)?;
        Ok(())
    }

    fn path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
        Some(base.join("footprint").join("gui.json"))
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn save_load_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("conf").join("gui.json");
        let settings = Settings {
            ui_scale: 1.3,
            last_view: Some("timeline".into()),
            ..Settings::default()
        };
        settings.save_to(&path).expect("save");
        let (loaded, warning) = Settings::load_from(&path);
        assert_eq!(warning, None);
        assert_eq!(loaded, settings);
        let mode = path.metadata().expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "settings must be private");
    }

    #[test]
    fn missing_file_means_defaults_without_warning() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (settings, warning) = Settings::load_from(&dir.path().join("nope.json"));
        assert_eq!(settings, Settings::default());
        assert_eq!(warning, None);
    }

    #[test]
    fn corrupt_and_newer_files_fall_back_with_a_warning() {
        let dir = tempfile::tempdir().expect("tempdir");
        let corrupt_path = dir.path().join("corrupt.json");
        fs::write(&corrupt_path, b"{ not json").expect("write");
        let (settings, warning) = Settings::load_from(&corrupt_path);
        assert_eq!(settings, Settings::default());
        assert!(warning.expect("warning").contains("corrupt"));

        let newer_path = dir.path().join("newer.json");
        fs::write(&newer_path, br#"{"version": 999}"#).expect("write");
        let (settings, warning) = Settings::load_from(&newer_path);
        assert_eq!(settings, Settings::default());
        assert!(warning.expect("warning").contains("newer"));
    }

    #[test]
    fn ui_scale_is_clamped_on_load() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("gui.json");
        fs::write(&path, br#"{"version": 1, "ui_scale": 9.0}"#).expect("write");
        let (settings, _) = Settings::load_from(&path);
        assert_eq!(settings.ui_scale, UI_SCALE_MAX);
    }
}
