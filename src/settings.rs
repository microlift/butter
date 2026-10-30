//! User preferences, stored as TOML so source builds need no schema install.

use serde::{Deserialize, Serialize};

use crate::config::Paths;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// List pre-releases among the available runners.
    pub show_prereleases: bool,
    /// A `cargo` to use instead of the one Butter finds.
    pub cargo_override: Option<String>,
    /// Keep the shared Cargo target directory between builds.
    pub keep_build_cache: bool,
    /// Runner given to new bottles.
    pub default_runner: Option<String>,
    /// How runners are compiled.
    pub build_profile: BuildProfile,
    /// Whether Butter has looked for a system install of Rosemary, which it
    /// does once, on first launch.
    pub checked_system_runner: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BuildProfile {
    /// Cargo's release profile: portable across CPUs.
    #[default]
    Release,
    /// Rosemary's `fullnative` profile: LTO, tuned for this machine's CPU.
    Native,
}

impl BuildProfile {
    pub const ALL: [BuildProfile; 2] = [BuildProfile::Release, BuildProfile::Native];

    pub fn cargo_profile(self) -> &'static str {
        match self {
            BuildProfile::Release => "release",
            BuildProfile::Native => "fullnative",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            BuildProfile::Release => "Release",
            BuildProfile::Native => "Optimized for this CPU",
        }
    }
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            show_prereleases: false,
            cargo_override: None,
            keep_build_cache: true,
            default_runner: None,
            build_profile: BuildProfile::default(),
            checked_system_runner: false,
        }
    }
}

impl Settings {
    pub fn load() -> Settings {
        let path = Paths::get().settings_file();
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = Paths::get().settings_file();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        crate::util::write_atomic(&path, toml::to_string_pretty(self)?.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_keys_take_defaults() {
        let settings: Settings = toml::from_str("show_prereleases = true").unwrap();
        assert!(settings.show_prereleases);
        assert!(settings.keep_build_cache);
        assert_eq!(settings.build_profile, BuildProfile::Release);
    }
}
