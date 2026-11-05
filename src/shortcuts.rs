//! Shortcuts: desktop entries that run a bottle's application from the
//! desktop's applications menu, installed through the Dynamic Launcher portal.
//!
//! A shortcut runs `butter --run BOTTLE … APP %U`. The links and files it is
//! asked to open are handed to the application as URLs it can reach from
//! inside the bottle, which Rosemary delivers to `-application:openURLs:`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Context, bail};
use gtk::gio::prelude::*;
use gtk::{gio, glib};
use sha2::{Digest, Sha256};

use crate::apps::App;
use crate::bottle::{Backend, Bottle};
use crate::config::{self, Paths};

/// The command-line flag shortcuts use.
pub const RUN_FLAG: &str = "--run";

/// Where host files appear inside a bottle.
const HOST_MOUNT: &str = "/Volumes/Host";

const PORTAL_BUS: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const LAUNCHER: &str = "org.freedesktop.portal.DynamicLauncher";
const REQUEST: &str = "org.freedesktop.portal.Request";

/// Shown for applications without an icon of their own.
const FALLBACK_ICON: &[u8] =
    include_bytes!("../data/icons/hicolor/scalable/apps/io.github.microlift.butter.svg");

/// A shortcut to make: to a bottle's application, or to a plugin's action.
#[derive(Debug, Clone)]
pub struct Shortcut {
    /// The desktop file id.
    pub id: String,
    pub name: String,
    pub comment: String,
    /// Butter's arguments; `%U` follows them.
    pub args: Vec<String>,
    /// Freedesktop categories, such as `Game;`.
    pub categories: String,
    /// What it opens: MIME types, and `x-scheme-handler/` types for links.
    pub types: Vec<String>,
    /// The icon, a PNG or SVG.
    pub icon: Option<PathBuf>,
    /// An `X-Butter-…` key naming what it belongs to, and its value.
    pub owner: (&'static str, String),
}

impl Shortcut {
    /// A shortcut to `target` in the bottle `bottle`.
    #[allow(clippy::too_many_arguments)]
    pub fn for_app(
        bottle: &str,
        bottle_name: &str,
        name: &str,
        target: &Path,
        args: &[String],
        backend: Backend,
        app: &App,
        icon: Option<PathBuf>,
    ) -> Shortcut {
        let mut argv = vec![RUN_FLAG.to_string(), bottle.to_string()];
        if backend != Backend::Auto {
            argv.extend(["--backend".into(), backend_name(backend).into()]);
        }
        for arg in args {
            argv.extend(["--arg".into(), arg.clone()]);
        }
        argv.push(target.display().to_string());
        Shortcut {
            id: desktop_id(bottle, target),
            name: name.to_string(),
            comment: format!("Run in the {bottle_name} bottle with Rosemary"),
            args: argv,
            categories: app.categories().to_string(),
            types: app.handled_types(),
            icon,
            owner: ("X-Butter-Bottle", bottle.to_string()),
        }
    }

    /// A shortcut to `plugin`'s `action`, opening the links of `schemes`
    /// and the files of `mime_types`. Invalid ones are left out.
    #[allow(clippy::too_many_arguments)]
    pub fn for_plugin(
        plugin: &str,
        action: &str,
        name: &str,
        comment: &str,
        schemes: &[String],
        mime_types: &[String],
        categories: &str,
        icon: Option<PathBuf>,
    ) -> Shortcut {
        let mut types: Vec<String> = mime_types
            .iter()
            .filter(|t| crate::apps::valid_mime_type(t))
            .cloned()
            .collect();
        types.extend(
            schemes
                .iter()
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| crate::apps::valid_scheme(s) && s != "file")
                .map(|s| format!("x-scheme-handler/{s}")),
        );
        let categories: String = categories
            .split(';')
            .map(str::trim)
            .filter(|c| !c.is_empty() && c.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
            .map(|c| format!("{c};"))
            .collect();
        Shortcut {
            id: crate::plugins::desktop_id(plugin, action),
            name: name.to_string(),
            comment: comment.to_string(),
            args: vec![
                crate::plugins::FLAG.to_string(),
                plugin.to_string(),
                action.to_string(),
            ],
            categories,
            types,
            icon,
            owner: ("X-Butter-Plugin", plugin.to_string()),
        }
    }

