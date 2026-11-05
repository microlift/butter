//! The main window: a Bottles tab and a Runners tab, with bottle details
//! pushed on a navigation view.

use std::cell::{OnceCell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::bottle::{self, Backend, Bottle};
use crate::config;
use crate::sessions::{Session, Sessions, Status};
use crate::settings::Settings;
use crate::shortcuts::Launch;
use crate::ui::bottle_page::BottlePage;
use crate::ui::bottles_page::BottlesPage;
use crate::ui::process_manager::{ProcessManager, WeakProcessManager};
use crate::ui::runners_page::RunnersPage;

#[derive(Clone)]
pub struct Window(Rc<Inner>);

thread_local! {
    /// Butter's one main window, once made.
    static CURRENT: RefCell<Option<Window>> = const { RefCell::new(None) };
}

pub struct Inner {
    window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    nav: adw::NavigationView,
    stack: adw::ViewStack,
    settings: RefCell<Settings>,
    /// Jobs running now, by key, so the same one is not started twice.
    busy: RefCell<HashSet<String>>,
    /// The progress dialogs of jobs running now, by the same keys, so a
    /// dialog the user closed can be brought back.
    logs: RefCell<HashMap<String, adw::Dialog>>,
    bottles: OnceCell<BottlesPage>,
    runners: OnceCell<RunnersPage>,
    /// Programs running in bottles.
    sessions: Sessions,
    /// Pages on the navigation stack. Their widgets hold only weak
    /// references back, so this keeps them alive until they are popped.
    open: RefCell<Vec<(adw::NavigationPage, Box<dyn std::any::Any>)>>,
    /// Process managers by bottle. A popped out terminal keeps its manager
    /// alive after its page is popped, and opening it again reuses it.
    managers: RefCell<HashMap<String, WeakProcessManager>>,
}

/// The shell [`Window::open_shell`] starts, a path in the bottle.
const SHELL: &str = "/bin/bash";

impl Window {
    pub fn new(app: &adw::Application) -> Window {
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title(config::APP_NAME)
            .default_width(900)
            .default_height(680)
            .width_request(360)
            .height_request(400)
            .build();
        if config::APP_ID.ends_with("Devel") {
            window.add_css_class("devel");
        }

        let stack = adw::ViewStack::new();
        let switcher = adw::ViewSwitcher::builder()
            .stack(&stack)
            .policy(adw::ViewSwitcherPolicy::Wide)
            .build();
        let switcher_bar = adw::ViewSwitcherBar::builder().stack(&stack).build();

        let header = adw::HeaderBar::builder().title_widget(&switcher).build();
        let new_button = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text("New Bottle")
            .action_name("win.new-bottle")
            .build();
        header.pack_start(&new_button);
        let menu = gio::Menu::new();
        menu.append(Some("_Preferences"), Some("win.preferences"));
        menu.append(Some("_Keyboard Shortcuts"), Some("win.show-help-overlay"));
        menu.append(Some("_Getting Started"), Some("win.tutorial"));
        menu.append(Some("_About Butter"), Some("app.about"));
        header.pack_end(
            &gtk::MenuButton::builder()
                .icon_name("open-menu-symbolic")
                .menu_model(&menu)
                .primary(true)
                .tooltip_text("Main Menu")
                .build(),
        );

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&stack));
        toolbar.add_bottom_bar(&switcher_bar);

        // Narrow windows move the switcher to the bottom.
        let breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            550.0,
            adw::LengthUnit::Sp,
        ));
        breakpoint.add_setter(&switcher_bar, "reveal", Some(&true.to_value()));
        breakpoint.add_setter(
            &header,
            "title-widget",
            Some(&None::<gtk::Widget>.to_value()),
        );
        window.add_breakpoint(breakpoint);

        let root = adw::NavigationPage::builder()
            .title(config::APP_NAME)
            .tag("main")
            .child(&toolbar)
            .build();
        let nav = adw::NavigationView::new();
        nav.add(&root);
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&nav));
        window.set_content(Some(&toasts));

        let this = Window(Rc::new(Inner {
            window,
            toasts,
            nav,
            stack,
            settings: RefCell::new(Settings::load()),
            busy: RefCell::default(),
            logs: RefCell::default(),
            bottles: OnceCell::new(),
            runners: OnceCell::new(),
            sessions: Sessions::new(),
            open: RefCell::default(),
            managers: RefCell::default(),
        }));
        let weak = Rc::downgrade(&this.0);
        this.0.nav.connect_popped(move |_, popped| {
            if let Some(inner) = weak.upgrade() {
                inner.open.borrow_mut().retain(|(page, _)| page != popped);
            }
        });

        let bottles = BottlesPage::new(&this);
        this.0.stack.add_titled_with_icon(
            bottles.widget(),
            Some("bottles"),
            "Bottles",
            "package-x-generic-symbolic",
        );
        let runners = RunnersPage::new(&this);
        this.0.stack.add_titled_with_icon(
            runners.widget(),
            Some("runners"),
            "Runners",
            "system-run-symbolic",
        );
        let _ = this.0.bottles.set(bottles);
        let _ = this.0.runners.set(runners);

        this.setup_actions();
        this.refresh();
        CURRENT.with(|current| *current.borrow_mut() = Some(this.clone()));
        this.0.window.connect_destroy(|_| {
            CURRENT.with(|current| current.borrow_mut().take());
        });
        this
    }

    /// The main window, if it has been made.
    pub fn current() -> Option<Window> {
        CURRENT.with(|current| current.borrow().clone())
    }

    /// Run what a shortcut asked for. A window that was never shown stays
    /// hidden unless something goes wrong, and closes once the programs it
    /// started have all ended.
    pub fn run_shortcut(&self, launch: &Launch) {
        let Some(bottle) = bottle::load(&launch.bottle) else {
            self.present();
            self.error(format!("There is no bottle named {}", launch.bottle));
            return;
        };
        let name = bottle
            .config
            .programs
            .iter()
            .find(|p| p.path == launch.target)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| {
                if crate::apps::is_bundle(&launch.target) {
                    crate::apps::read(&launch.target).name
                } else {
                    bottle::program_name(&launch.target)
                }
            });
        let args = launch.arguments(&bottle);
        self.launch(&bottle, &launch.target, &args, launch.backend, &name);
    }

    /// Start `target` in `bottle` and report how it ends.
    pub fn launch(
        &self,
        bottle: &Bottle,
        target: &std::path::Path,
        args: &[String],
        backend: Backend,
        name: &str,
    ) {
        if let Some(session) = self.start(bottle, target, args, backend, name) {
            let toast = adw::Toast::builder()
                .title(glib::markup_escape_text(&format!("Starting {name}…")))
                .button_label("Show")
                .build();
            let (window, bottle, id) = (self.clone(), bottle.clone(), session.id);
            toast.connect_button_clicked(move |_| {
                window.show_processes(&bottle, Some(id));
            });
            self.toast(toast);
        }
    }

    /// Start a shell in `bottle`, `rosemary run /bin/bash`, and show its
    /// terminal in the process manager.
    pub fn open_shell(&self, bottle: &Bottle) {
        let shell = std::path::Path::new(SHELL);
        if let Some(session) = self.start(bottle, shell, &[], Backend::Auto, "Shell") {
            self.show_processes(bottle, Some(session.id));
        }
    }

    /// Start `target` in `bottle`, reporting failure and how it ends.
    fn start(
        &self,
        bottle: &Bottle,
        target: &std::path::Path,
        args: &[String],
        backend: Backend,
        name: &str,
    ) -> Option<Rc<Session>> {
        let on_end = {
            let window = self.clone();
            let bottle = bottle.clone();
            move |session: &Session| {
                let status = session.status();
                if matches!(&status, Status::Exited(s) if s.success()) {
                    window.close_if_unused();
                    return;
                }
                window.present();
                let toast = adw::Toast::builder()
                    .title(glib::markup_escape_text(&format!(
                        "{}: {}",
                        session.name,
                        status.describe()
                    )))
                    .button_label("Show Output")
                    .timeout(10)
                    .build();
                let (window2, id) = (window.clone(), session.id);
                toast.connect_button_clicked(move |_| {
                    window2.show_processes(&bottle, Some(id));
                });
                window.toast(toast);
            }
        };
        match self
            .sessions()
            .launch(bottle, target, args, backend, name, on_end)
        {
            Ok(session) => Some(session),
            Err(e) => {
                self.present();
                self.error(format!("Could not start {name}: {e:#}"));
                None
            }
        }
    }

    /// Close a window that was never shown, once nothing it started runs.
    fn close_if_unused(&self) {
        let window = self.0.window.clone();
        let sessions = self.0.sessions.clone();
        glib::idle_add_local_once(move || {
            // Never shown, so never realized: `close` would do nothing.
            if !window.is_visible() && !sessions.any_linked_running() {
                window.destroy();
            }
        });
    }

    fn setup_actions(&self) {
        let new_bottle = gio::ActionEntry::builder("new-bottle")
            .activate(glib::clone!(
                #[weak(rename_to = inner)]
                self.0,
                move |_: &adw::ApplicationWindow, _, _| {
                    crate::ui::new_bottle::present(&Window(inner));
                }
            ))
            .build();
        let preferences = gio::ActionEntry::builder("preferences")
            .activate(glib::clone!(
                #[weak(rename_to = inner)]
                self.0,
                move |_: &adw::ApplicationWindow, _, _| {
                    crate::ui::preferences::present(&Window(inner));
                }
            ))
            .build();
        let show_runners = gio::ActionEntry::builder("show-runners")
            .activate(glib::clone!(
                #[weak(rename_to = inner)]
                self.0,
                move |_: &adw::ApplicationWindow, _, _| {
                    Window(inner).show_runners();
                }
            ))
            .build();
        let tutorial = gio::ActionEntry::builder("tutorial")
            .activate(glib::clone!(
                #[weak(rename_to = inner)]
                self.0,
                move |_: &adw::ApplicationWindow, _, _| {
                    crate::ui::tutorial::present(&Window(inner));
                }
            ))
            .build();
        self.0
            .window
            .add_action_entries([new_bottle, preferences, show_runners, tutorial]);
        self.0.window.set_help_overlay(Some(&shortcuts()));
    }

    pub fn present(&self) {
        self.0.window.present();
    }

    pub fn widget(&self) -> adw::ApplicationWindow {
        self.0.window.clone()
    }

    pub fn toast(&self, toast: adw::Toast) {
        self.0.toasts.add_toast(toast);
    }

    pub fn notify(&self, text: &str) {
        self.toast(adw::Toast::new(text));
    }

    /// Show an error briefly; the full text is in the tooltip-less toast.
    pub fn error(&self, text: impl std::fmt::Display) {
        let toast = adw::Toast::builder()
            .title(glib::markup_escape_text(&text.to_string()))
            .timeout(8)
            .build();
        self.toast(toast);
    }

    pub fn settings(&self) -> Settings {
        self.0.settings.borrow().clone()
    }

    /// Change the settings and save them.
    pub fn update_settings(&self, change: impl FnOnce(&mut Settings)) {
        let mut settings = self.0.settings.borrow_mut();
        change(&mut settings);
        if let Err(e) = settings.save() {
            drop(settings);
            self.error(format!("Could not save preferences: {e}"));
        }
    }

    /// Mark `key` busy; false if it already was.
    pub fn begin(&self, key: &str) -> bool {
        self.0.busy.borrow_mut().insert(key.to_string())
    }

    pub fn end(&self, key: &str) {
        self.0.busy.borrow_mut().remove(key);
        self.0.logs.borrow_mut().remove(key);
    }

    /// Remember the dialog following the job `key`, for `show_log`.
    pub fn set_log(&self, key: &str, dialog: &adw::Dialog) {
        self.0
            .logs
            .borrow_mut()
            .insert(key.to_string(), dialog.clone());
    }

    /// Bring back the progress dialog of the running job `key`.
    pub fn show_log(&self, key: &str) {
        let dialog = self.0.logs.borrow().get(key).cloned();
        if let Some(dialog) = dialog
            && dialog.parent().is_none()
        {
            dialog.present(Some(&self.0.window));
        }
    }

    /// Whether any job whose key starts with `prefix` is running.
    pub fn is_busy_prefix(&self, prefix: &str) -> bool {
        self.0.busy.borrow().iter().any(|k| k.starts_with(prefix))
    }

    pub fn is_busy(&self, key: &str) -> bool {
        self.0.busy.borrow().contains(key)
    }

    /// Carry on with runner builds that were cut short when Butter last quit.
    pub fn resume_builds(&self) {
        if let Some(page) = self.0.runners.get() {
            page.resume_builds();
        }
    }

    /// Re-read bottles and runners from disk everywhere.
    pub fn refresh(&self) {
        if let Some(page) = self.0.bottles.get() {
            page.refresh();
        }
        if let Some(page) = self.0.runners.get() {
            page.refresh_installed();
        }
    }

    pub fn refresh_releases(&self, force: bool) {
        if let Some(page) = self.0.runners.get() {
            page.refresh_releases(force);
        }
    }

    pub fn refresh_toolchain(&self) {
        if let Some(page) = self.0.runners.get() {
            page.refresh_toolchain();
        }
    }

    pub fn show_runners(&self) {
        self.0.nav.pop_to_tag("main");
        self.0.stack.set_visible_child_name("runners");
    }

    pub fn sessions(&self) -> &Sessions {
        &self.0.sessions
    }

    pub fn show_bottle(&self, bottle: Bottle) {
        let page = BottlePage::new(self, bottle);
        self.push(page.widget().clone(), page);
    }

    /// Open a bottle's process manager, showing `focus`'s terminal if given.
    pub fn show_processes(&self, bottle: &Bottle, focus: Option<u64>) {
        let existing = self
            .0
            .managers
            .borrow()
            .get(&bottle.id)
            .and_then(WeakProcessManager::upgrade);
        let manager = match existing {
            Some(manager) => {
                let page = manager.widget();
                let shown = self.0.open.borrow().iter().any(|(p, _)| p == page);
                if shown {
                    self.0.nav.pop_to_page(page);
                } else {
                    self.push(page.clone(), manager.clone());
                }
                manager
            }
            None => {
                let manager = ProcessManager::new(self, bottle);
                self.0
                    .managers
                    .borrow_mut()
                    .insert(bottle.id.clone(), manager.downgrade());
                self.push(manager.widget().clone(), manager.clone());
                manager
            }
        };
        if let Some(session) = focus {
            manager.focus(session);
        }
    }

    fn push(&self, page: adw::NavigationPage, keep: impl std::any::Any) {
        self.0.nav.push(&page);
        self.0.open.borrow_mut().push((page, Box::new(keep)));
    }

    /// Leave a bottle's page, as after deleting it.
    pub fn pop_to_main(&self) {
        self.0.nav.pop_to_tag("main");
    }
}

