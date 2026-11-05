//! Plugins: actions written in Python that Butter shows and runs.
//!
//! A plugin is a folder holding a `plugin.toml`, which says what the plugin
//! is called, which actions it offers and which settings it has, and a
//! Python file implementing the actions with the `butter` module (in
//! `sdk/python/`). Butter reads the manifest itself and shows the actions
//! and settings without running any Python.
//!
//! Each action runs in a new `python3` process (see [`run`]). Butter writes
//! one `invoke` request, as a line of JSON, to its standard input. The
//! plugin answers on standard output, one JSON object per line: progress
//! (`stage`, `progress`, `log`), calls to Butter (`call`, which Butter
//! answers with a `reply` on standard input), and finally `done` or `fail`.
//! What the plugin prints goes to standard error, which becomes its log.
//! `done` carries [`Effect`]s, such as starting a program, which the
//! interface carries out once the action has finished.
//!
//! Plugins come from three places, later ones replacing earlier ones with
//! the same id: those built into Butter, `$XDG_DATA_DIRS/butter/plugins/`,
//! and `$XDG_DATA_HOME/butter/plugins/`.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::bottle::{self, Arch, Backend, Bottle};
use crate::config::{self, Paths};
use crate::process::Reporter;
use crate::settings::Settings;
use crate::{apps, github, runner, util};

/// The version of the protocol; a plugin asking for a newer one is refused.
pub const API: u32 = 1;

/// The command-line flag shortcuts to plugin actions use.
pub const FLAG: &str = "--plugin";

const MANIFEST: &str = "plugin.toml";

/// Puts the SDK, given as the first argument, on the path and hands over to
/// it. `-I` keeps the user's site packages and `PYTHON*` variables out.
const BOOT: &str = "import sys; sys.path.insert(0, sys.argv[1]); \
                    from butter._host import main; main()";

type Files = &'static [(&'static str, &'static [u8])];

/// The `butter` Python module, written to the cache before a plugin runs.
const SDK: Files = &[
    (
        "butter/__init__.py",
        include_bytes!("../sdk/python/butter/__init__.py"),
    ),
    (
        "butter/_host.py",
        include_bytes!("../sdk/python/butter/_host.py"),
    ),
    (
        "butter/archive.py",
        include_bytes!("../sdk/python/butter/archive.py"),
    ),
];

/// Plugins that come with Butter, by id.
const BUNDLED: &[(&str, Files)] = &[(
    "roblox",
    &[
        (
            "plugin.toml",
            include_bytes!("../plugins/roblox/plugin.toml"),
        ),
        ("plugin.py", include_bytes!("../plugins/roblox/plugin.py")),
        (
            "roblox_deploy.py",
            include_bytes!("../plugins/roblox/roblox_deploy.py"),
        ),
        (
            "roblox_install.py",
            include_bytes!("../plugins/roblox/roblox_install.py"),
        ),
        ("icon.svg", include_bytes!("../plugins/roblox/icon.svg")),
    ],
)];

// --- Manifests ---

fn default_entry() -> String {
    "plugin.py".into()
}

fn default_api() -> u32 {
    1
}

/// A plugin's `plugin.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    /// Letters, digits, `-` and `_`; also the name of its folder.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub website: Option<String>,
    /// The Python file implementing the actions, in the plugin's folder.
    #[serde(default = "default_entry")]
    pub entry: String,
    /// The protocol version the plugin is written for.
    #[serde(default = "default_api")]
    pub api: u32,
    /// An SVG or PNG in the plugin's folder.
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub actions: Vec<Action>,
    #[serde(default)]
    pub settings: Vec<Setting>,
}

