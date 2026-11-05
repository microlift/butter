//! Getting Started: a short tour for new users, one page per task. It shows
//! once on first launch and can be opened again from the main menu.

use std::path::Path;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::config::{self, Paths};
use crate::ui::{self, window::Window};
use crate::{bottle, runner};

/// Show the tour on first launch: when it has never been shown and there are
/// no bottles yet, so people upgrading from an earlier Butter aren't shown it.
pub fn first_run(window: &Window) {
    if window.settings().tutorial_seen {
        return;
    }
    window.update_settings(|s| s.tutorial_seen = true);
    if bottle::list().is_empty() {
        present(window);
    }
}

/// Open the tour.
pub fn present(window: &Window) -> adw::Dialog {
    let dialog = adw::Dialog::builder()
        .title("Getting Started")
        .content_width(600)
        .content_height(680)
        .build();

    let carousel = adw::Carousel::builder()
        .vexpand(true)
        .allow_scroll_wheel(false)
        .build();
    carousel.append(&welcome_page());
    carousel.append(&runners_page());
    carousel.append(&bottle_page());
    carousel.append(&terminal_page());
    carousel.append(&logs_page());
    carousel.append(&shortcut_page());
    carousel.append(&done_page(window, &dialog));

    let back = gtk::Button::with_mnemonic("_Back");
    let next = gtk::Button::with_mnemonic("_Next");
    next.add_css_class("suggested-action");
    let dots = adw::CarouselIndicatorDots::builder()
        .carousel(&carousel)
        .build();
    let bar = gtk::CenterBox::builder()
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .start_widget(&back)
        .center_widget(&dots)
        .end_widget(&next)
        .build();

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&carousel));
    toolbar.add_bottom_bar(&bar);
    dialog.set_child(Some(&toolbar));

    // Keep the buttons in step with the page, however it was reached.
    let update = Rc::new({
        let (back, next) = (back.clone(), next.clone());
        move |index: u32, pages: u32| {
            back.set_visible(index > 0);
            next.set_label(if index + 1 >= pages { "_Done" } else { "_Next" });
        }
    });
    update(0, carousel.n_pages());
    carousel.connect_page_changed({
        let update = update.clone();
        move |carousel, index| update(index, carousel.n_pages())
    });

    back.connect_clicked({
        let carousel = carousel.clone();
        move |_| {
            let index = carousel.position().round() as u32;
            if index > 0 {
                carousel.scroll_to(&carousel.nth_page(index - 1), true);
            }
        }
    });
    next.connect_clicked({
        let (carousel, dialog) = (carousel.clone(), dialog.clone());
        move |_| {
            let index = carousel.position().round() as u32;
            if index + 1 < carousel.n_pages() {
                carousel.scroll_to(&carousel.nth_page(index + 1), true);
            } else {
                dialog.close();
            }
        }
    });

    dialog.present(Some(&window.widget()));
    dialog
}

/// A page: an icon, a title, a sentence of explanation and some lists,
/// scrolling when the window is short. Smaller than an `AdwStatusPage`, so
/// the steps start above the fold.
fn page(
    icon: &str,
    title: &str,
    description: &str,
    groups: &[adw::PreferencesGroup],
) -> gtk::ScrolledWindow {
    let image = gtk::Image::builder()
        .icon_name(icon)
        .pixel_size(64)
        .margin_bottom(6)
        .build();
    image.add_css_class("dim-label");
    let heading = gtk::Label::builder()
        .label(title)
        .wrap(true)
        .justify(gtk::Justification::Center)
        .build();
    heading.add_css_class("title-1");
    let text = gtk::Label::builder()
        .label(description)
        .use_markup(true)
        .wrap(true)
        .justify(gtk::Justification::Center)
        .build();

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(12)
        .margin_end(12)
        .build();
    content.append(&image);
    content.append(&heading);
    content.append(&text);
    for group in groups {
        group.set_margin_top(12);
        content.append(group);
    }
    let clamp = adw::Clamp::builder()
        .maximum_size(480)
        .child(&content)
        .build();
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&clamp)
        .hexpand(true)
        .vexpand(true)
        .build()
}

