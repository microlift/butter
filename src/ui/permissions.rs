//! Onboarding and the dialogs that ask for optional sandbox permissions; see
//! `crate::permissions`.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::config;
use crate::permissions::{self, Feature, Plan, Sandbox, Session, State, Status};
use crate::ui::{self, window::Window};

/// Whether Butter is drawing through X11 rather than Wayland.
fn session() -> Session {
    let x11 = gtk::gdk::Display::default().is_some_and(|d| d.backend().is_x11());
    Session { x11 }
}

/// Every feature worth showing on this display.
pub fn relevant() -> Vec<Feature> {
    let session = session();
    Feature::ALL
        .into_iter()
        .filter(|f| f.relevant(session))
        .collect()
}

fn save(window: &Window, state: &State) {
    if let Err(e) = state.save() {
        window.error(format!("Could not save permission choices: {e}"));
    }
}

/// Wait for `dialog` to close.
async fn closed(dialog: &impl IsA<adw::Dialog>) {
    let (tx, rx) = async_channel::bounded(1);
    dialog.connect_closed(move |_| {
        let _ = tx.try_send(());
    });
    let _ = rx.recv().await;
}

/// At launch, compare the sandbox with the user's earlier answers: say once
/// when permissions were taken away, remind them of commands they did not
/// run, and walk them through steps they have not seen.
/// Finishes when the user has closed every dialog it showed.
pub async fn review(window: &Window) {
    let Some(sandbox) = Sandbox::current() else {
        return;
    };
    let mut state = State::load();
    let first_run = state.steps.is_empty();
    let review = state.review(&sandbox.context, session());
    save(window, &state);

    if !review.removed.is_empty() {
        removed(window, &review.removed).await;
    }
    if !review.forgotten.is_empty() {
        forgotten(window, &review.forgotten).await;
    }
    if !review.ask.is_empty() {
        onboarding(window, &review.ask, first_run).await;
    }
}

/// Shell code in a card, with comments dimmed, above a button that copies
/// all of it.
fn command_block(script: &str) -> gtk::Box {
    let view = gtk::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::None)
        .top_margin(12)
        .bottom_margin(12)
        .left_margin(12)
        .right_margin(12)
        .build();
    let buffer = view.buffer();
    let comment = gtk::TextTag::builder()
        .name("comment")
        .foreground("#8a8a8a")
        .style(gtk::pango::Style::Italic)
        .build();
    buffer.tag_table().add(&comment);
    let mut end = buffer.end_iter();
    for line in script.split_inclusive('\n') {
        if line.trim_start().starts_with('#') {
            buffer.insert_with_tags(&mut end, line, &[&comment]);
        } else {
            buffer.insert(&mut end, line);
        }
    }

    let scrolled = gtk::ScrolledWindow::builder()
        .child(&view)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let frame = gtk::Frame::builder().child(&scrolled).build();
    frame.add_css_class("view");

    let content = adw::ButtonContent::builder()
        .icon_name("edit-copy-symbolic")
        .label("_Copy to Clipboard")
        .use_underline(true)
        .build();
    let copy = gtk::Button::builder()
        .child(&content)
        .halign(gtk::Align::Center)
        .build();
    copy.add_css_class("pill");
    copy.add_css_class("suggested-action");
    let text = script.to_string();
    copy.connect_clicked(move |button| {
        button.clipboard().set_text(&text);
        let content = content.clone();
        content.set_icon_name("object-select-symbolic");
        content.set_label("Copied");
        glib::spawn_future_local(async move {
            glib::timeout_future_seconds(2).await;
            content.set_icon_name("edit-copy-symbolic");
            content.set_label("_Copy to Clipboard");
        });
    });

    let block = gtk::Box::new(gtk::Orientation::Vertical, 18);
    block.append(&frame);
    block.append(&copy);
    block
}

/// A heading, a paragraph and `command_block`.
fn command_page(heading: &str, body: &str, script: &str) -> gtk::Box {
    let title = gtk::Label::builder()
        .label(heading)
        .wrap(true)
        .justify(gtk::Justification::Center)
        .build();
    title.add_css_class("title-1");
    let text = gtk::Label::builder()
        .label(body)
        .wrap(true)
        .use_markup(true)
        .justify(gtk::Justification::Center)
        .build();
    let page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(12)
        .margin_end(12)
        .build();
    page.append(&title);
    page.append(&text);
    page.append(&command_block(script));
    page
}

