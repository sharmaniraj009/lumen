//! Lumen — a Spotlight-style search launcher for Ubuntu / GNOME.
//!
//!   lumen               toggle the search window (starts the daemon if needed)
//!   lumen --background  start the daemon without showing the window
//!   lumen --quit        stop the running daemon

mod calc;
mod providers;

use gtk::prelude::*;
use gtk::{gdk, gio, glib, pango};
use providers::*;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{fs, thread};

const APP_ID: &str = "io.github.lumen.Launcher";
const OBJECT_PATH: &str = "/io/github/lumen/Launcher";
const PANEL_WIDTH: i32 = 780;
const LIST_WIDTH: i32 = 360;
const RESULTS_HEIGHT: i32 = 440;
const SHADOW_MARGIN: i32 = 24;

const CATEGORY_ORDER: &[&str] = &[
    "Top Hit", "Calculator", "Conversion", "Clock", "Weather", "Commands", "Websites", "Applications",
    "System Settings", "Slack", "Definition", "Wikipedia", "Documents", "Folders", "Files", "Claude",
    "Web Suggestions", "Search the Web",
];

fn category_limit(c: &str) -> usize {
    match c {
        "Applications" | "Documents" => 6,
        "Slack" => 5,
        "Folders" | "System Settings" | "Commands" | "Web Suggestions" => 4,
        _ => 3,
    }
}