/// A group of rows, each beside the icon of the button or thing it talks
/// about. Row text is Pango markup.
fn group(title: &str, description: &str, rows: &[(&str, &str)]) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title(title).build();
    if !description.is_empty() {
        group.set_description(Some(description));
    }
    let list = ui::boxed_list();
    for (icon, text) in rows {
        let row = adw::ActionRow::builder()
            .title(*text)
            .focusable(false)
            .build();
        row.add_prefix(&gtk::Image::from_icon_name(icon));
        list.append(&row);
    }
    group.add(&list);
    group
}

/// Numbered steps, done in order.
fn steps(title: &str, description: &str, rows: &[&str]) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title(title).build();
    if !description.is_empty() {
        group.set_description(Some(description));
    }
    let list = ui::boxed_list();
    for (i, text) in rows.iter().enumerate() {
        let row = adw::ActionRow::builder()
            .title(*text)
            .focusable(false)
            .build();
        let number = gtk::Label::builder()
            .label((i + 1).to_string())
            .width_chars(2)
            .build();
        number.add_css_class("numeric");
        number.add_css_class("heading");
        number.add_css_class("accent");
        row.add_prefix(&number);
        list.append(&row);
    }
    group.add(&list);
    group
}

fn welcome_page() -> gtk::ScrolledWindow {
    page(
        config::APP_ID,
        "Welcome to Butter",
        "Butter runs Mac apps on Linux with Rosemary. This quick tour shows you \
         around in a minute.",
        &[group(
            "You'll Learn How To",
            "",
            &[
                ("system-run-symbolic", "Install a runner"),
                ("package-x-generic-symbolic", "Create your first bottle"),
                ("utilities-terminal-symbolic", "Open terminals"),
                ("text-x-generic-symbolic", "Find logs"),
                (
                    "view-app-grid-symbolic",
                    "Add apps to your applications menu",
                ),
            ],
        )],
    )
}

fn runners_page() -> gtk::ScrolledWindow {
    page(
        "system-run-symbolic",
        "Install a Runner",
        "A runner is a build of Rosemary, which runs the Mac apps. Every bottle \
         needs one. Open the <b>Runners</b> tab and choose one way to get it.",
        &[
            steps(
                "Download an Official Build",
                "Recommended: quick, and needs nothing beyond an OpenGL or Vulkan driver",
                &[
                    "Find the newest release under <b>Available</b>",
                    "Click <b>Install</b>",
                ],
            ),
            steps(
                "Build from Source",
                "Compiles Rosemary on this computer. Slower, but works for any release or recent commit",
                &[
                    "Under <b>Build Environment</b>, install the Rust toolchain and any \
                     libraries listed as missing",
                    "Expand a release and click <b>Build</b> beside <b>Build from Source</b>, \
                     or click <b>Build</b> on a commit under <b>Development Builds</b>",
                ],
            ),
            group(
                "Already Have Rosemary?",
                "Use the buttons beside Installed",
                &[
                    (
                        "system-search-symbolic",
                        "<b>Detect System Installs</b> adds the <tt>rosemary</tt> on your PATH",
                    ),
                    (
                        "list-add-symbolic",
                        "<b>Add a Rosemary Build…</b> adds any <tt>rosemary</tt> binary",
                    ),
                ],
            ),
        ],
    )
}

fn bottle_page() -> gtk::ScrolledWindow {
    page(
        "package-x-generic-symbolic",
        "Create Your First Bottle",
        "A bottle is a Mac system of its own, with its own apps, settings and runner.",
        &[
            steps(
                "Create It",
                "",
                &[
                    "Click <b>New Bottle</b> (<b>+</b>) at the top left, or press <b>Ctrl+N</b>",
                    "Name it and choose a runner",
                    "Keep <b>Install Darwin Base</b> on, as most apps need it",
                    "Click <b>Create</b>",
                ],
            ),
            group(
                "Run Apps",
                "",
                &[
                    (
                        "view-app-grid-symbolic",
                        "Apps installed in the bottle's <tt>/Applications</tt> appear on its \
                         page by themselves. Click one to run it",
                    ),
                    (
                        "media-playback-start-symbolic",
                        "<b>Run</b> in the bottle's header runs any other app bundle or executable",
                    ),
                ],
            ),
        ],
    )
}

