mod feed;

use tauri::{Emitter, Manager};

use crate::feed::{start_feed as start_feed_runtime, FeedState};

#[tauri::command]
fn start_feed(app: tauri::AppHandle, state: tauri::State<'_, FeedState>) -> Result<(), String> {
    let url = feed::configured_ws_url()?;
    start_feed_runtime(app, state.inner().clone(), url);
    Ok(())
}

#[tauri::command]
fn stop_feed_command(state: tauri::State<'_, FeedState>) {
    state.stop();
}

#[tauri::command]
fn feed_status_command(state: tauri::State<'_, FeedState>) -> feed::FeedStatus {
    state.status()
}

#[tauri::command]
fn configured_ws_url_command() -> Result<String, String> {
    feed::configured_ws_url()
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    tauri::Builder::default()
        .manage(FeedState::default())
        .invoke_handler(tauri::generate_handler![
            start_feed,
            stop_feed_command,
            feed_status_command,
            configured_ws_url_command
        ])
        .setup(|app| {
            let state = app.state::<FeedState>().inner().clone();
            let handle = app.handle().clone();
            match feed::configured_ws_url() {
                Ok(url) => start_feed_runtime(handle, state, url),
                Err(error) => {
                    let _ = handle.emit("feed-event", feed::FeedEvent::error(error));
                }
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .map_err(Into::into)
}
