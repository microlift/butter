//! The Plugins tab: each plugin's actions, settings and shortcuts.

use std::cell::RefCell;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::config::Paths;
use crate::plugins::{self, Plugin, Scope, State};
use crate::ui::{self, window::Window};

pub struct PluginsPage {
    page: adw::PreferencesPage,
    groups: RefCell<Vec<adw::PreferencesGroup>>,
    window: Window,
}

fn escape(text: &str) -> String {
    glib::markup_escape_text(text).to_string()
}

impl PluginsPage {
    pub fn new(window: &Window) -> PluginsPage {
        PluginsPage {
            page: adw::PreferencesPage::new(),
            groups: RefCell::default(),
            window: window.clone(),
        }
    }

    pub fn widget(&self) -> &adw::PreferencesPage {
        &self.page
    }

    /// Look for plugins again and rebuild the page.
    pub fn refresh(&self) {
        for group in self.groups.borrow_mut().drain(..) {
            self.page.remove(&group);
        }
        let found = plugins::discover();
        let settings = self.window.settings();
        let mut groups = vec![self.intro(&settings)];
        for plugin in &found.plugins {
            let enabled = !settings.disabled_plugins.iter().any(|id| id == plugin.id());
            groups.push(self.plugin_group(plugin, enabled));
        }
        if !found.problems.is_empty() {
            let group = adw::PreferencesGroup::builder()
                .title("Not Loaded")
                .description("These folders hold plugins Butter could not use")
                .build();
            for (dir, problem) in &found.problems {
                let row = adw::ActionRow::builder()
                    .title(escape(&dir.display().to_string()))
                    .subtitle(escape(problem))
                    .build();
                let warning = gtk::Image::from_icon_name("dialog-warning-symbolic");
                warning.add_css_class("warning");
                row.add_prefix(&warning);
                group.add(&row);
            }
            groups.push(group);
        }
        for group in &groups {
            self.page.add(group);
        }
        *self.groups.borrow_mut() = groups;
    }

    fn intro(&self, settings: &crate::settings::Settings) -> adw::PreferencesGroup {
        let folder = Paths::get().plugins();
        let group = adw::PreferencesGroup::builder()
            .title("Plugins")
            .description(escape(&format!(
                "Plugins add actions written in Python. To install one, put its folder in {}",
                folder.display()
            )))
            .build();
        let open = ui::icon_button("folder-open-symbolic", "Open Plugins Folder");
        let window = self.window.clone();
        open.connect_clicked(move |_| {
            let folder = Paths::get().plugins();
            let _ = std::fs::create_dir_all(&folder);
            ui::open_folder(&window.widget(), &folder);
        });
        let reload = ui::icon_button("view-refresh-symbolic", "Look for Plugins Again");
        let window = self.window.clone();
        reload.connect_clicked(move |_| window.refresh_plugins());
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        buttons.append(&reload);
        buttons.append(&open);
        group.set_header_suffix(Some(&buttons));

        if let Err(e) = plugins::python(settings) {
            let row = adw::ActionRow::builder()
                .title("Python Is Missing")
                .subtitle(escape(&format!("{e:#}")))
                .build();
            let warning = gtk::Image::from_icon_name("dialog-warning-symbolic");
            warning.add_css_class("warning");
            row.add_prefix(&warning);
            let choose = gtk::Button::builder()
                .label("Preferences")
                .action_name("win.preferences")
                .valign(gtk::Align::Center)
                .build();
            row.add_suffix(&choose);
            group.add(&row);
        }
        group
    }