const CSS_DARK: &str = r#"
window.lumen { background: transparent; }
.panel {
    background-color: rgba(36, 36, 40, 0.95);
    border-radius: 18px;
    border: 1px solid rgba(255, 255, 255, 0.13);
    box-shadow: 0 8px 22px rgba(0, 0, 0, 0.28);
    color: #f2f2f4;
}
.search-icon { color: rgba(255,255,255,0.55); }
entry.search, entry.search:focus, entry.search:focus-within {
    font-size: 26px; font-weight: 300; background: transparent; border: none;
    box-shadow: none; outline: none; color: #ffffff; caret-color: #4a9dff;
    padding: 6px 4px; min-height: 40px;
}
entry.search > text > placeholder { color: rgba(255,255,255,0.35); }
entry.search > text > selection { background-color: rgba(74, 157, 255, 0.45); }
.divider { background-color: rgba(255,255,255,0.10); min-height: 1px; min-width: 1px; }
list, list > row { background: transparent; }
row.header { padding: 10px 14px 3px 14px; }
row.header label { font-size: 11px; font-weight: 700; color: rgba(255,255,255,0.48); }
row.result { padding: 5px 8px; margin: 0 6px; border-radius: 8px; outline: none; }
row.result:selected { background-color: #2f6fe0; }
row.result .title { font-size: 14px; color: #f4f4f6; }
row.result .subtitle { font-size: 11px; color: rgba(255,255,255,0.50); }
row.result:selected .title { color: #ffffff; }
row.result:selected .subtitle { color: rgba(255,255,255,0.85); }
.preview { padding: 22px; }
.ptitle { font-size: 20px; font-weight: 600; color: #ffffff; }
.pbig { font-size: 38px; font-weight: 300; color: #ffffff; }
.pbody { font-size: 13px; color: rgba(255,255,255,0.82); }
.pmono { font-family: monospace; font-size: 11px; color: rgba(255,255,255,0.70); }
.pkey { font-size: 12px; color: rgba(255,255,255,0.45); }
.pval { font-size: 12px; color: rgba(255,255,255,0.85); }
.claude-q { font-size: 15px; font-weight: 600; color: #ffffff; }
.claude-badge { font-size: 11px; font-weight: 700; color: #d97757; }
.hint { font-size: 11px; color: rgba(255,255,255,0.40); padding: 6px 14px; }
scrollbar { background: transparent; border: none; }
scrollbar slider { background-color: rgba(255,255,255,0.22); min-width: 5px; border-radius: 4px; }
"#;

fn css_for(dark: bool) -> String {
    if dark {
        return CSS_DARK.to_string();
    }
    CSS_DARK
        .replace("rgba(36, 36, 40, 0.95)", "rgba(246, 246, 248, 0.96)")
        .replace("#f2f2f4", "#1d1d1f")
        .replace("row.result .title { font-size: 14px; color: #f4f4f6; }", "row.result .title { font-size: 14px; color: #1d1d1f; }")
        .replace("color: #ffffff; caret", "color: #111111; caret")
        .replace(".ptitle { font-size: 20px; font-weight: 600; color: #ffffff; }", ".ptitle { font-size: 20px; font-weight: 600; color: #111111; }")
        .replace(".pbig { font-size: 38px; font-weight: 300; color: #ffffff; }", ".pbig { font-size: 38px; font-weight: 300; color: #111111; }")
        .replace(".claude-q { font-size: 15px; font-weight: 600; color: #ffffff; }", ".claude-q { font-size: 15px; font-weight: 600; color: #111111; }")
        .replace("rgba(255,255,255,", "rgba(0,0,0,")
        .replace("rgba(255, 255, 255, 0.13)", "rgba(0, 0, 0, 0.12)")
        .replace("row.result:selected .subtitle { color: rgba(0,0,0,0.85); }", "row.result:selected .subtitle { color: rgba(255,255,255,0.85); }")
}

fn dark_mode() -> bool {
    gio::Settings::new("org.gnome.desktop.interface").string("color-scheme") == "prefer-dark"
}

// ------------------------------------------------------------------ messages from workers

enum Msg {
    Results { gen: u64, items: Vec<Item> },
    Image { url: String, bytes: Vec<u8> },
    ImageFailed { url: String },
    ClaudeDelta { id: u64, text: String },
    ClaudeDone { id: u64, error: Option<String> },
}

struct AppEntry {
    info: gio::AppInfo,
    name: String,
    generic: String,
    keywords: Vec<String>,
    exec: String,
    desc: String,
    id: String,
    icon: Option<String>,
}

#[derive(Default)]
struct ClaudeState {
    question: String,
    answer: String,
    running: bool,
    error: Option<String>,
    label: Option<gtk::Label>,
}

struct State {
    gen: u64,
    results: HashMap<&'static str, Vec<Item>>,
    rows: Vec<(gtk::ListBoxRow, Item)>,
    selected_key: Option<String>,
    suppress_select: bool,
    shown_at: Instant,
    debounce: Option<glib::SourceId>,
    usage: Usage,
    apps: Vec<AppEntry>,
    recent: Arc<HashSet<String>>,
    claude: ClaudeState,
    image_cache: HashMap<String, gdk::Texture>,
    pending_image: Option<(String, gtk::Picture, &'static str)>, // url, target, fallback icon
}

struct Ui {
    window: gtk::ApplicationWindow,
    entry: gtk::Entry,
    body: gtk::Box,
    listbox: gtk::ListBox,
    scroller: gtk::ScrolledWindow,
    preview: gtk::Box,
}

struct Lumen {
    ui: Ui,
    st: RefCell<State>,
    tx: async_channel::Sender<Msg>,
    files: FileIndex,
    slack: SlackIndex,
    claude_current: Arc<AtomicU64>,
    css: gtk::CssProvider,
}

type L = Rc<Lumen>;

// ------------------------------------------------------------------ fast client path

fn session_bus() -> Option<gio::DBusConnection> {
    gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).ok()
}

fn daemon_running(bus: &gio::DBusConnection) -> bool {
    bus.call_sync(
        Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", "org.freedesktop.DBus", "NameHasOwner",
        Some(&(APP_ID,).to_variant()), Some(glib::VariantTy::new("(b)").unwrap()),
        gio::DBusCallFlags::NONE, 1000, gio::Cancellable::NONE,
    )
    .ok()
    .and_then(|v| v.get::<(bool,)>())
    .is_some_and(|(b,)| b)
}

/// Ask the running daemon to run one of its actions over D-Bus. Takes a few
/// milliseconds, so the hotkey feels instant.
fn remote_action(bus: &gio::DBusConnection, action: &str) -> bool {
    let pd = glib::VariantDict::new(None);
    for var in ["XDG_ACTIVATION_TOKEN", "DESKTOP_STARTUP_ID"] {
        if let Ok(tok) = std::env::var(var) {
            if !tok.is_empty() {
                pd.insert("activation-token", &tok);
                pd.insert("desktop-startup-id", &tok);
                break;
            }
        }
    }
    let params = glib::Variant::tuple_from_iter([
        action.to_variant(),
        glib::Variant::array_from_iter_with_type(glib::VariantTy::VARIANT, Vec::<glib::Variant>::new()),
        pd.end(),
    ]);
    bus.call_sync(Some(APP_ID), OBJECT_PATH, "org.gtk.Actions", "Activate", Some(&params), None,
                  gio::DBusCallFlags::NONE, 2000, gio::Cancellable::NONE)
        .is_ok()
}

fn main() -> glib::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let background = args.iter().any(|a| a == "--background");
    let quit = args.iter().any(|a| a == "--quit");
    if let Some(bus) = session_bus() {
        if daemon_running(&bus) {
            if !background {
                remote_action(&bus, if quit { "quit" } else { "toggle" });
            }
            return glib::ExitCode::SUCCESS;
        }
    }
    if quit {
        return glib::ExitCode::SUCCESS;
    }

    let app = gtk::Application::builder().application_id(APP_ID).build();
    let lumen: Rc<RefCell<Option<L>>> = Rc::new(RefCell::new(None));
    let first = Rc::new(Cell::new(true));
    {
        let lumen = lumen.clone();
        app.connect_startup(move |app| {
            std::mem::forget(app.hold()); // keep running while the window is hidden
            let l = build(app);
            *lumen.borrow_mut() = Some(l);
        });
    }
    {
        let lumen = lumen.clone();
        app.connect_activate(move |_| {
            let show = !(first.get() && background);
            first.set(false);
            if let (true, Some(l)) = (show, lumen.borrow().as_ref()) {
                toggle(l);
            }
        });
    }
    app.run_with_args::<&str>(&[])
}

// ------------------------------------------------------------------ building the UI

fn build(app: &gtk::Application) -> L {
    let display = gdk::Display::default().expect("no display");
    let css = gtk::CssProvider::new();
    css.load_from_string(&css_for(dark_mode()));
    gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);

    let window = gtk::ApplicationWindow::builder().application(app).title("Lumen").decorated(false).resizable(false).build();
    window.add_css_class("lumen");

    // The window is just big enough for the panel and its shadow, so nothing else on screen is
    // covered. GNOME centres new windows; summon() maps it at full height and then shrinks it
    // (the top edge stays put), leaving the search bar in the upper part of the screen.
    let outer = gtk::Box::builder().orientation(gtk::Orientation::Vertical)
        .margin_top(SHADOW_MARGIN).margin_bottom(SHADOW_MARGIN).margin_start(SHADOW_MARGIN).margin_end(SHADOW_MARGIN).build();
    window.set_child(Some(&outer));
    let panel = gtk::Box::builder().orientation(gtk::Orientation::Vertical).width_request(PANEL_WIDTH).build();
    panel.add_css_class("panel");
    outer.append(&panel);

    let bar = gtk::Box::builder().spacing(10).margin_start(18).margin_end(18).margin_top(10).margin_bottom(10).build();
    let search_icon = gtk::Image::from_icon_name("system-search-symbolic");
    search_icon.set_pixel_size(24);
    search_icon.add_css_class("search-icon");
    bar.append(&search_icon);
    let entry = gtk::Entry::builder().placeholder_text("Lumen Search").hexpand(true).has_frame(false).build();
    entry.add_css_class("search");
    bar.append(&entry);
    panel.append(&bar);

    let body = gtk::Box::builder().orientation(gtk::Orientation::Vertical).visible(false).build();
    let div = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    div.add_css_class("divider");
    body.append(&div);
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let listbox = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Browse).margin_top(4).margin_bottom(6).build();
    let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&listbox).build();
    scroller.set_size_request(LIST_WIDTH, RESULTS_HEIGHT);
    content.append(&scroller);
    let vdiv = gtk::Box::new(gtk::Orientation::Vertical, 0);
    vdiv.add_css_class("divider");
    content.append(&vdiv);
    let preview = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(10).build();
    preview.add_css_class("preview");
    let pscroll = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).hexpand(true).child(&preview).build();
    content.append(&pscroll);
    body.append(&content);
    let div2 = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    div2.add_css_class("divider");
    body.append(&div2);
    let hint = gtk::Label::builder()
        .label("↵ open    Ctrl+↵ show in folder    Ctrl+C copy    ends with ? asks Claude    > run command    Esc close")
        .xalign(0.0).build();
    hint.add_css_class("hint");
    body.append(&hint);
    panel.append(&body);

    let (tx, rx) = async_channel::unbounded::<Msg>();
    let l: L = Rc::new(Lumen {
        ui: Ui { window: window.clone(), entry: entry.clone(), body, listbox: listbox.clone(), scroller, preview },
        st: RefCell::new(State {
            gen: 0,
            results: HashMap::new(),
            rows: vec![],
            selected_key: None,
            suppress_select: false,
            shown_at: Instant::now(),
            debounce: None,
            usage: Usage::load(),
            apps: vec![],
            recent: Arc::new(HashSet::new()),
            claude: ClaudeState::default(),
            image_cache: HashMap::new(),
            pending_image: None,
        }),
        tx,
        files: start_file_index(),
        slack: start_slack_index(),
        claude_current: Arc::new(AtomicU64::new(0)),
        css,
    });
    reload_apps(&l);

    // worker results arrive here, on the main thread
    {
        let l = l.clone();
        glib::spawn_future_local(async move {
            while let Ok(msg) = rx.recv().await {
                handle_msg(&l, msg);
            }
        });
    }

    let monitor = gio::AppInfoMonitor::get();
    {
        let l = l.clone();
        monitor.connect_changed(move |_| reload_apps(&l));
    }
    std::mem::forget(monitor);

    let settings = gio::Settings::new("org.gnome.desktop.interface");
    {
        let l = l.clone();
        settings.connect_changed(Some("color-scheme"), move |_, _| l.css.load_from_string(&css_for(dark_mode())));
    }
    std::mem::forget(settings);

    {
        let l = l.clone();
        entry.connect_changed(move |_| on_changed(&l));
    }
    {
        let l = l.clone();
        listbox.connect_row_selected(move |_, row| {
            if let Some(row) = row {
                on_row_selected(&l, row);
            }
        });
    }
    {
        let l = l.clone();
        listbox.connect_row_activated(move |_, row| {
            let item = l.st.borrow().rows.iter().find(|(r, _)| r == row).map(|(_, i)| i.clone());
            if let Some(item) = item {
                activate(&l, &item, false);
            }
        });
    }
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let l = l.clone();
        keys.connect_key_pressed(move |_, key, _, state| on_key(&l, key, state));
    }
    window.add_controller(keys);
    {
        let l = l.clone();
        window.connect_is_active_notify(move |w| {
            if !w.is_active() && w.is_visible() && l.st.borrow().shown_at.elapsed() > Duration::from_millis(500) {
                dismiss(&l);
            }
        });
    }
    {
        let l = l.clone();
        window.connect_close_request(move |_| {
            dismiss(&l);
            glib::Propagation::Stop
        });
    }

    let toggle_action = gio::SimpleAction::new("toggle", None);
    {
        let l = l.clone();
        toggle_action.connect_activate(move |_, _| toggle(&l));
    }
    app.add_action(&toggle_action);
    let quit_action = gio::SimpleAction::new("quit", None);
    {
        let app = app.clone();
        quit_action.connect_activate(move |_, _| app.quit());
    }
    app.add_action(&quit_action);
    if let Ok(spec) = std::env::var("LUMEN_SELFTEST") {
        selftest(&l, spec);
    }
    l
}

/// Developer self-test: LUMEN_SELFTEST="dir|query1|query2|..." shows the window, types each
/// query, waits, then prints the result rows and saves a PNG of the panel to dir.
fn selftest(l: &L, spec: String) {
    let mut parts: Vec<String> = spec.split('|').map(String::from).collect();
    let dir = parts.remove(0);
    let l = l.clone();
    glib::timeout_add_local_once(Duration::from_millis(300), move || {
        summon(&l);
        for (i, q) in parts.into_iter().enumerate() {
            let l = l.clone();
            let dir = dir.clone();
            let delay = 800 + i as u64 * 4000;
            glib::timeout_add_local_once(Duration::from_millis(delay), {
                let l = l.clone();
                let q = q.clone();
                move || {
                    if let Some(rest) = q.strip_prefix("!enter ") {
                        l.ui.entry.set_text(rest);
                        glib::timeout_add_local_once(Duration::from_millis(400), {
                            let l = l.clone();
                            move || { if let Some(it) = selected_item(&l) { activate(&l, &it, false); } }
                        });
                    } else if q != "!noop" {
                        l.ui.entry.set_text(&q);
                    }
                }
            });
            glib::timeout_add_local_once(Duration::from_millis(delay + 3700), move || {
                let rows: Vec<String> = l.st.borrow().rows.iter().take(8).map(|(_, i)| format!("[{}] {}", i.category, i.title)).collect();
                let (w, h) = (l.ui.window.width(), l.ui.window.height());
                println!("QUERY {q:?} window={w}x{h} rows={rows:?}");
                if let Some(a) = Some(l.st.borrow().claude.answer.clone()).filter(|a| !a.is_empty()) {
                    println!("  claude: {a:?}");
                }
                let paintable = gtk::WidgetPaintable::new(Some(&l.ui.window));
                let snap = gtk::Snapshot::new();
                paintable.snapshot(&snap, w as f64, h as f64);
                if let (Some(node), Some(native)) = (snap.to_node(), l.ui.window.native()) {
                    let tex = native.renderer().unwrap().render_texture(node, None);
                    let name: String = q.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect();
                    let _ = tex.save_to_png(format!("{dir}/rs_{i}_{name}.png"));
                }
            });
        }
    });
}

fn reload_apps(l: &L) {
    let mut apps = vec![];
    for info in gio::AppInfo::all() {
        if !info.should_show() {
            continue;
        }
        let (generic, keywords) = match info.downcast_ref::<gio::DesktopAppInfo>() {
            Some(d) => (
                d.generic_name().map(|s| s.to_string()).unwrap_or_default(),
                d.keywords().iter().map(|k| k.to_lowercase()).collect(),
            ),
            None => (String::new(), vec![]),
        };
        let exec = info.executable().file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        apps.push(AppEntry {
            name: info.display_name().to_string(),
            generic,
            keywords,
            exec,
            desc: info.description().map(|s| s.to_string()).unwrap_or_default(),
            id: info.id().map(|s| s.to_string()).unwrap_or_else(|| info.name().to_string()),
            icon: info.icon().and_then(|i| gio::prelude::IconExt::to_string(&i)).map(|s| s.to_string()),
            info,
        });
    }
    l.st.borrow_mut().apps = apps;
}

fn app_items(st: &State, q: &str) -> Vec<Item> {
    let mut out = vec![];
    for a in &st.apps {
        let s = text_score(q, &a.name, true)
            .max(text_score(q, &a.generic, true) * 0.7)
            .max(text_score(q, &a.exec, true) * 0.8)
            .max(a.keywords.iter().map(|k| text_score(q, k, true)).fold(0.0, f64::max) * 0.65);
        if s <= 0.0 {
            continue;
        }
        let icon = a.icon.clone().map(Icon::Serialized).unwrap_or(Icon::Name("application-x-executable".into()));
        let subtitle = if !a.generic.is_empty() { a.generic.clone() } else if !a.desc.is_empty() { a.desc.clone() } else { "Application".into() };
        let mut p = Preview::new(PreviewKind::App, a.name.clone(), a.desc.clone());
        p.meta = vec![("Kind".into(), "Application".into()), ("Command".into(), if a.exec.is_empty() { "—".into() } else { a.exec.clone() })];
        out.push(Item {
            title: a.name.clone(),
            subtitle,
            icon,
            category: "Applications",
            score: s + 8.0,
            action: Action::LaunchApp(a.id.clone()),
            alt: None,
            key: format!("app:{}", a.id),
            preview: p,
        });
    }
    out
}

// ------------------------------------------------------------------ show / hide

fn toggle(l: &L) {
    if l.ui.window.is_visible() {
        dismiss(l);
    } else {
        summon(l);
    }
}

fn summon(l: &L) {
    {
        let mut st = l.st.borrow_mut();
        st.shown_at = Instant::now();
        st.recent = Arc::new(recent_paths());
    }
    l.ui.body.set_visible(true); // map at full height so GNOME centres the whole panel
    l.ui.window.present();
    l.ui.entry.grab_focus();
    l.ui.entry.select_region(0, -1);
    let l = l.clone();
    glib::timeout_add_local_once(Duration::from_millis(40), move || {
        if l.ui.entry.text().trim().is_empty() {
            l.ui.body.set_visible(false);
        }
    });
}

fn dismiss(l: &L) {
    l.ui.window.set_visible(false);
}

fn recent_paths() -> HashSet<String> {
    gtk::RecentManager::default()
        .items()
        .iter()
        .filter_map(|i| {
            let uri = i.uri();
            uri.starts_with("file://").then(|| gio::File::for_uri(&uri).path()).flatten()
        })
        .map(|p| p.to_string_lossy().to_string())
        .collect()
}

// ------------------------------------------------------------------ searching

fn on_changed(l: &L) {
    let q = l.ui.entry.text().to_string();
    let gen = {
        let mut st = l.st.borrow_mut();
        st.gen += 1;
        if let Some(id) = st.debounce.take() {
            id.remove();
        }
        st.results.clear();
        st.selected_key = None;
        st.gen
    };
    if q.trim().is_empty() {
        l.ui.body.set_visible(false);
        return;
    }
    let ql = q.trim().to_lowercase();
    let mut local = vec![];
    for f in [calc_items, unit_items, time_items, quick_items] {
        local.extend(f(&q));
    }
    let is_shell = q.trim().starts_with('>');
    if !is_shell {
        local.extend(app_items(&l.st.borrow(), &ql));
        local.extend(settings_items(&ql));
        local.extend(system_items(&ql));
        local.extend(slack_items(&l.slack, &q));
    }
    let answered = local.iter().any(|i| matches!(i.category, "Calculator" | "Conversion" | "Clock"));
    if !answered {
        local.extend(claude_item(&q));
    }
    local.extend(web_fallbacks(&q));
    merge(l, local, gen);
    if answered || is_shell {
        return; // a math/time answer or a shell command doesn't need file or web lookups
    }

    let tx = l.tx.clone();
    let files = l.files.clone();
    let recent = l.st.borrow().recent.clone();
    let q2 = q.clone();
    thread::spawn(move || {
        let items = file_items(&files, &q2, &recent);
        let exclude: HashSet<String> = items.iter().map(|i| i.key.trim_start_matches("file:").to_string()).collect();
        let _ = tx.send_blocking(Msg::Results { gen, items });
        let _ = tx.send_blocking(Msg::Results { gen, items: content_items(&q2, &exclude) });
    });

    if config().online {
        let l2 = l.clone();
        let id = glib::timeout_add_local_once(Duration::from_millis(250), move || {
            l2.st.borrow_mut().debounce = None;
            if l2.st.borrow().gen != gen {
                return;
            }
            for p in ONLINE {
                let tx = l2.tx.clone();
                let q = q.clone();
                let p = *p;
                thread::spawn(move || {
                    let items = p(&q);
                    if !items.is_empty() {
                        let _ = tx.send_blocking(Msg::Results { gen, items });
                    }
                });
            }
        });
        l.st.borrow_mut().debounce = Some(id);
    }
}

fn handle_msg(l: &L, msg: Msg) {
    match msg {
        Msg::Results { gen, items } => merge(l, items, gen),
        Msg::ImageFailed { url } => image_fallback(l, &url),
        Msg::Image { url, bytes } => {
            let Ok(tex) = gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes)) else { return image_fallback(l, &url) };
            let mut st = l.st.borrow_mut();
            st.image_cache.insert(url.clone(), tex.clone());
            if let Some((u, pic, _)) = &st.pending_image {
                if *u == url {
                    pic.set_paintable(Some(&tex));
                }
            }
        }
        Msg::ClaudeDelta { id, text } => {
            if id != l.claude_current.load(Ordering::SeqCst) {
                return;
            }
            let mut st = l.st.borrow_mut();
            st.claude.answer.push_str(&text);
            if let Some(lbl) = &st.claude.label {
                lbl.set_text(&st.claude.answer);
            }
        }
        Msg::ClaudeDone { id, error } => {
            if id != l.claude_current.load(Ordering::SeqCst) {
                return;
            }
            let mut st = l.st.borrow_mut();
            st.claude.running = false;
            st.claude.error = error.clone();
            if let Some(lbl) = &st.claude.label {
                if let Some(e) = error {
                    lbl.set_text(&format!("Couldn't get an answer: {e}"));
                } else if st.claude.answer.trim().is_empty() {
                    lbl.set_text("(no answer)");
                }
            }
        }
    }
}

