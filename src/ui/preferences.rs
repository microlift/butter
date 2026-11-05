//! The preferences dialog.

use adw::prelude::*;
use gtk::glib;

use crate::config::{self, Paths};
use crate::process::background;
use crate::settings::BuildProfile;
use crate::ui::{self, window::Window};
use crate::{toolchain, util};

pub fn present(window: &Window) {
    let settings = window.settings();

    let prereleases = adw::SwitchRow::builder()
        .title("Show Pre-releases")
        .subtitle("List release candidates and other test builds")
        .active(settings.show_prereleases)
        .build();
    prereleases.connect_active_notify({
        let window = window.clone();
        move |row| {
            let active = row.is_active();
            window.update_settings(|s| s.show_prereleases = active);
            window.refresh_releases(false);
        }
    });
    let releases = adw::PreferencesGroup::builder().title("Releases").build();
    releases.add(&prereleases);

    let labels: Vec<&str> = BuildProfile::ALL.iter().map(|p| p.label()).collect();
    let selected = BuildProfile::ALL
        .iter()
        .position(|p| *p == settings.build_profile)
        .unwrap_or(0) as u32;
    let profile = ui::combo_row(
        "Build Profile",
        Some("Optimized builds link with LTO and only run on CPUs like this one"),
        &labels,
        selected,
    );
    profile.connect_selected_notify({
        let window = window.clone();
        move |row| {
            let profile = BuildProfile::ALL
                .get(row.selected() as usize)
                .copied()
                .unwrap_or_default();
            window.update_settings(|s| s.build_profile = profile);
        }
    });

    let keep_cache = adw::SwitchRow::builder()
        .title("Keep Build Cache")
        .subtitle("Later releases build much faster, at the cost of a few gigabytes")
        .active(settings.keep_build_cache)
        .build();
    keep_cache.connect_active_notify({
        let window = window.clone();
        move |row| {
            let active = row.is_active();
            window.update_settings(|s| s.keep_build_cache = active);
        }
    });

    let cache = adw::ActionRow::builder()
        .title("Build Cache")
        .subtitle("Calculating…")
        .build();
    let clear = gtk::Button::builder()
        .label("Clear")
        .valign(gtk::Align::Center)
        .build();
    cache.add_suffix(&clear);
    let measure = {
        let cache = cache.clone();
        move || {
            let cache = cache.clone();
            glib::spawn_future_local(async move {
                let bytes = background(|| {
                    let paths = Paths::get();
                    util::dir_size(&paths.build_target())
                        + util::dir_size(&paths.sources())
                        + util::dir_size(&paths.downloads())
                })
                .await;
                cache.set_subtitle(&util::format_size(bytes));
            });
        }
    };
    measure();
    clear.connect_clicked({
        let window = window.clone();
        move |_| {
            if window.is_busy_prefix("runner:") {
                window.notify("Wait for the running build to finish");
                return;
            }
            let measure = measure.clone();
            glib::spawn_future_local(async move {
                background(|| {
                    let paths = Paths::get();
                    for dir in [paths.build_target(), paths.sources(), paths.downloads()] {
                        let _ = std::fs::remove_dir_all(dir);
                    }
                })
                .await;
                measure();
            });
        }
    });

    let cargo = adw::EntryRow::builder()
        .title("Cargo Executable (blank to find one)")
        .text(settings.cargo_override.clone().unwrap_or_default())
        .show_apply_button(true)
        .build();
    cargo.connect_apply({
        let window = window.clone();
        move |row| {
            let text = row.text().trim().to_string();
            window.update_settings(|s| s.cargo_override = (!text.is_empty()).then_some(text));
            window.refresh_toolchain();
        }
    });

    let remove_toolchain = adw::ActionRow::builder()
        .title("Butter's Rust Toolchain")
        .subtitle(if toolchain::managed_installed() {
            "Installed"
        } else {
            "Not installed"
        })
        .build();
    let remove = gtk::Button::builder()
        .label("Remove")
        .valign(gtk::Align::Center)
        .sensitive(toolchain::managed_installed())
        .build();
    remove.add_css_class("destructive-action");
    remove_toolchain.add_suffix(&remove);
    remove.connect_clicked({
        let window = window.clone();
        let row = remove_toolchain.clone();
        move |button| {
            if window.is_busy("toolchain") || window.is_busy_prefix("runner:") {
                window.notify("Wait for the running job to finish");
                return;
            }
            let window = window.clone();
            let row = row.clone();
            let button = button.clone();
            glib::spawn_future_local(async move {
                match background(toolchain::remove_managed).await {
                    Ok(()) => {
                        row.set_subtitle("Not installed");
                        button.set_sensitive(false);
                    }
                    Err(e) => window.error(format!("Could not remove the toolchain: {e}")),
                }
                window.refresh_toolchain();
            });
        }
    });

    let building = adw::PreferencesGroup::builder()
        .title("Building Runners")
        .description(if config::is_flatpak() {
            "Runners are compiled inside Butter's sandbox, against the libraries it ships"
        } else {
            "Runners are compiled with this computer's toolchain and libraries"
        })
        .build();
    building.add(&profile);
    building.add(&keep_cache);
    building.add(&cache);
    building.add(&cargo);
    building.add(&remove_toolchain);

    let python = adw::EntryRow::builder()
        .title("Python Executable (blank to find python3)")
        .text(settings.python.clone().unwrap_or_default())
        .show_apply_button(true)
        .build();
    python.connect_apply({
        let window = window.clone();
        move |row| {
            let text = row.text().trim().to_string();
            window.update_settings(|s| s.python = (!text.is_empty()).then_some(text));
            window.refresh_plugins();
        }
    });
    let plugins = adw::PreferencesGroup::builder()
        .title("Plugins")
        .description("Plugins are Python scripts; they run with Python 3.9 or newer")
        .build();
    plugins.add(&python);

    let page = adw::PreferencesPage::new();
    page.add(&releases);
    page.add(&building);
    page.add(&plugins);
    if let Some(group) = ui::permissions::preferences_group(window) {
        page.add(&group);
    }
    let dialog = adw::PreferencesDialog::new();
    dialog.add(&page);
    dialog.present(Some(&window.widget()));
}
