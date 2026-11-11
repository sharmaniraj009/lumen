//! Search providers. Everything here produces plain `Send` data so it can run on worker
//! threads; GTK objects are only created on the main thread (see main.rs).

use crate::calc;
use chrono::{Offset, TimeZone, Utc};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::{env, fs, thread};

// ------------------------------------------------------------------ data types

#[derive(Clone, Debug)]
pub enum Icon {
    Name(String),
    /// A serialized gio::Icon (from an AppInfo)
    Serialized(String),
    /// Icon derived from the file's content type
    File { path: String, is_dir: bool },
}

#[derive(Clone, Debug)]
pub enum Action {
    LaunchApp(String),
    OpenUri(String),
    OpenPath(String),
    RevealPath(String),
    Spawn(Vec<String>),
    Copy(String),
    ToggleDark,
    AskClaude(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PreviewKind {
    Big,
    App,
    File,
    Web,
}

#[derive(Clone, Debug)]
pub struct Preview {
    pub kind: PreviewKind,
    pub title: String,
    pub body: String,
    pub meta: Vec<(String, String)>,
    pub path: Option<String>,
    pub image_url: Option<String>,
}

impl Preview {
    pub fn new(kind: PreviewKind, title: impl Into<String>, body: impl Into<String>) -> Self {
        Preview { kind, title: title.into(), body: body.into(), meta: vec![], path: None, image_url: None }
    }
}

#[derive(Clone, Debug)]
pub struct Item {
    pub title: String,
    pub subtitle: String,
    pub icon: Icon,
    pub category: &'static str,
    pub score: f64,
    pub action: Action,
    pub alt: Option<Action>,
    pub key: String,
    pub preview: Preview,
}

impl Item {
    fn new(title: impl Into<String>, subtitle: impl Into<String>, icon: Icon, category: &'static str, score: f64, action: Action) -> Self {
        let title = title.into();
        Item {
            key: format!("{category}:{title}"),
            preview: Preview::new(PreviewKind::App, title.clone(), ""),
            title,
            subtitle: subtitle.into(),
            icon,
            category,
            score,
            action,
            alt: None,
        }
    }
    fn key(mut self, k: impl Into<String>) -> Self {
        self.key = k.into();
        self
    }
    fn preview(mut self, p: Preview) -> Self {
        self.preview = p;
        self
    }
}

fn icon(name: &str) -> Icon {
    Icon::Name(name.to_string())
}

// ------------------------------------------------------------------ paths & config

pub fn home() -> String {
    env::var("HOME").unwrap_or_else(|_| "/".into())
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    env::var(var).map(PathBuf::from).unwrap_or_else(|_| Path::new(&home()).join(fallback))
}

pub fn data_dir() -> PathBuf {
    let d = xdg("XDG_DATA_HOME", ".local/share").join("lumen");
    let _ = fs::create_dir_all(&d);
    d
}

pub fn config_dir() -> PathBuf {
    let d = xdg("XDG_CONFIG_HOME", ".config").join("lumen");
    let _ = fs::create_dir_all(&d);
    d
}

fn cache_dir() -> PathBuf {
    let d = xdg("XDG_CACHE_HOME", ".cache").join("lumen");
    let _ = fs::create_dir_all(&d);
    d
}

pub struct Config {
    pub search_engine: String,
    pub online: bool,
    pub index_hidden: bool,
    pub extra_index_paths: Vec<String>,
    pub max_indexed_files: usize,
    pub claude: bool,
    pub claude_model: String,
}

pub fn config() -> &'static Config {
    static CFG: OnceLock<Config> = OnceLock::new();
    CFG.get_or_init(|| {
        let path = config_dir().join("config.json");
        let v: Value = fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(|| {
            let default = serde_json::json!({
                "search_engine": "google",
                "online": true,
                "index_hidden": false,
                "extra_index_paths": [],
                "max_indexed_files": 400000,
                "claude": true,
                "claude_model": "haiku"
            });
            let _ = fs::write(&path, serde_json::to_string_pretty(&default).unwrap());
            default
        });
        Config {
            search_engine: v["search_engine"].as_str().unwrap_or("google").to_string(),
            online: v["online"].as_bool().unwrap_or(true),
            index_hidden: v["index_hidden"].as_bool().unwrap_or(false),
            extra_index_paths: v["extra_index_paths"]
                .as_array()
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default(),
            max_indexed_files: v["max_indexed_files"].as_u64().unwrap_or(400_000) as usize,
            claude: v["claude"].as_bool().unwrap_or(true),
            claude_model: v["claude_model"].as_str().unwrap_or("haiku").to_string(),
        }
    })
}

fn engine() -> (&'static str, &'static str) {
    match config().search_engine.as_str() {
        "duckduckgo" => ("DuckDuckGo", "https://duckduckgo.com/?q="),
        "bing" => ("Bing", "https://www.bing.com/search?q="),
        "brave" => ("Brave", "https://search.brave.com/search?q="),
        _ => ("Google", "https://www.google.com/search?q="),
    }
}

pub fn web_search_url(q: &str) -> String {
    format!("{}{}", engine().1, urlencoding::encode(q))
}

// ------------------------------------------------------------------ usage memory

pub struct Usage {
    counts: HashMap<String, u64>,
    path: PathBuf,
}

impl Usage {
    pub fn load() -> Self {
        let path = data_dir().join("usage.json");
        let counts = fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        Usage { counts, path }
    }
    pub fn boost(&self, key: &str) -> f64 {
        (self.counts.get(key).copied().unwrap_or(0) as f64).ln_1p() * 8.0
    }
    pub fn record(&mut self, key: &str) {
        *self.counts.entry(key.to_string()).or_insert(0) += 1;
        let _ = fs::write(&self.path, serde_json::to_string(&self.counts).unwrap_or_default());
    }
}

// ------------------------------------------------------------------ matching

/// Fuzzy match quality of `q` (lowercase) against `text`, 0..100.
pub fn text_score(q: &str, text: &str, fuzzy: bool) -> f64 {
    if text.is_empty() || q.is_empty() {
        return 0.0;
    }
    let t = text.to_lowercase();
    if t == q {
        return 100.0;
    }
    if t.starts_with(q) {
        return 90.0 - ((t.len() - q.len()).min(20) as f64) * 0.3;
    }
    let words: Vec<&str> = t.split(|c: char| " -_.:/()".contains(c)).filter(|w| !w.is_empty()).collect();
    if words.iter().any(|w| w.starts_with(q)) {
        return 75.0;
    }
    if q.len() >= 2 {
        let initials: String = words.iter().filter_map(|w| w.chars().next()).collect();
        if initials.starts_with(q) {
            return 68.0;
        }
    }
    if t.contains(q) {
        return 60.0;
    }
    if fuzzy && q.chars().count() >= 3 {
        let mut it = t.chars();
        if q.chars().all(|c| it.any(|x| x == c)) {
            return 30.0;
        }
    }
    0.0
}

// ------------------------------------------------------------------ instant providers

pub fn calc_items(q: &str) -> Vec<Item> {
    let Some(v) = calc::calculate(q) else { return vec![] };
    let text = calc::format_number(v);
    vec![Item::new(text.clone(), format!("{} =  ·  Enter to copy", q.trim()), icon("accessories-calculator"), "Calculator", 1000.0, Action::Copy(calc::plain(v)))
        .key("calc")
        .preview(Preview::new(PreviewKind::Big, text, format!("{} =", q.trim())))]
}

pub fn unit_items(q: &str) -> Vec<Item> {
    let Some(c) = calc::convert(q) else { return vec![] };
    let v = calc::round6(c.value);
    let text = format!("{} {}", calc::format_number(v), c.label);
    vec![Item::new(text.clone(), format!("{} =  ·  Enter to copy", c.from), icon("accessories-calculator"), "Conversion", if c.implicit { 900.0 } else { 990.0 }, Action::Copy(calc::plain(v)))
        .key("units")
        .preview(Preview::new(PreviewKind::Big, text, format!("{} =", c.from)))]
}

const TZ_ALIASES: &[(&str, &str)] = &[
    ("india", "Asia/Kolkata"), ("ist", "Asia/Kolkata"), ("delhi", "Asia/Kolkata"), ("new delhi", "Asia/Kolkata"),
    ("mumbai", "Asia/Kolkata"), ("bangalore", "Asia/Kolkata"), ("bengaluru", "Asia/Kolkata"), ("chennai", "Asia/Kolkata"),
    ("hyderabad", "Asia/Kolkata"), ("pune", "Asia/Kolkata"), ("pst", "America/Los_Angeles"), ("pdt", "America/Los_Angeles"),
    ("san francisco", "America/Los_Angeles"), ("sf", "America/Los_Angeles"), ("seattle", "America/Los_Angeles"),
    ("california", "America/Los_Angeles"), ("est", "America/New_York"), ("edt", "America/New_York"), ("nyc", "America/New_York"),
    ("boston", "America/New_York"), ("washington", "America/New_York"), ("cst", "America/Chicago"), ("mst", "America/Denver"),
    ("uk", "Europe/London"), ("england", "Europe/London"), ("bst", "Europe/London"), ("gmt", "UTC"), ("utc", "UTC"),
    ("japan", "Asia/Tokyo"), ("jst", "Asia/Tokyo"), ("china", "Asia/Shanghai"), ("beijing", "Asia/Shanghai"),
    ("germany", "Europe/Berlin"), ("france", "Europe/Paris"), ("cet", "Europe/Paris"), ("australia", "Australia/Sydney"),
    ("uae", "Asia/Dubai"), ("singapore", "Asia/Singapore"), ("korea", "Asia/Seoul"), ("russia", "Europe/Moscow"),
    ("brazil", "America/Sao_Paulo"), ("canada", "America/Toronto"), ("nepal", "Asia/Kathmandu"), ("pakistan", "Asia/Karachi"),
    ("bangladesh", "Asia/Dhaka"), ("italy", "Europe/Rome"), ("spain", "Europe/Madrid"), ("netherlands", "Europe/Amsterdam"),
];

fn resolve_tz(place: &str) -> Option<chrono_tz::Tz> {
    let p = place.trim().to_lowercase();
    if let Some((_, tz)) = TZ_ALIASES.iter().find(|(k, _)| *k == p) {
        return tz.parse().ok();
    }
    chrono_tz::TZ_VARIANTS
        .iter()
        .find(|tz| tz.name().rsplit('/').next().map(|c| c.replace('_', " ").to_lowercase()) == Some(p.clone()))
        .copied()
}

pub fn time_items(q: &str) -> Vec<Item> {
    let l = q.trim().trim_end_matches('?').trim().to_lowercase();
    let place: Option<String> = if ["time", "date", "clock", "what time is it", "what's the time"].contains(&l.as_str()) {
        None
    } else if let Some(rest) = ["time in ", "time at ", "what time is it in ", "what's the time in ", "date in ", "clock in "]
        .iter()
        .find_map(|p| l.strip_prefix(p))
    {
        Some(rest.to_string())
    } else if let Some(rest) = l.strip_suffix(" time") {
        Some(rest.to_string())
    } else {
        return vec![];
    };
    let local = chrono::Local::now();
    let (text, date, tzname, label, rel, copy) = match &place {
        Some(p) => {
            let Some(tz) = resolve_tz(p) else { return vec![] };
            let now = Utc::now().with_timezone(&tz);
            let diff = (now.offset().fix().local_minus_utc() - local.offset().local_minus_utc()) as f64 / 3600.0;
            let rel = if diff == 0.0 {
                "same time as here".to_string()
            } else {
                format!("{} h {} you", diff.abs(), if diff > 0.0 { "ahead of" } else { "behind" })
            };
            let label: String = p.split(' ').map(|w| {
                let mut c = w.chars();
                c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
            }).collect::<Vec<_>>().join(" ");
            (now.format("%-I:%M %p").to_string(), now.format("%A, %-d %B %Y").to_string(), format!("{} (UTC{})", tz.name(), now.format("%:z")), label, rel, now.format("%H:%M").to_string())
        }
        None => (
            local.format("%-I:%M %p").to_string(),
            local.format("%A, %-d %B %Y").to_string(),
            format!("UTC{}", local.format("%:z")),
            "Local".to_string(),
            "your local time".to_string(),
            local.format("%H:%M").to_string(),
        ),
    };
    vec![Item::new(format!("{text} — {label}"), format!("{date} · {tzname} · {rel}"), icon("preferences-system-time"), "Clock", 980.0, Action::Copy(copy))
        .key("clock")
        .preview(Preview::new(PreviewKind::Big, text, format!("{label}\n{date}\n{tzname}\n{rel}")))]
}

const SETTINGS: &[(&str, &str, &str, &str)] = &[
    ("Wi-Fi", "wifi", "wifi wireless network internet hotspot", "network-wireless"),
    ("Network", "network", "ethernet vpn proxy wired", "network-wired"),
    ("Bluetooth", "bluetooth", "headphones pair devices", "bluetooth"),
    ("Displays", "display", "monitor resolution screen night light scale", "video-display"),
    ("Sound", "sound", "audio volume speaker microphone output input", "audio-speakers"),
    ("Power", "power", "battery suspend energy", "battery"),
    ("Appearance", "background", "wallpaper theme dark mode background", "preferences-desktop-wallpaper"),
    ("Notifications", "notifications", "do not disturb alerts", "preferences-system-notifications"),
    ("Search Settings", "search", "search locations", "system-search"),
    ("Apps", "applications", "default apps permissions", "preferences-desktop-apps"),
    ("Privacy & Security", "privacy", "location camera microphone screen lock", "preferences-system-privacy"),
    ("Online Accounts", "online-accounts", "google microsoft accounts", "goa-panel"),
    ("Sharing", "sharing", "remote desktop file sharing", "preferences-system-sharing"),
    ("Mouse & Touchpad", "mouse", "trackpad scroll pointer", "input-mouse"),
    ("Keyboard", "keyboard", "shortcuts layout input source keybindings", "input-keyboard"),
    ("Printers", "printers", "printer print scanner", "printer"),
    ("Accessibility", "universal-access", "a11y zoom large text screen reader", "preferences-desktop-accessibility"),
    ("Multitasking", "multitasking", "workspaces hot corner", "preferences-system-windows"),
    ("Users", "users", "account password login avatar", "system-users"),
    ("Date & Time", "datetime", "clock timezone", "preferences-system-time"),
    ("Region & Language", "region", "locale language format", "preferences-desktop-locale"),
    ("System / About", "system", "about device info storage hostname update", "computer"),
    ("Color", "color", "color profile calibration", "preferences-color"),
    ("Ubuntu Desktop", "ubuntu", "dock panel desktop icons", "preferences-desktop"),
];

fn keyword_score(q: &str, name: &str, kw: &str, weight: f64) -> f64 {
    let k = kw.split(' ').map(|k| text_score(q, k, false)).fold(0.0, f64::max) * weight;
    text_score(q, name, false).max(k)
}

pub fn settings_items(q: &str) -> Vec<Item> {
    SETTINGS
        .iter()
        .filter_map(|(name, panel, kw, ic)| {
            let s = keyword_score(q, name, kw, 0.7);
            (s > 0.0).then(|| {
                Item::new(*name, "System Settings", icon(ic), "System Settings", s, Action::Spawn(vec!["gnome-control-center".into(), panel.to_string()]))
                    .key(format!("settings:{panel}"))
                    .preview(Preview::new(PreviewKind::App, *name, format!("Open the {name} settings panel.")))
            })
        })
        .collect()
}

pub fn system_items(q: &str) -> Vec<Item> {
    let sv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let cmds: Vec<(&str, &str, &str, Action)> = vec![
        ("Lock Screen", "lock screen", "system-lock-screen", Action::Spawn(sv(&["loginctl", "lock-session"]))),
        ("Sleep", "sleep suspend", "weather-clear-night", Action::Spawn(sv(&["systemctl", "suspend"]))),
        ("Log Out…", "log out logout sign out", "system-log-out", Action::Spawn(sv(&["gnome-session-quit", "--logout"]))),
        ("Restart…", "restart reboot", "system-reboot", Action::Spawn(sv(&["gnome-session-quit", "--reboot"]))),
        ("Shut Down…", "shut down shutdown power off poweroff", "system-shutdown", Action::Spawn(sv(&["gnome-session-quit", "--power-off"]))),
        ("Empty Trash", "empty trash bin", "user-trash", Action::Spawn(sv(&["gio", "trash", "--empty"]))),
        ("Open Trash", "trash bin recycle", "user-trash-full", Action::Spawn(sv(&["gio", "open", "trash:///"]))),
        ("Screenshot", "screenshot screen capture print screen", "applets-screenshooter",
         Action::Spawn(sv(&["gdbus", "call", "--session", "--dest", "org.gnome.Shell", "--object-path", "/org/gnome/Shell",
                            "--method", "org.gnome.Shell.Eval", "Main.screenshotUI.open()"]))),
        ("Toggle Dark Mode", "dark mode light mode theme toggle", "weather-clear-night", Action::ToggleDark),
    ];
    cmds.into_iter()
        .filter_map(|(name, kw, ic, action)| {
            let s = keyword_score(q, name, kw, 0.85);
            (s > 0.0).then(|| {
                Item::new(name, "System Command", icon(ic), "Commands", s, action)
                    .key(format!("cmd:{name}"))
                    .preview(Preview::new(PreviewKind::App, name, "System action"))
            })
        })
        .collect()
}

fn find_terminal() -> Option<Vec<String>> {
    for t in [["ptyxis", "--"], ["gnome-terminal", "--"], ["kgx", "-e"], ["x-terminal-emulator", "-e"], ["konsole", "-e"], ["xterm", "-e"]] {
        if env::var("PATH").unwrap_or_default().split(':').any(|d| Path::new(d).join(t[0]).exists()) {
            return Some(vec![t[0].into(), t[1].into()]);
        }
    }
    None
}

fn looks_like_url(s: &str) -> bool {
    if s.contains(' ') || s.len() < 4 {
        return false;
    }
    let rest = s.strip_prefix("https://").or_else(|| s.strip_prefix("http://")).unwrap_or(s);
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.split(':').next().unwrap_or("");
    if host == "localhost" {
        return true;
    }
    let parts: Vec<&str> = host.split('.').collect();
    parts.len() >= 2
        && parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_alphanumeric() || c == '-'))
        && parts.last().is_some_and(|tld| tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic()))
}