    pub fn id(&self) -> String {
        self.id.clone()
    }

    /// The desktop entry. The portal sets `Name=` and `Icon=` itself, so
    /// they are only written for a shortcut Butter installs on its own.
    fn desktop_entry(&self, program: &str, icon: Option<&Path>) -> String {
        let mut exec: Vec<String> = std::iter::once(program)
            .chain(self.args.iter().map(String::as_str))
            .map(quote_exec_arg)
            .collect();
        exec.push("%U".into());

        let mut entry = String::from("[Desktop Entry]\nType=Application\n");
        let mut line = |key: &str, value: &str| {
            entry.push_str(&format!("{key}={}\n", escape_value(value)));
        };
        line("Name", &self.name);
        line("Comment", &self.comment);
        line("Exec", &exec.join(" "));
        if let Some(icon) = icon {
            line("Icon", &icon.display().to_string());
        }
        line("Terminal", "false");
        line("StartupNotify", "false");
        if !self.categories.is_empty() {
            line("Categories", &self.categories);
        }
        if !self.types.is_empty() {
            line("MimeType", &format!("{};", self.types.join(";")));
        }
        line(self.owner.0, &self.owner.1);
        entry
    }

    fn icon_bytes(&self) -> glib::Bytes {
        self.icon
            .as_deref()
            .and_then(|path| std::fs::read(path).ok())
            .map(glib::Bytes::from_owned)
            .unwrap_or_else(|| glib::Bytes::from_static(FALLBACK_ICON))
    }

    /// The extension of its icon file, for a shortcut Butter writes itself.
    fn icon_extension(&self) -> &'static str {
        match self.icon.as_deref().and_then(Path::extension) {
            Some(ext) if ext.eq_ignore_ascii_case("svg") => "svg",
            Some(_) => "png",
            None => "svg",
        }
    }
}

/// The desktop file id of the shortcut to `target` in `bottle`. The portal
/// needs it to start with Butter's own id.
pub fn desktop_id(bottle: &str, target: &Path) -> String {
    let bottle: String = bottle
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let hash = Sha256::digest(target.as_os_str().as_encoded_bytes());
    let hash: String = hash[..4].iter().map(|b| format!("{b:02x}")).collect();
    format!("{}.{bottle}.app_{hash}.desktop", config::APP_ID)
}

fn backend_name(backend: Backend) -> &'static str {
    match backend {
        Backend::Auto => "auto",
        Backend::Native => "native",
        Backend::Emulated => "emulated",
    }
}

/// One `Exec=` argument, quoted as the Desktop Entry specification asks.
fn quote_exec_arg(arg: &str) -> String {
    let arg = arg.replace('%', "%%");
    let reserved = |c: char| " \t\n\"'\\><~|&;$*?#()`".contains(c);
    if !arg.is_empty() && !arg.contains(reserved) {
        return arg;
    }
    let mut quoted = String::from("\"");
    for c in arg.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    quoted
}

/// A desktop entry string value: backslashes and line breaks are escaped.
fn escape_value(value: &str) -> String {
    let mut escaped = String::new();
    for c in value.chars() {
        match c {
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\t' => escaped.push_str("\\t"),
            '\r' => escaped.push_str("\\r"),
            c => escaped.push(c),
        }
    }
    escaped
}

