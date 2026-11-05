//! Bottles: a Rosemary prefix, the runner it uses, and how to run its programs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

use crate::config::Paths;
use crate::gpu::Gpu;
use crate::permissions::{self, Feature};
use crate::runner::{self, Runner};
use crate::util;

const MANIFEST: &str = "bottle.toml";

/// Generates the small enums the settings pages pick from: each has its
/// variants in order, a label, and the value Rosemary is given.
macro_rules! choice {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => ($label:literal, $value:expr)),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "kebab-case")]
        pub enum $name { #[default] $($variant),+ }

        #[allow(dead_code)]
        impl $name {
            pub const ALL: &[$name] = &[$($name::$variant),+];
            pub fn label(self) -> &'static str {
                match self { $($name::$variant => $label),+ }
            }
            pub fn value(self) -> Option<&'static str> {
                match self { $($name::$variant => $value),+ }
            }
            pub fn index(self) -> u32 {
                Self::ALL.iter().position(|v| *v == self).unwrap_or(0) as u32
            }
            pub fn from_index(i: u32) -> Self {
                Self::ALL.get(i as usize).copied().unwrap_or_default()
            }
        }
    };
}

choice!(
    /// Which execution backend runs a program.
    Backend {
        Auto => ("Automatic", None),
        Native => ("Native", Some("--native")),
        Emulated => ("Emulated (x86-64)", Some("--emulated")),
    }
);

choice!(
    /// The guest architecture of a prefix's Darwin base.
    Arch {
        Host => ("Automatic", None),
        X86_64 => ("x86-64", Some("x86_64")),
        Aarch64 => ("ARM64", Some("aarch64")),
    }
);

choice!(
    /// What happens when a guest writes a file the Darwin base owns.
    SystemFiles {
        Repair => ("Restore at next start", Some("repair")),
        Warn => ("Allow, and warn", Some("warn")),
        Immutable => ("Read-only", Some("immutable")),
    }
);

choice!(
    /// `ROSEMARY_DECORATIONS`
    Decorations {
        Client => ("Mac-style titlebar", None),
        Server => ("Desktop titlebar", Some("server")),
    }
);

choice!(
    /// `ROSEMARY_MENU`
    Menu {
        Default => ("Default", None),
        Titlebar => ("In the titlebar", Some("titlebar")),
        Strip => ("Below the titlebar", Some("strip")),
        Hidden => ("Hidden", Some("none")),
    }
);

choice!(
    /// `ROSEMARY_SWAP_INTERVAL`
    SwapInterval {
        Default => ("Application's choice", None),
        Immediate => ("Off (unlocked)", Some("0")),
        Vsync => ("VSync", Some("1")),
        Adaptive => ("Adaptive VSync", Some("-1")),
    }
);

choice!(
    /// `ROSEMARY_METAL_PRESENTATION_MODE`
    PresentMode {
        Default => ("Default (Mailbox)", None),
        Immediate => ("Immediate (unlocked)", Some("immediate")),
        Mailbox => ("Mailbox", Some("mailbox")),
        Fifo => ("VSync (FIFO)", Some("fifo")),
        FifoRelaxed => ("Adaptive VSync", Some("fifo_relaxed")),
    }
);

choice!(
    /// `ROSEMARY_COMMAND_KEY`: the host key programs see as Command.
    CommandKey {
        Super => ("Super", None),
        SuperCtrlEdit => ("Super, and Ctrl in editing shortcuts", Some("super+ctrl:edit")),
        Ctrl => ("Ctrl (swapped with Super)", Some("ctrl")),
        Alt => ("Alt (swapped with Super)", Some("alt")),
    }
);

choice!(
    /// `ROSEMARY_TSO`
    Tso {
        Auto => ("Automatic", None),
        On => ("On", Some("1")),
        Off => ("Off", Some("0")),
    }
);

choice!(
    /// `ROSEMARY_LINK_WAYS`
    LinkWays {
        Four => ("4 (Default)", None),
        Two => ("2", Some("2")),
        One => ("1", Some("1")),
    }
);