pub fn file_preview(path: &str) -> Preview {
    let mut p = Preview::new(PreviewKind::File, Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or(path.into()), "");
    p.path = Some(path.into());
    p
}

fn file_item(path: &str, is_dir: bool, subtitle: String, category: &'static str, score: f64) -> Item {
    let name = Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string());
    let mut it = Item::new(name, subtitle, Icon::File { path: path.into(), is_dir }, category, score, Action::OpenPath(path.into()))
        .key(format!("file:{path}"))
        .preview(file_preview(path));
    it.alt = Some(Action::RevealPath(path.into()));
    it
}

pub fn quick_items(q: &str) -> Vec<Item> {
    let s = q.trim();
    let mut out = vec![];
    if let Some(cmd) = s.strip_prefix('>') {
        let cmd = cmd.trim();
        if let (false, Some(mut term)) = (cmd.is_empty(), find_terminal()) {
            term.extend(["bash".into(), "-c".into(), format!("{cmd}; exec bash")]);
            out.push(Item::new(format!("Run “{cmd}”"), "Run in terminal", icon("utilities-terminal"), "Commands", 1100.0, Action::Spawn(term))
                .key("shell")
                .preview(Preview::new(PreviewKind::App, cmd, "Runs this command in a new terminal window.")));
        }
        return out;
    }
    if looks_like_url(s) {
        let url = if s.starts_with("http") { s.to_string() } else { format!("https://{s}") };
        out.push(Item::new(format!("Open {url}"), "Website", icon("web-browser"), "Websites", 970.0, Action::OpenUri(url.clone()))
            .key(format!("url:{url}"))
            .preview(Preview::new(PreviewKind::App, url, "Open in your default browser.")));
    }
    if s.starts_with('/') || s.starts_with('~') {
        let path = if let Some(r) = s.strip_prefix('~') { format!("{}{}", home(), r) } else { s.to_string() };
        if let Ok(md) = fs::metadata(&path) {
            out.push(file_item(&path, md.is_dir(), path.clone(), "Files", 960.0));
        }
    }
    out
}

