//! Desktop notifications when a download finishes, fails, or fails its
//! checksum. Driven by [`Manager::subscribe`], which only reports downloads
//! that finish while the app runs (never pauses or removals).

use gtf_engine::{Event, Manager};
use tauri::AppHandle;
use tauri_plugin_notification::NotificationExt;
use tokio::sync::broadcast::error::RecvError;

/// Title and body of the notification for an event.
fn text(event: &Event) -> (&'static str, String) {
    match event {
        Event::Completed { file_name, .. } => ("Download complete", file_name.clone()),
        Event::ChecksumMismatch { file_name, .. } => ("Checksum mismatch", file_name.clone()),
        Event::Failed {
            file_name, error, ..
        } => {
            let mut error: String = error.chars().take(100).collect();
            if error.is_empty() {
                error = "unknown error".into();
            }
            ("Download failed", format!("{file_name}\n{error}"))
        }
    }
}

/// Show notifications for download events while `notify_on_complete` is on.
/// Clicking a notification calls `on_click` (Windows reports clicks only
/// while the toast is on screen, not from the Action Center).
pub fn spawn(
    app: AppHandle,
    manager: Manager,
    on_click: impl Fn(&AppHandle) + Send + Sync + 'static,
) {
    let handle = app.clone();
    let _ = app.notification().on_action(move |action| {
        if action.action_id() == "tap" {
            on_click(&handle);
        }
    });
    let mut events = manager.subscribe();
    tauri::async_runtime::spawn(async move {
        loop {
            let event = match events.recv().await {
                Ok(event) => event,
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            };
            if !manager.settings().notify_on_complete {
                continue;
            }
            let (title, body) = text(&event);
            let _ = app.notification().builder().title(title).body(body).show();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_texts() {
        let (t, b) = text(&Event::Completed {
            id: 1,
            file_name: "file.zip".into(),
        });
        assert_eq!((t, b.as_str()), ("Download complete", "file.zip"));
        let (t, b) = text(&Event::Failed {
            id: 1,
            file_name: "file.zip".into(),
            error: "x".repeat(500),
        });
        assert_eq!(t, "Download failed");
        assert!(b.starts_with("file.zip\n") && b.len() <= 9 + 100);
    }
}
