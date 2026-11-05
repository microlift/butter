//! One bottle: its programs, how Rosemary runs them, and its prefix.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::apps::{self, App};
use crate::bottle::{
    self, Backend, Bottle, CommandKey, Decorations, EnvTarget, EnvVar, LinkWays, Menu, PrefixHome,
    PresentMode, Program, SwapInterval, SyscallTrace, SystemFiles, Tso,
};
use crate::gpu::{self, Gpu};
use crate::permissions::{self, Feature};
use crate::runner;
use crate::shortcuts;
use crate::ui::{
    self, env_dialog, new_bottle, process_manager, program_dialog, task, window::Window,
};

#[derive(Clone)]
pub struct BottlePage(Rc<Inner>);

/// A row of the Applications list.
#[derive(Clone)]
struct Entry {
    program: Program,
    /// Its index among the programs added by hand, if it is one.
    added: Option<usize>,
    /// What the bundle says about itself, for a bundle.
    app: Option<App>,
}

/// An application's icon for a list row.
fn app_icon(app: Option<&App>) -> gtk::Image {
    let texture = app
        .and_then(|a| a.icon_png.as_deref())
        .and_then(|path| gtk::gdk::Texture::from_filename(path).ok());
    let image = match texture {
        Some(texture) => gtk::Image::from_paintable(Some(&texture)),
        None => gtk::Image::from_icon_name("application-x-executable"),
    };
    image.set_pixel_size(32);
    image
}

struct Inner {
    page: adw::NavigationPage,
    window: Window,
    bottle: RefCell<Bottle>,
    programs: gtk::ListBox,
    /// Bundles found in the bottle, and those added by hand.
    apps: RefCell<Vec<App>>,
    /// Watch the folders applications are installed in.
    monitors: RefCell<Vec<gio::FileMonitor>>,
    rescan_due: Cell<bool>,
    env: gtk::ListBox,
    banner: adw::Banner,
    /// A runner change asked for while programs were running, applied once
    /// they have all ended.
    pending_runner: RefCell<Option<Option<String>>>,
    /// Set while the Runner row is changed by Butter, not the user.
    syncing_runner: Cell<bool>,
}