pub fn web_fallbacks(q: &str) -> Vec<Item> {
    let s = q.trim();
    let name = engine().0;
    vec![
        Item::new(format!("Search {name} for “{s}”"), "Web Search", icon("web-browser"), "Search the Web", 5.0, Action::OpenUri(web_search_url(s)))
            .key("web")
            .preview(Preview::new(PreviewKind::App, s, format!("Search {name} in your browser."))),
        Item::new(format!("Search YouTube for “{s}”"), "Video", icon("video-x-generic"), "Search the Web", 4.0,
                  Action::OpenUri(format!("https://www.youtube.com/results?search_query={}", urlencoding::encode(s))))
            .key("yt"),
        Item::new(format!("Search Maps for “{s}”"), "Maps", icon("mark-location"), "Search the Web", 3.0,
                  Action::OpenUri(format!("https://www.google.com/maps/search/{}", urlencoding::encode(s))))
            .key("maps"),
    ]
}

// ------------------------------------------------------------------ files

const SKIP_DIRS: &[&str] = &[
    "node_modules", "__pycache__", "venv", ".venv", "site-packages", "dist-packages", "target", "build", ".git",
    ".cache", ".local", ".npm", ".cargo", ".rustup", "snap", ".mozilla", ".config", ".var", "Trash", ".gradle", ".m2",
];