fn terminal_page() -> gtk::ScrolledWindow {
    page(
        "utilities-terminal-symbolic",
        "Open Terminals",
        "Every program gets a terminal of its own, where you can read its output \
         and type into it.",
        &[
            steps(
                "See a Program's Terminal",
                "",
                &[
                    "Click <b>Process Manager</b> in a bottle's header",
                    "Choose a program from the list",
                ],
            ),
            group(
                "More",
                "",
                &[
                    (
                        "media-playback-start-symbolic",
                        "For a shell inside the bottle, choose <b>Run</b> ▸ \
                         <b>Open Rosemary Shell</b>",
                    ),
                    (
                        "window-new-symbolic",
                        "<b>Pop Out Terminal</b> moves a terminal into a window of its own",
                    ),
                    (
                        "edit-copy-symbolic",
                        "Copy and paste with <b>Ctrl+Shift+C</b> and <b>Ctrl+Shift+V</b>, \
                         or right-click",
                    ),
                    (
                        "system-shutdown-symbolic",
                        "The power button stops everything running in the bottle",
                    ),
                ],
            ),
        ],
    )
}

fn logs_page() -> gtk::ScrolledWindow {
    let folder = glib::markup_escape_text(&home_relative(&Paths::get().bottles()));
    let saved =
        format!("Every run is saved in the bottle's <tt>logs</tt> folder, under <tt>{folder}</tt>");
    page(
        "text-x-generic-symbolic",
        "Find Logs",
        "When something goes wrong, the log usually says why. Include it when you \
         report a problem.",
        &[
            group(
                "Program Logs",
                "",
                &[
                    (
                        "text-x-generic-symbolic",
                        "On a bottle's page, <b>Last Run Log</b> under <b>Prefix</b> shows \
                         the output of the program started most recently",
                    ),
                    (
                        "dialog-warning-symbolic",
                        "When a program fails, click <b>Show Output</b> on the notification \
                         to open its terminal",
                    ),
                    (
                        "edit-copy-symbolic",
                        "To copy a log, right-click a terminal and choose <b>Select All</b>, \
                         then <b>Copy</b>",
                    ),
                    ("folder-open-symbolic", &saved),
                ],
            ),
            group(
                "Install and Build Logs",
                "",
                &[(
                    "system-run-symbolic",
                    "A runner's install or build shows its log as it runs. Closed it? Click \
                     <b>Downloading…</b> or <b>Building…</b> to bring it back, or \
                     <b>Show Log</b> once it ends",
                )],
            ),
        ],
    )
}

fn shortcut_page() -> gtk::ScrolledWindow {
    page(
        "view-app-grid-symbolic",
        "Add Apps to Your Menu",
        "Start a Mac app from your desktop like any other app. It also opens the \
         files and links it supports.",
        &[
            steps(
                "Create a Shortcut",
                "",
                &[
                    "On a bottle's page, find the app under <b>Applications</b>",
                    "Click <b>More</b> (⋮) beside it and choose <b>Create Shortcut…</b>",
                    "Confirm, if your desktop asks",
                ],
            ),
            group(
                "",
                "",
                &[(
                    "view-more-symbolic",
                    "To take it out of the menu, choose <b>Remove Shortcut</b> in the same place",
                )],
            ),
        ],
    )
}

/// `path`, with the home folder written as `~`.
fn home_relative(path: &Path) -> String {
    match path.strip_prefix(glib::home_dir()) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// What the last page's button does.
type NextStep = fn(&Window);

/// The last page, with a button for the first thing left to do.
fn done_page(window: &Window, dialog: &adw::Dialog) -> adw::StatusPage {
    let page = adw::StatusPage::builder()
        .icon_name("emblem-ok-symbolic")
        .title("You're All Set")
        .description(
            "Open this tour again any time from the main menu, under \
             <b>Getting Started</b>.",
        )
        .hexpand(true)
        .vexpand(true)
        .build();

    let next: Option<(&str, NextStep)> = if runner::installed().is_empty() {
        Some(("_Install a Runner", |w| w.show_runners()))
    } else if bottle::list().is_empty() {
        Some(("_Create a Bottle", |w| ui::new_bottle::present(w)))
    } else {
        None
    };
    if let Some((label, action)) = next {
        let button = gtk::Button::builder()
            .label(label)
            .use_underline(true)
            .halign(gtk::Align::Center)
            .build();
        button.add_css_class("pill");
        button.add_css_class("suggested-action");
        let (window, dialog) = (window.clone(), dialog.clone());
        button.connect_clicked(move |_| {
            dialog.close();
            action(&window);
        });
        page.set_child(Some(&button));
    }
    page
}