/// Show the row's icon instead of an empty box when a preview image can't be loaded.
fn image_fallback(l: &L, url: &str) {
    let st = l.st.borrow();
    let Some((u, pic, icon_name)) = &st.pending_image else { return };
    if u != url {
        return;
    }
    let theme = gtk::IconTheme::for_display(&pic.display());
    let icon = theme.lookup_icon(icon_name, &[], 96, 1, gtk::TextDirection::None, gtk::IconLookupFlags::empty());
    pic.set_size_request(96, 96);
    pic.set_paintable(Some(&icon));
}

fn merge(l: &L, items: Vec<Item>, gen: u64) {
    {
        let mut st = l.st.borrow_mut();
        if gen != st.gen || items.is_empty() {
            return;
        }
        for mut it in items {
            if st.results.values().flatten().any(|o| o.key == it.key) {
                continue;
            }
            it.score += st.usage.boost(&it.key);
            st.results.entry(it.category).or_default().push(it);
        }
    }
    render(l);
}

fn render(l: &L) {
    if l.ui.entry.text().trim().is_empty() {
        return;
    }
    let lb = &l.ui.listbox;
    lb.remove_all();
    let mut groups: HashMap<&'static str, Vec<Item>> = {
        let st = l.st.borrow();
        st.results
            .iter()
            .map(|(c, rs)| {
                let mut v = rs.clone();
                v.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
                v.truncate(category_limit(c));
                (*c, v)
            })
            .collect()
    };
    let top = groups
        .iter()
        .filter(|(c, _)| !matches!(**c, "Search the Web" | "Web Suggestions"))
        .filter_map(|(_, v)| v.first())
        .max_by(|a, b| a.score.partial_cmp(&b.score).unwrap())
        .cloned();
    if let Some(top) = top.filter(|t| t.score >= 55.0) {
        if let Some(v) = groups.get_mut(top.category) {
            v.retain(|i| i.key != top.key);
        }
        groups.insert("Top Hit", vec![top]);
    }
    let mut rows = vec![];
    let mut order: Vec<&'static str> = CATEGORY_ORDER.to_vec();
    order.extend(groups.keys().filter(|c| !CATEGORY_ORDER.contains(c)).copied().collect::<Vec<_>>());
    for cat in order {
        let Some(items) = groups.get(cat).filter(|v| !v.is_empty()) else { continue };
        let header = gtk::ListBoxRow::builder().selectable(false).activatable(false).focusable(false).build();
        header.add_css_class("header");
        header.set_child(Some(&gtk::Label::builder().label(cat.to_uppercase()).xalign(0.0).build()));
        lb.append(&header);
        for it in items {
            let row = result_row(it);
            lb.append(&row);
            rows.push((row, it.clone()));
        }
    }
    l.ui.body.set_visible(true);
    let target = {
        let mut st = l.st.borrow_mut();
        let t = rows
            .iter()
            .find(|(_, i)| Some(&i.key) == st.selected_key.as_ref())
            .or(rows.first())
            .map(|(r, _)| r.clone());
        st.rows = rows;
        st.suppress_select = true;
        t
    };
    if let Some(t) = target {
        lb.select_row(Some(&t));
    }
    l.st.borrow_mut().suppress_select = false;
}