pub struct FileEntry {
    name: String, // lowercase
    path: String,
    is_dir: bool,
}

pub type FileIndex = Arc<RwLock<Vec<FileEntry>>>;

pub fn start_file_index() -> FileIndex {
    let index: FileIndex = Arc::new(RwLock::new(Vec::new()));
    let idx = index.clone();
    thread::spawn(move || loop {
        let entries = scan_files();
        *idx.write().unwrap() = entries;
        thread::sleep(Duration::from_secs(600));
    });
    index
}

fn scan_files() -> Vec<FileEntry> {
    let cfg = config();
    let mut stack: Vec<PathBuf> = vec![PathBuf::from(home())];
    stack.extend(cfg.extra_index_paths.iter().map(|p| PathBuf::from(p.replacen('~', &home(), 1))));
    let mut out = Vec::with_capacity(100_000);
    while let Some(dir) = stack.pop() {
        if out.len() >= cfg.max_indexed_files {
            break;
        }
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if (name.starts_with('.') && !cfg.index_hidden) || SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            let Ok(ft) = e.file_type() else { continue };
            let path = e.path();
            if ft.is_dir() {
                stack.push(path.clone());
            }
            out.push(FileEntry { name: name.to_lowercase(), path: path.to_string_lossy().to_string(), is_dir: ft.is_dir() });
        }
    }
    out
}