/// Something a plugin can do, shown as a row with a Run button.
#[derive(Debug, Clone, Deserialize)]
pub struct Action {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
    #[serde(default)]
    pub scope: Scope,
    /// Not listed: only run from shortcuts and links.
    #[serde(default)]
    pub hidden: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scope {
    /// Listed on the Plugins page.
    #[default]
    Global,
    /// Listed on each bottle's page, and run in that bottle.
    Bottle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SettingKind {
    Bool,
    /// One line of text.
    String,
    /// Several lines of text, such as JSON.
    Text,
    Int,
    /// One of `options`.
    Choice,
    /// A bottle's id, or none.
    Bottle,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Choice {
    pub value: String,
    pub label: String,
}

/// A setting, shown in the plugin's settings dialog.
#[derive(Debug, Clone, Deserialize)]
pub struct Setting {
    pub key: String,
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
    #[serde(rename = "type")]
    pub kind: SettingKind,
    #[serde(default)]
    pub default: Option<toml::Value>,
    #[serde(default)]
    pub options: Vec<Choice>,
    #[serde(default)]
    pub min: Option<i64>,
    #[serde(default)]
    pub max: Option<i64>,
}

impl Setting {
    /// The value until the user chooses one.
    pub fn default_value(&self) -> toml::Value {
        if let Some(value) = &self.default
            && self.accepts(value)
        {
            return value.clone();
        }
        match self.kind {
            SettingKind::Bool => toml::Value::Boolean(false),
            SettingKind::Int => toml::Value::Integer(self.min.unwrap_or(0).max(0)),
            SettingKind::Choice => toml::Value::String(
                self.options
                    .first()
                    .map(|c| c.value.clone())
                    .unwrap_or_default(),
            ),
            SettingKind::String | SettingKind::Text | SettingKind::Bottle => {
                toml::Value::String(String::new())
            }
        }
    }

    /// Whether `value` is one this setting can hold.
    pub fn accepts(&self, value: &toml::Value) -> bool {
        match (self.kind, value) {
            (SettingKind::Bool, toml::Value::Boolean(_)) => true,
            (SettingKind::Int, toml::Value::Integer(n)) => {
                self.min.is_none_or(|min| *n >= min) && self.max.is_none_or(|max| *n <= max)
            }
            (SettingKind::Choice, toml::Value::String(s)) => {
                self.options.iter().any(|c| c.value == *s)
            }
            (
                SettingKind::String | SettingKind::Text | SettingKind::Bottle,
                toml::Value::String(_),
            ) => true,
            _ => false,
        }
    }
}

/// Letters, digits, `-` and `_`.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A path inside the plugin's folder.
fn valid_relative(path: &str) -> bool {
    let path = Path::new(path);
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

impl Manifest {
    pub fn parse(text: &str) -> anyhow::Result<Manifest> {
        let manifest: Manifest = toml::from_str(text)?;
        manifest.validate()?;
        Ok(manifest)
    }

    fn validate(&self) -> anyhow::Result<()> {
        if !valid_id(&self.id) {
            bail!("the id {:?} is not letters, digits, - and _", self.id);
        }
        if self.name.trim().is_empty() {
            bail!("the plugin has no name");
        }
        if self.api > API {
            bail!(
                "it needs plugin API {}; this Butter has {API}. Update Butter",
                self.api
            );
        }
        if !valid_relative(&self.entry) {
            bail!("the entry {:?} is not a file in the plugin", self.entry);
        }
        if let Some(icon) = &self.icon
            && !valid_relative(icon)
        {
            bail!("the icon {icon:?} is not a file in the plugin");
        }
        let mut seen = std::collections::HashSet::new();
        for action in &self.actions {
            if !valid_id(&action.id) {
                bail!(
                    "the action id {:?} is not letters, digits, - and _",
                    action.id
                );
            }
            if !seen.insert(&action.id) {
                bail!("the action {:?} is listed twice", action.id);
            }
        }
        let mut seen = std::collections::HashSet::new();
        for setting in &self.settings {
            if !valid_id(&setting.key) {
                bail!(
                    "the setting key {:?} is not letters, digits, - and _",
                    setting.key
                );
            }
            if !seen.insert(&setting.key) {
                bail!("the setting {:?} is listed twice", setting.key);
            }
            if setting.kind == SettingKind::Choice && setting.options.is_empty() {
                bail!("the setting {:?} has no options", setting.key);
            }
            if let Some(default) = &setting.default
                && !setting.accepts(default)
            {
                bail!("the default of {:?} does not fit its type", setting.key);
            }
        }
        Ok(())
    }
}

// --- Installed plugins ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Built into Butter.
    Bundled,
    /// Installed for everyone, under `$XDG_DATA_DIRS`.
    System,
    /// Installed for this user.
    User,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Bundled => "Built in",
            Source::System => "Installed for everyone",
            Source::User => "Installed for you",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Plugin {
    pub manifest: Manifest,
    pub dir: PathBuf,
    pub source: Source,
}

impl Plugin {
    pub fn id(&self) -> &str {
        &self.manifest.id
    }

    pub fn name(&self) -> &str {
        &self.manifest.name
    }

    pub fn action(&self, id: &str) -> Option<&Action> {
        self.manifest.actions.iter().find(|a| a.id == id)
    }

    pub fn setting(&self, key: &str) -> Option<&Setting> {
        self.manifest.settings.iter().find(|s| s.key == key)
    }

    /// Where the plugin keeps files that should last.
    pub fn data_dir(&self) -> PathBuf {
        Paths::get().plugin_data(self.id())
    }

    /// Where the plugin keeps files it can make again.
    pub fn cache_dir(&self) -> PathBuf {
        Paths::get().plugin_cache(self.id())
    }

    pub fn icon(&self) -> Option<PathBuf> {
        let icon = self.dir.join(self.manifest.icon.as_deref()?);
        icon.is_file().then_some(icon)
    }

    /// Each setting's value: the user's, where it is still valid, or its
    /// default.
    pub fn values(&self) -> BTreeMap<String, toml::Value> {
        let state = State::load(self.id());
        values_of(&self.manifest, &state)
    }
}

fn values_of(manifest: &Manifest, state: &State) -> BTreeMap<String, toml::Value> {
    manifest
        .settings
        .iter()
        .map(|setting| {
            let value = state
                .settings
                .get(&setting.key)
                .filter(|v| setting.accepts(v))
                .cloned()
                .unwrap_or_else(|| setting.default_value());
            (setting.key.clone(), value)
        })
        .collect()
}

/// What Butter remembers for a plugin, in
/// `$XDG_CONFIG_HOME/butter/plugins/<id>.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// The settings the user changed.
    pub settings: toml::Table,
    /// Desktop file ids of the shortcuts made to its actions.
    pub shortcuts: Vec<String>,
}

impl State {
    pub fn load(id: &str) -> State {
        std::fs::read_to_string(Paths::get().plugin_state(id))
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Change a plugin's state and save it.
    pub fn update(id: &str, change: impl FnOnce(&mut State)) -> anyhow::Result<()> {
        let mut state = State::load(id);
        change(&mut state);
        let path = Paths::get().plugin_state(id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        util::write_atomic(&path, toml::to_string_pretty(&state)?.as_bytes())
    }
}

/// Set (or with None, reset) one of `plugin`'s settings.
pub fn set_setting(plugin: &Plugin, key: &str, value: Option<toml::Value>) -> anyhow::Result<()> {
    let setting = plugin
        .setting(key)
        .with_context(|| format!("{} has no setting {key:?}", plugin.name()))?;
    if let Some(value) = &value
        && !setting.accepts(value)
    {
        bail!("{value} is not a valid value for {key:?}");
    }
    State::update(plugin.id(), |state| match value {
        Some(value) => {
            state.settings.insert(key.to_string(), value);
        }
        None => {
            state.settings.remove(key);
        }
    })
}

/// What [`discover`] found.
#[derive(Debug, Default)]
pub struct Found {
    /// By name.
    pub plugins: Vec<Plugin>,
    /// Folders that are not usable plugins, and why.
    pub problems: Vec<(PathBuf, String)>,
}

/// Every plugin, the built-in ones first written out to the cache.
pub fn discover() -> Found {
    let paths = Paths::get();
    let mut found = Found::default();
    let mut by_id: BTreeMap<String, Plugin> = BTreeMap::new();

    for (id, files) in BUNDLED {
        let dir = paths.bundled_plugins().join(id);
        if let Err(e) = sync_tree(&dir, files) {
            found
                .problems
                .push((dir.clone(), format!("could not unpack: {e:#}")));
            continue;
        }
        match load(&dir, Source::Bundled) {
            Ok(plugin) => {
                by_id.insert(plugin.id().to_string(), plugin);
            }
            Err(e) => found.problems.push((dir, format!("{e:#}"))),
        }
    }
    let system = gtk::glib::system_data_dirs()
        .into_iter()
        .map(|d| (d.join("butter").join("plugins"), Source::System));
    // Later folders win, so the user's come last and the first system
    // folder (the most important) after the others.
    let mut system: Vec<_> = system.collect();
    system.reverse();
    for (root, source) in system.into_iter().chain([(paths.plugins(), Source::User)]) {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        let mut dirs: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.join(MANIFEST).is_file())
            .collect();
        dirs.sort();
        for dir in dirs {
            match load(&dir, source) {
                Ok(plugin) => {
                    by_id.insert(plugin.id().to_string(), plugin);
                }
                Err(e) => found.problems.push((dir, format!("{e:#}"))),
            }
        }
    }
    found.plugins = by_id.into_values().collect();
    found
        .plugins
        .sort_by_key(|p| p.manifest.name.to_lowercase());
    found
}

/// The plugin `id`, if there is one.
pub fn find(id: &str) -> Option<Plugin> {
    discover().plugins.into_iter().find(|p| p.id() == id)
}

fn load(dir: &Path, source: Source) -> anyhow::Result<Plugin> {
    let text = std::fs::read_to_string(dir.join(MANIFEST))
        .with_context(|| format!("could not read {}", dir.join(MANIFEST).display()))?;
    let manifest = Manifest::parse(&text).context("invalid plugin.toml")?;
    if !dir.join(&manifest.entry).is_file() {
        bail!("its entry, {}, is missing", manifest.entry);
    }
    Ok(Plugin {
        manifest,
        dir: dir.to_path_buf(),
        source,
    })
}

/// Make `dir` hold exactly `files`, rewriting it only when they changed.
fn sync_tree(dir: &Path, files: Files) -> anyhow::Result<()> {
    let mut hash = Sha256::new();
    for (name, data) in files {
        hash.update(name.as_bytes());
        hash.update([0]);
        hash.update((data.len() as u64).to_le_bytes());
        hash.update(data);
    }
    let stamp: String = hash.finalize()[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let stamp_file = dir.join(".butter-stamp");
    if std::fs::read_to_string(&stamp_file).ok().as_deref() == Some(stamp.as_str()) {
        return Ok(());
    }
    let staging = dir.with_extension("new");
    let _ = std::fs::remove_dir_all(&staging);
    for (name, data) in files {
        let path = staging.join(name);
        std::fs::create_dir_all(path.parent().context("no parent")?)?;
        std::fs::write(&path, data)?;
    }
    std::fs::write(staging.join(".butter-stamp"), &stamp)?;
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    std::fs::rename(&staging, dir)?;
    Ok(())
}

// --- Running actions ---

/// The Python interpreter plugins run with: the one chosen in Preferences,
/// or `python3` on the `PATH`.
pub fn python(settings: &Settings) -> anyhow::Result<PathBuf> {
    match settings
        .python
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        Some(chosen) => which::which(chosen)
            .with_context(|| format!("the Python chosen in Preferences, {chosen}, was not found")),
        None => which::which("python3")
            .context("plugins need Python 3. Install python3, or choose a Python in Preferences"),
    }
}

/// An action to run.
#[derive(Debug, Clone)]
pub struct Request {
    pub plugin: Plugin,
    pub action: String,
    /// The bottle a bottle action runs in.
    pub bottle: Option<Bottle>,
    /// Links and files to open.
    pub open: Vec<String>,
    /// Whether Butter's window is shown.
    pub interactive: bool,
}

impl Request {
    fn message(&self) -> Value {
        let plugin = &self.plugin;
        let settings: serde_json::Map<String, Value> = plugin
            .values()
            .into_iter()
            .map(|(key, value)| {
                let empty_bottle = plugin
                    .setting(&key)
                    .is_some_and(|s| s.kind == SettingKind::Bottle)
                    && value.as_str() == Some("");
                let value = match empty_bottle {
                    true => Value::Null,
                    false => serde_json::to_value(&value).unwrap_or(Value::Null),
                };
                (key, value)
            })
            .collect();
        json!({
            "t": "invoke",
            "api": plugin.manifest.api,
            "butter": config::VERSION,
            "action": self.action,
            "entry": plugin.manifest.entry,
            "plugin": {
                "id": plugin.id(),
                "name": plugin.name(),
                "version": plugin.manifest.version,
                "dir": plugin.dir,
                "data_dir": plugin.data_dir(),
                "cache_dir": plugin.cache_dir(),
            },
            "settings": settings,
            "bottle": self.bottle.as_ref().map(bottle_json),
            "open": self.open,
            "interactive": self.interactive,
            "host": {
                "arch": std::env::consts::ARCH,
                "flatpak": config::is_flatpak(),
            },
        })
    }
}

/// How an action ended.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Outcome {
    pub message: Option<String>,
    pub effects: Vec<Effect>,
}

/// Something an action asked the interface to do once it has finished.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Effect {
    /// Start a program, as its Run button does.
    Launch {
        bottle: String,
        target: PathBuf,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        open: Vec<String>,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        backend: Backend,
    },
    /// Add a shortcut to one of the plugin's actions to the desktop's menu.
    Shortcut {
        name: String,
        action: String,
        #[serde(default)]
        schemes: Vec<String>,
        #[serde(default)]
        mime_types: Vec<String>,
        #[serde(default)]
        icon: Option<PathBuf>,
        #[serde(default)]
        categories: String,
        #[serde(default)]
        comment: Option<String>,
    },
    /// Show a message.
    Notify { text: String },
    /// Open a folder (an absolute path) or a web page (an `https:` link).
    Open { location: String },
}

/// Run `request`'s action to completion, reporting through `reporter`.
pub fn run(request: &Request, reporter: &Reporter) -> anyhow::Result<Outcome> {
    let plugin = &request.plugin;
    if request.action.is_empty() || plugin.action(&request.action).is_none() {
        bail!("{} has no action {:?}", plugin.name(), request.action);
    }
    let python = python(&Settings::load())?;
    let sdk = Paths::get().python_sdk();
    sync_tree(&sdk, SDK).context("could not write Butter's Python module")?;
    std::fs::create_dir_all(plugin.data_dir())?;
    std::fs::create_dir_all(plugin.cache_dir())?;
    let mut calls = Calls { plugin, reporter };
    talk(
        &python,
        &sdk,
        &plugin.dir,
        &request.message(),
        reporter,
        &mut |method, params| calls.call(method, params),
    )
}

/// Run the Python side and hold the conversation described at the top.
fn talk(
    python: &Path,
    sdk: &Path,
    dir: &Path,
    invoke: &Value,
    reporter: &Reporter,
    call: &mut dyn FnMut(&str, Value) -> anyhow::Result<Value>,
) -> anyhow::Result<Outcome> {
    let mut child = Command::new(python)
        .args(["-I", "-B", "-u", "-X", "utf8", "-c", BOOT])
        .arg(sdk)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // A group of its own, so cancelling reaches what the plugin started.
        .process_group(0)
        .spawn()
        .with_context(|| format!("could not start {}", python.display()))?;
    let pid = child.id() as libc::pid_t;

    // Its log.
    let (logged, log_done) = std::sync::mpsc::channel::<()>();
    let stderr = child.stderr.take().context("no stderr")?;
    let log_reporter = reporter.clone();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut buf = Vec::new();
        while reader
            .read_until(b'\n', &mut buf)
            .map(|n| n > 0)
            .unwrap_or(false)
        {
            let text = String::from_utf8_lossy(&buf);
            let text = text.trim_end().rsplit('\r').next().unwrap_or_default();
            log_reporter.line(text.to_string());
            buf.clear();
        }
        let _ = logged.send(());
    });