/// The command a shortcut runs Butter with. In the Flatpak the portal
/// replaces it with `flatpak run`. Elsewhere it is a full path, since the
/// portal checks that it exists with a `PATH` of its own.
fn program() -> String {
    if config::is_flatpak() {
        return "butter".into();
    }
    let current = std::env::current_exe().ok();
    if let Some(found) = glib::find_program_in_path("butter")
        && let Some(current) = &current
        && std::fs::canonicalize(&found).ok().as_ref() == Some(current)
    {
        // Through a link that stays put when Butter is updated, such as a
        // Nix profile's.
        return found.display().to_string();
    }
    current
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "butter".into())
}

/// A run asked for on the command line, as shortcuts do:
/// `--run BOTTLE [--backend B] [--arg A]... PROGRAM [URL-OR-FILE]...`
#[derive(Debug, Clone, PartialEq)]
pub struct Launch {
    pub bottle: String,
    pub target: PathBuf,
    pub args: Vec<String>,
    pub backend: Backend,
    /// Links and files to open, as the desktop gave them.
    pub open: Vec<String>,
}

impl Launch {
    /// None when `args` (without the program name) do not ask for a run.
    pub fn parse(args: &[String]) -> anyhow::Result<Option<Launch>> {
        if args.first().map(String::as_str) != Some(RUN_FLAG) {
            return Ok(None);
        }
        let mut rest = args[1..].iter();
        let bottle = rest.next().context("--run needs a bottle")?.clone();
        let mut backend = Backend::Auto;
        let mut extra = Vec::new();
        let target = loop {
            match rest.next().map(String::as_str) {
                Some("--backend") => {
                    let name = rest.next().context("--backend needs a value")?;
                    backend = *Backend::ALL
                        .iter()
                        .find(|b| backend_name(**b) == name)
                        .with_context(|| format!("unknown backend {name}"))?;
                }
                Some("--arg") => extra.push(rest.next().context("--arg needs a value")?.clone()),
                Some(target) => break PathBuf::from(target),
                None => bail!("--run needs a program"),
            }
        };
        Ok(Some(Launch {
            bottle,
            target,
            args: extra,
            backend,
            open: rest.cloned().collect(),
        }))
    }

    /// The program's arguments: its own, then what it was asked to open,
    /// spelled the way it sees them in `bottle`.
    pub fn arguments(&self, bottle: &Bottle) -> Vec<String> {
        self.arguments_in(&bottle.drive())
    }

    fn arguments_in(&self, drive: &Path) -> Vec<String> {
        let mut args = self.args.clone();
        let open: Vec<String> = self.open.iter().map(|a| guest_url(drive, a)).collect();
        // Rosemary hands these to NSUserDefaults' NSArgumentDomain, as macOS
        // does for a launch argument. Info.plist does not say which one an
        // application reads, so the first link is given under all of them.
        if let Some(link) = open.iter().find(|a| is_link(a)) {
            for flag in PROTOCOL_FLAGS {
                args.extend([flag.to_string(), link.clone()]);
            }
        }
        args.extend(open);
        args
    }
}

/// The arguments Rosemary takes for the link an application was opened with.
const PROTOCOL_FLAGS: [&str; 3] = ["-protocolString", "-protocolUrl", "-protocolUri"];

/// Whether `arg` is a link to a protocol handler rather than a file.
fn is_link(arg: &str) -> bool {
    let Some((scheme, _)) = arg.split_once(':') else {
        return false;
    };
    !scheme.is_empty()
        && !scheme.eq_ignore_ascii_case("file")
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
}

/// A link or file to open, as a URL the guest can follow. Files become
/// `file:` URLs: inside the bottle's drive by their path there, anywhere
/// else through the host mount. Other links are left alone.
pub fn guest_url(drive: &Path, arg: &str) -> String {
    let path = if arg.starts_with('/') {
        Some(PathBuf::from(arg))
    } else if arg
        .get(..5)
        .is_some_and(|s| s.eq_ignore_ascii_case("file:"))
    {
        gio::File::for_uri(arg).path()
    } else {
        None
    };
    let Some(path) = path else {
        return arg.to_string();
    };
    let guest = match path.strip_prefix(drive) {
        Ok(rest) => Path::new("/").join(rest),
        Err(_) => Path::new(HOST_MOUNT).join(path.strip_prefix("/").unwrap_or(&path)),
    };
    gio::File::for_path(guest).uri().into()
}