pub fn file_items(index: &FileIndex, q: &str, recent: &HashSet<String>) -> Vec<Item> {
    let ql = q.trim().to_lowercase();
    let tokens: Vec<&str> = ql.split_whitespace().collect();
    if tokens.is_empty() || ql.len() < 2 {
        return vec![];
    }
    let (first, rest) = (tokens[0], &tokens[1..]);
    let seps: Vec<String> = [' ', '_', '-', '.'].iter().map(|c| format!("{c}{first}")).collect();
    let entries = index.read().unwrap();
    let mut hits: Vec<(f64, &FileEntry)> = entries
        .iter()
        .filter(|e| e.name.contains(first) && rest.iter().all(|t| e.name.contains(t)))
        .map(|e| {
            let stem = e.name.rsplit_once('.').map(|(s, _)| s).unwrap_or(&e.name);
            let mut s = if e.name == first || stem == first {
                60.0
            } else if e.name.starts_with(first) {
                54.0
            } else if seps.iter().any(|sep| e.name.contains(sep.as_str())) {
                45.0
            } else {
                30.0
            };
            s -= e.path.matches('/').count() as f64 * 0.8;
            if e.is_dir {
                s += 3.0;
            }
            if recent.contains(&e.path) {
                s += 25.0;
            }
            (s, e)
        })
        .collect();
    hits.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    let h = home();
    hits.into_iter()
        .take(10)
        .map(|(s, e)| {
            let dir = Path::new(&e.path).parent().map(|p| p.to_string_lossy().replacen(&h, "~", 1)).unwrap_or_default();
            file_item(&e.path, e.is_dir, dir, if e.is_dir { "Folders" } else { "Documents" }, s)
        })
        .collect()
}