    fn plugin_group(&self, plugin: &Plugin, enabled: bool) -> adw::PreferencesGroup {
        let manifest = &plugin.manifest;
        let mut about = vec![];
        if !manifest.description.is_empty() {
            about.push(manifest.description.clone());
        }
        let mut facts = vec![];
        if !manifest.version.is_empty() {
            facts.push(format!("Version {}", manifest.version));
        }
        if !manifest.authors.is_empty() {
            facts.push(format!("By {}", manifest.authors.join(", ")));
        }
        facts.push(plugin.source.label().to_string());
        about.push(facts.join(" · "));
        let group = adw::PreferencesGroup::builder()
            .title(escape(plugin.name()))
            .description(escape(&about.join("\n")))
            .build();

        let buttons = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .build();
        if enabled && !manifest.settings.is_empty() {
            let settings = ui::icon_button("emblem-system-symbolic", "Settings");
            let (window, p) = (self.window.clone(), plugin.clone());
            settings.connect_clicked(move |_| ui::plugins::present_settings(&window, &p));
            buttons.append(&settings);
        }
        let menu = gio::Menu::new();
        menu.append(Some("Open Plugin _Folder"), Some("plugin.open-folder"));
        menu.append(Some("Open _Data Folder"), Some("plugin.open-data"));
        menu.append(Some("Remove _Shortcuts"), Some("plugin.remove-shortcuts"));
        if manifest.website.is_some() {
            menu.append(Some("_Website"), Some("plugin.website"));
        }
        buttons.append(
            &gtk::MenuButton::builder()
                .icon_name("view-more-symbolic")
                .tooltip_text("More")
                .menu_model(&menu)
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build(),
        );
        let switch = gtk::Switch::builder()
            .active(enabled)
            .valign(gtk::Align::Center)
            .tooltip_text(if enabled { "Switch Off" } else { "Switch On" })
            .build();
        let (window, id) = (self.window.clone(), plugin.id().to_string());
        switch.connect_active_notify(move |switch| {
            let on = switch.is_active();
            let id = id.clone();
            window.update_settings(|s| {
                s.disabled_plugins.retain(|d| *d != id);
                if !on {
                    s.disabled_plugins.push(id);
                }
            });
            // Rebuilding the page inside the switch's own handler would
            // destroy it mid-signal.
            let window = window.clone();
            glib::idle_add_local_once(move || window.refresh_plugins());
        });
        buttons.append(&switch);
        group.set_header_suffix(Some(&buttons));
        group.insert_action_group("plugin", Some(&self.plugin_actions(plugin)));

        if !enabled {
            group.add(&ui::placeholder_row("Switched off"));
            return group;
        }
        let actions: Vec<_> = manifest
            .actions
            .iter()
            .filter(|a| a.scope == Scope::Global && !a.hidden)
            .collect();
        for action in &actions {
            let row = adw::ActionRow::builder()
                .title(escape(&action.title))
                .subtitle(escape(&action.subtitle))
                .build();
            let run = ui::icon_button("media-playback-start-symbolic", "Run");
            row.add_suffix(&run);
            row.set_activatable_widget(Some(&run));
            let (window, p, id) = (self.window.clone(), plugin.clone(), action.id.clone());
            run.connect_clicked(move |_| {
                let (window, p, id) = (window.clone(), p.clone(), id.clone());
                glib::spawn_future_local(async move {
                    ui::plugins::run(&window, &p, &id, None, Vec::new()).await;
                });
            });
            group.add(&row);
        }
        if actions.is_empty() {
            let has_bottle_actions = manifest.actions.iter().any(|a| a.scope == Scope::Bottle);
            group.add(&ui::placeholder_row(if has_bottle_actions {
                "Its actions are on each bottle's page"
            } else {
                "It has no actions to show"
            }));
        }
        group
    }

    fn plugin_actions(&self, plugin: &Plugin) -> gio::SimpleActionGroup {
        let group = gio::SimpleActionGroup::new();
        let add = |name: &str, activate: Box<dyn Fn()>| {
            let action = gio::SimpleAction::new(name, None);
            action.connect_activate(move |_, _| activate());
            group.add_action(&action);
        };
        let (window, dir) = (self.window.clone(), plugin.dir.clone());
        add(
            "open-folder",
            Box::new(move || ui::open_folder(&window.widget(), &dir)),
        );
        let (window, dir) = (self.window.clone(), plugin.data_dir());
        add(
            "open-data",
            Box::new(move || {
                let _ = std::fs::create_dir_all(&dir);
                ui::open_folder(&window.widget(), &dir);
            }),
        );
        let (window, p) = (self.window.clone(), plugin.clone());
        add(
            "remove-shortcuts",
            Box::new(move || {
                let (window, p) = (window.clone(), p.clone());
                glib::spawn_future_local(async move {
                    ui::plugins::remove_shortcuts(&window, &p).await;
                });
            }),
        );
        if let Some(website) = plugin
            .manifest
            .website
            .clone()
            .filter(|w| w.starts_with("https://") || w.starts_with("http://"))
        {
            let window = self.window.clone();
            add(
                "website",
                Box::new(move || {
                    gtk::UriLauncher::new(&website).launch(
                        Some(&window.widget()),
                        gio::Cancellable::NONE,
                        |_| {},
                    );
                }),
            );
        }
        if let Some(action) = group.lookup_action("remove-shortcuts")
            && let Ok(action) = action.downcast::<gio::SimpleAction>()
        {
            action.set_enabled(!State::load(plugin.id()).shortcuts.is_empty());
        }
        group
    }
}
