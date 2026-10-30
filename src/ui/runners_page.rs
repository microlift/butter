//! Runners: the Rust toolchain, installed Rosemary builds, and releases
//! available from GitHub.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::github::{self, Commit, Origin, Release};
use crate::process::{Reporter, background};
use crate::runner::{self, Kind};
use crate::ui::{self, task, window::Window};
use crate::{bottle, markdown, toolchain, util};

#[derive(Clone)]
pub struct RunnersPage(Rc<Inner>);

struct Inner {
    page: adw::PreferencesPage,
    window: Window,
    toolchain_row: adw::ActionRow,
    toolchain_button: gtk::Button,
    requirements: adw::ExpanderRow,
    requirement_rows: RefCell<Vec<adw::ActionRow>>,
    installed: gtk::ListBox,
    releases: gtk::ListBox,
    releases_group: adw::PreferencesGroup,
    refresh_button: gtk::Button,
    spinner: gtk::Spinner,
    /// The last release list, so installs can redraw without a fetch.
    known: RefCell<Vec<Release>>,
    commits: gtk::ListBox,
    commits_group: adw::PreferencesGroup,
    commits_refresh: gtk::Button,
    commits_spinner: gtk::Spinner,
    known_commits: RefCell<Vec<Commit>>,
}

impl RunnersPage {
    pub fn new(window: &Window) -> RunnersPage {
        let page = adw::PreferencesPage::new();

        let toolchain_group = adw::PreferencesGroup::builder()
            .title("Build Environment")
            .description(
                "Needed only to build runners from Rosemary's source. \
                 Official builds bring everything they need",
            )
            .build();
        let toolchain_row = adw::ActionRow::builder()
            .title("Rust Toolchain")
            .subtitle("Looking…")
            .build();
        let toolchain_button = gtk::Button::builder()
            .label("Install")
            .valign(gtk::Align::Center)
            .tooltip_text("Install a private Rust toolchain with rustup")
            .build();
        toolchain_row.add_suffix(&toolchain_button);
        toolchain_group.add(&toolchain_row);
        let requirements = adw::ExpanderRow::builder()
            .title("Libraries and Tools")
            .subtitle("Looking…")
            .build();
        toolchain_group.add(&requirements);
        page.add(&toolchain_group);

        let installed_group = adw::PreferencesGroup::builder().title("Installed").build();
        let installed_header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let detect_system = ui::icon_button("system-search-symbolic", "Detect System Installs");
        let add_custom = ui::icon_button("list-add-symbolic", "Add a Rosemary Build…");
        installed_header.append(&detect_system);
        installed_header.append(&add_custom);
        installed_group.set_header_suffix(Some(&installed_header));
        let installed = ui::boxed_list();
        installed_group.add(&installed);
        page.add(&installed_group);

        let releases_group = adw::PreferencesGroup::builder()
            .title("Available")
            .description("Tagged releases from github.com/microlift/rosemary")
            .build();
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let spinner = gtk::Spinner::new();
        let refresh_button = ui::icon_button("view-refresh-symbolic", "Check for Releases");
        header.append(&spinner);
        header.append(&refresh_button);
        releases_group.set_header_suffix(Some(&header));
        let releases = ui::boxed_list();
        releases_group.add(&releases);
        page.add(&releases_group);

        let commits_group = adw::PreferencesGroup::builder()
            .title("Development Builds")
            .description("Recent commits to Rosemary, to build and try the newest changes")
            .build();
        let commits_header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let commits_spinner = gtk::Spinner::new();
        let commits_refresh = ui::icon_button("view-refresh-symbolic", "Check for New Commits");
        commits_header.append(&commits_spinner);
        commits_header.append(&commits_refresh);
        commits_group.set_header_suffix(Some(&commits_header));
        let commits = ui::boxed_list();
        commits_group.add(&commits);
        page.add(&commits_group);

        let this = RunnersPage(Rc::new(Inner {
            page,
            window: window.clone(),
            toolchain_row,
            toolchain_button: toolchain_button.clone(),
            requirements,
            requirement_rows: RefCell::default(),
            installed,
            releases,
            releases_group,
            refresh_button: refresh_button.clone(),
            spinner,
            known: RefCell::default(),
            commits,
            commits_group,
            commits_refresh: commits_refresh.clone(),
            commits_spinner,
            known_commits: RefCell::default(),
        }));

        let weak = Rc::downgrade(&this.0);
        toolchain_button.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                RunnersPage(inner).install_toolchain();
            }
        });
        let weak = Rc::downgrade(&this.0);
        refresh_button.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                RunnersPage(inner).refresh_releases(true);
            }
        });
        let weak = Rc::downgrade(&this.0);
        commits_refresh.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                RunnersPage(inner).refresh_commits(true);
            }
        });
        let weak = Rc::downgrade(&this.0);
        detect_system.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                RunnersPage(inner).detect_system(false);
            }
        });
        let weak = Rc::downgrade(&this.0);
        add_custom.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                RunnersPage(inner).add_custom();
            }
        });

        if !window.settings().checked_system_runner {
            window.update_settings(|s| s.checked_system_runner = true);
            this.detect_system(true);
        }
        this.refresh_toolchain();
        this.refresh_releases(false);
        // Goes to GitHub only if the saved list is over 12 hours old.
        this.refresh_commits(false);
        this
    }

    pub fn widget(&self) -> &adw::PreferencesPage {
        &self.0.page
    }

    fn window(&self) -> &Window {
        &self.0.window
    }

    pub fn refresh_toolchain(&self) {
        let this = self.clone();
        let settings = self.window().settings();
        glib::spawn_future_local(async move {
            let (found, requirements, managed) = background(move || {
                (
                    toolchain::detect_all(&settings),
                    toolchain::build_requirements(),
                    toolchain::managed_installed(),
                )
            })
            .await;
            let inner = &this.0;
            match found.first() {
                Some(t) => inner.toolchain_row.set_subtitle(&format!(
                    "{} · {}",
                    t.describe(),
                    glib::markup_escape_text(&t.cargo.display().to_string())
                )),
                None => inner
                    .toolchain_row
                    .set_subtitle("Not found. Butter can install one for itself"),
            }
            let installing = this.window().is_busy("toolchain");
            inner.toolchain_button.set_label(if installing {
                "Installing…"
            } else if managed {
                "Update"
            } else {
                "Install"
            });
            if found.is_empty() {
                inner.toolchain_button.add_css_class("suggested-action");
            } else {
                inner.toolchain_button.remove_css_class("suggested-action");
            }

            for row in inner.requirement_rows.take() {
                inner.requirements.remove(&row);
            }
            let missing: Vec<_> = requirements
                .iter()
                .filter(|r| r.blocking())
                .map(|r| r.name)
                .collect();
            let absent: Vec<_> = requirements
                .iter()
                .filter(|r| !r.found && r.optional)
                .map(|r| r.name)
                .collect();
            inner.requirements.set_subtitle(&if !missing.is_empty() {
                format!("Missing: {}", missing.join(", "))
            } else if absent.is_empty() {
                "Everything Rosemary needs was found".to_string()
            } else {
                format!(
                    "Everything Rosemary needs was found. Optional, not found: {}",
                    absent.join(", ")
                )
            });
            let mut rows = Vec::new();
            for requirement in requirements {
                let row = adw::ActionRow::builder()
                    .title(requirement.name)
                    .subtitle(glib::markup_escape_text(&requirement.detail))
                    .build();
                let (icon_name, css) = match (requirement.found, requirement.optional) {
                    (true, _) => ("object-select-symbolic", "success"),
                    (false, true) => ("dialog-information-symbolic", "dim-label"),
                    (false, false) => ("dialog-warning-symbolic", "warning"),
                };
                let icon = gtk::Image::from_icon_name(icon_name);
                icon.add_css_class(css);
                row.add_prefix(&icon);
                inner.requirements.add_row(&row);
                rows.push(row);
            }
            *inner.requirement_rows.borrow_mut() = rows;
        });
    }

    fn install_toolchain(&self) {
        let this = self.clone();
        glib::spawn_future_local(async move {
            let window = this.window().clone();
            if !window.begin("toolchain") {
                window.show_log("toolchain");
                return;
            }
            this.0.toolchain_button.set_label("Installing…");
            let result = task::run_keyed(
                &window,
                Some("toolchain"),
                "Rust Toolchain",
                toolchain::install_managed,
            )
            .await;
            window.end("toolchain");
            this.refresh_toolchain();
            if let Ok(message) = result {
                window.notify(&message);
            }
        });
    }

    pub fn refresh_installed(&self) {
        let list = &self.0.installed;
        list.remove_all();
        let runners = runner::installed();
        if runners.is_empty() {
            list.append(&ui::placeholder_row(
                "No runners installed yet; install one below",
            ));
        }
        let default = self.window().settings().default_runner;
        for r in runners {
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&r.info.name))
                .subtitle(glib::markup_escape_text(&r.summary()))
                .subtitle_lines(2)
                .build();
            row.add_prefix(&gtk::Image::from_icon_name(match r.info.kind {
                Kind::Source => "system-run-symbolic",
                Kind::Binary => "package-x-generic-symbolic",
                Kind::Custom => "application-x-executable-symbolic",
                Kind::System => "application-x-executable-symbolic",
            }));
            if default.as_deref() == Some(r.id.as_str()) {
                let badge = gtk::Label::new(Some("Default"));
                badge.add_css_class("dim-label");
                badge.add_css_class("caption");
                row.add_suffix(&badge);
            } else {
                let make_default =
                    ui::icon_button("emblem-default-symbolic", "Use for New Bottles");
                let window = self.window().clone();
                let id = r.id.clone();
                make_default.connect_clicked(move |_| {
                    let id = id.clone();
                    window.update_settings(|s| s.default_runner = Some(id));
                    window.refresh();
                });
                row.add_suffix(&make_default);
            }
            let remove = ui::icon_button("user-trash-symbolic", "Remove");
            let this = self.clone();
            remove.connect_clicked(move |_| this.remove(r.clone()));
            row.add_suffix(&remove);
            list.append(&row);
        }
        // Install buttons depend on what is installed. With no list yet, a
        // fetch is pending or its error is showing; leave either alone.
        let known = self.0.known.borrow().clone();
        if !known.is_empty() {
            self.show_releases(&known);
        }
        let commits = self.0.known_commits.borrow().clone();
        if !commits.is_empty() {
            self.show_commits(&commits);
        }
    }

    fn remove(&self, r: runner::Runner) {
        let this = self.clone();
        glib::spawn_future_local(async move {
            let window = this.window().clone();
            let users: Vec<String> = bottle::using_runner(&r.id)
                .into_iter()
                .map(|b| b.config.name)
                .collect();
            let body = match (r.info.kind, users.is_empty()) {
                (Kind::System, true) => {
                    "Butter will forget this runner. The binary itself is not deleted.".to_string()
                }
                (Kind::Custom, true) => {
                    "Butter will forget this runner. The binary itself is not deleted.".to_string()
                }
                (Kind::Source, true) => {
                    "The runner will be deleted. It can be built again later.".to_string()
                }
                (Kind::Binary, true) => {
                    "The runner will be deleted. It can be downloaded again later.".to_string()
                }
                (_, false) => format!(
                    "These bottles use it and will not run until given another runner: {}",
                    users.join(", ")
                ),
            };
            if !ui::confirm(
                &window.widget(),
                &format!("Remove {}?", r.info.name),
                &body,
                "_Remove",
                true,
            )
            .await
            {
                return;
            }
            let id = r.id.clone();
            match background(move || runner::remove(&id)).await {
                Ok(()) => {
                    if window.settings().default_runner.as_deref() == Some(r.id.as_str()) {
                        window.update_settings(|s| s.default_runner = None);
                    }
                    window.refresh();
                }
                Err(e) => window.error(format!("Could not remove the runner: {e}")),
            }
        });
    }

    fn add_custom(&self) {
        let this = self.clone();
        glib::spawn_future_local(async move {
            let window = this.window().clone();
            let dialog = gtk::FileDialog::builder()
                .title("Choose a rosemary Executable")
                .modal(true)
                .build();
            let Some(path) = dialog
                .open_future(Some(&window.widget()))
                .await
                .ok()
                .and_then(|f| f.path())
            else {
                return;
            };

            let entry = gtk::Entry::builder()
                .text("My Rosemary Build")
                .activates_default(true)
                .build();
            let alert = adw::AlertDialog::new(
                Some("Add Runner"),
                Some(&format!("Name the build at {}", path.display())),
            );
            alert.set_extra_child(Some(&entry));
            alert.add_responses(&[("cancel", "_Cancel"), ("add", "_Add")]);
            alert.set_response_appearance("add", adw::ResponseAppearance::Suggested);
            alert.set_default_response(Some("add"));
            alert.set_close_response("cancel");
            if alert.choose_future(Some(&window.widget())).await != "add" {
                return;
            }
            match runner::add_custom(&entry.text(), &path) {
                Ok(r) => {
                    if window.settings().default_runner.is_none() {
                        window.update_settings(|s| s.default_runner = Some(r.id.clone()));
                    }
                    window.refresh();
                }
                Err(e) => window.error(format!("Could not add the runner: {e}")),
            }
        });
    }

    /// Look for `rosemary` on the PATH and add it as a runner. When `quiet`,
    /// as on first launch, finding nothing new says nothing.
    fn detect_system(&self, quiet: bool) {
        let this = self.clone();
        glib::spawn_future_local(async move {
            let window = this.window().clone();
            let found = background(|| {
                let path = which::which_global("rosemary").ok()?;
                // Compared through symbolic links, so a binary already added
                // by another path is not added twice.
                let real = path.canonicalize().unwrap_or_else(|_| path.clone());
                let known = runner::installed()
                    .iter()
                    .any(|r| r.executable().canonicalize().is_ok_and(|e| e == real));
                Some((path, known))
            })
            .await;
            let path = match found {
                Some((path, false)) => path,
                Some((path, true)) => {
                    if !quiet {
                        window.notify(&format!("{} is already a runner", path.display()));
                    }
                    return;
                }
                None => {
                    if !quiet {
                        window.notify("No system install of Rosemary was found");
                    }
                    return;
                }
            };
            // An empty name takes add_system's default. It runs the binary to
            // ask its version, so off the main thread.
            let added = background({
                let path = path.clone();
                move || runner::add_system("", &path)
            })
            .await;
            match added {
                Ok(r) => {
                    if window.settings().default_runner.is_none() {
                        window.update_settings(|s| s.default_runner = Some(r.id.clone()));
                    }
                    window.refresh();
                    window.notify(&format!("Found Rosemary at {}", path.display()));
                }
                Err(e) if quiet => eprintln!("butter: could not add {}: {e}", path.display()),
                Err(e) => window.error(format!("Could not add the runner: {e}")),
            }
        });
    }

    /// Fetch the release list; from the cache unless `force`.
    pub fn refresh_releases(&self, force: bool) {
        let this = self.clone();
        glib::spawn_future_local(async move {
            let inner = &this.0;
            inner.spinner.start();
            inner.refresh_button.set_sensitive(false);
            let result = background(move || github::releases(force)).await;
            inner.spinner.stop();
            inner.refresh_button.set_sensitive(true);
            match result {
                Ok((list, origin)) => {
                    inner.releases_group.set_description(Some(match origin {
                        Origin::StaleCache => {
                            "GitHub could not be reached; showing the releases seen last time"
                        }
                        _ => "Tagged releases from github.com/microlift/rosemary",
                    }));
                    *inner.known.borrow_mut() = list.clone();
                    this.show_releases(&list);
                }
                Err(e) => {
                    inner.releases.remove_all();
                    let row = ui::placeholder_row("Could not get the release list");
                    row.set_subtitle(&glib::markup_escape_text(&format!("{e:#}")));
                    row.set_subtitle_selectable(true);
                    inner.releases.append(&row);
                    if force {
                        this.window().error("Could not get the release list");
                    }
                }
            }
        });
    }

    /// Fetch the commit list. Unless `force`, GitHub is asked only when the
    /// saved list is over 12 hours old.
    pub fn refresh_commits(&self, force: bool) {
        let this = self.clone();
        glib::spawn_future_local(async move {
            let inner = &this.0;
            inner.commits_spinner.start();
            inner.commits_refresh.set_sensitive(false);
            let result = background(move || github::commits(force)).await;
            inner.commits_spinner.stop();
            inner.commits_refresh.set_sensitive(true);
            match result {
                Ok((list, origin)) => {
                    let mut description =
                        "Recent commits to Rosemary, to build and try the newest changes"
                            .to_string();
                    if origin == Origin::StaleCache {
                        description
                            .push_str(". GitHub could not be reached; showing the last list");
                    }
                    if let Some(at) = github::commits_fetched_at() {
                        description.push_str(&format!(". Checked {}", util::format_date(at)));
                    }
                    inner.commits_group.set_description(Some(&description));
                    *inner.known_commits.borrow_mut() = list.clone();
                    this.show_commits(&list);
                }
                Err(e) => {
                    inner.commits.remove_all();
                    let row = ui::placeholder_row("Could not get the commit list");
                    row.set_subtitle(&glib::markup_escape_text(&format!("{e:#}")));
                    row.set_subtitle_selectable(true);
                    inner.commits.append(&row);
                    if force {
                        this.window().error("Could not get the commit list");
                    }
                }
            }
        });
    }

    fn show_commits(&self, commits: &[Commit]) {
        let list = &self.0.commits;
        list.remove_all();
        if commits.is_empty() {
            list.append(&ui::placeholder_row("No commits found"));
            return;
        }
        for commit in commits {
            let mut subtitle = vec![commit.short_sha().to_string()];
            if !commit.author.is_empty() {
                subtitle.push(commit.author.clone());
            }
            if let Some(date) = &commit.date {
                subtitle.push(util::format_iso_date(date));
            }
            let title = if commit.summary.is_empty() {
                "(no message)"
            } else {
                &commit.summary
            };
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(title))
                .title_lines(2)
                .subtitle(glib::markup_escape_text(&subtitle.join(" · ")))
                .build();
            row.add_prefix(&gtk::Image::from_icon_name("emblem-system-symbolic"));
            if !commit.html_url.is_empty() {
                let view = ui::icon_button("adw-external-link-symbolic", "View on GitHub");
                let this = self.clone();
                let url = commit.html_url.clone();
                view.connect_clicked(move |_| this.open_url(&url));
                row.add_suffix(&view);
            }
            row.add_suffix(&self.build_commit_button(&commit.sha));
            list.append(&row);
        }
    }

    fn show_releases(&self, releases: &[Release]) {
        let list = &self.0.releases;
        list.remove_all();
        let show_pre = self.window().settings().show_prereleases;
        let shown: Vec<&Release> = releases
            .iter()
            .filter(|r| show_pre || !r.prerelease)
            .collect();
        if shown.is_empty() {
            list.append(&ui::placeholder_row(if releases.is_empty() {
                "No releases found"
            } else {
                "Only pre-releases are available; they can be shown in Preferences"
            }));
            return;
        }
        let latest = shown
            .iter()
            .find(|r| !r.prerelease)
            .map(|r| r.tag_name.clone());
        for release in shown {
            let mut subtitle = Vec::new();
            if let Some(date) = &release.published_at {
                subtitle.push(util::format_iso_date(date));
            }
            if release.prerelease {
                subtitle.push("Pre-release".into());
            } else if latest.as_ref() == Some(&release.tag_name) {
                subtitle.push("Latest".into());
            }
            let row = adw::ExpanderRow::builder()
                .title(glib::markup_escape_text(release.title()))
                .subtitle(subtitle.join(" · "))
                .build();

            // The official build is the quick way in; building from source
            // stays available below, and is all there is for a release with
            // no build for this architecture.
            let build = self.build_button(&release.tag_name);
            let has_binary = release.steam_tool().is_some();
            if has_binary {
                row.add_suffix(&self.download_button(release));
            } else {
                row.add_suffix(&build);
            }

            row.add_row(&self.notes_row(release));
            if has_binary {
                let source = adw::ActionRow::builder()
                    .title("Build from Source")
                    .subtitle("Compile this release with the Rust toolchain on this computer")
                    .build();
                build.remove_css_class("suggested-action");
                source.add_suffix(&build);
                row.add_row(&source);
            }
            if !release.html_url.is_empty() {
                let link = adw::ActionRow::builder()
                    .title("View on GitHub")
                    .activatable(true)
                    .build();
                link.add_suffix(&gtk::Image::from_icon_name("adw-external-link-symbolic"));
                let url = release.html_url.clone();
                let window = self.window().clone();
                link.connect_activated(move |_| {
                    gtk::UriLauncher::new(&url).launch(
                        Some(&window.widget()),
                        gtk::gio::Cancellable::NONE,
                        |_| {},
                    );
                });
                row.add_row(&link);
            }
            list.append(&row);
        }
    }

    /// A row whose button shows the release notes right below it. When GitHub
    /// cannot supply them, the button opens the release page instead.
    fn notes_row(&self, release: &Release) -> adw::ActionRow {
        let row = adw::ActionRow::builder().title("Release Notes").build();
        let button = gtk::Button::builder()
            .label("Show")
            .valign(gtk::Align::Center)
            .build();
        row.add_suffix(&button);

        let shown = Rc::new(RefCell::new(None::<gtk::ListBoxRow>));
        let parent = Rc::new(RefCell::new(None::<adw::ExpanderRow>));

        let this = self.clone();
        let tag = release.tag_name.clone();
        let url = release.html_url.clone();
        let anchor = row.clone();
        button.connect_clicked(move |button| {
            // Toggle off when the notes are already showing.
            if let Some(notes) = shown.borrow_mut().take() {
                if let Some(expander) = parent.borrow().as_ref() {
                    expander.remove(&notes);
                }
                button.set_label("Show");
                return;
            }
            let Some(expander) = anchor
                .ancestor(adw::ExpanderRow::static_type())
                .and_downcast::<adw::ExpanderRow>()
            else {
                return;
            };
            let this = this.clone();
            let (tag, url) = (tag.clone(), url.clone());
            let button = button.clone();
            let (shown, parent) = (shown.clone(), parent.clone());
            glib::spawn_future_local(async move {
                button.set_sensitive(false);
                let result = background(move || github::release_notes(&tag)).await;
                button.set_sensitive(true);
                match result {
                    Ok(notes) => {
                        let notes = notes.trim();
                        let label = gtk::Label::builder()
                            .wrap(true)
                            .wrap_mode(gtk::pango::WrapMode::WordChar)
                            .xalign(0.0)
                            .selectable(true)
                            .margin_top(12)
                            .margin_bottom(12)
                            .margin_start(12)
                            .margin_end(12)
                            .build();
                        if notes.is_empty() {
                            label.set_label("No release notes.");
                        } else {
                            label.set_markup(&markdown::to_pango(notes));
                        }
                        let notes_row = gtk::ListBoxRow::new();
                        notes_row.set_activatable(false);
                        notes_row.set_child(Some(&label));
                        expander.add_row(&notes_row);
                        *shown.borrow_mut() = Some(notes_row);
                        *parent.borrow_mut() = Some(expander);
                        button.set_label("Hide");
                    }
                    Err(_) => this.open_url(&url),
                }
            });
        });
        row
    }

    fn open_url(&self, url: &str) {
        gtk::UriLauncher::new(url).launch(
            Some(&self.window().widget()),
            gtk::gio::Cancellable::NONE,
            |_| {},
        );
    }

    /// A button that installs runner `id`, labelled for whether it is
    /// installed already or being installed now.
    fn install_button(&self, id: &str, labels: [&str; 3]) -> gtk::Button {
        let [install, again, busy] = labels;
        let installed = runner::find(id).is_some_and(|r| r.is_usable());
        let busy_now = self.window().is_busy(&format!("runner:{id}"));
        let button = gtk::Button::builder()
            .valign(gtk::Align::Center)
            .label(if busy_now {
                busy
            } else if installed {
                again
            } else {
                install
            })
            .build();
        if busy_now {
            // Still clickable: it brings the log back if it was closed.
            button.set_tooltip_text(Some("Show the log"));
        }
        if !installed {
            button.add_css_class("suggested-action");
        }
        button
    }

    fn download_button(&self, release: &Release) -> gtk::Button {
        let id = runner::id_for_binary(&release.tag_name);
        let button = self.install_button(&id, ["Install", "Reinstall", "Downloading…"]);
        button.set_tooltip_text(Some("Download the official build"));
        let this = self.clone();
        let release = release.clone();
        button.connect_clicked(move |_| {
            let release = release.clone();
            this.install(
                runner::id_for_binary(&release.tag_name),
                format!("Downloading Rosemary {}", release.tag_name),
                move |r| runner::install_binary(&release, r),
            );
        });
        button
    }

    fn build_button(&self, tag: &str) -> gtk::Button {
        let target = runner::Target::release(tag);
        let button = self.install_button(&target.id, ["Build", "Rebuild", "Building…"]);
        if !self.window().is_busy(&format!("runner:{}", target.id)) {
            button.set_tooltip_text(Some("Compile this release on this computer"));
        }
        self.connect_build(&button, target);
        button
    }

    fn build_commit_button(&self, sha: &str) -> gtk::Button {
        let target = runner::Target::commit(sha);
        let button = self.install_button(&target.id, ["Build", "Rebuild", "Building…"]);
        if !self.window().is_busy(&format!("runner:{}", target.id)) {
            button.set_tooltip_text(Some("Compile this commit on this computer"));
        }
        self.connect_build(&button, target);
        button
    }

    fn connect_build(&self, button: &gtk::Button, target: runner::Target) {
        let this = self.clone();
        button.connect_clicked(move |_| {
            let settings = this.window().settings();
            let target = target.clone();
            this.install(
                target.id.clone(),
                format!("Building Rosemary {}", target.label),
                move |r| runner::build_from_source(&target, &settings, r),
            );
        });
    }

    /// Run `job`, which installs runner `id`, with its progress showing.
    fn install<F>(&self, id: String, title: String, job: F)
    where
        F: FnOnce(&Reporter) -> anyhow::Result<String> + Send + 'static,
    {
        let this = self.clone();
        glib::spawn_future_local(async move {
            let window = this.window().clone();
            let key = format!("runner:{id}");
            if !window.begin(&key) {
                // Already running: the button asks for its log.
                window.show_log(&key);
                return;
            }
            this.refresh_installed();
            let result = task::run_keyed(&window, Some(&key), &title, job).await;
            window.end(&key);
            if let Ok(message) = result {
                if window.settings().default_runner.is_none() {
                    window.update_settings(|s| s.default_runner = Some(id));
                }
                window.notify(&message);
            }
            window.refresh();
        });
    }
}