/// Full-text document search through GNOME LocalSearch (Tracker).
pub fn content_items(q: &str, exclude: &HashSet<String>) -> Vec<Item> {
    if q.trim().len() < 3 {
        return vec![];
    }
    let mut cmd = std::process::Command::new("localsearch");
    cmd.args(["search", "-t", "-l", "6"]).args(q.split_whitespace());
    let Ok(out) = cmd.output() else { return vec![] };
    let h = home();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let line = strip_ansi(line);
            let uri = line.trim().split_whitespace().next()?;
            let path = urlencoding::decode(uri.strip_prefix("file://")?).ok()?.to_string();
            if exclude.contains(&path) || !Path::new(&path).exists() {
                return None;
            }
            let dir = Path::new(&path).parent().map(|p| p.to_string_lossy().replacen(&h, "~", 1)).unwrap_or_default();
            Some(file_item(&path, false, format!("Contents match · {dir}"), "Documents", 20.0))
        })
        .collect()
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            for x in it.by_ref() {
                if x.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

// ------------------------------------------------------------------ online providers

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(5))
            .user_agent("Lumen/2.0 (desktop launcher for Linux)")
            .build()
    })
}

fn get_json(url: &str) -> Option<Value> {
    let s = agent().get(url).call().ok()?.into_string().ok()?;
    serde_json::from_str(&s).ok()
}

pub fn get_bytes(url: &str) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    agent().get(url).call().ok()?.into_reader().take(10_000_000).read_to_end(&mut buf).ok()?;
    Some(buf)
}

use std::io::Read;

fn currency_code(word: &str) -> Option<String> {
    let w = word.to_lowercase();
    let named = match w.as_str() {
        "$" | "us$" | "dollar" | "dollars" | "usd" | "bucks" => "USD",
        "€" | "euro" | "euros" => "EUR",
        "£" | "sterling" => "GBP",
        "₹" | "rs" | "rupee" | "rupees" => "INR",
        "¥" | "yen" => "JPY",
        "yuan" | "rmb" => "CNY",
        "won" => "KRW",
        "franc" | "francs" => "CHF",
        "dirham" | "dirhams" => "AED",
        "riyal" => "SAR",
        "ruble" | "rubles" => "RUB",
        "peso" | "pesos" => "MXN",
        "real" | "reais" => "BRL",
        "lira" => "TRY",
        "baht" => "THB",
        "ringgit" => "MYR",
        "rand" => "ZAR",
        "taka" => "BDT",
        _ => "",
    };
    if !named.is_empty() {
        return Some(named.into());
    }
    (w.len() == 3 && w.chars().all(|c| c.is_ascii_alphabetic())).then(|| w.to_uppercase())
}

fn parse_currency(q: &str) -> Option<(f64, String, String)> {
    let l = q.trim().to_lowercase();
    let (left, right) = [" to ", " in ", " into ", " as "].iter().find_map(|sep| l.split_once(sep))?;
    let right = right.trim();
    let left = left.trim();
    let sym: String = left.chars().take_while(|c| "$€£₹¥".contains(*c)).collect();
    let after = left[sym.len()..].trim();
    let num_len = after.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ',')).unwrap_or(after.len());
    let num: f64 = after[..num_len].replace(',', "").parse().ok()?;
    let unit = after[num_len..].trim();
    let a = if !sym.is_empty() { currency_code(&sym)? } else { currency_code(unit)? };
    let b = currency_code(right)?;
    Some((num, a, b))
}

fn rates() -> Option<HashMap<String, f64>> {
    static RATES: Mutex<Option<(u64, HashMap<String, f64>)>> = Mutex::new(None);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    let mut guard = RATES.lock().ok()?;
    if let Some((ts, r)) = guard.as_ref() {
        if now - ts < 6 * 3600 {
            return Some(r.clone());
        }
    }
    let path = cache_dir().join("rates-rs.json");
    if let Some(v) = fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()) {
        let ts = v["ts"].as_u64().unwrap_or(0);
        if now - ts < 6 * 3600 {
            let r: HashMap<String, f64> = serde_json::from_value(v["rates"].clone()).ok()?;
            *guard = Some((ts, r.clone()));
            return Some(r);
        }
    }
    let data = get_json("https://open.er-api.com/v6/latest/USD")?;
    let r: HashMap<String, f64> = serde_json::from_value(data["rates"].clone()).ok()?;
    let _ = fs::write(&path, serde_json::json!({"ts": now, "rates": r}).to_string());
    *guard = Some((now, r.clone()));
    Some(r)
}

pub fn currency_items(q: &str) -> Vec<Item> {
    let Some((v, a, b)) = parse_currency(q) else { return vec![] };
    if calc::convert(q).is_some() {
        return vec![];
    }
    let Some(r) = rates() else { return vec![] };
    let (Some(ra), Some(rb)) = (r.get(&a), r.get(&b)) else { return vec![] };
    let val = v / ra * rb;
    let rate = rb / ra;
    let text = format!("{} {b}", calc::format_number((val * 100.0).round() / 100.0));
    vec![Item::new(text.clone(), format!("{} {a} =  ·  1 {a} = {:.4} {b}", calc::format_number(v), rate), icon("accessories-calculator"), "Conversion", 995.0, Action::Copy(format!("{val:.2}")))
        .key("currency")
        .preview(Preview::new(PreviewKind::Big, text, format!("{} {a} =\n\nRate: 1 {a} = {rate:.6} {b}\nSource: open.er-api.com (updated daily)", calc::format_number(v))))]
}