/// Ask the desktop to add `shortcut` to its applications menu. The user
/// confirms it, and may rename it, in a dialog of the desktop's. Returns the
/// name it was given, or None if the user cancelled.
///
/// Outside the Flatpak, a desktop without the portal gets the desktop entry
/// written into `~/.local/share/applications` instead.
pub async fn install(shortcut: &Shortcut) -> anyhow::Result<Option<String>> {
    match install_with_portal(shortcut).await {
        Err(e) if !config::is_flatpak() => {
            eprintln!("butter: the Dynamic Launcher portal failed ({e:#}); writing the shortcut");
            install_files(shortcut)?;
            Ok(Some(shortcut.name.clone()))
        }
        result => result,
    }
}

/// Remove the shortcut with the desktop file id `id`.
pub async fn uninstall(id: &str) -> anyhow::Result<()> {
    let written = remove_files(id);
    let result = async {
        let bus = gio::bus_get_future(gio::BusType::Session).await?;
        call(
            &bus,
            "Uninstall",
            glib::Variant::tuple_from_iter([id.to_variant(), no_options()]),
            "()",
        )
        .await?;
        anyhow::Ok(())
    }
    .await;
    match result {
        Err(_) if written => Ok(()),
        result => result,
    }
}

async fn install_with_portal(shortcut: &Shortcut) -> anyhow::Result<Option<String>> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let bus = gio::bus_get_future(gio::BusType::Session).await?;
    let unique = bus
        .unique_name()
        .context("not connected to the session bus")?;
    let sender = unique.trim_start_matches(':').replace('.', "_");
    let token = format!(
        "butter_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let expected = format!("{PORTAL_PATH}/request/{sender}/{token}");

    // Listen before asking, so a quick answer is not missed.
    let (sender, responses) = async_channel::bounded(1);
    let listen = |path: &str| {
        let sender = sender.clone();
        bus.subscribe_to_signal(
            Some(PORTAL_BUS),
            Some(REQUEST),
            Some("Response"),
            Some(path),
            None,
            gio::DBusSignalFlags::NONE,
            move |signal| {
                let _ = sender.try_send(signal.parameters.clone());
            },
        )
    };
    let mut subscriptions = vec![listen(&expected)];

    let icon = gio::BytesIcon::new(&shortcut.icon_bytes())
        .serialize()
        .context("could not serialize the icon")?;
    let options = glib::VariantDict::new(None);
    options.insert("handle_token", &token);
    let reply = call(
        &bus,
        "PrepareInstall",
        glib::Variant::tuple_from_iter([
            "".to_variant(),
            shortcut.name.to_variant(),
            glib::Variant::from_variant(&icon),
            options.end(),
        ]),
        "(o)",
    )
    .await
    .context("the Dynamic Launcher portal is not available")?;
    // Old portals choose the path themselves.
    if let Some(handle) = reply.child_value(0).str()
        && handle != expected
    {
        subscriptions.push(listen(handle));
    }

    let response = responses
        .recv()
        .await
        .context("no answer from the portal")?;
    drop(subscriptions);
    let code = response.child_value(0).get::<u32>().unwrap_or(2);
    let results = glib::VariantDict::new(Some(&response.child_value(1)));
    match code {
        0 => {}
        1 => return Ok(None),
        _ => bail!("the desktop could not prepare the shortcut"),
    }
    let token: String = results
        .lookup("token")
        .ok()
        .flatten()
        .context("the portal gave no token")?;
    let name: String = results
        .lookup("name")
        .ok()
        .flatten()
        .unwrap_or_else(|| shortcut.name.clone());

    call(
        &bus,
        "Install",
        glib::Variant::tuple_from_iter([
            token.to_variant(),
            shortcut.id().to_variant(),
            shortcut.desktop_entry(&program(), None).to_variant(),
            no_options(),
        ]),
        "()",
    )
    .await
    .context("the desktop refused the shortcut")?;
    Ok(Some(name))
}

