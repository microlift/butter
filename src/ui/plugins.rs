//! Running plugin actions, carrying out what they ask for, and the settings
//! dialog plugins get from their manifest.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use adw::prelude::*;
use gtk::{gio, glib};

use crate::bottle::{self, Bottle};
use crate::plugins::{self, Effect, Plugin, SettingKind, State};
use crate::shortcuts::{self, Launch};
use crate::ui::{self, task, window::Window};

/// Run `plugin`'s `action`, in `bottle` for a bottle action, showing its
/// progress, then do what it asked for.
pub async fn run(
    window: &Window,
    plugin: &Plugin,
    action: &str,
    bottle: Option<Bottle>,
    open: Vec<String>,
) {
    let Some(described) = plugin.action(action) else {
        window.present();
        window.error(format!("{} has no action {action}", plugin.name()));
        window.close_if_unused();
        return;
    };
    let key = format!("plugin:{}:{action}", plugin.id());
    if !window.begin(&key) {
        window.show_log(&key);
        return;
    }
    // Hidden, the window was opened by a shortcut or link: the dialog gets
    // out of the way once the action succeeds.
    let interactive = window.widget().is_visible();
    let request = plugins::Request {
        plugin: plugin.clone(),
        action: action.to_string(),
        bottle,
        open,
        interactive,
    };
    let effects: Arc<Mutex<Vec<Effect>>> = Arc::default();
    let title = match &request.bottle {
        Some(bottle) => format!("{} in {}", described.title, bottle.config.name),
        None => described.title.clone(),
    };
    let result = task::run_with(window, Some(&key), &title, true, !interactive, {
        let effects = effects.clone();
        let title = title.clone();
        move |reporter| {
            let outcome = plugins::run(&request, reporter)?;
            *effects.lock().unwrap() = outcome.effects;
            Ok(outcome.message.unwrap_or_else(|| format!("{title}: done")))
        }
    })
    .await;
    window.end(&key);
    window.refresh();
    if result.is_ok() {
        let effects = std::mem::take(&mut *effects.lock().unwrap());
        for effect in effects {
            apply(window, plugin, effect).await;
        }
    }
    window.close_if_unused();
}

/// Do one thing an action asked for.
async fn apply(window: &Window, plugin: &Plugin, effect: Effect) {
    match effect {
        Effect::Launch {
            bottle,
            target,
            args,
            open,
            name,
            backend,
        } => {
            let Some(bottle) = bottle::load(&bottle) else {
                window.present();
                window.error(format!(
                    "{} asked to start a program in {bottle}, which is not a bottle",
                    plugin.name()
                ));
                return;
            };
            let launch = Launch {
                bottle: bottle.id.clone(),
                target,
                args,
                backend,
                open,
            };
            let name = name.unwrap_or_else(|| bottle::program_name(&launch.target));
            let args = launch.arguments(&bottle);
            window.launch(&bottle, &launch.target, &args, launch.backend, &name);
        }
        Effect::Shortcut {
            name,
            action,
            schemes,
            mime_types,
            icon,
            categories,
            comment,
        } => {
            if plugin.action(&action).is_none() {
                window.error(format!(
                    "{} asked for a shortcut to {action}, which it lacks",
                    plugin.name()
                ));
                return;
            }
            let comment = comment.unwrap_or_else(|| format!("Run with {}", plugin.name()));
            let icon = icon.filter(|p| p.is_file()).or_else(|| plugin.icon());
            let shortcut = shortcuts::Shortcut::for_plugin(
                plugin.id(),
                &action,
                &name,
                &comment,
                &schemes,
                &mime_types,
                &categories,
                icon,
            );
            match shortcuts::install(&shortcut).await {
                Ok(Some(name)) => {
                    let id = shortcut.id();
                    let saved = State::update(plugin.id(), |state| {
                        if !state.shortcuts.contains(&id) {
                            state.shortcuts.push(id);
                        }
                    });
                    if let Err(e) = saved {
                        window.error(format!("Could not save {}'s state: {e:#}", plugin.name()));
                    }
                    window.notify(&format!("Added {name} to the applications menu"));
                }
                Ok(None) => {}
                Err(e) => window.error(format!("Could not create the shortcut: {e:#}")),
            }
        }
        Effect::Notify { text } => window.notify(&text),
        Effect::Open { location } => {
            let parent = window.widget();
            if location.starts_with('/') {
                let _ = std::fs::create_dir_all(&location);
                ui::open_folder(&parent, std::path::Path::new(&location));
            } else if location.starts_with("https://") || location.starts_with("http://") {
                gtk::UriLauncher::new(&location).launch(
                    Some(&parent),
                    gio::Cancellable::NONE,
                    |_| {},
                );
            } else {
                window.error(format!("{} asked to open {location}", plugin.name()));
            }
        }
    }
}