choice!(
    /// `ROSEMARY_SYSCALL_TRACE`
    SyscallTrace {
        Off => ("Off", None),
        Unknown => ("Unknown calls", Some("unknown")),
        All => ("All calls", Some("all")),
    }
);

choice!(
    /// Who a bottle's environment variable is set for.
    EnvTarget {
        Guest => ("Programs", None),
        Rosemary => ("Rosemary", None),
        Both => ("Programs and Rosemary", None),
    }
);

impl EnvTarget {
    pub fn guest(self) -> bool {
        self != EnvTarget::Rosemary
    }

    pub fn rosemary(self) -> bool {
        self != EnvTarget::Guest
    }

    /// Where a variable named `name` most likely belongs: Rosemary's own
    /// settings and those of the host libraries it loads are Rosemary's,
    /// anything else is for the programs.
    pub fn suggested(name: &str) -> EnvTarget {
        const HOST: &[&str] = &[
            "ROSEMARY_",
            "SDL_",
            "VK_",
            "MESA_",
            "RADV_",
            "DRI_",
            "LD_",
            "RUST_",
            "__",
        ];
        match HOST.iter().any(|p| name.starts_with(p)) {
            true => EnvTarget::Rosemary,
            false => EnvTarget::Guest,
        }
    }
}

choice!(
    /// `ROSEMARY_PREFIX_HOME`
    PrefixHome {
        Prefix => ("Isolated (Recommended)", None),
        Host => ("Host home folder", Some("host")),
    }
);

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Program {
    pub name: String,
    /// An `.app` bundle or a Mach-O executable.
    pub path: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub backend: Backend,
}

/// A variable the user set for a bottle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvVar {
    pub name: String,
    /// Unset, the programs get this computer's value of the variable.
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub target: EnvTarget,
    #[serde(default = "yes")]
    pub enabled: bool,
}

impl EnvVar {
    /// Why the variable cannot be set as it is, if it cannot.
    pub fn problem(&self) -> Option<&'static str> {
        if !valid_env_name(&self.name) {
            return Some("Names use letters, digits and underscores");
        }
        match &self.value {
            // Rosemary already has this computer's value of everything.
            None if !self.target.guest() => {
                Some("Only programs can copy a value from this computer")
            }
            // `ROSEMARY_ENVIRON` holds one variable per line.
            Some(v) if v.contains(['\n', '\0']) => Some("Values cannot hold line breaks"),
            _ => None,
        }
    }
}