    // Cancelling asks the plugin to stop, then makes it.
    let finished = Arc::new(AtomicBool::new(false));
    let watcher = {
        let finished = finished.clone();
        let reporter = reporter.clone();
        std::thread::spawn(move || {
            while !finished.load(Ordering::SeqCst) {
                if reporter.is_cancelled() {
                    // SAFETY: plain syscalls on a process group we created.
                    unsafe { libc::killpg(pid, libc::SIGTERM) };
                    let asked = Instant::now();
                    while !finished.load(Ordering::SeqCst) {
                        if asked.elapsed() > Duration::from_secs(5) {
                            unsafe { libc::killpg(pid, libc::SIGKILL) };
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        })
    };

    let mut stdin = child.stdin.take().context("no stdin")?;
    let stdout = child.stdout.take().context("no stdout")?;
    let mut ended: Option<Result<Outcome, String>> = None;
    if writeln!(stdin, "{invoke}")
        .and_then(|_| stdin.flush())
        .is_ok()
    {
        ended = converse(BufReader::new(stdout), &mut stdin, reporter, call);
    }
    drop(stdin);
    let status = child.wait()?;
    finished.store(true, Ordering::SeqCst);
    let _ = watcher.join();
    // The last lines of the log, such as a traceback; a program the plugin
    // left running may hold the pipe open, so don't wait for it long.
    let _ = log_done.recv_timeout(Duration::from_secs(2));

    if reporter.is_cancelled() {
        bail!("cancelled");
    }
    match ended {
        Some(Ok(outcome)) => Ok(outcome),
        Some(Err(message)) => bail!("{message}"),
        None => bail!("the plugin stopped without finishing ({status})"),
    }
}

/// Read the plugin's messages until it closes its output, answering its
/// calls. Returns how it said it ended, if it did.
fn converse(
    stdout: impl BufRead,
    stdin: &mut impl Write,
    reporter: &Reporter,
    call: &mut dyn FnMut(&str, Value) -> anyhow::Result<Value>,
) -> Option<Result<Outcome, String>> {
    let mut ended = None;
    for line in stdout.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            reporter.line(line);
            continue;
        };
        let text = |key: &str| message.get(key).and_then(Value::as_str).unwrap_or_default();
        match text("t") {
            "stage" => reporter.stage(
                text("title"),
                message.get("fraction").and_then(Value::as_f64),
            ),
            "progress" => {
                if let Some(fraction) = message.get("fraction").and_then(Value::as_f64) {
                    reporter.progress(fraction);
                }
            }
            "log" => reporter.line(text("text")),
            "call" => {
                let id = message.get("id").cloned().unwrap_or(Value::Null);
                let params = message.get("params").cloned().unwrap_or(json!({}));
                let reply = match call(text("method"), params) {
                    Ok(value) => json!({"t": "reply", "id": id, "ok": value}),
                    Err(e) => json!({"t": "reply", "id": id, "error": format!("{e:#}")}),
                };
                if writeln!(stdin, "{reply}")
                    .and_then(|_| stdin.flush())
                    .is_err()
                {
                    break;
                }
            }
            "done" => {
                let effects = message
                    .get("effects")
                    .cloned()
                    .map(serde_json::from_value::<Vec<Effect>>)
                    .unwrap_or(Ok(Vec::new()));
                ended = Some(match effects {
                    Ok(effects) => Ok(Outcome {
                        message: message
                            .get("message")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        effects,
                    }),
                    Err(e) => Err(format!(
                        "the plugin asked for something Butter can't do: {e}"
                    )),
                });
            }
            "fail" => ended = Some(Err(text("message").to_string())),
            _ => reporter.line(line.clone()),
        }
    }
    ended
}

/// A bottle as plugins see it.
fn bottle_json(bottle: &Bottle) -> Value {
    json!({
        "id": bottle.id,
        "name": bottle.config.name,
        "dir": bottle.dir,
        "prefix": bottle.prefix(),
        "drive": bottle.drive(),
        "runner": bottle.config.runner,
        "arch": bottle.config.arch.value(),
        "ready": bottle.is_ready(),
    })
}

/// Answers the calls a plugin makes.
struct Calls<'a> {
    plugin: &'a Plugin,
    reporter: &'a Reporter,
}

fn params<T: serde::de::DeserializeOwned>(params: Value) -> anyhow::Result<T> {
    serde_json::from_value(params).context("bad parameters")
}

#[derive(Deserialize)]
struct IdParams {
    id: String,
}

#[derive(Deserialize)]
struct CreateParams {
    name: String,
    #[serde(default)]
    runner: Option<String>,
    #[serde(default)]
    arch: Option<String>,
}

#[derive(Deserialize)]
struct RunParams {
    bottle: String,
    target: PathBuf,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    backend: Backend,
}

#[derive(Deserialize)]
struct GetParams {
    url: String,
    #[serde(default)]
    headers: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct DownloadParams {
    url: String,
    dest: PathBuf,
    #[serde(default)]
    sha256: Option<String>,
}

#[derive(Deserialize)]
struct SetParams {
    key: String,
    value: Value,
}

#[derive(Deserialize)]
struct PathParams {
    path: PathBuf,
}

fn check_url(url: &str) -> anyhow::Result<()> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        bail!("only http and https links can be fetched, not {url}");
    }
    Ok(())
}