impl BottlePage {
    pub fn new(window: &Window, bottle: Bottle) -> BottlePage {
        let name = bottle.config.name.clone();
        let programs = ui::boxed_list();
        let env = ui::boxed_list();
        let banner = adw::Banner::new("");

        let content = adw::PreferencesPage::new();
        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        toolbar.add_top_bar(&header);
        toolbar.add_top_bar(&banner);
        toolbar.set_content(Some(&content));
        let page = adw::NavigationPage::builder()
            .title(&name)
            .tag(format!("bottle:{}", bottle.id))
            .child(&toolbar)
            .build();

        let this = BottlePage(Rc::new(Inner {
            page,
            window: window.clone(),
            bottle: RefCell::new(bottle),
            programs,
            apps: RefCell::default(),
            monitors: RefCell::default(),
            rescan_due: Cell::new(false),
            env,
            banner,
            pending_runner: RefCell::new(None),
            syncing_runner: Cell::new(false),
        }));

        let run = gtk::MenuButton::builder()
            .icon_name("media-playback-start-symbolic")
            .tooltip_text("Run…")
            .menu_model(&{
                let menu = gtk::gio::Menu::new();
                menu.append(Some("Run App _Bundle…"), Some("bottle.run-bundle"));
                menu.append(Some("Run _Executable…"), Some("bottle.run-executable"));
                menu.append(Some("Open Rosemary _Shell"), Some("bottle.open-shell"));
                menu
            })
            .build();
        header.pack_end(&run);
        let processes = gtk::Button::builder()
            .icon_name("utilities-terminal-symbolic")
            .tooltip_text("Process Manager")
            .build();
        let weak = this.weak();
        processes.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                let this = BottlePage(inner);
                this.window().show_processes(&this.bottle(), None);
            }
        });
        header.pack_end(&processes);
        header.pack_end(&process_manager::power_button(window, &this.bottle().id));
        this.setup_actions();

        // The banner offers a button only when the prefix is missing.
        let weak = this.weak();
        this.0.banner.connect_button_clicked(move |_| {
            let Some(inner) = weak.upgrade() else { return };
            let this = BottlePage(inner);
            glib::spawn_future_local(async move {
                new_bottle::create_prefix(this.window(), &this.bottle(), true).await;
                this.refresh_banner();
                this.watch_applications();
                this.rescan();
            });
        });

        content.add(&this.programs_group());
        if let Some(group) = this.plugins_group() {
            content.add(&group);
        }
        content.add(&this.runtime_group());
        content.add(&this.graphics_group());
        content.add(&this.display_group());
        content.add(&this.advanced_group());
        content.add(&this.environment_group());
        content.add(&this.prefix_group());
        content.add(&this.danger_group());
        this.refresh_programs();
        this.watch_applications();
        this.rescan();
        this.refresh_environment();
        this.refresh_banner();
        this
    }

    pub fn widget(&self) -> &adw::NavigationPage {
        &self.0.page
    }

    fn bottle(&self) -> Bottle {
        self.0.bottle.borrow().clone()
    }

    fn window(&self) -> &Window {
        &self.0.window
    }

    /// Change the bottle's settings and save them.
    fn edit(&self, change: impl FnOnce(&mut bottle::Config)) {
        let mut bottle = self.0.bottle.borrow_mut();
        change(&mut bottle.config);
        if let Err(e) = bottle.save() {
            drop(bottle);
            self.window()
                .error(format!("Could not save the bottle: {e}"));
        }
    }

    fn weak(&self) -> std::rc::Weak<Inner> {
        Rc::downgrade(&self.0)
    }

    fn setup_actions(&self) {
        let group = gtk::gio::SimpleActionGroup::new();
        for (name, bundle) in [("run-bundle", true), ("run-executable", false)] {
            let action = gtk::gio::SimpleAction::new(name, None);
            let weak = self.weak();
            action.connect_activate(move |_, _| {
                let Some(inner) = weak.upgrade() else { return };
                let this = BottlePage(inner);
                glib::spawn_future_local(async move {
                    if let Some(path) = program_dialog::choose_target(this.window(), bundle).await {
                        this.launch(&path, &[], Backend::Auto, &bottle::program_name(&path));
                    }
                });
            });
            group.add_action(&action);
        }
        let shell = gtk::gio::SimpleAction::new("open-shell", None);
        let weak = self.weak();
        shell.connect_activate(move |_, _| {
            if let Some(inner) = weak.upgrade() {
                let this = BottlePage(inner);
                this.window().open_shell(&this.bottle());
            }
        });
        group.add_action(&shell);
        self.0.page.insert_action_group("bottle", Some(&group));
    }

    fn refresh_banner(&self) {
        let bottle = self.bottle();
        let (text, button) = if bottle.runner().is_none() {
            ("This bottle's runner is not installed", None)
        } else if !bottle.is_ready() {
            ("This bottle's prefix has not been created", Some("Create"))
        } else {
            ("", None)
        };
        self.0.banner.set_title(text);
        self.0.banner.set_button_label(button);
        self.0.banner.set_revealed(!text.is_empty());
    }

    /// Start `target` in this bottle and report how it ends.
    fn launch(&self, target: &Path, args: &[String], backend: Backend, name: &str) {
        self.window()
            .launch(&self.bottle(), target, args, backend, name);
    }

    fn programs_group(&self) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::builder()
            .title("Applications")
            .description(
                "Apps installed in the bottle's Applications folder appear here by themselves",
            )
            .build();
        let rescan = ui::icon_button("view-refresh-symbolic", "Look for Applications Again");
        let weak = self.weak();
        rescan.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                BottlePage(inner).rescan();
            }
        });
        let add = ui::icon_button("list-add-symbolic", "Add Application…");
        let weak = self.weak();
        add.connect_clicked(move |_| {
            let Some(inner) = weak.upgrade() else { return };
            let this = BottlePage(inner);
            let page = this.clone();
            program_dialog::present(this.window(), None, move |program| {
                page.edit(|c| c.programs.push(program));
                page.rescan();
                page.window().refresh();
            });
        });
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        buttons.append(&rescan);
        buttons.append(&add);
        group.set_header_suffix(Some(&buttons));
        group.add(&self.0.programs);
        group
    }

    /// The actions plugins offer in bottles, if any plugin does.
    fn plugins_group(&self) -> Option<adw::PreferencesGroup> {
        let disabled = self.window().settings().disabled_plugins;
        let mut rows = Vec::new();
        for plugin in crate::plugins::discover().plugins {
            if disabled.iter().any(|id| id == plugin.id()) {
                continue;
            }
            for action in &plugin.manifest.actions {
                if action.scope != crate::plugins::Scope::Bottle || action.hidden {
                    continue;
                }
                let row = adw::ActionRow::builder()
                    .title(glib::markup_escape_text(&action.title))
                    .subtitle(glib::markup_escape_text(&format!(
                        "{} · {}",
                        plugin.name(),
                        action.subtitle
                    )))
                    .build();
                let run = ui::icon_button("media-playback-start-symbolic", "Run");
                row.add_suffix(&run);
                row.set_activatable_widget(Some(&run));
                let (weak, p, id) = (self.weak(), plugin.clone(), action.id.clone());
                run.connect_clicked(move |_| {
                    let Some(inner) = weak.upgrade() else { return };
                    let this = BottlePage(inner);
                    let (p, id) = (p.clone(), id.clone());
                    glib::spawn_future_local(async move {
                        let window = this.window().clone();
                        ui::plugins::run(&window, &p, &id, Some(this.bottle()), Vec::new()).await;
                        // The plugin may have installed applications.
                        this.rescan();
                    });
                });
                rows.push(row);
            }
        }
        if rows.is_empty() {
            return None;
        }
        let group = adw::PreferencesGroup::builder()
            .title("Plugins")
            .description("Actions plugins offer for this bottle")
            .build();
        for row in rows {
            group.add(&row);
        }
        Some(group)
    }

    /// Follow the folders applications are installed in, so new ones show
    /// up without a refresh.
    fn watch_applications(&self) {
        let mut monitors = Vec::new();
        for dir in self.bottle().application_dirs() {
            let Ok(monitor) = gio::File::for_path(&dir)
                .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
            else {
                continue;
            };
            let weak = self.weak();
            monitor.connect_changed(move |_, _, _, _| {
                let Some(inner) = weak.upgrade() else { return };
                // An install writes many files; look once it has settled.
                if inner.rescan_due.replace(true) {
                    return;
                }
                let weak = Rc::downgrade(&inner);
                glib::timeout_add_seconds_local_once(1, move || {
                    if let Some(inner) = weak.upgrade() {
                        inner.rescan_due.set(false);
                        BottlePage(inner).rescan();
                    }
                });
            });
            monitors.push(monitor);
        }
        *self.0.monitors.borrow_mut() = monitors;
    }

    /// Read the bottle's applications, and the bundles added by hand, in the
    /// background, then show them.
    fn rescan(&self) {
        let bottle = self.bottle();
        let weak = self.weak();
        glib::spawn_future_local(async move {
            let found = crate::process::background(move || {
                let mut found: Vec<App> = bottle
                    .application_dirs()
                    .iter()
                    .flat_map(|dir| apps::scan(dir))
                    .collect();
                for program in &bottle.config.programs {
                    if apps::is_bundle(&program.path)
                        && !found.iter().any(|a| a.path == program.path)
                    {
                        found.push(apps::read(&program.path));
                    }
                }
                for app in &mut found {
                    app.icon_png = app.render_icon();
                }
                found
            })
            .await;
            if let Some(inner) = weak.upgrade() {
                *inner.apps.borrow_mut() = found;
                BottlePage(inner).refresh_programs();
            }
        });
    }

    /// The rows: applications found in the bottle and programs added by
    /// hand, by name. An added program stands in for the app at its path.
    fn entries(&self) -> Vec<Entry> {
        let bottle = self.bottle();
        let apps = self.0.apps.borrow();
        let mut entries: Vec<Entry> = bottle
            .config
            .programs
            .iter()
            .enumerate()
            .map(|(index, program)| Entry {
                program: program.clone(),
                added: Some(index),
                app: apps.iter().find(|a| a.path == program.path).cloned(),
            })
            .collect();
        for app in apps.iter().filter(|a| a.installed) {
            if !entries.iter().any(|e| e.program.path == app.path) {
                entries.push(Entry {
                    program: Program {
                        name: app.name.clone(),
                        path: app.path.clone(),
                        args: Vec::new(),
                        backend: Backend::Auto,
                    },
                    added: None,
                    app: Some(app.clone()),
                });
            }
        }
        entries.sort_by_key(|e| e.program.name.to_lowercase());
        entries
    }

    fn refresh_programs(&self) {
        let list = &self.0.programs;
        list.remove_all();
        let entries = self.entries();
        if entries.is_empty() {
            list.append(&ui::placeholder_row(
                "Install an app into the bottle, or add one to run it with one click",
            ));
        }
        let bottle = self.bottle();
        let drive = bottle.drive();
        for entry in entries {
            let program = &entry.program;
            // Inside the bottle, where the app sees itself.
            let location = match program.path.strip_prefix(&drive) {
                Ok(rest) => Path::new("/").join(rest).display().to_string(),
                Err(_) => program.path.display().to_string(),
            };
            let subtitle = match entry.app.as_ref().and_then(|a| a.version.as_deref()) {
                Some(version) => format!("{location} · {version}"),
                None => location,
            };
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&program.name))
                .subtitle(glib::markup_escape_text(&subtitle))
                .subtitle_lines(1)
                .build();
            row.add_prefix(&app_icon(entry.app.as_ref()));
            if !program.path.exists() {
                let warning = gtk::Image::from_icon_name("dialog-warning-symbolic");
                warning.set_tooltip_text(Some("Not found"));
                warning.add_css_class("warning");
                row.add_suffix(&warning);
            }
            let shortcut = shortcuts::desktop_id(&bottle.id, &program.path);
            let has_shortcut = bottle.config.shortcuts.contains(&shortcut);
            if has_shortcut {
                let pinned = gtk::Image::from_icon_name("view-app-grid-symbolic");
                pinned.set_tooltip_text(Some("In the applications menu"));
                pinned.add_css_class("dim-label");
                row.add_suffix(&pinned);
            }
            let run = ui::icon_button("media-playback-start-symbolic", "Run");
            row.add_suffix(&run);
            row.set_activatable_widget(Some(&run));

            let menu = gio::Menu::new();
            if entry.app.is_some() {
                if has_shortcut {
                    menu.append(Some("Remove _Shortcut"), Some("app-row.remove-shortcut"));
                } else {
                    menu.append(Some("Create _Shortcut…"), Some("app-row.create-shortcut"));
                }
            }
            menu.append(Some("Compile _Ahead of Time"), Some("app-row.compile"));
            if entry.added.is_some() {
                menu.append(Some("_Edit…"), Some("app-row.edit"));
                menu.append(Some("_Remove"), Some("app-row.remove"));
            } else {
                menu.append(Some("_Customize…"), Some("app-row.edit"));
            }
            row.add_suffix(
                &gtk::MenuButton::builder()
                    .icon_name("view-more-symbolic")
                    .tooltip_text("More")
                    .menu_model(&menu)
                    .valign(gtk::Align::Center)
                    .css_classes(["flat"])
                    .build(),
            );
            row.insert_action_group("app-row", Some(&self.row_actions(&entry, shortcut)));

            let weak = self.weak();
            let p = program.clone();
            run.connect_clicked(move |_| {
                if let Some(inner) = weak.upgrade() {
                    BottlePage(inner).launch(&p.path, &p.args, p.backend, &p.name);
                }
            });
            list.append(&row);
        }
    }

    fn row_actions(&self, entry: &Entry, shortcut: String) -> gio::SimpleActionGroup {
        let group = gio::SimpleActionGroup::new();
        let add = |name: &str, activate: Box<dyn Fn(BottlePage)>| {
            let action = gio::SimpleAction::new(name, None);
            let weak = self.weak();
            action.connect_activate(move |_, _| {
                if let Some(inner) = weak.upgrade() {
                    activate(BottlePage(inner));
                }
            });
            group.add_action(&action);
        };

        let e = entry.clone();
        add(
            "create-shortcut",
            Box::new(move |this| {
                let e = e.clone();
                glib::spawn_future_local(async move { this.create_shortcut(&e).await });
            }),
        );
        let id = shortcut.clone();
        add(
            "remove-shortcut",
            Box::new(move |this| {
                let id = id.clone();
                glib::spawn_future_local(async move { this.remove_shortcut(&id).await });
            }),
        );
        let path = entry.program.path.clone();
        add("compile", Box::new(move |this| this.compile(path.clone())));
        let e = entry.clone();
        add(
            "edit",
            Box::new(move |this| {
                let page = this.clone();
                let index = e.added;
                program_dialog::present(this.window(), Some(e.program.clone()), move |changed| {
                    page.edit(|c| match index.and_then(|i| c.programs.get_mut(i)) {
                        Some(slot) => *slot = changed,
                        None => c.programs.push(changed),
                    });
                    page.rescan();
                    page.window().refresh();
                });
            }),
        );
        let e = entry.clone();
        add(
            "remove",
            Box::new(move |this| {
                let Some(index) = e.added else { return };
                this.edit(|c| {
                    if index < c.programs.len() {
                        c.programs.remove(index);
                    }
                });
                // Its row goes, and with it the only way to remove the
                // shortcut, unless the app is installed in the bottle.
                let installed = e.app.as_ref().is_some_and(|a| a.installed);
                if !installed && this.bottle().config.shortcuts.contains(&shortcut) {
                    let (this, id) = (this.clone(), shortcut.clone());
                    glib::spawn_future_local(async move { this.remove_shortcut(&id).await });
                }
                this.rescan();
                this.window().refresh();
            }),
        );
        group
    }

    /// Ask the desktop to add `entry` to its applications menu.
    async fn create_shortcut(&self, entry: &Entry) {
        let Some(app) = entry.app.clone() else { return };
        let bottle = self.bottle();
        let shortcut = shortcuts::Shortcut::for_app(
            &bottle.id,
            &bottle.config.name,
            &entry.program.name,
            &entry.program.path,
            &entry.program.args,
            entry.program.backend,
            &app,
            app.icon_png.clone(),
        );
        match shortcuts::install(&shortcut).await {
            Ok(Some(name)) => {
                let id = shortcut.id();
                self.edit(|c| {
                    if !c.shortcuts.contains(&id) {
                        c.shortcuts.push(id);
                    }
                });
                self.refresh_programs();
                self.window()
                    .notify(&format!("Added {name} to the applications menu"));
            }
            Ok(None) => {}
            Err(e) => self
                .window()
                .error(format!("Could not create the shortcut: {e:#}")),
        }
    }

    async fn remove_shortcut(&self, id: &str) {
        let result = shortcuts::uninstall(id).await;
        // Forgotten either way: a shortcut the desktop no longer has is gone.
        self.edit(|c| c.shortcuts.retain(|s| s != id));
        self.refresh_programs();
        match result {
            Ok(()) => self.window().notify("Shortcut removed"),
            Err(e) => self
                .window()
                .error(format!("Could not remove the shortcut: {e:#}")),
        }
    }

    /// A combo row bound to one of the bottle's choices.
    fn choice<T: Copy + 'static>(
        &self,
        title: &str,
        subtitle: Option<&str>,
        all: &'static [T],
        label: fn(T) -> &'static str,
        get: fn(&bottle::Config) -> T,
        set: fn(&mut bottle::Config, T),
    ) -> adw::ComboRow {
        let labels: Vec<&str> = all.iter().map(|v| label(*v)).collect();
        let current = get(&self.bottle().config);
        let selected = all
            .iter()
            .position(|v| label(*v) == label(current))
            .unwrap_or(0) as u32;
        let row = ui::combo_row(title, subtitle, &labels, selected);
        let weak = self.weak();
        row.connect_selected_notify(move |row| {
            if let (Some(inner), Some(value)) = (weak.upgrade(), all.get(row.selected() as usize)) {
                BottlePage(inner).edit(|c| set(c, *value));
            }
        });
        row
    }

    /// A switch row bound to one of the bottle's flags.
    fn switch(
        &self,
        title: &str,
        subtitle: &str,
        get: fn(&bottle::Config) -> bool,
        set: fn(&mut bottle::Config, bool),
    ) -> adw::SwitchRow {
        let row = adw::SwitchRow::builder()
            .title(title)
            .subtitle(subtitle)
            .active(get(&self.bottle().config))
            .build();
        let weak = self.weak();
        row.connect_active_notify(move |row| {
            if let Some(inner) = weak.upgrade() {
                let active = row.is_active();
                BottlePage(inner).edit(|c| set(c, active));
            }
        });
        row
    }

    /// Show the bottle's current runner in `row` without treating it as a
    /// choice by the user.
    fn select_runner(&self, row: &adw::ComboRow, ids: &[Option<String>]) {
        let current = self.bottle().config.runner;
        if let Some(index) = ids.iter().position(|id| *id == current) {
            self.0.syncing_runner.set(true);
            row.set_selected(index as u32);
            self.0.syncing_runner.set(false);
        }
    }

    fn runtime_group(&self) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::builder().title("Runtime").build();

        let runners = runner::installed();
        let current = self.bottle().config.runner;
        let mut labels: Vec<String> = runners.iter().map(|r| r.info.name.clone()).collect();
        let mut ids: Vec<Option<String>> = runners.iter().map(|r| Some(r.id.clone())).collect();
        if let Some(id) = &current
            && !ids.contains(&Some(id.clone()))
        {
            labels.push(format!("{id} (not installed)"));
            ids.push(Some(id.clone()));
        }
        if current.is_none() {
            labels.push("None".into());
            ids.push(None);
        }
        let selected = ids.iter().position(|id| *id == current).unwrap_or(0) as u32;
        let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let runner_row = ui::combo_row(
            "Runner",
            Some("The version of Rosemary to run this bottle with"),
            &label_refs,
            selected,
        );
        let ids = Rc::new(ids);

        // Applies the pending change once nothing runs in the bottle.
        let weak = self.weak();
        let (row, all) = (runner_row.clone(), ids.clone());
        self.window().sessions().connect_changed(move || {
            let Some(inner) = weak.upgrade() else {
                return false;
            };
            let this = BottlePage(inner);
            let pending = this.0.pending_runner.borrow().clone();
            let Some(id) = pending else { return true };
            if this.window().sessions().is_running(&this.bottle().id) {
                return true;
            }
            this.0.pending_runner.borrow_mut().take();
            this.edit(|c| c.runner = id.clone());
            this.select_runner(&row, &all);
            this.refresh_banner();
            this.window().refresh();
            this.window().notify("Applied the new runner");
            true
        });

        let weak = self.weak();
        let all = ids.clone();
        runner_row.connect_selected_notify(move |row| {
            let Some(inner) = weak.upgrade() else { return };
            let this = BottlePage(inner);
            if this.0.syncing_runner.get() {
                return;
            }
            let Some(id) = all.get(row.selected() as usize).cloned() else {
                return;
            };
            if id == this.bottle().config.runner {
                this.0.pending_runner.borrow_mut().take();
                return;
            }
            let sessions = this.window().sessions();
            if !sessions.is_running(&this.bottle().id) {
                this.0.pending_runner.borrow_mut().take();
                this.edit(|c| c.runner = id);
                this.refresh_banner();
                this.window().refresh();
                return;
            }
            // Programs still run with the current runner: keep it, and
            // switch once they are gone.
            *this.0.pending_runner.borrow_mut() = Some(id);
            this.select_runner(row, &all);
            this.window()
                .error("Still running: hit power, then Stop. Runner changes after.");
            let weak = this.weak();
            glib::timeout_add_seconds_local(2, move || {
                let Some(inner) = weak.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                let this = BottlePage(inner);
                if this.0.pending_runner.borrow().is_none() {
                    return glib::ControlFlow::Break;
                }
                this.window().sessions().poll();
                glib::ControlFlow::Continue
            });
        });
        group.add(&runner_row);

        group.add(&self.choice(
            "Backend",
            Some("Rosemary backend to run programs with"),
            Backend::ALL,
            Backend::label,
            |c| c.backend,
            |c, v| c.backend = v,
        ));
        group.add(&self.choice(
            "Home Folder",
            Some("Where programs keep documents and settings"),
            PrefixHome::ALL,
            PrefixHome::label,
            |c| c.prefix_home,
            |c, v| c.prefix_home = v,
        ));
        group.add(&self.switch(
            "Disable Metal Support",
            "Hides the Vulkan driver from Rosemary. This may help with older hardware and applications where Metal translation doesn't work properly yet",
            |c| c.disable_metal,
            |c, v| c.disable_metal = v,
        ));
        group.add(&self.steam_switch());
        group.add(&self.switch(
            "Cache Compiled Code",
            "Keep the code of emulated programs compiled ahead of time, so later runs start faster",
            |c| c.aot_cache,
            |c, v| c.aot_cache = v,
        ));
        group.add(&self.switch(
            "Compile in Background",
            "Compile an emulated program ahead of time, at low priority, the first time it runs",
            |c| c.aot_background,
            |c, v| c.aot_background = v,
        ));
        group
    }

    /// The Steam bridge, which only works when the sandbox can reach Steam's
    /// folders. Switching it on without them shows how to allow them.
    fn steam_switch(&self) -> adw::SwitchRow {
        let row = self.switch(
            "Steam Integration",
            "Bridge Steamworks calls to the Steam client on this computer",
            |c| c.steam,
            |c, v| c.steam = v,
        );
        if permissions::available(Feature::Steam) {
            return row;
        }
        row.set_subtitle(
            "Bridge Steamworks calls to the Steam client on this computer. \
             Butter needs permission to reach Steam's folders first",
        );
        let warning = ui::icon_button("dialog-warning-symbolic", "Allow Access to Steam");
        warning.set_visible(row.is_active());
        let weak = self.weak();
        warning.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                ui::permissions::request(BottlePage(inner).window(), Feature::Steam);
            }
        });
        row.add_suffix(&warning);
        let weak = self.weak();
        row.connect_active_notify(move |row| {
            warning.set_visible(row.is_active());
            if row.is_active()
                && let Some(inner) = weak.upgrade()
            {
                ui::permissions::request(BottlePage(inner).window(), Feature::Steam);
            }
        });
        row
    }

    /// A combo row choosing one of this computer's GPUs, or none in
    /// particular.
    fn gpu_row(
        &self,
        title: &str,
        subtitle: &str,
        gpus: &[Gpu],
        get: fn(&bottle::Config) -> Option<Gpu>,
        set: fn(&mut bottle::Config, Option<Gpu>),
    ) -> adw::ComboRow {
        let current = get(&self.bottle().config);
        let mut choices: Vec<Option<Gpu>> = vec![None];
        choices.extend(gpus.iter().cloned().map(Some));
        let mut labels = vec!["Automatic".to_string()];
        for gpu in gpus {
            // Two of the same card are told apart by where they sit.
            let twin = gpus.iter().filter(|g| g.name == gpu.name).count() > 1;
            labels.push(match twin {
                true => format!("{} ({})", gpu.name, gpu.slot),
                false => gpu.name.clone(),
            });
        }
        let selected = match &current {
            None => 0,
            Some(gpu) => match gpus.iter().position(|g| g.slot == gpu.slot) {
                Some(i) => i + 1,
                // Kept, so that a card taken out for a while is still
                // asked for when it is back.
                None => {
                    labels.push(format!("{} (not found)", gpu.name));
                    choices.push(current.clone());
                    choices.len() - 1
                }
            },
        } as u32;
        let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let row = ui::combo_row(title, Some(subtitle), &label_refs, selected);
        let weak = self.weak();
        row.connect_selected_notify(move |row| {
            if let (Some(inner), Some(gpu)) = (weak.upgrade(), choices.get(row.selected() as usize))
            {
                let gpu = gpu.clone();
                BottlePage(inner).edit(|c| set(c, gpu));
            }
        });
        row
    }

    fn graphics_group(&self) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::builder().title("Graphics").build();
        let gpus = gpu::list();
        group.add(&self.gpu_row(
            "Metal GPU",
            "The graphics card Metal programs render on",
            &gpus,
            |c| c.metal_gpu.clone(),
            |c, v| c.metal_gpu = v,
        ));
        group.add(&self.choice(
            "Metal Presentation",
            Some("How finished frames of Metal programs reach the screen"),
            PresentMode::ALL,
            PresentMode::label,
            |c| c.present_mode,
            |c, v| c.present_mode = v,
        ));
        group.add(&self.gpu_row(
            "OpenGL GPU",
            "The graphics card OpenGL programs render on",
            &gpus,
            |c| c.opengl_gpu.clone(),
            |c, v| c.opengl_gpu = v,
        ));
        group.add(&self.choice(
            "OpenGL Sync",
            Some("Override the swap interval of OpenGL programs"),
            SwapInterval::ALL,
            SwapInterval::label,
            |c| c.swap_interval,
            |c, v| c.swap_interval = v,
        ));
        group.add(&self.switch(
            "Host OpenGL Features",
            "Offer the driver's own OpenGL version and extensions instead of a Mac's. \
             Some programs then take newer paths no Mac has tested",
            |c| c.gl_host_features,
            |c, v| c.gl_host_features = v,
        ));
        group.add(&self.switch(
            "Shader Cache",
            "Keep translated Metal shaders and pipelines, so later runs start faster",
            |c| c.shader_cache,
            |c, v| c.shader_cache = v,
        ));

        let refresh = adw::SpinRow::builder()
            .title("Refresh Rate")
            .subtitle("Hertz programs are told the display runs at; 0 uses the display's own")
            .adjustment(&gtk::Adjustment::new(0.0, 0.0, 1000.0, 1.0, 10.0, 0.0))
            .numeric(true)
            .build();
        refresh.set_value(self.bottle().config.refresh_rate as f64);
        let weak = self.weak();
        refresh.connect_value_notify(move |row| {
            if let Some(inner) = weak.upgrade() {
                let rate = row.value().round() as u32;
                let this = BottlePage(inner);
                if this.bottle().config.refresh_rate != rate {
                    this.edit(|c| c.refresh_rate = rate);
                }
            }
        });
        group.add(&refresh);
        group
    }

    fn display_group(&self) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::builder()
            .title("Display and Input")
            .build();
        group.add(&self.titlebars_row());
        group.add(&self.choice(
            "Menu Bar",
            Some("Where an application's main menu is shown"),
            Menu::ALL,
            Menu::label,
            |c| c.menu,
            |c, v| c.menu = v,
        ));
        group.add(&self.choice(
            "Command Key",
            Some("The key programs see as ⌘"),
            CommandKey::ALL,
            CommandKey::label,
            |c| c.command_key,
            |c, v| c.command_key = v,
        ));
        group.add(&self.switch(
            "Confine Pointer",
            "Keep the pointer inside the focused window; some ports need it for dragging",
            |c| c.confine_pointer,
            |c, v| c.confine_pointer = v,
        ));
        group.add(&self.switch(
            "Plain Control-Click",
            "Treat a click with Ctrl held as an ordinary click, not a secondary click",
            |c| c.plain_ctrl_click,
            |c, v| c.plain_ctrl_click = v,
        ));
        group
    }

    fn advanced_group(&self) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::builder()
            .title("Advanced")
            .description("Experimental options. Change these only to work around a problem")
            .build();
        let expander = adw::ExpanderRow::builder()
            .title("Emulator and Runtime")
            .build();
        expander.add_row(&self.switch(
            "Process Server",
            "Let programs in this bottle see each other, for ps, top and task_for_pid",
            |c| c.process_server,
            |c, v| c.process_server = v,
        ));
        expander.add_row(&self.switch(
            "Dead Flag Elimination",
            "Faster in some programs, but may be inaccurate",
            |c| c.dead_flags,
            |c, v| c.dead_flags = v,
        ));
        expander.add_row(&self.switch(
            "Native Call Return",
            "May speed up some emulated programs",
            |c| c.call_return,
            |c, v| c.call_return = v,
        ));
        expander.add_row(&self.switch(
            "Strict Imports",
            "Refuse to run programs with unresolved imports",
            |c| c.strict_imports,
            |c, v| c.strict_imports = v,
        ));
        expander.add_row(&self.switch(
            "Host Library Passthrough",
            "Let imports Rosemary does not implement bind to libraries on this computer",
            |c| c.passthrough,
            |c, v| c.passthrough = v,
        ));
        expander.add_row(&self.choice(
            "Total Store Order",
            Some("Keep x86 memory ordering for emulated programs on ARM. Slower, but some programs need it"),
            Tso::ALL,
            Tso::label,
            |c| c.tso,
            |c, v| c.tso = v,
        ));
        let rows = adw::SpinRow::builder()
            .title("Block-Link Table Rows")
            .subtitle("Rows per thread linking translated code; 0 uses Rosemary's default")
            .adjustment(&gtk::Adjustment::new(
                0.0,
                0.0,
                16_777_216.0,
                1024.0,
                65536.0,
                0.0,
            ))
            .numeric(true)
            .build();
        rows.set_value(self.bottle().config.link_rows as f64);
        let weak = self.weak();
        rows.connect_value_notify(move |row| {
            if let Some(inner) = weak.upgrade() {
                let value = row.value().round() as u32;
                let this = BottlePage(inner);
                if this.bottle().config.link_rows != value {
                    this.edit(|c| c.link_rows = value);
                }
            }
        });
        expander.add_row(&rows);
        expander.add_row(&self.choice(
            "Block-Link Table Ways",
            Some("Rows one address may occupy in the table"),
            LinkWays::ALL,
            LinkWays::label,
            |c| c.link_ways,
            |c, v| c.link_ways = v,
        ));
        expander.add_row(&self.switch(
            "Raw Syscall Interposition",
            "Trap system calls programs make without libc. Native programs on x86-64 only",
            |c| c.syscall_interpose,
            |c, v| c.syscall_interpose = v,
        ));
        group.add(&expander);

        let debugging = adw::ExpanderRow::builder().title("Debugging").build();
        debugging.add_row(&self.choice(
            "Trace System Calls",
            Some("Log system calls to the program's output. Native programs on x86-64 only"),
            SyscallTrace::ALL,
            SyscallTrace::label,
            |c| c.syscall_trace,
            |c, v| c.syscall_trace = v,
        ));
        debugging.add_row(&self.switch(
            "Trace Cocoa Bindings",
            "Log each value Cocoa bindings hand to a bound object",
            |c| c.trace_bindings,
            |c, v| c.trace_bindings = v,
        ));
        debugging.add_row(&self.switch(
            "Rosemary libSystem Extensions",
            "Expose Rosemary's own additions to libSystem, for tools built for them",
            |c| c.libsystem_extensions,
            |c, v| c.libsystem_extensions = v,
        ));
        group.add(&debugging);
        group
    }

    /// Who draws titlebars. Choosing the desktop's on a compositor that
    /// draws none shows a warning: Rosemary then draws them with libdecor.
    fn titlebars_row(&self) -> adw::ComboRow {
        let row = self.choice(
            "Window Titlebars",
            None,
            Decorations::ALL,
            Decorations::label,
            |c| c.decorations,
            |c, v| c.decorations = v,
        );
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        content.append(&gtk::Image::from_icon_name("dialog-warning-symbolic"));
        // Two short lines, so the row keeps room for its title on a
        // narrow window.
        content.append(
            &gtk::Label::builder()
                .label("May cause issues")
                .wrap(true)
                .max_width_chars(9)
                .justify(gtk::Justification::Center)
                .css_classes(["caption"])
                .build(),
        );
        let warning = gtk::Button::builder()
            .child(&content)
            .tooltip_text("Details")
            .valign(gtk::Align::Center)
            .visible(false)
            .css_classes(["flat", "warning"])
            .build();
        row.add_suffix(&warning);

        // Only once the compositor is known to draw no titlebars itself.
        let lacks = Rc::new(Cell::new(false));
        let show = {
            let (warning, lacks) = (warning.clone(), lacks.clone());
            move |row: &adw::ComboRow| {
                let chosen = Decorations::from_index(row.selected()) == Decorations::Server;
                warning.set_visible(chosen && lacks.get());
            }
        };
        row.connect_selected_notify(show.clone());
        let r = row.clone();
        glib::spawn_future_local(async move {
            let answer = crate::process::background(crate::wayland::server_decorations).await;
            lacks.set(answer == Some(false));
            show(&r);
        });

        let weak = self.weak();
        let r = row.clone();
        warning.connect_clicked(move |_| {
            let Some(inner) = weak.upgrade() else { return };
            let this = BottlePage(inner);
            let row = r.clone();
            glib::spawn_future_local(async move {
                let dialog = adw::AlertDialog::new(
                    Some("Desktop Titlebars May Cause Issues"),
                    Some(
                        "This desktop does not draw titlebars for the windows that ask it to, \
                         so Rosemary will likely draw them with libdecor instead.\n\n\
                         libdecor uses GTK 3, an old version of GTK, which cannot share a \
                         program with the newer GTK that WebKitGTK uses. Applications that \
                         use WebKitGTK may crash or fail to open their windows.\n\n\
                         If an application has trouble, switch back to the Mac-style titlebar.",
                    ),
                );
                dialog.add_responses(&[
                    ("keep", "_Keep Desktop Titlebar"),
                    ("client", "_Use Mac-Style Titlebar"),
                ]);
                dialog.set_response_appearance("client", adw::ResponseAppearance::Suggested);
                dialog.set_default_response(Some("client"));
                dialog.set_close_response("keep");
                if dialog.choose_future(Some(&this.window().widget())).await == "client" {
                    row.set_selected(Decorations::Client.index());
                }
            });
        });
        row
    }

    fn environment_group(&self) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::builder()
            .title("Environment Variables")
            .description(
                "Mac programs see only the variables set for them. Variables for \
                 Rosemary override the settings above",
            )
            .build();
        let add = adw::ActionRow::builder()
            .title("Add Variable…")
            .activatable(true)
            .build();
        add.add_prefix(&gtk::Image::from_icon_name("list-add-symbolic"));
        let weak = self.weak();
        add.connect_activated(move |_| {
            if let Some(inner) = weak.upgrade() {
                BottlePage(inner).edit_variable(None);
            }
        });
        // NAME=value, for whoever the name suggests.
        let entry = adw::EntryRow::builder()
            .title("NAME=value")
            .show_apply_button(true)
            .build();
        let weak = self.weak();
        entry.connect_apply(move |entry| {
            let Some(inner) = weak.upgrade() else { return };
            let this = BottlePage(inner);
            let text = entry.text();
            let Some((key, value)) = text.split_once('=') else {
                this.window().notify("Write variables as NAME=value");
                return;
            };
            let name = key.trim().to_string();
            let var = EnvVar {
                target: EnvTarget::suggested(&name),
                name,
                value: Some(value.to_string()),
                enabled: true,
            };
            if let Some(problem) = var.problem() {
                this.window().notify(problem);
                return;
            }
            this.edit(|c| c.set_variable(var));
            entry.set_text("");
            this.refresh_environment();
        });
        let forward = adw::EntryRow::builder()
            .title("Passed On to Launched Programs")
            .tooltip_text(
                "Comma-separated names of variables a program may set for \
                 another Mac program it starts. End a name with * to match \
                 every name it begins",
            )
            .text(self.bottle().config.forward_env.join(", "))
            .show_apply_button(true)
            .build();
        let weak = self.weak();
        forward.connect_apply(move |entry| {
            let Some(inner) = weak.upgrade() else { return };
            let this = BottlePage(inner);
            let names: Vec<String> = entry
                .text()
                .split(',')
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty())
                .collect();
            if let Some(bad) = names.iter().find(|n| !bottle::valid_forward_name(n)) {
                this.window()
                    .notify(&format!("{bad} is not a variable name"));
                return;
            }
            entry.set_text(&names.join(", "));
            this.edit(|c| c.forward_env = names);
        });

        group.add(&self.0.env);
        let more = ui::boxed_list();
        more.append(&add);
        more.append(&entry);
        more.append(&forward);
        more.set_margin_top(12);
        group.add(&more);
        group
    }

    /// Add a variable, or edit `existing`.
    fn edit_variable(&self, existing: Option<EnvVar>) {
        let weak = self.weak();
        env_dialog::present(self.window(), existing, move |var, renamed| {
            if let Some(inner) = weak.upgrade() {
                let this = BottlePage(inner);
                this.edit(|c| {
                    match renamed.and_then(|old| c.variables.iter().position(|v| v.name == old)) {
                        // In its place, unless the new name is taken.
                        Some(i) if !c.variables.iter().any(|v| v.name == var.name) => {
                            c.variables[i] = var;
                        }
                        Some(i) => {
                            c.variables.remove(i);
                            c.set_variable(var);
                        }
                        None => c.set_variable(var),
                    }
                });
                this.refresh_environment();
            }
        });
    }

    fn refresh_environment(&self) {
        let list = &self.0.env;
        list.remove_all();
        let variables = self.bottle().config.variables;
        list.set_visible(!variables.is_empty());
        for var in variables {
            let value = match &var.value {
                Some(value) => glib::markup_escape_text(value).to_string(),
                None => "<i>This computer's value</i>".to_string(),
            };
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&var.name))
                .subtitle(value)
                .activatable(true)
                .build();
            row.add_css_class("property");
            let target = gtk::Label::builder()
                .label(var.target.label())
                .valign(gtk::Align::Center)
                .build();
            target.add_css_class("dim-label");
            target.add_css_class("caption");
            row.add_suffix(&target);
            let enabled = gtk::Switch::builder()
                .active(var.enabled)
                .valign(gtk::Align::Center)
                .tooltip_text("Set this variable")
                .build();
            let weak = self.weak();
            let name = var.name.clone();
            enabled.connect_active_notify(move |switch| {
                if let Some(inner) = weak.upgrade() {
                    let active = switch.is_active();
                    BottlePage(inner).edit(|c| {
                        if let Some(v) = c.variables.iter_mut().find(|v| v.name == name) {
                            v.enabled = active;
                        }
                    });
                }
            });
            row.add_suffix(&enabled);
            let remove = ui::icon_button("user-trash-symbolic", "Remove");
            let weak = self.weak();
            let name = var.name.clone();
            remove.connect_clicked(move |_| {
                if let Some(inner) = weak.upgrade() {
                    let this = BottlePage(inner);
                    this.edit(|c| c.variables.retain(|v| v.name != name));
                    this.refresh_environment();
                }
            });
            row.add_suffix(&remove);
            let weak = self.weak();
            row.connect_activated(move |_| {
                if let Some(inner) = weak.upgrade() {
                    // The switch may have changed it since the row was made.
                    let this = BottlePage(inner);
                    let current = this
                        .bottle()
                        .config
                        .variables
                        .into_iter()
                        .find(|v| v.name == var.name);
                    this.edit_variable(current);
                }
            });
            list.append(&row);
        }
    }

    /// Run `rosemary aot` for `target` in a task dialog, so its next runs
    /// start from code compiled ahead of time.
    fn compile(&self, target: std::path::PathBuf) {
        let this = self.clone();
        glib::spawn_future_local(async move {
            let bottle = this.bottle();
            if !bottle.config.aot_cache {
                this.window()
                    .error("Turn on Cache Compiled Code to compile ahead of time");
                return;
            }
            let key = format!("bottle:{}", bottle.id);
            if !this.window().begin(&key) {
                this.window().notify("This bottle is already busy");
                return;
            }
            match bottle.aot_command(&target) {
                Ok(mut command) => {
                    let _ = task::run(this.window(), "Compiling Ahead of Time", move |r| {
                        r.stage("rosemary aot", None);
                        r.run_checked(&mut command)?;
                        Ok("Compiled".into())
                    })
                    .await;
                }
                Err(e) => this.window().error(e),
            }
            this.window().end(&key);
        });
    }

    /// Run `rosemary prefix ACTION` for this bottle in a task dialog.
    fn prefix_task(&self, title: &str, action: &'static str, args: Vec<&'static str>) {
        let this = self.clone();
        let title = title.to_string();
        glib::spawn_future_local(async move {
            let bottle = this.bottle();
            let key = format!("bottle:{}", bottle.id);
            if !this.window().begin(&key) {
                this.window().notify("This bottle is already busy");
                return;
            }
            match bottle.prefix_command(action, &args) {
                Ok(mut command) => {
                    let _ = task::run(this.window(), &title, move |r| {
                        r.stage(format!("rosemary prefix {action}"), None);
                        r.run_checked(&mut command)?;
                        Ok("Done".into())
                    })
                    .await;
                }
                Err(e) => this.window().error(e),
            }
            this.window().end(&key);
            this.refresh_banner();
        });
    }

    fn prefix_group(&self) -> adw::PreferencesGroup {
        let bottle = self.bottle();
        let group = adw::PreferencesGroup::builder()
            .title("Prefix")
            .description("The Darwin filesystem programs in this bottle see as /")
            .build();

        let location = adw::ActionRow::builder()
            .title("Location")
            .subtitle(glib::markup_escape_text(
                &bottle.prefix().display().to_string(),
            ))
            .subtitle_selectable(true)
            .build();
        let open = ui::icon_button("folder-open-symbolic", "Open in Files");
        let weak = self.weak();
        open.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                let this = BottlePage(inner);
                let bottle = this.bottle();
                let target = if bottle.drive().is_dir() {
                    bottle.drive()
                } else {
                    bottle.dir.clone()
                };
                ui::open_folder(&this.window().widget(), &target);
            }
        });
        location.add_suffix(&open);
        group.add(&location);

        let size = adw::ActionRow::builder()
            .title("Disk Usage")
            .subtitle("Calculating…")
            .build();
        let dir = bottle.dir.clone();
        let size_row = size.clone();
        glib::spawn_future_local(async move {
            let bytes = crate::process::background(move || crate::util::dir_size(&dir)).await;
            size_row.set_subtitle(&crate::util::format_size(bytes));
        });
        group.add(&size);

        let policy = self.choice(
            "System Files",
            Some("When a program changes a file the base owns"),
            SystemFiles::ALL,
            SystemFiles::label,
            |c| c.system_files,
            |c, v| c.system_files = v,
        );
        let weak = self.weak();
        // Rosemary records the policy in the prefix; re-provision to change it.
        policy.connect_selected_notify(move |_| {
            if let Some(inner) = weak.upgrade() {
                let this = BottlePage(inner);
                if this.bottle().is_ready() {
                    let args = this.bottle().create_args(true);
                    this.prefix_task("Changing System File Policy", "create", args);
                }
            }
        });
        group.add(&policy);

        let maintenance = adw::ExpanderRow::builder()
            .title("Maintenance")
            .subtitle("Check and repair the Darwin base")
            .build();
        let actions: [(&str, &str, &str, &'static str, Vec<&'static str>); 5] = [
            ("Details", "", "Prefix Details", "info", vec![]),
            (
                "Verify",
                "Hash every system file against the manifest",
                "Verifying System Files",
                "verify",
                vec![],
            ),
            (
                "Repair",
                "Put back system files that differ without touching user-made system files",
                "Repairing System Files",
                "repair",
                vec![],
            ),
            (
                "Reset System Files",
                "Completely wipe anything unexpected in system files. Possibly destructive, use as a last resort.",
                "Resetting System Files",
                "reset-system",
                vec![],
            ),
            (
                "Update Base",
                "Move to the base release the runner pins",
                "Updating the Darwin Base",
                "update",
                vec![],
            ),
        ];
        for (title, subtitle, task_title, action, args) in actions {
            let row = adw::ActionRow::builder()
                .title(title)
                .subtitle(subtitle)
                .activatable(true)
                .build();
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            let weak = self.weak();
            row.connect_activated(move |_| {
                if let Some(inner) = weak.upgrade() {
                    BottlePage(inner).prefix_task(task_title, action, args.clone());
                }
            });
            maintenance.add_row(&row);
        }
        group.add(&maintenance);

        let log = adw::ActionRow::builder()
            .title("Last Run Log")
            .subtitle("Output of the most recently started program")
            .activatable(true)
            .build();
        log.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        let weak = self.weak();
        log.connect_activated(move |_| {
            if let Some(inner) = weak.upgrade() {
                let this = BottlePage(inner);
                let text = std::fs::read_to_string(this.bottle().last_log())
                    .unwrap_or_else(|_| "Nothing has run in this bottle yet.".into());
                ui::show_text(&this.window().widget(), "Last Run", &text);
            }
        });
        group.add(&log);
        group
    }

    fn danger_group(&self) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::new();
        let delete = gtk::Button::builder()
            .label("_Delete Bottle")
            .use_underline(true)
            .halign(gtk::Align::Center)
            .build();
        delete.add_css_class("pill");
        delete.add_css_class("destructive-action");
        let weak = self.weak();
        delete.connect_clicked(move |_| {
            let Some(inner) = weak.upgrade() else { return };
            let this = BottlePage(inner);
            glib::spawn_future_local(async move {
                let bottle = this.bottle();
                let window = this.window().clone();
                if window.sessions().is_running(&bottle.id) {
                    window.error(format!(
                        "Programs are still running in {}; stop the bottle first",
                        bottle.config.name
                    ));
                    return;
                }
                if !ui::confirm(
                    &window.widget(),
                    &format!("Delete {}?", bottle.config.name),
                    "Its prefix, and every file programs saved inside it, will be permanently deleted.",
                    "_Delete",
                    true,
                )
                .await
                {
                    return;
                }
                // Its shortcuts would run a bottle that is gone.
                for id in &bottle.config.shortcuts {
                    if let Err(e) = shortcuts::uninstall(id).await {
                        eprintln!("butter: could not remove the shortcut {id}: {e:#}");
                    }
                }
                let result = crate::process::background(move || bottle::delete(&bottle)).await;
                match result {
                    Ok(()) => {
                        window.pop_to_main();
                        window.refresh();
                        window.notify("Bottle deleted");
                    }
                    Err(e) => window.error(format!("Could not delete the bottle: {e}")),
                }
            });
        });
        group.add(&delete);
        group
    }
}