fn no_options() -> glib::Variant {
    glib::VariantDict::new(None).end()
}

async fn call(
    bus: &gio::DBusConnection,
    method: &str,
    parameters: glib::Variant,
    reply: &str,
) -> anyhow::Result<glib::Variant> {
    let reply_type = glib::VariantTy::new(reply).expect("a valid type");
    Ok(bus
        .call_future(
            Some(PORTAL_BUS),
            PORTAL_PATH,
            LAUNCHER,
            method,
            Some(&parameters),
            Some(reply_type),
            gio::DBusCallFlags::NONE,
            -1,
        )
        .await?)
}

fn applications_dir() -> PathBuf {
    glib::user_data_dir().join("applications")
}

fn install_files(shortcut: &Shortcut) -> anyhow::Result<()> {
    let id = shortcut.id();
    let stem = id.trim_end_matches(".desktop");
    let icon = Paths::get()
        .shortcut_icons()
        .join(format!("{stem}.{}", shortcut.icon_extension()));
    std::fs::create_dir_all(icon.parent().context("icon folder")?)?;
    crate::util::write_atomic(&icon, &shortcut.icon_bytes())?;
    let dir = applications_dir();
    std::fs::create_dir_all(&dir)?;
    crate::util::write_atomic(
        &dir.join(&id),
        shortcut.desktop_entry(&program(), Some(&icon)).as_bytes(),
    )?;
    refresh_desktop_database(&dir);
    Ok(())
}

/// Remove a shortcut Butter wrote itself; false if there was none.
fn remove_files(id: &str) -> bool {
    let dir = applications_dir();
    let removed = std::fs::remove_file(dir.join(id)).is_ok();
    let stem = id.trim_end_matches(".desktop");
    for ext in ["png", "svg"] {
        let _ = std::fs::remove_file(Paths::get().shortcut_icons().join(format!("{stem}.{ext}")));
    }
    if removed {
        refresh_desktop_database(&dir);
    }
    removed
}