fn make_image(icon: &Icon, size: i32) -> gtk::Image {
    let img = match icon {
        Icon::Name(n) => gtk::Image::from_icon_name(n),
        Icon::Serialized(s) => gio::Icon::for_string(s)
            .map(|i| gtk::Image::from_gicon(&i))
            .unwrap_or_else(|_| gtk::Image::from_icon_name("application-x-executable")),
        Icon::File { path, is_dir } => {
            if *is_dir {
                gtk::Image::from_icon_name("folder")
            } else {
                let (ct, _) = gio::content_type_guess(Some(Path::new(path)), None);
                gtk::Image::from_gicon(&gio::content_type_get_icon(&ct))
            }
        }
    };
    img.set_pixel_size(size);
    img
}

fn result_row(it: &Item) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.add_css_class("result");
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    b.append(&make_image(&it.icon, 30));
    let text = gtk::Box::builder().orientation(gtk::Orientation::Vertical).valign(gtk::Align::Center).hexpand(true).build();
    let t = gtk::Label::builder().label(&it.title).xalign(0.0).ellipsize(pango::EllipsizeMode::End).build();
    t.add_css_class("title");
    text.append(&t);
    if !it.subtitle.is_empty() {
        let s = gtk::Label::builder().label(&it.subtitle).xalign(0.0).ellipsize(pango::EllipsizeMode::End).build();
        s.add_css_class("subtitle");
        text.append(&s);
    }
    b.append(&text);
    row.set_child(Some(&b));
    row
}

