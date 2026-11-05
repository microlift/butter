mod apps;
mod bottle;
mod config;
mod github;
mod gpu;
mod icons;
mod markdown;
mod permissions;
mod process;
mod procs;
mod runner;
mod sessions;
mod settings;
mod shortcuts;
mod supervisor;
mod toolchain;
mod ui;
mod util;

use adw::prelude::*;
use gtk::{gio, glib};

fn main() -> glib::ExitCode {
    // Butter starts each program through a copy of itself; see `supervisor`.
    let mut args = std::env::args_os().skip(1);
    if args.next().is_some_and(|a| a == supervisor::FLAG) {
        std::process::exit(supervisor::main(args.collect()));
    }

    glib::set_application_name(config::APP_NAME);

    let app = adw::Application::builder()
        .application_id(config::APP_ID)
        .resource_base_path("/io/github/microlift/Butter")
        // Shortcuts run `butter --run …`, which a running Butter handles.
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();

    app.connect_startup(|app| {
        icons::init();
        gtk::Window::set_default_icon_name(config::APP_ID);
        let about = gio::ActionEntry::builder("about")
            .activate(|app: &adw::Application, _, _| show_about(app))
            .build();
        let quit = gio::ActionEntry::builder("quit")
            .activate(|app: &adw::Application, _, _| app.quit())
            .build();
        app.add_action_entries([about, quit]);
        app.set_accels_for_action("app.quit", &["<Primary>q"]);
        app.set_accels_for_action("win.new-bottle", &["<Primary>n"]);
        app.set_accels_for_action("win.preferences", &["<Primary>comma"]);
        app.set_accels_for_action("win.show-help-overlay", &["<Primary>question"]);
        app.set_accels_for_action("win.tutorial", &["F1"]);
    });

    app.connect_activate(|app| {
        if let Some(window) = ui::window::Window::current() {
            window.present();
            return;
        }
        let window = ui::window::Window::new(app);
        window.present();
        // The tour waits for the sandbox questions, so they don't overlap.
        glib::spawn_future_local(async move {
            ui::permissions::review(&window).await;
            ui::tutorial::first_run(&window);
        });
    });

    app.connect_command_line(|app, command_line| {
        let args: Vec<String> = command_line
            .arguments()
            .iter()
            .skip(1)
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        match shortcuts::Launch::parse(&args) {
            Ok(Some(launch)) => {
                // Made, but not shown, when Butter was not already open.
                let window =
                    ui::window::Window::current().unwrap_or_else(|| ui::window::Window::new(app));
                window.run_shortcut(&launch);
            }
            Ok(None) => app.activate(),
            Err(e) => {
                eprintln!("butter: {e}");
                return glib::ExitCode::FAILURE;
            }
        }
        glib::ExitCode::SUCCESS
    });

    app.run()
}

fn show_about(app: &adw::Application) {
    let about = adw::AboutDialog::builder()
        .application_name(config::APP_NAME)
        .application_icon(config::APP_ID)
        .developer_name("The Butter Contributors")
        .version(config::VERSION)
        .website(config::WEBSITE)
        .issue_url(format!("{}/issues", config::WEBSITE))
        .comments("Manage Rosemary runners and bottles for running Mac software")
        .license_type(gtk::License::Gpl30)
        .build();
    about.add_link(
        "Rosemary",
        &format!(
            "https://github.com/{}/{}",
            config::ROSEMARY_OWNER,
            config::ROSEMARY_REPO
        ),
    );
    about.present(app.active_window().as_ref());
}
