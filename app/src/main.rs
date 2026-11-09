#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod browser;
mod notify;

use gtf_engine::{AddOptions, DownloadId, Manager, Settings, Snapshot, Status};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager as _, RunEvent, State, WindowEvent};
use tauri_plugin_dialog::DialogExt;

struct App(Manager);

#[tauri::command]
fn list(app: State<App>) -> Vec<Snapshot> {
    app.0.snapshot()
}

#[derive(serde::Serialize)]
struct AddResult {
    added: usize,
    errors: Vec<String>,
}

/// Optional per-download settings from the add dialog's "More options".
#[derive(Default, serde::Deserialize)]
struct AddExtras {
    speed_limit: Option<u64>,
    expected_sha256: Option<String>,
}

#[tauri::command]
fn add_downloads(
    app: State<App>,
    urls: Vec<String>,
    dir: Option<String>,
    connections: Option<u32>,
    options: Option<AddExtras>,
) -> AddResult {
    let dir = dir.filter(|d| !d.trim().is_empty()).map(PathBuf::from);
    let extras = options.unwrap_or_default();
    let mut added = 0;
    let mut errors = vec![];
    for url in urls.iter().map(|u| u.trim()).filter(|u| !u.is_empty()) {
        let opts = AddOptions {
            dir: dir.clone(),
            connections,
            speed_limit: extras.speed_limit,
            // A hash only makes sense for a single file.
            expected_sha256: extras.expected_sha256.clone().filter(|_| urls.len() == 1),
            ..Default::default()
        };
        match app.0.add_with(url, opts) {
            Ok(_) => added += 1,
            Err(e) => errors.push(format!("{url}: {e:#}")),
        }
    }
    AddResult { added, errors }
}

#[tauri::command]
fn pause(app: State<App>, id: DownloadId) {
    app.0.pause(id)
}

#[tauri::command]
fn resume(app: State<App>, id: DownloadId) {
    app.0.resume(id)
}

/// Start now, even outside the scheduled hours.
#[tauri::command]
fn force_start(app: State<App>, id: DownloadId) {
    app.0.force_start(id)
}

#[tauri::command]
fn pause_all(app: State<App>) {
    app.0.pause_all()
}

#[tauri::command]
fn resume_all(app: State<App>) {
    app.0.resume_all()
}

#[tauri::command]
fn remove(app: State<App>, id: DownloadId, delete_file: bool) {
    app.0.remove(id, delete_file)
}

/// Bytes/second; `None` (or 0) removes the limit.
#[tauri::command]
fn set_speed_limit(app: State<App>, id: DownloadId, limit: Option<u64>) {
    app.0.set_speed_limit(id, limit)
}

#[tauri::command]
fn clear_completed(app: State<App>) {
    app.0.clear_completed()
}

#[tauri::command]
fn get_settings(app: State<App>) -> Settings {
    app.0.settings()
}

#[tauri::command]
fn set_settings(app: State<App>, settings: Settings) -> Result<(), String> {
    let start_with_windows = settings.start_with_windows;
    app.0.set_settings(settings);
    // Keep the startup entry in line with the setting. On failure, store
    // what is actually registered so the checkbox doesn't lie.
    if start_with_windows != autostart::is_enabled() {
        if let Err(e) = autostart::set(start_with_windows) {
            let mut s = app.0.settings();
            s.start_with_windows = autostart::is_enabled();
            app.0.set_settings(s);
            return Err(e);
        }
    }
    Ok(())
}

#[tauri::command]
async fn pick_folder(app: tauri::AppHandle, start: Option<String>) -> Option<String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut dialog = app.dialog().file();
    if let Some(s) = start.filter(|s| Path::new(s).is_dir()) {
        dialog = dialog.set_directory(s);
    }
    dialog.pick_folder(move |p| {
        let _ = tx.send(p);
    });
    rx.await
        .ok()
        .flatten()
        .and_then(|p| p.into_path().ok())
        .map(|p| p.display().to_string())
}

#[tauri::command]
fn open_file(app: State<App>, id: DownloadId) -> Result<(), String> {
    match app.0.file_path(id) {
        Some((path, Status::Completed)) if path.exists() => open_path(&path),
        _ => Err("The file isn't available yet.".into()),
    }
}