impl Calls<'_> {
    fn call(&mut self, method: &str, p: Value) -> anyhow::Result<Value> {
        match method {
            "bottles.list" => Ok(Value::Array(
                bottle::list().iter().map(bottle_json).collect(),
            )),
            "bottles.get" => {
                let p: IdParams = params(p)?;
                Ok(bottle::load(&p.id)
                    .map(|b| bottle_json(&b))
                    .unwrap_or(Value::Null))
            }
            "bottles.create" => self.create_bottle(params(p)?),
            "bottles.run" => {
                let p: RunParams = params(p)?;
                let bottle = bottle::load(&p.bottle)
                    .with_context(|| format!("there is no bottle {}", p.bottle))?;
                let mut command = bottle.run_command(&p.target, &p.args, p.backend)?;
                let status = self.reporter.run(&mut command)?;
                Ok(json!({ "code": status.code().unwrap_or(-1) }))
            }
            "http.get" => {
                let p: GetParams = params(p)?;
                check_url(&p.url)?;
                let mut request = github::agent().get(&p.url);
                for (key, value) in &p.headers {
                    request = request.header(key.as_str(), value.as_str());
                }
                let mut response = request
                    .call()
                    .with_context(|| format!("could not fetch {}", p.url))?;
                let text = response
                    .body_mut()
                    .with_config()
                    .limit(16 << 20)
                    .read_to_string()?;
                Ok(json!({ "text": text }))
            }
            "http.download" => {
                let p: DownloadParams = params(p)?;
                check_url(&p.url)?;
                runner::download(&p.url, &p.dest, self.reporter)?;
                let digest = sha256_file(&p.dest)?;
                if let Some(expected) = &p.sha256
                    && !expected.eq_ignore_ascii_case(&digest)
                {
                    let _ = std::fs::remove_file(&p.dest);
                    bail!("{} is not the expected file (SHA-256 {digest})", p.url);
                }
                Ok(json!({ "sha256": digest }))
            }
            "settings.set" => {
                let p: SetParams = params(p)?;
                let value = match p.value {
                    Value::Null => None,
                    value => Some(toml::Value::try_from(value).context("not a setting value")?),
                };
                set_setting(self.plugin, &p.key, value)?;
                Ok(Value::Null)
            }
            "apps.info" => {
                let p: PathParams = params(p)?;
                if !apps::is_bundle(&p.path) {
                    bail!("{} is not an application bundle", p.path.display());
                }
                let app = apps::read(&p.path);
                Ok(json!({
                    "name": app.name,
                    "version": app.version,
                    "category": app.category,
                    "schemes": app.schemes,
                    "mime_types": app.mime_types,
                    "icon": app.render_icon(),
                }))
            }
            _ => bail!("Butter has no call named {method:?}; it may be too old for this plugin"),
        }
    }

    fn create_bottle(&self, p: CreateParams) -> anyhow::Result<Value> {
        let arch = match p.arch.as_deref() {
            None | Some("") => Arch::Host,
            Some("x86_64") => Arch::X86_64,
            Some("aarch64") => Arch::Aarch64,
            Some(other) => bail!("unknown architecture {other:?}"),
        };
        let usable = |id: &str| runner::find(id).is_some_and(|r| r.is_usable());
        let runner = match p.runner {
            Some(id) if usable(&id) => id,
            Some(id) => bail!("the runner {id} is not installed"),
            None => Settings::load()
                .default_runner
                .filter(|id| usable(id))
                .or_else(|| {
                    runner::installed()
                        .into_iter()
                        .find(|r| r.is_usable())
                        .map(|r| r.id)
                })
                .context(
                    "there is no runner to make a bottle with; install one on the Runners page",
                )?,
        };
        let mut config = bottle::Config::new(&p.name, Some(runner));
        config.arch = arch;
        let bottle = bottle::create(config)?;
        self.reporter
            .stage(format!("Creating the {} bottle", bottle.config.name), None);
        let mut command = bottle.prefix_command("create", &bottle.create_args(true))?;
        self.reporter
            .run_checked(&mut command)
            .context("could not create the bottle's prefix")?;
        Ok(bottle_json(&bottle))
    }
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buf = vec![0; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

// --- The command line ---

/// An action asked for on the command line, as plugin shortcuts do:
/// `--plugin PLUGIN ACTION [--bottle BOTTLE] [URL-OR-FILE]...`
#[derive(Debug, Clone, PartialEq)]
pub struct Invocation {
    pub plugin: String,
    pub action: String,
    pub bottle: Option<String>,
    pub open: Vec<String>,
}

impl Invocation {
    /// None when `args` (without the program name) do not ask for a plugin.
    pub fn parse(args: &[String]) -> anyhow::Result<Option<Invocation>> {
        if args.first().map(String::as_str) != Some(FLAG) {
            return Ok(None);
        }
        let mut rest = args[1..].iter().peekable();
        let plugin = rest.next().context("--plugin needs a plugin")?.clone();
        let action = rest.next().context("--plugin needs an action")?.clone();
        let mut bottle = None;
        if rest.peek().map(|s| s.as_str()) == Some("--bottle") {
            rest.next();
            bottle = Some(rest.next().context("--bottle needs a bottle")?.clone());
        }
        Ok(Some(Invocation {
            plugin,
            action,
            bottle,
            open: rest.cloned().collect(),
        }))
    }
}

/// The desktop file id of a shortcut to `plugin`'s `action`. The portal
/// needs it to start with Butter's own id.
pub fn desktop_id(plugin: &str, action: &str) -> String {
    let clean = |s: &str| -> String {
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect()
    };
    format!(
        "{}.plugin_{}.{}.desktop",
        config::APP_ID,
        clean(plugin),
        clean(action)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"
id = "demo"
name = "Demo"
version = "1.0"

[[actions]]
id = "hello"
title = "Say Hello"

[[actions]]
id = "here"
title = "In a Bottle"
scope = "bottle"

[[settings]]
key = "greeting"
title = "Greeting"
type = "string"
default = "hi"

[[settings]]
key = "loud"
title = "Loud"
type = "bool"

[[settings]]
key = "count"
title = "Count"
type = "int"
min = 1
max = 5
default = 2

[[settings]]
key = "mode"
title = "Mode"
type = "choice"
options = [{ value = "a", label = "A" }, { value = "b", label = "B" }]

[[settings]]
key = "where"
title = "Bottle"
type = "bottle"
"#;

    #[test]
    fn manifests() {
        let manifest = Manifest::parse(MANIFEST).unwrap();
        assert_eq!(manifest.entry, "plugin.py");
        assert_eq!(manifest.actions[1].scope, Scope::Bottle);
        let mut state = State::default();
        state
            .settings
            .insert("loud".into(), toml::Value::Boolean(true));
        state
            .settings
            .insert("count".into(), toml::Value::Integer(9));
        state
            .settings
            .insert("mode".into(), toml::Value::String("b".into()));
        let values = values_of(&manifest, &state);
        assert_eq!(values["greeting"].as_str(), Some("hi"));
        assert_eq!(values["loud"].as_bool(), Some(true));
        assert_eq!(values["count"].as_integer(), Some(2), "out of range");
        assert_eq!(values["mode"].as_str(), Some("b"));
        assert_eq!(values["where"].as_str(), Some(""));

        for bad in [
            "id = \"../x\"\nname = \"X\"",
            "id = \"x\"\nname = \" \"",
            "id = \"x\"\nname = \"X\"\napi = 99",
            "id = \"x\"\nname = \"X\"\nentry = \"../evil.py\"",
            "id = \"x\"\nname = \"X\"\n[[actions]]\nid = \"a\"\ntitle = \"A\"\n[[actions]]\nid = \"a\"\ntitle = \"B\"",
            "id = \"x\"\nname = \"X\"\n[[settings]]\nkey = \"c\"\ntitle = \"C\"\ntype = \"choice\"",
            "id = \"x\"\nname = \"X\"\n[[settings]]\nkey = \"c\"\ntitle = \"C\"\ntype = \"int\"\ndefault = \"no\"",
        ] {
            assert!(Manifest::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn bundled_plugins_are_valid() {
        for (id, files) in BUNDLED {
            let (_, toml) = files.iter().find(|(n, _)| *n == "plugin.toml").unwrap();
            let manifest = Manifest::parse(std::str::from_utf8(toml).unwrap()).unwrap();
            assert_eq!(manifest.id, *id);
            assert!(files.iter().any(|(n, _)| *n == manifest.entry));
            if let Some(icon) = &manifest.icon {
                assert!(files.iter().any(|(n, _)| n == icon));
            }
        }
    }

    #[test]
    fn command_lines() {
        let s = |args: &[&str]| args.iter().map(|a| a.to_string()).collect::<Vec<_>>();
        assert_eq!(Invocation::parse(&s(&["--run", "x"])).unwrap(), None);
        assert!(Invocation::parse(&s(&["--plugin", "roblox"])).is_err());
        assert_eq!(
            Invocation::parse(&s(&["--plugin", "roblox", "player", "roblox-player:1+x"]))
                .unwrap()
                .unwrap(),
            Invocation {
                plugin: "roblox".into(),
                action: "player".into(),
                bottle: None,
                open: vec!["roblox-player:1+x".into()],
            }
        );
        let with_bottle = Invocation::parse(&s(&["--plugin", "p", "a", "--bottle", "b"]))
            .unwrap()
            .unwrap();
        assert_eq!(with_bottle.bottle.as_deref(), Some("b"));
        assert!(with_bottle.open.is_empty());
        assert_eq!(
            desktop_id("roblox", "player"),
            "io.github.microlift.butter.plugin_roblox.player.desktop"
        );
    }

    #[test]
    fn effects() {
        let effects: Vec<Effect> = serde_json::from_value(json!([
            {"type": "launch", "bottle": "b", "target": "/x/Roblox.app", "open": ["roblox://x"]},
            {"type": "shortcut", "name": "Roblox", "action": "player", "schemes": ["roblox"]},
            {"type": "notify", "text": "hi"},
        ]))
        .unwrap();
        assert!(
            matches!(&effects[0], Effect::Launch { backend: Backend::Auto, open, .. } if open.len() == 1)
        );
        assert!(matches!(&effects[1], Effect::Shortcut { action, .. } if action == "player"));
        assert!(serde_json::from_value::<Effect>(json!({"type": "format_disk"})).is_err());
    }

    /// The SDK, a plugin and Butter's side of the conversation, with a real
    /// Python.
    #[test]
    fn talks_to_python() {
        let Ok(python) = which::which("python3") else {
            eprintln!("skipped: no python3");
            return;
        };
        let root = std::env::temp_dir().join(format!("butter-plugin-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let sdk = root.join("sdk");
        sync_tree(&sdk, SDK).unwrap();
        // Unchanged files are not rewritten.
        sync_tree(&sdk, SDK).unwrap();
        let dir = root.join("demo");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("helper.py"), "WORD = 'helped'\n").unwrap();
        std::fs::write(
            dir.join("plugin.py"),
            r#"
import sys, butter, helper

@butter.action("hello")
def hello(ctx):
    ctx.stage("Greeting", 0.5)
    ctx.progress(0.25)
    ctx.log(ctx.setting("greeting"), helper.WORD, ctx.open)
    print("printed")
    answer = ctx._channel.call("echo", {"x": 1})
    ctx.log("echo", answer["x"])
    try:
        ctx._channel.call("nope", {})
    except butter.PluginError as e:
        ctx.log("refused:", e)
    ctx.launch(butter.Bottle({"id": "b", "name": "B", "dir": "/b", "prefix": "/b/p", "drive": "/b/p/root"}), "/b/p/root/X.app", open=ctx.open, name="X")
    ctx.notify("done!")
    return "said " + ctx.setting("greeting")

@butter.action("fail")
def fail(ctx):
    raise butter.PluginError("no luck")

@butter.action("crash")
def crash(ctx):
    raise ValueError("oops")
"#,
        )
        .unwrap();
        let invoke = |action: &str| {
            json!({
                "t": "invoke", "api": 1, "action": action, "entry": "plugin.py",
                "plugin": {"id": "demo", "name": "Demo", "dir": dir, "data_dir": root.join("data"), "cache_dir": root.join("cache")},
                "settings": {"greeting": "hello"}, "bottle": null, "open": ["demo://x"],
            })
        };
        let conversation = |action: &str| {
            let (receiver, _) = crate::process::spawn({
                let (python, sdk, dir, invoke) =
                    (python.clone(), sdk.clone(), dir.clone(), invoke(action));
                move |reporter| {
                    let outcome = talk(
                        &python,
                        &sdk,
                        &dir,
                        &invoke,
                        reporter,
                        &mut |method, params| match method {
                            "echo" => Ok(params),
                            _ => bail!("unknown call"),
                        },
                    )?;
                    Ok(serde_json::to_string(&(outcome.message, outcome.effects.len())).unwrap())
                }
            });
            let mut lines = Vec::new();
            let mut result = None;
            while let Ok(event) = receiver.recv_blocking() {
                match event {
                    crate::process::Event::Line(line) => lines.push(line),
                    crate::process::Event::Finished(r) => result = Some(r),
                    _ => {}
                }
            }
            (lines, result.unwrap())
        };

        let (lines, result) = conversation("hello");
        assert_eq!(result.unwrap(), r#"["said hello",2]"#, "{lines:#?}");
        assert!(
            lines.contains(&"hello helped ['demo://x']".to_string()),
            "{lines:#?}"
        );
        assert!(lines.contains(&"printed".to_string()), "{lines:#?}");
        assert!(lines.contains(&"echo 1".to_string()), "{lines:#?}");
        assert!(
            lines.contains(&"refused: unknown call".to_string()),
            "{lines:#?}"
        );

        let (_, result) = conversation("fail");
        assert_eq!(result.unwrap_err(), "no luck");
        let (lines, result) = conversation("crash");
        assert_eq!(result.unwrap_err(), "ValueError: oops");
        assert!(lines.iter().any(|l| l.contains("Traceback")), "{lines:#?}");
        let (_, result) = conversation("missing");
        assert!(result.unwrap_err().contains("no handler"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cancelling_stops_the_plugin() {
        let Ok(python) = which::which("python3") else {
            return;
        };
        let root =
            std::env::temp_dir().join(format!("butter-plugin-cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let sdk = root.join("sdk");
        sync_tree(&sdk, SDK).unwrap();
        let dir = root.join("slow");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("plugin.py"),
            "import time, butter\n@butter.action('wait')\ndef wait(ctx):\n    ctx.log('waiting')\n    time.sleep(60)\n",
        )
        .unwrap();
        let invoke = json!({
            "t": "invoke", "api": 1, "action": "wait",
            "plugin": {"id": "slow", "name": "Slow", "dir": dir, "data_dir": root.join("d"), "cache_dir": root.join("c")},
        });
        let (receiver, canceller) = crate::process::spawn(move |reporter| {
            talk(&python, &sdk, &dir, &invoke, reporter, &mut |_, _| {
                Ok(Value::Null)
            })?;
            Ok(String::new())
        });
        let started = Instant::now();
        let mut result = None;
        while let Ok(event) = receiver.recv_blocking() {
            match event {
                crate::process::Event::Line(line) if line == "waiting" => canceller.cancel(),
                crate::process::Event::Finished(r) => result = Some(r),
                _ => {}
            }
        }
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(result.unwrap().unwrap_err(), "cancelled");
        let _ = std::fs::remove_dir_all(&root);
    }
}