/// Remove the shortcuts made to `plugin`'s actions.
pub async fn remove_shortcuts(window: &Window, plugin: &Plugin) {
    let ids = State::load(plugin.id()).shortcuts;
    let mut failed = None;
    for id in &ids {
        if let Err(e) = shortcuts::uninstall(id).await {
            failed = Some(e);
        }
    }
    // Forgotten either way: a shortcut the desktop no longer has is gone.
    let _ = State::update(plugin.id(), |state| state.shortcuts.clear());
    match failed {
        Some(e) => window.error(format!("Could not remove a shortcut: {e:#}")),
        None if ids.is_empty() => window.notify("There were no shortcuts"),
        None => window.notify("Shortcuts removed"),
    }
}

/// The settings dialog `plugin`'s manifest describes. Changes are saved as
/// they are made.
pub fn present_settings(window: &Window, plugin: &Plugin) {
    let values = plugin.values();
    let group = adw::PreferencesGroup::builder()
        .title(glib::markup_escape_text(plugin.name()))
        .description(glib::markup_escape_text(&plugin.manifest.description))
        .build();
    let save = {
        let window = window.clone();
        let plugin = plugin.clone();
        move |key: &str, value: Option<toml::Value>| {
            if let Err(e) = plugins::set_setting(&plugin, key, value) {
                window.error(format!("Could not save the setting: {e:#}"));
            }
        }
    };
    let save = Rc::new(save);
    for setting in &plugin.manifest.settings {
        let value = values
            .get(&setting.key)
            .cloned()
            .unwrap_or_else(|| setting.default_value());
        let key = setting.key.clone();
        let title = glib::markup_escape_text(&setting.title).to_string();
        let subtitle = glib::markup_escape_text(&setting.subtitle).to_string();
        let save = save.clone();
        let row: gtk::Widget = match setting.kind {
            SettingKind::Bool => {
                let row = adw::SwitchRow::builder()
                    .title(&title)
                    .subtitle(&subtitle)
                    .active(value.as_bool().unwrap_or(false))
                    .build();
                row.connect_active_notify(move |row| {
                    save(&key, Some(toml::Value::Boolean(row.is_active())));
                });
                row.upcast()
            }
            SettingKind::Int => {
                let min = setting.min.unwrap_or(i32::MIN as i64) as f64;
                let max = setting.max.unwrap_or(i32::MAX as i64) as f64;
                let row = adw::SpinRow::with_range(min, max, 1.0);
                row.set_title(&title);
                row.set_subtitle(&subtitle);
                row.set_value(value.as_integer().unwrap_or(0) as f64);
                row.connect_value_notify(move |row| {
                    save(&key, Some(toml::Value::Integer(row.value() as i64)));
                });
                row.upcast()
            }
            SettingKind::String => {
                let row = adw::EntryRow::builder()
                    .title(&title)
                    .text(value.as_str().unwrap_or_default())
                    .show_apply_button(true)
                    .build();
                if !setting.subtitle.is_empty() {
                    row.set_tooltip_text(Some(&setting.subtitle));
                }
                row.connect_apply(move |row| {
                    save(
                        &key,
                        Some(toml::Value::String(row.text().trim().to_string())),
                    );
                });
                row.upcast()
            }
            SettingKind::Choice => {
                let labels: Vec<&str> = setting.options.iter().map(|c| c.label.as_str()).collect();
                let selected = setting
                    .options
                    .iter()
                    .position(|c| Some(c.value.as_str()) == value.as_str())
                    .unwrap_or(0) as u32;
                let row = ui::combo_row(&title, Some(&subtitle), &labels, selected);
                let values: Vec<String> = setting.options.iter().map(|c| c.value.clone()).collect();
                row.connect_selected_notify(move |row| {
                    if let Some(value) = values.get(row.selected() as usize) {
                        save(&key, Some(toml::Value::String(value.clone())));
                    }
                });
                row.upcast()
            }
            SettingKind::Bottle => {
                let current = value.as_str().unwrap_or_default().to_string();
                let mut ids = vec![String::new()];
                let mut labels = vec!["None".to_string()];
                for bottle in bottle::list() {
                    ids.push(bottle.id.clone());
                    labels.push(bottle.config.name.clone());
                }
                if !current.is_empty() && !ids.contains(&current) {
                    ids.push(current.clone());
                    labels.push(format!("{current} (missing)"));
                }
                let selected = ids.iter().position(|id| *id == current).unwrap_or(0) as u32;
                let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
                let row = ui::combo_row(&title, Some(&subtitle), &refs, selected);
                row.connect_selected_notify(move |row| {
                    if let Some(id) = ids.get(row.selected() as usize) {
                        save(&key, Some(toml::Value::String(id.clone())));
                    }
                });
                row.upcast()
            }
            SettingKind::Text => {
                let row = adw::ActionRow::builder()
                    .title(&title)
                    .subtitle(&subtitle)
                    .activatable(true)
                    .build();
                row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
                let text = Rc::new(RefCell::new(value.as_str().unwrap_or_default().to_string()));
                let heading = setting.title.clone();
                row.connect_activated(move |row| {
                    let (save, key, text) = (save.clone(), key.clone(), text.clone());
                    let current = text.borrow().clone();
                    edit_text(row, &heading, &current, move |new| {
                        *text.borrow_mut() = new.clone();
                        save(&key, Some(toml::Value::String(new)));
                    });
                });
                row.upcast()
            }
        };
        group.add(&row);
    }

    let reset = gtk::Button::builder()
        .label("_Reset")
        .use_underline(true)
        .tooltip_text("Reset to Defaults")
        .valign(gtk::Align::Center)
        .build();
    reset.add_css_class("flat");
    group.set_header_suffix(Some(&reset));

    let page = adw::PreferencesPage::new();
    page.add(&group);
    let dialog = adw::PreferencesDialog::builder()
        .title(format!("{} Settings", plugin.name()))
        .build();
    dialog.add(&page);
    {
        let (window, plugin, dialog) = (window.clone(), plugin.clone(), dialog.clone());
        reset.connect_clicked(move |_| {
            if let Err(e) = State::update(plugin.id(), |state| state.settings.clear()) {
                window.error(format!("Could not reset the settings: {e:#}"));
            }
            dialog.close();
            present_settings(&window, &plugin);
        });
    }
    dialog.present(Some(&window.widget()));
}

/// Edit several lines of text, such as JSON, in a dialog of their own.
fn edit_text(
    parent: &impl IsA<gtk::Widget>,
    title: &str,
    text: &str,
    done: impl Fn(String) + 'static,
) {
    let view = gtk::TextView::builder()
        .monospace(true)
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(12)
        .bottom_margin(12)
        .left_margin(12)
        .right_margin(12)
        .build();
    view.buffer().set_text(text);
    let scrolled = gtk::ScrolledWindow::builder()
        .child(&view)
        .vexpand(true)
        .build();
    let save = gtk::Button::with_mnemonic("_Save");
    save.add_css_class("suggested-action");
    let header = adw::HeaderBar::new();
    header.pack_end(&save);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&scrolled));
    let dialog = adw::Dialog::builder()
        .title(title)
        .content_width(560)
        .content_height(420)
        .child(&toolbar)
        .build();
    save.connect_clicked({
        let dialog = dialog.clone();
        move |_| {
            let buffer = view.buffer();
            let text = buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), false)
                .to_string();
            done(text);
            dialog.close();
        }
    });
    dialog.present(Some(parent));
}