#[tauri::command]
fn reveal(app: State<App>, id: DownloadId) -> Result<(), String> {
    let (path, _) = app.0.file_path(id).ok_or("Not found")?;
    if path.is_file() {
        reveal_path(&path)
    } else {
        let dir = if path.is_dir() {
            path
        } else {
            path.parent().map(Path::to_path_buf).unwrap_or(path)
        };
        open_path(&dir)
    }
}

fn open_path(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    let mut cmd = Command::new("explorer");
    #[cfg(target_os = "macos")]
    let mut cmd = Command::new("open");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = Command::new("xdg-open");
    cmd.arg(path).spawn().map(|_| ()).map_err(|e| e.to_string())
}

fn reveal_path(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        Command::new("explorer")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        open_path(path.parent().unwrap_or(path))
    }
}

const TRAY_ID: &str = "main";

/// Bring the main window back from the tray (or from being minimized).
fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// Tray icon with a small menu. Closing the window only hides it, so this
/// menu's "Quit" is the way to actually exit.
fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show Gimme That File", true, None::<&str>)?;
    let pause_all = MenuItem::with_id(app, "pause_all", "Pause all", true, None::<&str>)?;
    let resume_all = MenuItem::with_id(app, "resume_all", "Resume all", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &show,
            &PredefinedMenuItem::separator(app)?,
            &pause_all,
            &resume_all,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    let mut tray = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("Gimme That File")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main(app),
            "pause_all" => app.state::<App>().0.pause_all(),
            "resume_all" => app.state::<App>().0.resume_all(),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

/// "2 downloading · 12.4 MB/s" for the tray tooltip.
fn tray_tooltip(snaps: &[Snapshot]) -> String {
    let active: Vec<_> = snaps
        .iter()
        .filter(|s| s.status == Status::Downloading)
        .collect();
    if active.is_empty() {
        return "Gimme That File".into();
    }
    let speed: f64 = active.iter().map(|s| s.speed).sum();
    format!(
        "Gimme That File
{} downloading · {:.1} MB/s",
        active.len(),
        speed / 1_048_576.0
    )
}

fn main() {
    tauri::Builder::default()
        // Must be first: a second launch just shows the running instance
        // (which may be hidden in the tray) instead of starting another one.
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if !autostart::launched_hidden(&args) {
                show_main(app);
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .on_window_event(|window, event| {
            // The close button hides to the tray; downloads keep running.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .setup(|app| {
            // GIMMETHATFILE_DATA_DIR lets developers run with a throwaway
            // download list instead of their real one.
            let data_dir = match std::env::var_os("GIMMETHATFILE_DATA_DIR") {
                Some(dir) => PathBuf::from(dir),
                None => app.path().app_data_dir()?,
            };
            std::fs::create_dir_all(&data_dir)?;
            let manager = tauri::async_runtime::block_on(async move {
                Manager::load(data_dir.join("downloads.json"))
            })?;
            app.manage(App(manager.clone()));
            build_tray(app)?;
            browser::start(manager.clone(), app.handle().clone());
            notify::spawn(app.handle().clone(), manager.clone(), show_main);

            // The window starts invisible (tauri.conf.json) so that a launch
            // at sign-in can stay in the tray without a flash.
            if !autostart::launched_hidden(std::env::args()) {
                show_main(app.handle());
            }

            // Push live progress to the UI.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut last_version = 0;
                loop {
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    let version = manager.version();
                    if manager.has_active() || version != last_version {
                        last_version = version;
                        let snaps = manager.snapshot();
                        if let Some(tray) = handle.tray_by_id(TRAY_ID) {
                            let _ = tray.set_tooltip(Some(tray_tooltip(&snaps)));
                        }
                        let _ = handle.emit("downloads", snaps);
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list,
            add_downloads,
            pause,
            resume,
            force_start,
            pause_all,
            resume_all,
            remove,
            set_speed_limit,
            clear_completed,
            get_settings,
            set_settings,
            pick_folder,
            open_file,
            reveal,
        ])
        .build(tauri::generate_context!())
        .expect("failed to start Gimme That File")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                app.state::<App>().0.shutdown();
            }
        });
}
