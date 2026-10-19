//! Pen setup: the persisted pen API setting, starting the reader, and its status text.

use std::path::Path;

use eframe::egui;

use crate::i18n::{tr, tr_args};
use crate::input::{PenApi, PenEvent, WintabState};

/// Name of the settings file in the app data folder.
const SETTINGS_FILE: &str = "settings.txt";

/// The pen API saved in `dir`; WinTab when none is saved.
pub fn load_setting(dir: &Path) -> PenApi {
    let text = std::fs::read_to_string(dir.join(SETTINGS_FILE)).unwrap_or_default();
    match text.lines().find_map(|l| l.strip_prefix("pen = ")) {
        Some("ink") => PenApi::Ink,
        _ => PenApi::WinTab,
    }
}

/// Saves the pen API in `dir`.
pub fn save_setting(dir: &Path, api: PenApi) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let v = match api {
        PenApi::WinTab => "wintab",
        PenApi::Ink => "ink",
    };
    std::fs::write(dir.join(SETTINGS_FILE), format!("pen = {v}\n"))
}

/// A command-line override: `ink` or `wintab` as the first argument.
pub fn cli_override() -> Option<PenApi> {
    match std::env::args().nth(1).as_deref() {
        Some("ink") => Some(PenApi::Ink),
        Some("wintab") => Some(PenApi::WinTab),
        _ => None,
    }
}

pub fn api_name(api: PenApi) -> String {
    tr(match api {
        PenApi::WinTab => "pen-api-wintab",
        PenApi::Ink => "pen-api-ink",
    })
}

/// Status bar text: the active API and the WinTab device or why WinTab is not used.
pub fn status(api: PenApi, wintab: &WintabState) -> String {
    let note = match wintab {
        WintabState::Device { name, pressure } => tr_args(
            "pen-device",
            &[
                ("name", name),
                ("min", &pressure.0.to_string()),
                ("max", &pressure.1.to_string()),
            ],
        ),
        WintabState::NotSelected => tr("pen-wintab-not-selected"),
        WintabState::NoDriver => tr("pen-wintab-no-driver"),
        WintabState::BadDriver => tr("pen-wintab-bad-driver"),
        WintabState::NoService => tr("pen-wintab-no-service"),
        WintabState::NoContext => tr("pen-wintab-no-context"),
    };
    tr_args("status-pen", &[("api", &api_name(api)), ("note", &note)])
}

#[cfg(windows)]
pub fn start(cc: &eframe::CreationContext, setting: PenApi) -> String {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(RawWindowHandle::Win32(h)) = cc.window_handle().map(|h| h.as_raw()) else {
        return String::new();
    };
    let s = crate::pen_win::install(h.hwnd.get(), setting, cc.egui_ctx.clone());
    log::info!("pen api {:?}, wintab {:?}", s.api, s.wintab);
    status(s.api, &s.wintab)
}

#[cfg(not(windows))]
pub fn start(_: &eframe::CreationContext, _: PenApi) -> String {
    String::new()
}

#[cfg(windows)]
pub fn drain() -> Vec<PenEvent> {
    crate::pen_win::drain()
}

#[cfg(not(windows))]
pub fn drain() -> Vec<PenEvent> {
    Vec::new()
}

/// View menu entries that choose the pen API; returns the new choice.
pub fn menu(ui: &mut egui::Ui, current: PenApi) -> Option<PenApi> {
    ui.label(tr("menu-view-pen"));
    let mut pick = None;
    for api in [PenApi::WinTab, PenApi::Ink] {
        if ui.radio(current == api, api_name(api)).clicked() && current != api {
            pick = Some(api);
        }
    }
    pick
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pen_setting_is_saved_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_setting(dir.path()), PenApi::WinTab);
        save_setting(dir.path(), PenApi::Ink).unwrap();
        assert_eq!(load_setting(dir.path()), PenApi::Ink);
        save_setting(dir.path(), PenApi::WinTab).unwrap();
        assert_eq!(load_setting(dir.path()), PenApi::WinTab);
    }

    #[test]
    fn status_names_the_api_and_device() {
        let s = status(
            PenApi::WinTab,
            &WintabState::Device {
                name: "Cintiq".into(),
                pressure: (0, 8191),
            },
        );
        assert!(
            s.contains("WinTab") && s.contains("Cintiq") && s.contains("8191"),
            "{s}"
        );
        assert!(!status(PenApi::Ink, &WintabState::NoDriver).contains("pen-"));
    }
}
