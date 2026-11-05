//! The interface. Widgets are built in code; each page owns its widgets and
//! rebuilds its lists from disk when told something changed.

pub mod bottle_page;
pub mod bottles_page;
pub mod env_dialog;
pub mod new_bottle;
pub mod permissions;
pub mod preferences;
pub mod process_manager;
pub mod program_dialog;
pub mod runners_page;
pub mod task;
pub mod tutorial;
pub mod window;

use adw::prelude::*;

/// A `boxed-list` list box, the container rows sit in.
pub fn boxed_list() -> gtk::ListBox {
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    list.add_css_class("boxed-list");
    list
}

/// A flat icon button with a tooltip.
pub fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .valign(gtk::Align::Center)
        .build();
    button.add_css_class("flat");
    button
}

/// A row with a single dimmed line, for empty lists.
pub fn placeholder_row(text: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(text).build();
    row.add_css_class("dim-label");
    row
}

/// A combo row listing `labels`, starting at `selected`.
pub fn combo_row(
    title: &str,
    subtitle: Option<&str>,
    labels: &[&str],
    selected: u32,
) -> adw::ComboRow {
    let row = adw::ComboRow::builder()
        .title(title)
        .model(&gtk::StringList::new(labels))
        .selected(selected)
        .build();
    if let Some(subtitle) = subtitle {
        row.set_subtitle(subtitle);
    }
    row
}

/// Ask a yes-or-no question; true when the user chose `confirm`.
pub async fn confirm(
    parent: &impl IsA<gtk::Widget>,
    heading: &str,
    body: &str,
    confirm: &str,
    destructive: bool,
) -> bool {
    let dialog = adw::AlertDialog::new(Some(heading), Some(body));
    dialog.add_responses(&[("cancel", "_Cancel"), ("confirm", confirm)]);
    dialog.set_response_appearance(
        "confirm",
        if destructive {
            adw::ResponseAppearance::Destructive
        } else {
            adw::ResponseAppearance::Suggested
        },
    );
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.choose_future(Some(parent)).await == "confirm"
}

/// Show `text` in a scrollable, selectable monospace dialog.
pub fn show_text(parent: &impl IsA<gtk::Widget>, title: &str, text: &str) {
    let view = gtk::TextView::builder()
        .editable(false)
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
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&scrolled));
    let dialog = adw::Dialog::builder()
        .title(title)
        .content_width(640)
        .content_height(480)
        .child(&toolbar)
        .build();
    dialog.present(Some(parent));
}

/// Open a folder in the file manager.
pub fn open_folder(parent: &impl IsA<gtk::Window>, path: &std::path::Path) {
    let launcher = gtk::FileLauncher::new(Some(&gtk::gio::File::for_path(path)));
    launcher.launch(Some(parent), gtk::gio::Cancellable::NONE, |_| {});
}