fn shortcuts() -> gtk::ShortcutsWindow {
    let builder = gtk::Builder::from_string(
        r#"<interface>
  <object class="GtkShortcutsWindow" id="help_overlay">
    <property name="modal">True</property>
    <child>
      <object class="GtkShortcutsSection">
        <child>
          <object class="GtkShortcutsGroup">
            <property name="title">General</property>
            <child><object class="GtkShortcutsShortcut">
              <property name="title">New Bottle</property>
              <property name="accelerator">&lt;Primary&gt;n</property>
            </object></child>
            <child><object class="GtkShortcutsShortcut">
              <property name="title">Preferences</property>
              <property name="accelerator">&lt;Primary&gt;comma</property>
            </object></child>
            <child><object class="GtkShortcutsShortcut">
              <property name="title">Getting Started</property>
              <property name="accelerator">F1</property>
            </object></child>
            <child><object class="GtkShortcutsShortcut">
              <property name="title">Keyboard Shortcuts</property>
              <property name="accelerator">&lt;Primary&gt;question</property>
            </object></child>
            <child><object class="GtkShortcutsShortcut">
              <property name="title">Quit</property>
              <property name="accelerator">&lt;Primary&gt;q</property>
            </object></child>
          </object>
        </child>
        <child>
          <object class="GtkShortcutsGroup">
            <property name="title">Terminal</property>
            <child><object class="GtkShortcutsShortcut">
              <property name="title">Copy</property>
              <property name="accelerator">&lt;Primary&gt;&lt;Shift&gt;c</property>
            </object></child>
            <child><object class="GtkShortcutsShortcut">
              <property name="title">Paste</property>
              <property name="accelerator">&lt;Primary&gt;&lt;Shift&gt;v</property>
            </object></child>
            <child><object class="GtkShortcutsShortcut">
              <property name="title">Select All</property>
              <property name="accelerator">&lt;Primary&gt;&lt;Shift&gt;a</property>
            </object></child>
          </object>
        </child>
      </object>
    </child>
  </object>
</interface>"#,
    );
    builder.object("help_overlay").expect("shortcuts window")
}