pub fn weather_items(q: &str) -> Vec<Item> {
    let l = q.trim().to_lowercase();
    let place = if let Some(r) = ["weather", "forecast", "temperature"].iter().find_map(|p| l.strip_prefix(p)) {
        if !(r.is_empty() || r.starts_with(' ')) {
            return vec![];
        }
        let r = r.trim();
        ["in ", "at ", "for "].iter().find_map(|p| r.strip_prefix(p)).unwrap_or(r).trim().to_string()
    } else if let Some(r) = l.strip_suffix(" weather").or_else(|| l.strip_suffix(" forecast")) {
        r.trim().to_string()
    } else {
        return vec![];
    };
    let Some(d) = get_json(&format!("https://wttr.in/{}?format=j1", urlencoding::encode(&place))) else { return vec![] };
    let cur = &d["current_condition"][0];
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    let area = &d["nearest_area"][0];
    let city = { let c = s(&area["areaName"][0]["value"]); if c.is_empty() { "Your location".into() } else { c } };
    let country = s(&area["country"][0]["value"]);
    let desc = s(&cur["weatherDesc"][0]["value"]);
    if desc.is_empty() {
        return vec![];
    }
    let temp = s(&cur["temp_C"]);
    let mut lines = vec![
        format!("{desc}, feels like {}°C", s(&cur["FeelsLikeC"])),
        format!("Humidity {}% · Wind {} km/h", s(&cur["humidity"]), s(&cur["windspeedKmph"])),
        String::new(),
    ];
    let days = d["weather"].as_array().cloned().unwrap_or_default();
    for day in days.iter().take(3) {
        let name = chrono::NaiveDate::parse_from_str(&s(&day["date"]), "%Y-%m-%d").map(|d| d.format("%A").to_string()).unwrap_or_default();
        lines.push(format!("{name}:  {}° – {}°C", s(&day["mintempC"]), s(&day["maxtempC"])));
    }
    let dl = desc.to_lowercase();
    let ic = if dl.contains("sun") || dl.contains("clear") {
        "weather-clear"
    } else if dl.contains("rain") || dl.contains("shower") {
        "weather-showers"
    } else if dl.contains("cloud") || dl.contains("overcast") {
        "weather-overcast"
    } else {
        "weather-few-clouds"
    };
    let sub = days.first().map(|t| format!("{country} · H {}° L {}°", s(&t["maxtempC"]), s(&t["mintempC"]))).unwrap_or(country.clone());
    vec![Item::new(format!("{temp}°C  {desc} — {city}"), sub, icon(ic), "Weather", 985.0, Action::OpenUri(format!("https://wttr.in/{}", urlencoding::encode(&place))))
        .key("weather")
        .preview(Preview::new(PreviewKind::Big, format!("{temp}°C"), format!("{city}, {country}\n\n{}", lines.join("\n"))))]
}

pub fn definition_items(q: &str) -> Vec<Item> {
    let l = q.trim().trim_end_matches('?').trim();
    let lower = l.to_lowercase();
    let explicit = ["define ", "meaning of ", "definition of "].iter().find_map(|p| lower.strip_prefix(p)).map(str::to_string);
    let word = match &explicit {
        Some(w) => w.clone(),
        None if l.len() >= 3 && l.chars().all(|c| c.is_ascii_alphabetic() || c == '-' || c == '\'') => lower.clone(),
        None => return vec![],
    };
    let Some(d) = get_json(&format!("https://api.dictionaryapi.dev/api/v2/entries/en/{}", urlencoding::encode(&word))) else { return vec![] };
    let Some(entry) = d.as_array().and_then(|a| a.first()) else { return vec![] };
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    let w = s(&entry["word"]);
    let phon = s(&entry["phonetic"]);
    let mut lines = vec![];
    let mut first = None;
    for m in entry["meanings"].as_array().into_iter().flatten().take(3) {
        let pos = s(&m["partOfSpeech"]);
        lines.push(pos.to_uppercase());
        for (i, def) in m["definitions"].as_array().into_iter().flatten().take(3).enumerate() {
            let text = s(&def["definition"]);
            lines.push(format!("  {}. {text}", i + 1));
            first.get_or_insert_with(|| format!("{pos} · {text}"));
            let ex = s(&def["example"]);
            if !ex.is_empty() {
                lines.push(format!("     “{ex}”"));
            }
        }
        lines.push(String::new());
    }
    let Some(first) = first else { return vec![] };
    let mut p = Preview::new(PreviewKind::Web, format!("{w}  {phon}").trim().to_string(), lines.join("\n").trim().to_string());
    p.meta.push(("Source".into(), "Free Dictionary API".into()));
    vec![Item::new(w.clone(), first.chars().take(140).collect::<String>(), icon("accessories-dictionary"), "Definition", if explicit.is_some() { 70.0 } else { 45.0 }, Action::OpenUri(web_search_url(&format!("define {w}"))))
        .key(format!("def:{w}"))
        .preview(p)]
}