// ------------------------------------------------------------------ preview pane

fn on_row_selected(l: &L, row: &gtk::ListBoxRow) {
    let item = {
        let mut st = l.st.borrow_mut();
        let item = st.rows.iter().find(|(r, _)| r == row).map(|(_, i)| i.clone());
        if let (Some(i), false) = (&item, st.suppress_select) {
            st.selected_key = Some(i.key.clone());
        }
        item
    };
    if let Some(item) = item {
        show_preview(l, &item);
    }
    // keep the selected row visible
    if let Some(bounds) = row.compute_bounds(&l.ui.listbox) {
        let adj = l.ui.scroller.vadjustment();
        let (y, h) = (bounds.y() as f64, bounds.height() as f64);
        if y < adj.value() {
            adj.set_value((y - 24.0).max(0.0));
        } else if y + h > adj.value() + adj.page_size() {
            adj.set_value(y + h - adj.page_size());
        }
    }
}

fn label(text: &str, class: &str, center: bool) -> gtk::Label {
    let lbl = gtk::Label::builder()
        .label(text)
        .xalign(if center { 0.5 } else { 0.0 })
        .wrap(true)
        .wrap_mode(pango::WrapMode::WordChar)
        .max_width_chars(40)
        .justify(if center { gtk::Justification::Center } else { gtk::Justification::Left })
        .build();
    lbl.add_css_class(class);
    lbl
}