/// A name for `ROSEMARY_FORWARD_ENV_VARS`: a variable, or a prefix and `*`.
pub fn valid_forward_name(name: &str) -> bool {
    valid_env_name(name.strip_suffix('*').unwrap_or(name))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub name: String,
    #[serde(default)]
    pub runner: Option<String>,
    #[serde(default)]
    pub created: i64,
    #[serde(default)]
    pub arch: Arch,
    #[serde(default)]
    pub system_files: SystemFiles,
    #[serde(default)]
    pub backend: Backend,
    #[serde(default)]
    pub decorations: Decorations,
    #[serde(default)]
    pub menu: Menu,
    #[serde(default)]
    pub swap_interval: SwapInterval,
    #[serde(default)]
    pub present_mode: PresentMode,
    /// The refresh rate programs are told the display has, in hertz; 0
    /// leaves it to the display.
    #[serde(default)]
    pub refresh_rate: u32,
    /// The card Metal renders on; unset leaves it to Rosemary.
    #[serde(default)]
    pub metal_gpu: Option<Gpu>,
    /// The card OpenGL renders on; unset leaves it to the system.
    #[serde(default)]
    pub opengl_gpu: Option<Gpu>,
    #[serde(default)]
    pub prefix_home: PrefixHome,
    #[serde(default = "yes")]
    pub steam: bool,
    #[serde(default)]
    pub command_key: CommandKey,
    /// `ROSEMARY_MENU_CTRL`, which Rosemary replaced with
    /// `ROSEMARY_COMMAND_KEY`. Moved to `command_key` when the bottle is
    /// loaded.
    #[serde(default, rename = "menu_ctrl", skip_serializing)]
    legacy_menu_ctrl: Option<bool>,
    #[serde(default)]
    pub confine_pointer: bool,
    #[serde(default)]
    pub plain_ctrl_click: bool,
    /// Hide the Vulkan driver from Rosemary (`VK_DRIVER_FILES=/dev/null`).
    #[serde(default)]
    pub disable_metal: bool,
    /// Offer the GL driver's own version and extensions instead of a Mac's.
    #[serde(default)]
    pub gl_host_features: bool,
    /// Keep translated Metal shaders and Vulkan pipelines between runs.
    #[serde(default = "yes")]
    pub shader_cache: bool,
    /// Keep code compiled ahead of time between runs.
    #[serde(default = "yes")]
    pub aot_cache: bool,
    /// Compile uncached programs ahead of time in the background.
    #[serde(default = "yes")]
    pub aot_background: bool,
    #[serde(default = "yes")]
    pub process_server: bool,
    #[serde(default)]
    pub dead_flags: bool,
    #[serde(default)]
    pub call_return: bool,
    #[serde(default)]
    pub strict_imports: bool,
    #[serde(default)]
    pub tso: Tso,
    /// Rows in each guest thread's block-link table; 0 leaves it to Rosemary.
    #[serde(default)]
    pub link_rows: u32,
    #[serde(default)]
    pub link_ways: LinkWays,
    /// Let guest imports bind to host libraries' symbols.
    #[serde(default = "yes")]
    pub passthrough: bool,
    #[serde(default)]
    pub syscall_interpose: bool,
    #[serde(default)]
    pub syscall_trace: SyscallTrace,
    #[serde(default)]
    pub trace_bindings: bool,
    #[serde(default)]
    pub libsystem_extensions: bool,
    /// The user's own variables, in the order they were added.
    #[serde(default)]
    pub variables: Vec<EnvVar>,
    /// Variables a program may pass on to the programs it launches.
    #[serde(default)]
    pub forward_env: Vec<String>,
    /// Variables from before Rosemary kept the programs' environment apart
    /// from its own. Moved to `variables` when the bottle is loaded.
    #[serde(default, rename = "env", skip_serializing)]
    legacy_env: BTreeMap<String, String>,
    #[serde(default)]
    pub programs: Vec<Program>,
    /// Desktop file ids of the shortcuts made to this bottle's applications.
    #[serde(default)]
    pub shortcuts: Vec<String>,
}

impl Config {
    pub fn new(name: &str, runner: Option<String>) -> Config {
        // Round-trips through serde so the defaults live in one place.
        let mut config: Config = toml::from_str(&format!("name = {name:?}")).expect("defaults");
        config.runner = runner;
        config.created = util::now();
        config
    }

    /// Bring a manifest written by an older Butter up to date.
    fn upgrade(&mut self) {
        // These were set for Rosemary, whose programs then saw all of its
        // environment. Programs see only what they are given now, so give
        // them what they used to see, except Rosemary's own settings.
        for (name, value) in std::mem::take(&mut self.legacy_env) {
            let target = match name.starts_with("ROSEMARY_") {
                true => EnvTarget::Rosemary,
                false => EnvTarget::Both,
            };
            self.set_variable(EnvVar {
                name,
                value: Some(value),
                target,
                enabled: true,
            });
        }
        // Control used to stand in for Command in menu shortcuts unless
        // turned off. The closest Rosemary has now is Control standing in
        // for it in the editing shortcuts.
        if self.legacy_menu_ctrl.take() == Some(true) && self.command_key == CommandKey::Super {
            self.command_key = CommandKey::SuperCtrlEdit;
        }
    }