pub fn wikipedia_items(q: &str) -> Vec<Item> {
    let qt = q.trim();
    if qt.len() < 3 {
        return vec![];
    }
    let url = format!(
        "https://en.wikipedia.org/w/api.php?action=query&format=json&formatversion=2&redirects=1&generator=search&gsrlimit=1\
         &prop=extracts|pageimages|info&inprop=url&exintro=1&explaintext=1&exsentences=4&piprop=thumbnail&pithumbsize=320&gsrsearch={}",
        urlencoding::encode(qt)
    );
    let Some(d) = get_json(&url) else { return vec![] };
    let Some(p) = d["query"]["pages"].as_array().and_then(|a| a.first()) else { return vec![] };
    let extract = p["extract"].as_str().unwrap_or("").trim().to_string();
    let title = p["title"].as_str().unwrap_or("").to_string();
    if extract.is_empty() || extract.chars().take(200).collect::<String>().contains("may refer to") {
        return vec![];
    }
    let ql = qt.to_lowercase();
    let ts = text_score(&ql, &title, true);
    if ts < 30.0 && !extract.to_lowercase().chars().take(200).collect::<String>().contains(&ql) {
        return vec![];
    }
    let first_sentence: String = extract.split(". ").next().unwrap_or("").chars().take(120).collect();
    let page_url = p["fullurl"].as_str().map(String::from).unwrap_or_else(|| format!("https://en.wikipedia.org/wiki/{}", urlencoding::encode(&title.replace(' ', "_"))));
    let mut pv = Preview::new(PreviewKind::Web, title.clone(), extract);
    pv.image_url = p["thumbnail"]["source"].as_str().map(String::from);
    pv.meta.push(("Source".into(), "Wikipedia".into()));
    vec![Item::new(title.clone(), first_sentence, icon("accessories-dictionary"), "Wikipedia", 50.0 + ts * 0.3, Action::OpenUri(page_url))
        .key(format!("wiki:{title}"))
        .preview(pv)]
}

pub fn suggestion_items(q: &str) -> Vec<Item> {
    let Some(d) = get_json(&format!("https://duckduckgo.com/ac/?type=list&q={}", urlencoding::encode(q))) else { return vec![] };
    let ql = q.trim().to_lowercase();
    d[1].as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str())
        .filter(|s| s.to_lowercase() != ql)
        .take(4)
        .enumerate()
        .map(|(i, s)| {
            Item::new(s, "Web Suggestion", icon("system-search"), "Web Suggestions", 10.0 - i as f64, Action::OpenUri(web_search_url(s)))
                .key(format!("sugg:{s}"))
                .preview(Preview::new(PreviewKind::App, s, "Search the web for this suggestion."))
        })
        .collect()
}

// ------------------------------------------------------------------ Claude

/// Path to the Claude Code CLI; the hotkey's environment may not have ~/.local/bin on PATH.
pub fn claude_bin() -> Option<PathBuf> {
    static BIN: OnceLock<Option<PathBuf>> = OnceLock::new();
    BIN.get_or_init(|| {
        env::var("PATH")
            .unwrap_or_default()
            .split(':')
            .map(|d| Path::new(d).join("claude"))
            .chain([Path::new(&home()).join(".local/bin/claude"), Path::new(&home()).join(".claude/local/claude")])
            .find(|p| p.exists())
    })
    .clone()
}

/// Strips "ask", "claude" or "ai" prefixes; returns (question, explicitly-asked).
pub fn claude_question(q: &str) -> (String, bool) {
    let t = q.trim();
    let lower = t.to_lowercase();
    for p in ["ask claude ", "ask ", "claude ", "ai "] {
        if lower.starts_with(p) {
            return (t[p.len()..].trim().to_string(), true);
        }
    }
    (t.to_string(), t.ends_with('?'))
}

pub fn claude_item(q: &str) -> Option<Item> {
    if !config().claude || claude_bin().is_none() || q.trim().len() < 2 || q.trim().starts_with('>') {
        return None;
    }
    let (question, explicit) = claude_question(q);
    if question.is_empty() {
        return None;
    }
    let mut it = Item::new(format!("Ask Claude: “{question}”"), "AI answer · press Enter", icon("dialog-information"), "Claude",
                           if explicit { 1050.0 } else { 6.0 }, Action::AskClaude(question.clone()))
        .key("claude");
    it.preview = Preview::new(PreviewKind::App, "Ask Claude", "");
    Some(it)
}

pub type Provider = fn(&str) -> Vec<Item>;
pub const ONLINE: &[Provider] = &[currency_items, weather_items, definition_items, wikipedia_items, suggestion_items];

#[allow(dead_code)]
fn _assert_tz_used() {
    let _ = Utc.timestamp_opt(0, 0);
}
