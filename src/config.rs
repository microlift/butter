//! Application constants and on-disk locations.

use std::path::{Path, PathBuf};

pub const APP_ID: &str = "io.github.microlift.butter";
pub const APP_NAME: &str = "Butter";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const WEBSITE: &str = "https://github.com/microlift/butter";

/// The upstream repository runners are built from.
pub const ROSEMARY_OWNER: &str = "microlift";
pub const ROSEMARY_REPO: &str = "rosemary";

pub fn user_agent() -> String {
    format!("{APP_NAME}/{VERSION} (+{WEBSITE})")
}

/// Whether Butter is running inside a Flatpak sandbox.
pub fn is_flatpak() -> bool {
    Path::new("/.flatpak-info").exists()
}

/// Where Butter keeps everything. Inside a Flatpak, GLib already points these
/// at `~/.var/app/<id>/`, so the same code serves both builds.
#[derive(Debug, Clone)]
pub struct Paths {
    pub data: PathBuf,
    pub config: PathBuf,
    pub cache: PathBuf,
}

impl Paths {
    pub fn get() -> Paths {
        Paths {
            data: gtk::glib::user_data_dir().join("butter"),
            config: gtk::glib::user_config_dir().join("butter"),
            cache: gtk::glib::user_cache_dir().join("butter"),
        }
    }

    /// Installed runners, one directory per runner id.
    pub fn runners(&self) -> PathBuf {
        self.data.join("runners")
    }

    /// Bottles: Butter's metadata plus the Rosemary prefix inside it.
    pub fn bottles(&self) -> PathBuf {
        self.data.join("bottles")
    }

    /// A Rust toolchain Butter installed for itself with rustup.
    pub fn toolchain(&self) -> PathBuf {
        self.data.join("toolchain")
    }

    /// Unpacked sources of releases being built.
    pub fn sources(&self) -> PathBuf {
        self.cache.join("sources")
    }

    /// Downloaded release tarballs.
    pub fn downloads(&self) -> PathBuf {
        self.cache.join("downloads")
    }

    /// One Cargo target directory shared by every build, so a later release
    /// recompiles only what changed.
    pub fn build_target(&self) -> PathBuf {
        self.cache.join("target")
    }

    /// Versioned: lists cached before Butter read release assets lack them.
    pub fn releases_cache(&self) -> PathBuf {
        self.cache.join("releases-v2.json")
    }

    pub fn commits_cache(&self) -> PathBuf {
        self.cache.join("commits-v1.json")
    }

    /// Application icons, converted from `.icns` to PNG.
    pub fn icons(&self) -> PathBuf {
        self.cache.join("icons")
    }

    /// Shortcuts Butter wrote itself, where the Dynamic Launcher portal is
    /// missing.
    pub fn shortcut_icons(&self) -> PathBuf {
        self.data.join("shortcuts")
    }

    /// Runner builds started and not yet finished, so a build cut short by
    /// Butter quitting resumes on the next launch; see `runner::pending_builds`.
    pub fn pending_builds_file(&self) -> PathBuf {
        self.data.join("builds.toml")
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config.join("settings.toml")
    }

    /// The answers given during onboarding; see `permissions`.
    pub fn permissions_file(&self) -> PathBuf {
        self.config.join("permissions.toml")
    }
}