    /// Add `var`, in place of any variable of the same name.
    pub fn set_variable(&mut self, var: EnvVar) {
        match self.variables.iter_mut().find(|v| v.name == var.name) {
            Some(existing) => *existing = var,
            None => self.variables.push(var),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Bottle {
    pub id: String,
    pub dir: PathBuf,
    pub config: Config,
}

impl Bottle {
    /// The Rosemary prefix: the directory whose `root/` is the guest's `/`.
    pub fn prefix(&self) -> PathBuf {
        self.dir.join("prefix")
    }

    /// The guest's `/`.
    pub fn drive(&self) -> PathBuf {
        self.prefix().join("root")
    }

    /// Where applications are installed in the guest: `/Applications`, and
    /// each user's `~/Applications`.
    pub fn application_dirs(&self) -> Vec<PathBuf> {
        let drive = self.drive();
        let mut dirs = vec![drive.join("Applications")];
        if let Ok(users) = std::fs::read_dir(drive.join("Users")) {
            let mut homes: Vec<PathBuf> = users
                .flatten()
                .map(|e| e.path().join("Applications"))
                .filter(|p| p.is_dir())
                .collect();
            homes.sort();
            dirs.extend(homes);
        }
        dirs
    }

    pub fn logs(&self) -> PathBuf {
        self.dir.join("logs")
    }

    pub fn last_log(&self) -> PathBuf {
        self.logs().join("last-run.log")
    }

    /// Records of the programs running here, so a restarted Butter finds them.
    pub fn records(&self) -> PathBuf {
        self.dir.join("run")
    }

    /// Whether Rosemary has made the prefix.
    pub fn is_ready(&self) -> bool {
        self.prefix().join(".rosemary").is_dir()
    }

    pub fn runner(&self) -> Option<Runner> {
        self.config.runner.as_deref().and_then(runner::find)
    }

    fn require_runner(&self) -> anyhow::Result<Runner> {
        let Some(id) = &self.config.runner else {
            bail!(
                "{} has no runner; choose one in its settings",
                self.config.name
            );
        };
        let runner = runner::find(id).with_context(|| format!("runner {id} is not installed"))?;
        if !runner.is_usable() {
            bail!(
                "{} has no executable at {}",
                runner.info.name,
                runner.executable().display()
            );
        }
        Ok(runner)
    }

    pub fn save(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        util::write_atomic(
            &self.dir.join(MANIFEST),
            toml::to_string_pretty(&self.config)?.as_bytes(),
        )
    }

    /// The environment Rosemary is run with for this bottle. The programs
    /// it runs get only what `ROSEMARY_ENVIRON` gives them.
    pub fn environment(&self) -> Vec<(String, String)> {
        let c = &self.config;
        let mut env = vec![(
            "ROSEMARY_PREFIX".to_string(),
            self.prefix().display().to_string(),
        )];
        let mut set = |key: &str, value: Option<&str>| {
            if let Some(value) = value {
                env.push((key.to_string(), value.to_string()));
            }
        };
        set("ROSEMARY_DECORATIONS", c.decorations.value());
        set("ROSEMARY_MENU", c.menu.value());
        set("ROSEMARY_SWAP_INTERVAL", c.swap_interval.value());
        set("ROSEMARY_METAL_PRESENTATION_MODE", c.present_mode.value());
        let refresh_rate = c.refresh_rate.to_string();
        set(
            "ROSEMARY_SCREEN_REFRESH_RATE",
            (c.refresh_rate > 0).then_some(refresh_rate.as_str()),
        );
        set("ROSEMARY_PREFIX_HOME", c.prefix_home.value());
        // Without access to Steam's folders the bridge has nothing to load.
        let steam = c.steam && permissions::available(Feature::Steam);
        set("ROSEMARY_STEAM", (!steam).then_some("0"));
        set("ROSEMARY_COMMAND_KEY", c.command_key.value());
        set("ROSEMARY_CONFINE_POINTER", c.confine_pointer.then_some("1"));
        set(
            "ROSEMARY_PLAIN_CTRL_CLICK",
            c.plain_ctrl_click.then_some("1"),
        );
        set("VK_DRIVER_FILES", c.disable_metal.then_some("/dev/null"));
        set("ROSEMARY_SERVER", (!c.process_server).then_some("off"));
        set("ROSEMARY_DEAD_FLAGS", c.dead_flags.then_some("1"));
        set("ROSEMARY_CALL_RETURN", c.call_return.then_some("1"));
        set("ROSEMARY_STRICT_IMPORTS", c.strict_imports.then_some("1"));
        set(
            "ROSEMARY_GL_HOST_FEATURES",
            c.gl_host_features.then_some("1"),
        );
        set("ROSEMARY_SHADER_CACHE", (!c.shader_cache).then_some("off"));
        set("ROSEMARY_AOT_CACHE", (!c.aot_cache).then_some("off"));
        set(
            "ROSEMARY_AOT_BACKGROUND",
            (!c.aot_background).then_some("off"),
        );
        set("ROSEMARY_TSO", c.tso.value());
        let link_rows = c.link_rows.to_string();
        set(
            "ROSEMARY_LINK_ROWS",
            (c.link_rows > 0).then_some(link_rows.as_str()),
        );
        set("ROSEMARY_LINK_WAYS", c.link_ways.value());
        set("ROSEMARY_NO_PASSTHROUGH", (!c.passthrough).then_some("1"));
        set(
            "ROSEMARY_SYSCALL_INTERPOSE",
            c.syscall_interpose.then_some("1"),
        );
        set("ROSEMARY_SYSCALL_TRACE", c.syscall_trace.value());
        set("ROSEMARY_TRACE_BINDINGS", c.trace_bindings.then_some("1"));
        set(
            "ROSEMARY_EXPOSE_LIBSYSTEM_EXTENSIONS",
            c.libsystem_extensions.then_some("1"),
        );
        let forward = c.forward_env.join(",");
        set(
            "ROSEMARY_FORWARD_ENV_VARS",
            (!forward.is_empty()).then_some(forward.as_str()),
        );
        // Rosemary loads Vulkan and OpenGL in its own process, so these are
        // its variables, not the programs'.
        let gpus = [
            c.metal_gpu
                .as_ref()
                .filter(|_| !c.disable_metal)
                .map(Gpu::vulkan_env),
            c.opengl_gpu.as_ref().map(Gpu::opengl_env),
        ];
        for (key, value) in gpus.into_iter().flatten().flatten() {
            set(key, Some(&value));
        }

        let mut guest = Vec::new();
        let mut own = Vec::new();
        for var in c.variables.iter().filter(|v| v.enabled) {
            if var.problem().is_some() {
                continue;
            }
            if var.target.guest() {
                // A bare name copies this computer's value.
                guest.push(match &var.value {
                    Some(value) => format!("{}={value}", var.name),
                    None => var.name.clone(),
                });
            }
            if var.target.rosemary()
                && let Some(value) = &var.value
            {
                own.push((var.name.clone(), value.clone()));
            }
        }
        if !guest.is_empty() {
            env.push(("ROSEMARY_ENVIRON".to_string(), guest.join("\n")));
        }
        // The user's own variables win over everything above.
        for (key, value) in own {
            env.retain(|(k, _)| *k != key);
            env.push((key, value));
        }
        env
    }

    /// `rosemary prefix ACTION PREFIX ARGS...` for this bottle.
    ///
    /// Downloads of the Darwin base are allowed: Butter asks before running
    /// anything that may fetch one.
    pub fn prefix_command(&self, action: &str, args: &[&str]) -> anyhow::Result<Command> {
        let runner = self.require_runner()?;
        let mut command = runner.command();
        command
            .arg("prefix")
            .arg(action)
            .arg(self.prefix())
            .args(args)
            .env("ROSEMARY_PREFIX_SETUP", "download");
        Ok(command)
    }

    /// The arguments `rosemary prefix create` needs for this bottle's settings.
    pub fn create_args(&self, with_base: bool) -> Vec<&'static str> {
        if !with_base {
            return vec!["--no-base"];
        }
        let mut args = Vec::new();
        if let Some(arch) = self.config.arch.value() {
            args.extend(["--arch", arch]);
        }
        if let Some(policy) = self.config.system_files.value() {
            args.extend(["--system-files", policy]);
        }
        args
    }

    /// `rosemary run` for `target`, with this bottle's settings. It is
    /// started by [`crate::sessions::Sessions::launch`].
    pub fn run_command(
        &self,
        target: &Path,
        args: &[String],
        backend: Backend,
    ) -> anyhow::Result<Command> {
        let runner = self.require_runner()?;
        let backend = if backend == Backend::Auto {
            self.config.backend
        } else {
            backend
        };
        let mut command = runner.command();
        command.arg("run");
        if let Some(flag) = backend.value() {
            command.arg(flag);
        }
        command
            .arg(target)
            .args(args)
            .envs(self.environment())
            .current_dir(target.parent().unwrap_or(Path::new("/")));
        Ok(command)
    }

    /// `rosemary aot TARGET` with this bottle's settings: compile `target`
    /// ahead of time, along with the code earlier runs compiled on the fly.
    pub fn aot_command(&self, target: &Path) -> anyhow::Result<Command> {
        let mut command = self.require_runner()?.command();
        command
            .arg("aot")
            .arg(target)
            .envs(self.environment())
            .stdin(Stdio::null())
            .current_dir(target.parent().unwrap_or(Path::new("/")));
        Ok(command)
    }

    /// `rosemary server ARGS...` for this bottle's prefix.
    pub fn server_command(&self, args: &[&str]) -> anyhow::Result<Command> {
        let mut command = self.require_runner()?.command();
        command
            .arg("server")
            .args(args)
            .envs(self.environment())
            .stdin(Stdio::null());
        Ok(command)
    }
}

pub fn list() -> Vec<Bottle> {
    let Ok(entries) = std::fs::read_dir(Paths::get().bottles()) else {
        return Vec::new();
    };
    let mut list: Vec<Bottle> = entries
        .flatten()
        .filter_map(|e| load_dir(&e.path()))
        .collect();
    list.sort_by_key(|b| b.config.name.to_lowercase());
    list
}

fn load_dir(dir: &Path) -> Option<Bottle> {
    let text = std::fs::read_to_string(dir.join(MANIFEST)).ok()?;
    let mut config: Config = toml::from_str(&text)
        .map_err(|e| eprintln!("butter: skipping {}: {e}", dir.join(MANIFEST).display()))
        .ok()?;
    config.upgrade();
    Some(Bottle {
        id: dir.file_name()?.to_string_lossy().into_owned(),
        dir: dir.to_path_buf(),
        config,
    })
}

pub fn load(id: &str) -> Option<Bottle> {
    if id.is_empty() || id.contains('/') {
        return None;
    }
    load_dir(&Paths::get().bottles().join(id))
}

/// Make a bottle's directory and manifest. Its prefix is created separately,
/// with [`Bottle::prefix_command`], so the output can be shown.
pub fn create(config: Config) -> anyhow::Result<Bottle> {
    let name = config.name.trim();
    if name.is_empty() {
        bail!("a bottle needs a name");
    }
    if list()
        .iter()
        .any(|b| b.config.name.eq_ignore_ascii_case(name))
    {
        bail!("there is already a bottle named {name}");
    }
    let base = util::slugify(name);
    let root = Paths::get().bottles();
    let mut id = base.clone();
    let mut n = 2;
    while root.join(&id).exists() {
        id = format!("{base}-{n}");
        n += 1;
    }
    let bottle = Bottle {
        dir: root.join(&id),
        id,
        config: Config {
            name: name.to_string(),
            ..config
        },
    };
    bottle.save()?;
    Ok(bottle)
}

pub fn delete(bottle: &Bottle) -> anyhow::Result<()> {
    // Only ever a directory Butter made: one holding our manifest.
    if !bottle.dir.join(MANIFEST).is_file() {
        bail!("{} is not a bottle", bottle.dir.display());
    }
    std::fs::remove_dir_all(&bottle.dir)?;
    Ok(())
}

/// Bottles using the runner `id`.
pub fn using_runner(id: &str) -> Vec<Bottle> {
    list()
        .into_iter()
        .filter(|b| b.config.runner.as_deref() == Some(id))
        .collect()
}

pub fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A program's display name from its path: `Foo.app` -> `Foo`.
pub fn program_name(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.strip_suffix(".app")
        .map(str::to_string)
        .unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bottle(config: Config) -> Bottle {
        Bottle {
            id: "t".into(),
            dir: "/b/t".into(),
            config,
        }
    }

    #[test]
    fn defaults_survive_a_minimal_manifest() {
        let config = Config::new("Games", Some("rosemary-v0.2.0".into()));
        assert!(config.steam && config.process_server);
        assert!(config.shader_cache && config.aot_cache && config.aot_background);
        assert!(config.passthrough);
        assert!(!config.dead_flags);
        assert_eq!(config.command_key, CommandKey::Super);
        let text = toml::to_string_pretty(&config).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.runner.as_deref(), Some("rosemary-v0.2.0"));
    }

    #[test]
    fn environment_reflects_settings() {
        let mut config = Config::new("x", None);
        let env = bottle(config.clone()).environment();
        assert_eq!(
            env,
            vec![("ROSEMARY_PREFIX".to_string(), "/b/t/prefix".to_string())]
        );

        config.decorations = Decorations::Server;
        config.steam = false;
        config.swap_interval = SwapInterval::Adaptive;
        config.dead_flags = true;
        config.disable_metal = true;
        config.present_mode = PresentMode::FifoRelaxed;
        config.refresh_rate = 144;
        config.plain_ctrl_click = true;
        config.command_key = CommandKey::SuperCtrlEdit;
        config.gl_host_features = true;
        config.shader_cache = false;
        config.aot_background = false;
        config.tso = Tso::Off;
        config.link_rows = 131072;
        config.link_ways = LinkWays::Two;
        config.passthrough = false;
        config.syscall_trace = SyscallTrace::Unknown;
        config.forward_env = vec!["ROSEMARY_STATS".into(), "MY_*".into()];
        let var = |name: &str, value: Option<&str>, target| EnvVar {
            name: name.into(),
            value: value.map(str::to_string),
            target,
            enabled: true,
        };
        config.set_variable(var("ROSEMARY_STEAM", Some("1"), EnvTarget::Rosemary));
        config.set_variable(var("FOO", Some("bar"), EnvTarget::Guest));
        config.set_variable(var("DISPLAY", None, EnvTarget::Guest));
        config.set_variable(var("BOTH", Some("x"), EnvTarget::Both));
        config.set_variable(EnvVar {
            enabled: false,
            ..var("OFF", Some("1"), EnvTarget::Both)
        });
        // Rosemary already has the host's value of everything.
        config.set_variable(var("NOPE", None, EnvTarget::Rosemary));
        let env: BTreeMap<_, _> = bottle(config).environment().into_iter().collect();
        assert_eq!(env["ROSEMARY_DECORATIONS"], "server");
        assert_eq!(env["ROSEMARY_SWAP_INTERVAL"], "-1");
        assert_eq!(env["ROSEMARY_METAL_PRESENTATION_MODE"], "fifo_relaxed");
        assert_eq!(env["ROSEMARY_SCREEN_REFRESH_RATE"], "144");
        assert_eq!(env["ROSEMARY_PLAIN_CTRL_CLICK"], "1");
        assert_eq!(env["ROSEMARY_FORWARD_ENV_VARS"], "ROSEMARY_STATS,MY_*");
        assert_eq!(env["ROSEMARY_DEAD_FLAGS"], "1");
        assert_eq!(env["ROSEMARY_COMMAND_KEY"], "super+ctrl:edit");
        assert_eq!(env["ROSEMARY_GL_HOST_FEATURES"], "1");
        assert_eq!(env["ROSEMARY_SHADER_CACHE"], "off");
        assert_eq!(env["ROSEMARY_AOT_BACKGROUND"], "off");
        assert_eq!(env["ROSEMARY_TSO"], "0");
        assert_eq!(env["ROSEMARY_LINK_ROWS"], "131072");
        assert_eq!(env["ROSEMARY_LINK_WAYS"], "2");
        assert_eq!(env["ROSEMARY_NO_PASSTHROUGH"], "1");
        assert_eq!(env["ROSEMARY_SYSCALL_TRACE"], "unknown");
        assert!(!env.contains_key("ROSEMARY_AOT_CACHE"));
        assert!(!env.contains_key("ROSEMARY_MENU_CTRL"));
        assert_eq!(env["VK_DRIVER_FILES"], "/dev/null");
        assert_eq!(env["ROSEMARY_STEAM"], "1", "user variables win");
        assert_eq!(env["ROSEMARY_ENVIRON"], "FOO=bar\nDISPLAY\nBOTH=x");
        assert_eq!(env["BOTH"], "x");
        assert!(!env.contains_key("FOO") && !env.contains_key("OFF"));
        assert!(!env.contains_key("NOPE"));
    }

    #[test]
    fn gpus_are_rosemarys() {
        let gpu = Gpu {
            slot: "0000:03:00.0".into(),
            vendor: 0x1002,
            device: 0x73bf,
            driver: "amdgpu".into(),
            name: "AMD Radeon RX 6800".into(),
        };
        let mut config = Config::new("x", None);
        config.metal_gpu = Some(gpu.clone());
        config.opengl_gpu = Some(gpu);
        let text = toml::to_string_pretty(&config).unwrap();
        let config: Config = toml::from_str(&text).unwrap();
        let env: BTreeMap<_, _> = bottle(config.clone()).environment().into_iter().collect();
        assert_eq!(env["MESA_VK_DEVICE_SELECT"], "1002:73bf");
        assert_eq!(env["DRI_PRIME"], "pci-0000_03_00_0");

        let mut config = config;
        config.disable_metal = true;
        let env: BTreeMap<_, _> = bottle(config).environment().into_iter().collect();
        assert!(!env.contains_key("MESA_VK_DEVICE_SELECT"));
        assert!(env.contains_key("DRI_PRIME"));
    }

    #[test]
    fn old_variables_are_upgraded() {
        let mut config: Config =
            toml::from_str("name = \"x\"\n[env]\nROSEMARY_OBJC = \"builtin\"\nFOO = \"bar\"\n")
                .unwrap();
        config.upgrade();
        assert_eq!(config.variables.len(), 2);
        let env: BTreeMap<_, _> = bottle(config.clone()).environment().into_iter().collect();
        assert_eq!(env["ROSEMARY_OBJC"], "builtin");
        assert_eq!(env["FOO"], "bar");
        assert_eq!(env["ROSEMARY_ENVIRON"], "FOO=bar");
        let text = toml::to_string_pretty(&config).unwrap();
        assert!(!text.contains("[env]"), "{text}");
    }

    #[test]
    fn menu_ctrl_becomes_a_command_key() {
        let upgraded = |manifest: &str| {
            let mut config: Config = toml::from_str(manifest).unwrap();
            config.upgrade();
            config
        };
        let config = upgraded("name = \"x\"\nmenu_ctrl = true\n");
        assert_eq!(config.command_key, CommandKey::SuperCtrlEdit);
        let text = toml::to_string_pretty(&config).unwrap();
        assert!(!text.contains("menu_ctrl"), "{text}");
        assert!(text.contains("command_key = \"super-ctrl-edit\""), "{text}");

        let config = upgraded("name = \"x\"\nmenu_ctrl = false\n");
        assert_eq!(config.command_key, CommandKey::Super);
        // A key chosen since is kept.
        let config = upgraded("name = \"x\"\nmenu_ctrl = true\ncommand_key = \"alt\"\n");
        assert_eq!(config.command_key, CommandKey::Alt);
    }

    #[test]
    fn create_arguments() {
        let mut config = Config::new("x", None);
        assert_eq!(
            bottle(config.clone()).create_args(true),
            ["--system-files", "repair"]
        );
        config.arch = Arch::X86_64;
        config.system_files = SystemFiles::Immutable;
        assert_eq!(
            bottle(config.clone()).create_args(true),
            ["--arch", "x86_64", "--system-files", "immutable"]
        );
        assert_eq!(bottle(config).create_args(false), ["--no-base"]);
    }

    #[test]
    fn names() {
        assert!(valid_env_name("ROSEMARY_OBJC"));
        assert!(!valid_env_name("1X"));
        assert!(!valid_env_name("A-B"));
        assert!(valid_forward_name("ROSEMARY_*"));
        assert!(!valid_forward_name("*"));
        assert_eq!(EnvTarget::suggested("ROSEMARY_OBJC"), EnvTarget::Rosemary);
        assert_eq!(EnvTarget::suggested("MY_GAME_FLAG"), EnvTarget::Guest);
        assert_eq!(program_name(Path::new("/x/Foo Bar.app")), "Foo Bar");
        assert_eq!(program_name(Path::new("/x/tool")), "tool");
    }
}
