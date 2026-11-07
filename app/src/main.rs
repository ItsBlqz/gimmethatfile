#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use gtf_engine::{DownloadId, Manager, Settings, Snapshot, Status};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tauri::{Emitter, Manager as _, RunEvent, State};
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

#[tauri::command]
fn add_downloads(
    app: State<App>,
    urls: Vec<String>,
    dir: Option<String>,
    connections: Option<u32>,
) -> AddResult {
    let dir = dir.filter(|d| !d.trim().is_empty()).map(PathBuf::from);
    let mut added = 0;
    let mut errors = vec![];
    for url in urls.iter().map(|u| u.trim()).filter(|u| !u.is_empty()) {
        match app.0.add(url, dir.clone(), connections) {
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

#[tauri::command]
fn clear_completed(app: State<App>) {
    app.0.clear_completed()
}

#[tauri::command]
fn get_settings(app: State<App>) -> Settings {
    app.0.settings()
}

#[tauri::command]
fn set_settings(app: State<App>, settings: Settings) {
    app.0.set_settings(settings)
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

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
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

            // Push live progress to the UI.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut last_version = 0;
                loop {
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    let version = manager.version();
                    if manager.has_active() || version != last_version {
                        last_version = version;
                        let _ = handle.emit("downloads", manager.snapshot());
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
            pause_all,
            resume_all,
            remove,
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