fn show_preview(l: &L, it: &Item) {
    let pv = &l.ui.preview;
    while let Some(c) = pv.first_child() {
        pv.remove(&c);
    }
    {
        let mut st = l.st.borrow_mut();
        st.claude.label = None;
        st.pending_image = None;
    }
    let p = &it.preview;

    if let Action::AskClaude(q) = &it.action {
        let badge = label("✦ CLAUDE", "claude-badge", false);
        pv.append(&badge);
        pv.append(&label(q, "claude-q", false));
        let st = l.st.borrow();
        let text = if st.claude.question == *q && (!st.claude.answer.is_empty() || st.claude.running || st.claude.error.is_some()) {
            match (&st.claude.error, st.claude.answer.is_empty()) {
                (Some(e), _) => format!("Couldn't get an answer: {e}"),
                (None, true) => "Thinking…".to_string(),
                (None, false) => st.claude.answer.clone(),
            }
        } else {
            "Press Enter to ask Claude. The answer appears here; Ctrl+C copies it.".to_string()
        };
        drop(st);
        let ans = label(&text, "pbody", false);
        ans.set_selectable(true);
        pv.append(&ans);
        l.st.borrow_mut().claude.label = Some(ans);
        return;
    }

    match p.kind {
        PreviewKind::Big => {
            let big = label(&p.title, "pbig", false);
            big.set_selectable(true);
            pv.append(&big);
            if !p.body.is_empty() {
                pv.append(&label(&p.body, "pbody", false));
            }
        }
        _ => {
            let picture = preview_picture(l, it);
            pv.append(&picture);
            pv.append(&label(&p.title, "ptitle", true));
            if !p.body.is_empty() {
                pv.append(&label(&p.body, "pbody", false));
            }
            if let Some(snip) = p.path.as_deref().filter(|_| p.kind == PreviewKind::File).and_then(text_snippet) {
                let s = label(&snip, "pmono", false);
                s.set_max_width_chars(48);
                pv.append(&s);
            }
        }
    }
    let meta = if p.kind == PreviewKind::File { p.path.as_deref().map(file_meta).unwrap_or_default() } else { p.meta.clone() };
    if !meta.is_empty() {
        let grid = gtk::Grid::builder().column_spacing(12).row_spacing(4).margin_top(8).build();
        for (i, (k, v)) in meta.iter().enumerate() {
            let kl = gtk::Label::builder().label(k).xalign(1.0).yalign(0.0).build();
            kl.add_css_class("pkey");
            let vl = gtk::Label::builder().label(v).xalign(0.0).wrap(true).wrap_mode(pango::WrapMode::WordChar).selectable(true).max_width_chars(34).build();
            vl.add_css_class("pval");
            grid.attach(&kl, 0, i as i32, 1, 1);
            grid.attach(&vl, 1, i as i32, 1, 1);
        }
        pv.append(&grid);
    }
}

