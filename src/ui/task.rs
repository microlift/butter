//! A dialog that follows a background job: its stage, progress and output.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::process::{self, Event, Reporter};
use crate::ui::window::Window;

/// Lines kept in the log view; older ones are dropped.
const MAX_LINES: i32 = 5000;

/// Run `job` in the background while a dialog shows its progress, and return
/// how it ended. Closing the dialog does not stop the job; a toast reports
/// the end and can bring the log back.
pub async fn run<F>(window: &Window, title: &str, job: F) -> Result<String, String>
where
    F: FnOnce(&Reporter) -> anyhow::Result<String> + Send + 'static,
{
    run_keyed(window, None, title, true, job).await
}

/// Like `run`, for the job the window knows as `key` (see `Window::begin`):
/// while it runs, `Window::show_log(key)` brings the dialog back. Unless
/// `show`, the dialog starts out closed.
pub async fn run_keyed<F>(
    window: &Window,
    key: Option<&str>,
    title: &str,
    show: bool,
    job: F,
) -> Result<String, String>
where
    F: FnOnce(&Reporter) -> anyhow::Result<String> + Send + 'static,
{
    run_with(window, key, title, show, false, job).await
}

/// Like `run_keyed`; with `close_on_success`, the dialog closes itself once
/// the job has succeeded.
///
/// While the window is hidden, as when Butter was started by a shortcut,
/// the dialog is a window of its own. Closing it then lets Butter quit, if
/// nothing it started is running.
pub async fn run_with<F>(
    window: &Window,
    key: Option<&str>,
    title: &str,
    show: bool,
    close_on_success: bool,
    job: F,
) -> Result<String, String>
where
    F: FnOnce(&Reporter) -> anyhow::Result<String> + Send + 'static,
{
    let (receiver, canceller) = process::spawn(job);
    let floating = !window.widget().is_visible();

    let stage = gtk::Label::builder()
        .label("Starting…")
        .xalign(0.0)
        .wrap(true)
        .build();
    stage.add_css_class("heading");
    let progress = gtk::ProgressBar::new();
    let buffer = gtk::TextBuffer::new(None);
    let view = gtk::TextView::builder()
        .buffer(&buffer)
        .editable(false)
        .cursor_visible(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(8)
        .bottom_margin(8)
        .left_margin(8)
        .right_margin(8)
        .build();
    let end = buffer.create_mark(None, &buffer.end_iter(), false);
    let scrolled = gtk::ScrolledWindow::builder()
        .child(&view)
        .vexpand(true)
        .build();
    let frame = gtk::Frame::builder().child(&scrolled).build();
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    content.append(&stage);
    content.append(&progress);
    content.append(&frame);

    let button = gtk::Button::with_mnemonic("_Cancel");
    button.add_css_class("pill");
    button.set_halign(gtk::Align::Center);
    content.append(&button);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&content));
    let dialog = adw::Dialog::builder()
        .title(title)
        .content_width(680)
        .content_height(520)
        .child(&toolbar)
        .build();

    let finished = Rc::new(Cell::new(false));
    button.connect_clicked({
        let dialog = dialog.clone();
        let finished = finished.clone();
        move |button| {
            if finished.get() {
                dialog.close();
            } else {
                canceller.cancel();
                button.set_sensitive(false);
            }
        }
    });
    if let Some(key) = key {
        window.set_log(key, &dialog);
    }
    if floating {
        let window = window.clone();
        dialog.connect_closed(move |_| window.close_if_unused());
    }
    if show {
        if floating {
            dialog.present(None::<&gtk::Widget>);
        } else {
            dialog.present(Some(&window.widget()));
        }
    }

    // Pulse while nobody knows how far along the job is.
    let determinate = Rc::new(Cell::new(false));
    let pulse = glib::timeout_add_local(std::time::Duration::from_millis(150), {
        let progress = progress.clone();
        let determinate = determinate.clone();
        move || {
            if !determinate.get() {
                progress.pulse();
            }
            glib::ControlFlow::Continue
        }
    });

    let mut result = Err("the job ended without a result".to_string());
    while let Ok(event) = receiver.recv().await {
        match event {
            Event::Stage { title, fraction } => {
                stage.set_label(&title);
                determinate.set(false);
                if let Some(fraction) = fraction {
                    progress.set_fraction(fraction);
                }
            }
            Event::Progress(fraction) => {
                determinate.set(true);
                progress.set_fraction(fraction);
            }
            Event::Line(line) => {
                let mut iter = buffer.end_iter();
                buffer.insert(&mut iter, &line);
                buffer.insert(&mut iter, "\n");
                if buffer.line_count() > MAX_LINES {
                    let mut start = buffer.start_iter();
                    let mut cut = buffer
                        .iter_at_line(buffer.line_count() - MAX_LINES)
                        .unwrap_or(start);
                    buffer.delete(&mut start, &mut cut);
                }
                view.scroll_mark_onscreen(&end);
            }
            Event::Finished(r) => {
                result = r;
                break;
            }
        }
    }
    pulse.remove();
    finished.set(true);
    determinate.set(true);
    button.set_sensitive(true);
    button.set_label("_Close");
    match &result {
        Ok(_) if close_on_success && dialog.parent().is_some() => {
            dialog.close();
            return result;
        }
        Ok(message) => {
            stage.set_label(message);
            progress.set_fraction(1.0);
            button.add_css_class("suggested-action");
        }
        Err(error) => {
            stage.set_label(&format!("Failed: {error}"));
            stage.add_css_class("error");
            progress.set_fraction(0.0);
        }
    }

    // The user closed the dialog while the job ran: say how it went.
    if dialog.parent().is_none() {
        let text = match &result {
            Ok(message) => message.clone(),
            Err(_) => format!("{title} failed"),
        };
        let toast = adw::Toast::builder()
            .title(text)
            .button_label("Show Log")
            .build();
        let parent = window.widget();
        toast.connect_button_clicked(move |_| dialog.present(Some(&parent)));
        window.toast(toast);
    }
    result
}
