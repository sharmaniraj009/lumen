//! Hotkey client: tells the running Lumen daemon to show/hide over D-Bus. It links only
//! GIO (not GTK), so it starts in a few milliseconds. If the daemon isn't running it
//! starts `lumen` from the same directory.

use gtk::gio;
use gtk::glib::{self, prelude::*};

const APP_ID: &str = "io.github.lumen.Launcher";
const OBJECT_PATH: &str = "/io/github/lumen/Launcher";

fn main() {
    let action = if std::env::args().any(|a| a == "--quit") { "quit" } else { "toggle" };
    if let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
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
        // NO_AUTO_START: fail fast instead of waiting when the daemon isn't running.
        let sent = bus.call_sync(Some(APP_ID), OBJECT_PATH, "org.gtk.Actions", "Activate", Some(&params), None,
                                 gio::DBusCallFlags::NO_AUTO_START, 2000, gio::Cancellable::NONE);
        if sent.is_ok() || action == "quit" {
            return;
        }
    }
    let lumen = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("lumen")));
    if let Some(lumen) = lumen {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(lumen).exec();
        eprintln!("lumen-toggle: could not start lumen: {err}");
    }
}