fn preview_picture(l: &L, it: &Item) -> gtk::Widget {
    let p = &it.preview;
    if let Some(path) = p.path.as_deref().filter(|_| p.kind == PreviewKind::File) {
        let (ct, _) = gio::content_type_guess(Some(Path::new(path)), None);
        let small = fs::metadata(path).map(|m| m.is_file() && m.len() < 40_000_000).unwrap_or(false);
        if ct.starts_with("image/") && small {
            let pic = gtk::Picture::for_filename(path);
            pic.set_content_fit(gtk::ContentFit::Contain);
            pic.set_can_shrink(true);
            pic.set_size_request(300, 220);
            return pic.upcast();
        }
    }
    if let Some(url) = &p.image_url {
        let pic = gtk::Picture::new();
        pic.set_content_fit(gtk::ContentFit::Contain);
        pic.set_can_shrink(true);
        pic.set_size_request(300, 220);
        let mut st = l.st.borrow_mut();
        if let Some(tex) = st.image_cache.get(url) {
            pic.set_paintable(Some(tex));
        } else {
            let fallback = if it.key.starts_with("slack:") { "avatar-default" } else { "image-missing" };
            st.pending_image = Some((url.clone(), pic.clone(), fallback));
            let tx = l.tx.clone();
            let url = url.clone();
            thread::spawn(move || {
                let _ = match get_bytes(&url) {
                    Some(bytes) => tx.send_blocking(Msg::Image { url, bytes }),
                    None => tx.send_blocking(Msg::ImageFailed { url }),
                };
            });
        }
        return pic.upcast();
    }
    let img = make_image(&it.icon, 96);
    img.set_halign(gtk::Align::Center);
    img.set_margin_top(8);
    img.upcast()
}

fn text_snippet(path: &str) -> Option<String> {
    let md = fs::metadata(path).ok()?;
    if !md.is_file() || md.len() > 2_000_000 {
        return None;
    }
    let (ct, _) = gio::content_type_guess(Some(Path::new(path)), None);
    if !gio::content_type_is_a(&ct, "text/plain") {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(900)]).trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn file_meta(path: &str) -> Vec<(String, String)> {
    let mut meta = vec![];
    let Ok(md) = fs::metadata(path) else { return meta };
    if md.is_dir() {
        meta.push(("Kind".into(), "Folder".into()));
        if let Ok(rd) = fs::read_dir(path) {
            meta.push(("Items".into(), rd.count().to_string()));
        }
    } else {
        let (ct, _) = gio::content_type_guess(Some(Path::new(path)), None);
        meta.push(("Kind".into(), gio::content_type_get_description(&ct).to_string()));
        meta.push(("Size".into(), glib::format_size(md.len()).to_string()));
    }
    if let Ok(m) = md.modified() {
        let dt: chrono::DateTime<chrono::Local> = m.into();
        meta.push(("Modified".into(), dt.format("%-d %b %Y, %H:%M").to_string()));
    }
    let dir = Path::new(path).parent().map(|p| p.to_string_lossy().replacen(&home(), "~", 1)).unwrap_or_default();
    meta.push(("Where".into(), dir));
    meta
}

// ------------------------------------------------------------------ keyboard & activation

fn on_key(l: &L, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
    let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
    use gdk::Key;
    match key {
        Key::Escape => {
            if l.ui.entry.text().is_empty() {
                dismiss(l);
            } else {
                l.ui.entry.set_text("");
            }
        }
        Key::Down | Key::Tab => move_selection(l, 1),
        Key::Up | Key::ISO_Left_Tab => move_selection(l, -1),
        Key::n | Key::j if ctrl => move_selection(l, 1),
        Key::p | Key::k if ctrl => move_selection(l, -1),
        Key::Page_Down => move_selection(l, 6),
        Key::Page_Up => move_selection(l, -6),
        Key::Return | Key::KP_Enter => {
            if let Some(item) = selected_item(l) {
                activate(l, &item, ctrl);
            }
        }
        Key::c if ctrl && l.ui.entry.selection_bounds().is_none() => {
            if let Some(item) = selected_item(l) {
                let text = match &item.action {
                    Action::AskClaude(_) => l.st.borrow().claude.answer.clone(),
                    Action::Copy(t) => t.clone(),
                    _ => item.preview.path.clone().unwrap_or(item.title.clone()),
                };
                if !text.is_empty() {
                    l.ui.window.clipboard().set_text(&text);
                    dismiss(l);
                }
            }
        }
        _ => return glib::Propagation::Proceed,
    }
    glib::Propagation::Stop
}

fn selected_item(l: &L) -> Option<Item> {
    let row = l.ui.listbox.selected_row()?;
    l.st.borrow().rows.iter().find(|(r, _)| *r == row).map(|(_, i)| i.clone())
}

fn move_selection(l: &L, step: i32) {
    let rows: Vec<gtk::ListBoxRow> = l.st.borrow().rows.iter().map(|(r, _)| r.clone()).collect();
    if rows.is_empty() {
        return;
    }
    let cur = l.ui.listbox.selected_row().and_then(|r| rows.iter().position(|x| *x == r)).map(|i| i as i32).unwrap_or(-1);
    let i = (cur + step).clamp(0, rows.len() as i32 - 1) as usize;
    l.ui.listbox.select_row(Some(&rows[i]));
}