fn scrolled_page(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    let clamp = adw::Clamp::builder().maximum_size(560).child(child).build();
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&clamp)
        .vexpand(true)
        .build()
}

/// A button for `command_dialog`: its label, whether it is the suggested
/// action, and what it does before the dialog closes.
type Action<'a> = (&'a str, bool, Box<dyn Fn()>);

/// A dialog showing a command, with `buttons` along the bottom. Each button
/// closes the dialog after running its action.
fn command_dialog(
    window: &Window,
    heading: &str,
    body: &str,
    script: &str,
    buttons: Vec<Action>,
) -> adw::Dialog {
    let dialog = adw::Dialog::builder()
        .content_width(600)
        .content_height(560)
        .build();
    let bar = gtk::Box::builder()
        .spacing(12)
        .halign(gtk::Align::End)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    for (label, suggested, action) in buttons {
        let button = gtk::Button::with_mnemonic(label);
        if suggested {
            button.add_css_class("suggested-action");
        }
        let dialog = dialog.clone();
        button.connect_clicked(move |_| {
            action();
            dialog.close();
        });
        bar.append(&button);
    }
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&scrolled_page(&command_page(heading, body, script))));
    toolbar.add_bottom_bar(&bar);
    dialog.set_child(Some(&toolbar));
    dialog.present(Some(&window.widget()));
    dialog
}

fn app_id() -> &'static str {
    Sandbox::current().map_or(config::APP_ID, |s| s.app_id.as_str())
}