/// So that the desktop sees new file associations at once. Optional.
fn refresh_desktop_database(dir: &Path) {
    let _ = std::process::Command::new("update-desktop-database")
        .arg(dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_runs() {
        assert_eq!(Launch::parse(&[]).unwrap(), None);
        assert_eq!(Launch::parse(&strings(&["--other"])).unwrap(), None);
        assert!(Launch::parse(&strings(&["--run"])).is_err());
        assert!(Launch::parse(&strings(&["--run", "games"])).is_err());
        assert!(Launch::parse(&strings(&["--run", "g", "--backend", "x", "/a"])).is_err());
        let launch = Launch::parse(&strings(&[
            "--run",
            "games",
            "--backend",
            "emulated",
            "--arg",
            "-v",
            "/b/Foo.app",
            "foo://bar",
            "/home/u/a b.txt",
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(launch.bottle, "games");
        assert_eq!(launch.backend, Backend::Emulated);
        assert_eq!(launch.args, ["-v"]);
        assert_eq!(launch.target, PathBuf::from("/b/Foo.app"));
        assert_eq!(launch.open, ["foo://bar", "/home/u/a b.txt"]);
    }

    #[test]
    fn links_are_passed_as_protocol_arguments() {
        let launch = Launch {
            bottle: "g".into(),
            target: "/b/Foo.app".into(),
            args: vec![],
            backend: Backend::Auto,
            open: vec!["foo://bar".into(), "/home/u/a.txt".into()],
        };
        let args = launch.arguments_in(Path::new("/data/g/root"));
        assert_eq!(
            args[..6],
            [
                "-protocolString",
                "foo://bar",
                "-protocolUrl",
                "foo://bar",
                "-protocolUri",
                "foo://bar"
            ]
        );
        assert_eq!(args[6], "foo://bar");
        assert!(is_link("steam://run/1"));
        assert!(!is_link("file:///a"));
        assert!(!is_link("/a:b"));
    }

    #[test]
    fn files_become_guest_urls() {
        let drive = Path::new("/data/bottles/g/prefix/root");
        assert_eq!(guest_url(drive, "steam://run/1"), "steam://run/1");
        assert_eq!(
            guest_url(drive, "/home/u/a b.txt"),
            "file:///Volumes/Host/home/u/a%20b.txt"
        );
        assert_eq!(
            guest_url(drive, "file:///home/u/a%20b.txt"),
            "file:///Volumes/Host/home/u/a%20b.txt"
        );
        assert_eq!(
            guest_url(drive, "/data/bottles/g/prefix/root/Users/me/x.txt"),
            "file:///Users/me/x.txt"
        );
    }

    #[test]
    fn exec_arguments_are_quoted() {
        assert_eq!(quote_exec_arg("plain"), "plain");
        assert_eq!(quote_exec_arg("a b"), "\"a b\"");
        assert_eq!(quote_exec_arg("100%"), "100%%");
        assert_eq!(quote_exec_arg("$x\"y\\"), "\"\\$x\\\"y\\\\\"");
        assert_eq!(quote_exec_arg(""), "\"\"");
    }

    #[test]
    fn desktop_entry() {
        let app = App {
            category: Some("public.app-category.games".into()),
            schemes: vec!["foo".into()],
            mime_types: vec!["image/png".into()],
            ..App::default()
        };
        let shortcut = Shortcut::for_app(
            "games",
            "Games",
            "Foo",
            Path::new("/d/My Foo.app"),
            &["-x".into()],
            Backend::Native,
            &app,
            None,
        );
        let entry = shortcut.desktop_entry("/usr/bin/butter", None);
        assert!(entry.starts_with("[Desktop Entry]\n"));
        assert!(entry.contains(
            "\nExec=/usr/bin/butter --run games --backend native --arg -x \"/d/My Foo.app\" %U\n"
        ));
        assert!(entry.contains("\nMimeType=image/png;x-scheme-handler/foo;\n"));
        assert!(entry.contains("\nCategories=Game;\n"));
        assert!(!entry.contains("Icon="));
        let id = shortcut.id();
        assert!(id.starts_with("io.github.microlift.butter.games.app_"));
        assert!(id.ends_with(".desktop"));
        assert_eq!(id, desktop_id("games", Path::new("/d/My Foo.app")));
        assert!(entry.contains("\nX-Butter-Bottle=games\n"));
    }

    #[test]
    fn plugin_desktop_entry() {
        let shortcut = Shortcut::for_plugin(
            "roblox",
            "player",
            "Roblox",
            "Start Roblox",
            &[
                "roblox-player".into(),
                "Roblox".into(),
                "bad scheme".into(),
                "file".into(),
            ],
            &["image/*".into(), "text/x-lua".into()],
            "Game;;Bad Category;",
            Some("/x/icon.svg".into()),
        );
        let entry = shortcut.desktop_entry("/usr/bin/butter", None);
        assert!(
            entry.contains("\nExec=/usr/bin/butter --plugin roblox player %U\n"),
            "{entry}"
        );
        assert!(
            entry.contains(
                "\nMimeType=text/x-lua;x-scheme-handler/roblox-player;x-scheme-handler/roblox;\n"
            ),
            "{entry}"
        );
        assert!(entry.contains("\nCategories=Game;\n"), "{entry}");
        assert!(entry.contains("\nX-Butter-Plugin=roblox\n"));
        assert_eq!(shortcut.icon_extension(), "svg");
        assert_eq!(
            shortcut.id(),
            "io.github.microlift.butter.plugin_roblox.player.desktop"
        );
    }
}