fn spawn_detached(argv: &[String]) {
    if argv.is_empty() {
        return;
    }
    if let Ok(mut child) = Command::new(&argv[0]).args(&argv[1..]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).process_group(0).spawn() {
        thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

fn reveal(path: &str) {
    let uri = gio::File::for_path(path).uri().to_string();
    let ok = session_bus().and_then(|bus| {
        bus.call_sync(Some("org.freedesktop.FileManager1"), "/org/freedesktop/FileManager1", "org.freedesktop.FileManager1",
                      "ShowItems", Some(&(vec![uri], "").to_variant()), None, gio::DBusCallFlags::NONE, 2000, gio::Cancellable::NONE)
            .ok()
    });
    if ok.is_none() {
        if let Some(parent) = Path::new(path).parent() {
            let _ = gio::AppInfo::launch_default_for_uri(&gio::File::for_path(parent).uri(), gio::AppLaunchContext::NONE);
        }
    }
}

fn activate(l: &L, it: &Item, alt: bool) {
    let ctx = WidgetExt::display(&l.ui.window).app_launch_context();
    let action = if alt { it.alt.clone().unwrap_or(it.action.clone()) } else { it.action.clone() };
    let result: Result<(), String> = match &action {
        Action::AskClaude(q) => {
            ask_claude(l, q);
            l.st.borrow_mut().usage.record(&it.key);
            return; // stay open: the answer streams into the preview pane
        }
        Action::LaunchApp(id) => {
            let st = l.st.borrow();
            match st.apps.iter().find(|a| a.id == *id) {
                Some(a) => a.info.launch(&[], Some(&ctx)).map_err(|e| e.to_string()),
                None => Err("app not found".into()),
            }
        }
        Action::OpenUri(u) => gio::AppInfo::launch_default_for_uri(u, Some(&ctx)).map_err(|e| e.to_string()),
        Action::OpenPath(p) => gio::AppInfo::launch_default_for_uri(&gio::File::for_path(p).uri(), Some(&ctx)).map_err(|e| e.to_string()),
        Action::RevealPath(p) => {
            reveal(p);
            Ok(())
        }
        Action::Spawn(argv) => {
            spawn_detached(argv);
            Ok(())
        }
        Action::Copy(t) => {
            l.ui.window.clipboard().set_text(t);
            Ok(())
        }
        Action::ToggleDark => {
            let s = gio::Settings::new("org.gnome.desktop.interface");
            let next = if s.string("color-scheme") == "prefer-dark" { "default" } else { "prefer-dark" };
            s.set_string("color-scheme", next).map_err(|e| e.to_string())
        }
    };
    if let Err(e) = result {
        eprintln!("lumen: failed to open {}: {e}", it.title);
    }
    l.st.borrow_mut().usage.record(&it.key);
    dismiss(l);
}

// ------------------------------------------------------------------ Claude

fn ask_claude(l: &L, question: &str) {
    let Some(bin) = claude_bin() else { return };
    let id = l.claude_current.fetch_add(1, Ordering::SeqCst) + 1;
    {
        let mut st = l.st.borrow_mut();
        st.claude.question = question.to_string();
        st.claude.answer.clear();
        st.claude.error = None;
        st.claude.running = true;
        if let Some(lbl) = &st.claude.label {
            lbl.set_text("Thinking…");
        }
    }
    let tx = l.tx.clone();
    let current = l.claude_current.clone();
    let question = if question.starts_with('-') { format!(" {question}") } else { question.to_string() };
    let system = format!(
        "You are the AI answer box inside Lumen, a desktop search launcher on Ubuntu Linux. Answer the user's query \
         directly and concisely in plain text: no Markdown headings, tables or bold; short bullet lists with • are fine. \
         Keep answers under 150 words unless the user asks for more detail. Today is {}.",
        chrono::Local::now().format("%A, %-d %B %Y")
    );
    let model = config().claude_model.clone();
    thread::spawn(move || {
        let spawned = Command::new(bin)
            .args(["-p", "--output-format", "stream-json", "--verbose", "--include-partial-messages", "--model", &model,
                   "--tools", "", "--no-session-persistence", "--strict-mcp-config", "--setting-sources", "user",
                   "--system-prompt", &system])
            .arg(&question)
            .current_dir(home())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send_blocking(Msg::ClaudeDone { id, error: Some(e.to_string()) });
                return;
            }
        };
        let stdout = child.stdout.take().unwrap();
        let mut done = false;
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if current.load(Ordering::SeqCst) != id {
                break; // a newer question replaced this one
            }
            let Ok(ev) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
            match ev["type"].as_str() {
                Some("stream_event") => {
                    let e = &ev["event"];
                    if e["type"] == "content_block_delta" && e["delta"]["type"] == "text_delta" {
                        if let Some(t) = e["delta"]["text"].as_str() {
                            let _ = tx.send_blocking(Msg::ClaudeDelta { id, text: t.to_string() });
                        }
                    }
                }
                Some("result") => {
                    let error = (ev["is_error"].as_bool() == Some(true))
                        .then(|| ev["result"].as_str().unwrap_or("Claude returned an error").to_string());
                    let _ = tx.send_blocking(Msg::ClaudeDone { id, error });
                    done = true;
                    break;
                }
                _ => {}
            }
        }
        let _ = child.kill(); // the CLI lingers a couple of seconds after answering
        let stderr = child.stderr.take().map(|mut e| {
            let mut s = String::new();
            let _ = std::io::Read::read_to_string(&mut e, &mut s);
            s
        });
        let _ = child.wait();
        if !done {
            let msg = stderr.filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "Claude CLI exited without an answer".into());
            let _ = tx.send_blocking(Msg::ClaudeDone { id, error: Some(msg.lines().last().unwrap_or("").to_string()) });
        }
    });
}