/// "A", "A and B", "A, B and C".
fn names(features: &[Feature]) -> String {
    let titles: Vec<&str> = features.iter().map(|f| f.title()).collect();
    match titles.split_last() {
        Some((last, [])) => last.to_string(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
        None => String::new(),
    }
}

const RESTART: &str = "Paste this into a terminal, then restart Butter.";

/// "Forgetting something?": the user wanted features at their last launch,
/// but the permissions never arrived.
async fn forgotten(window: &Window, features: &[Feature]) {
    let script = permissions::script(app_id(), features, &[]);
    let body = format!(
        "You chose {}, but Butter still doesn't have the permissions {}. \
         Did the command run? {RESTART}",
        glib::markup_escape_text(&names(features)),
        if features.len() == 1 {
            "it needs"
        } else {
            "they need"
        },
    );
    let stop = {
        let window = window.clone();
        let features = features.to_vec();
        move || {
            let Some(sandbox) = Sandbox::current() else {
                return;
            };
            let mut state = State::load();
            for &feature in &features {
                state.answer(feature, false, &sandbox.context);
            }
            save(&window, &state);
        }
    };
    let dialog = command_dialog(
        window,
        "Forgetting Something?",
        &body,
        &script,
        vec![
            ("_Don't Use These Features", false, Box::new(stop)),
            ("_Remind Me Later", true, Box::new(|| {})),
        ],
    );
    closed(&dialog).await;
}

/// Said once, when permissions a feature had are gone.
async fn removed(window: &Window, features: &[Feature]) {
    let list: String = features
        .iter()
        .map(|f| format!("\n• {}", f.title()))
        .collect();
    let dialog = adw::AlertDialog::new(
        Some("Permissions Removed"),
        Some(&format!(
            "Butter no longer has the permissions these features need, so they \
             won't work:\n{list}\n\nYou can allow them again in Preferences."
        )),
    );
    dialog.add_responses(&[("allow", "_Allow Again…"), ("ok", "_OK")]);
    dialog.set_default_response(Some("ok"));
    dialog.set_close_response("ok");
    if dialog.choose_future(Some(&window.widget())).await == "allow" {
        let dialog = request_all(window, features);
        closed(&dialog).await;
    }
}

/// Show how to grant `feature`, as when it is switched on without the
/// permissions it needs. Records that the user wants it.
pub fn request(window: &Window, feature: Feature) {
    request_all(window, &[feature]);
}

fn request_all(window: &Window, features: &[Feature]) -> adw::Dialog {
    if let Some(sandbox) = Sandbox::current() {
        let mut state = State::load();
        for &feature in features {
            state.answer(feature, true, &sandbox.context);
        }
        save(window, &state);
    }
    let needs: String = features
        .iter()
        .flat_map(|f| f.needs())
        .map(|n| format!("\n• {}", glib::markup_escape_text(n)))
        .collect();
    let heading = match features {
        [feature] => format!("Allow {}?", feature.title()),
        _ => "Allow These Features?".into(),
    };
    command_dialog(
        window,
        &heading,
        &format!(
            "{} need{} more access than Butter's sandbox gives it:\n{needs}\n\n{RESTART}",
            glib::markup_escape_text(&names(features)),
            if features.len() == 1 { "s" } else { "" },
        ),
        &permissions::script(app_id(), features, &[]),
        vec![("_Done", true, Box::new(|| {}))],
    )
}

/// Show how to take `feature`'s permissions away again.
fn revoke(window: &Window, feature: Feature) {
    let Some(sandbox) = Sandbox::current() else {
        return;
    };
    let mut state = State::load();
    state.answer(feature, false, &sandbox.context);
    save(window, &state);
    let plan = Plan::new(&state, &sandbox.context, &[(feature, false)]);
    command_dialog(
        window,
        &format!("Remove {}?", feature.title()),
        &format!(
            "{RESTART} {} will stop working.",
            glib::markup_escape_text(feature.title())
        ),
        &permissions::script(app_id(), &[], &plan.revoke),
        vec![("_Done", true, Box::new(|| {}))],
    );
}

fn intro_page(first_run: bool) -> adw::StatusPage {
    let (title, description) = if first_run {
        (
            "Welcome to Butter",
            "Butter runs in a sandbox and asks for as little access to your computer \
             as it can. A few optional features need more. Choose the ones you want, \
             and Butter will show you how to allow exactly what they need.",
        )
    } else {
        (
            "Optional Features",
            "Some optional features need permissions Butter doesn't have, or need \
             different ones than before. Choose the ones you want.",
        )
    };
    adw::StatusPage::builder()
        .icon_name(config::APP_ID)
        .title(title)
        .description(description)
        .build()
}

fn step_page(feature: Feature, active: bool, granted: bool) -> (adw::StatusPage, adw::SwitchRow) {
    let switch = adw::SwitchRow::builder()
        .title(format!("Use {}", feature.title()))
        .active(active)
        .build();
    if granted {
        switch.set_subtitle("Already allowed");
    }
    let choice = ui::boxed_list();
    choice.append(&switch);

    let needs = adw::PreferencesGroup::builder()
        .title("Permissions Needed")
        .build();
    for need in feature.needs() {
        let row = adw::ActionRow::builder().title(*need).build();
        row.add_prefix(&gtk::Image::from_icon_name("channel-secure-symbolic"));
        needs.add(&row);
    }

    let content = gtk::Box::new(gtk::Orientation::Vertical, 24);
    content.append(&choice);
    content.append(&needs);
    let clamp = adw::Clamp::builder()
        .maximum_size(440)
        .child(&content)
        .build();
    let page = adw::StatusPage::builder()
        .icon_name(feature.icon())
        .title(feature.title())
        .description(feature.description())
        .child(&clamp)
        .build();
    (page, switch)
}

/// Walk the user through `features` one page each, then show the command
/// that grants what they chose. Their answers are recorded when they finish;
/// closing the dialog early leaves the steps to ask again next launch.
pub async fn onboarding(window: &Window, features: &[Feature], first_run: bool) {
    let Some(sandbox) = Sandbox::current() else {
        return;
    };
    let state = State::load();

    let stack = gtk::Stack::builder()
        .transition_duration(250)
        .vexpand(true)
        .build();
    let mut pages = vec!["intro".to_string()];
    stack.add_named(&intro_page(first_run), Some("intro"));
    let mut switches = Vec::new();
    for &feature in features {
        let granted = sandbox.context.has_feature(feature);
        let (page, switch) = step_page(feature, granted || state.wanted(feature), granted);
        stack.add_named(&page, Some(feature.id()));
        pages.push(feature.id().into());
        switches.push((feature, switch));
    }
    let finish = adw::Bin::new();
    stack.add_named(&finish, Some("finish"));
    pages.push("finish".into());

    let answers = Rc::new(move || {
        switches
            .iter()
            .map(|(f, s)| (*f, s.is_active()))
            .collect::<Vec<_>>()
    });

    let back = gtk::Button::with_mnemonic("_Back");
    let next = gtk::Button::with_mnemonic("_Next");
    next.add_css_class("suggested-action");
    let bar = gtk::CenterBox::builder()
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .start_widget(&back)
        .end_widget(&next)
        .build();

    let dialog = adw::Dialog::builder()
        .title(if first_run {
            "Welcome"
        } else {
            "Optional Features"
        })
        .content_width(600)
        .content_height(640)
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&stack));
    toolbar.add_bottom_bar(&bar);
    dialog.set_child(Some(&toolbar));

    let index = Rc::new(Cell::new(0usize));
    let show = Rc::new({
        let (stack, back, next, finish, answers) = (
            stack.clone(),
            back.clone(),
            next.clone(),
            finish.clone(),
            answers.clone(),
        );
        let index = index.clone();
        move |forward: bool| {
            let i = index.get();
            let last = i + 1 == pages.len();
            if last {
                let plan = Plan::new(&State::load(), &sandbox.context, &answers());
                let page: gtk::Widget = if plan.is_empty() {
                    adw::StatusPage::builder()
                        .icon_name("emblem-ok-symbolic")
                        .title("You're All Set")
                        .description(
                            "Nothing more to allow. You can change these later in Preferences.",
                        )
                        .build()
                        .upcast()
                } else {
                    scrolled_page(&command_page(
                        "Allow the Permissions",
                        &format!(
                            "{RESTART} The comments explain each command. \
                             You can change these later in Preferences."
                        ),
                        &permissions::script(&sandbox.app_id, &plan.grant, &plan.revoke),
                    ))
                    .upcast()
                };
                finish.set_child(Some(&page));
            }
            stack.set_visible_child_full(
                &pages[i],
                if forward {
                    gtk::StackTransitionType::SlideLeft
                } else {
                    gtk::StackTransitionType::SlideRight
                },
            );
            back.set_visible(i > 0);
            next.set_label(if last { "_Done" } else { "_Next" });
        }
    });
    show(true);

    back.connect_clicked({
        let (index, show) = (index.clone(), show.clone());
        move |_| {
            index.set(index.get().saturating_sub(1));
            show(false);
        }
    });
    next.connect_clicked({
        let (window, dialog) = (window.clone(), dialog.clone());
        let last = features.len() + 1;
        move |_| {
            if index.get() < last {
                index.set(index.get() + 1);
                show(true);
                return;
            }
            let mut state = State::load();
            for (feature, wanted) in answers() {
                state.answer(feature, wanted, &sandbox.context);
            }
            save(&window, &state);
            dialog.close();
        }
    });

    dialog.present(Some(&window.widget()));
    closed(&dialog).await;
}

/// The Preferences group listing optional features, or `None` outside
/// Flatpak, where Butter can reach everything it needs already.
pub fn preferences_group(window: &Window) -> Option<adw::PreferencesGroup> {
    let sandbox = Sandbox::current()?;
    let state = State::load();
    let group = adw::PreferencesGroup::builder()
        .title("Sandbox Permissions")
        .description("Optional features that need more access to this computer. Changes take effect when Butter restarts")
        .build();
    for feature in relevant() {
        let granted = sandbox.context.has_feature(feature);
        let step = state.steps.get(feature.id());
        let subtitle = match step.map(|s| (s.wanted, s.status)) {
            _ if granted => "Allowed",
            Some((true, Status::Pending)) => "Waiting for permission",
            Some((true, Status::Removed)) => "Permission removed",
            _ => "Not allowed",
        };
        let row = adw::ActionRow::builder()
            .title(feature.title())
            .subtitle(subtitle)
            .build();
        let button = gtk::Button::builder()
            .label(if granted { "Remove…" } else { "Allow…" })
            .valign(gtk::Align::Center)
            .build();
        let window = window.clone();
        button.connect_clicked(move |_| {
            if granted {
                revoke(&window, feature);
            } else {
                request(&window, feature);
            }
        });
        row.add_suffix(&button);
        group.add(&row);
    }
    let again = adw::ActionRow::builder()
        .title("Optional Features")
        .subtitle("Go through the choices from Butter's first launch again")
        .build();
    let review = gtk::Button::builder()
        .label("Review…")
        .valign(gtk::Align::Center)
        .build();
    let window = window.clone();
    review.connect_clicked(move |_| {
        let window = window.clone();
        glib::spawn_future_local(async move {
            onboarding(&window, &relevant(), false).await;
        });
    });
    again.add_suffix(&review);
    group.add(&again);
    Some(group)
}
