use std::sync::Arc;

use tauri::{Emitter as _, Manager as _, State};
use tokio_util::sync::CancellationToken;

use super::Result;
use crate::{app::App, live::LiveMonitor};

#[tauri::command]
#[specta::specta]
pub fn live_monitors() -> Result<Vec<LiveMonitor>> {
    Ok(crate::live::live_monitors()?)
}

#[tauri::command]
#[specta::specta]
pub async fn start_live_translation(
    state: State<'_, Arc<App>>,
    monitor_index: usize,
) -> Result<()> {
    let app = Arc::clone(&state);
    let handle = app.app_handle()?;
    let cancel = CancellationToken::new();
    let generation;
    {
        let mut running = app.live_cancel.lock();
        if running.is_some() {
            return Err(anyhow::anyhow!("live translation is already running").into());
        }
        generation = app
            .live_generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        *running = Some((generation, cancel.clone()));
    }
    let _ = handle.emit(crate::events::LIVE_TRANSLATION_STATUS.name, true);

    tauri::async_runtime::spawn(async move {
        if let Err(error) = crate::live::run(
            handle.clone(),
            Arc::clone(&app),
            monitor_index,
            cancel.clone(),
        )
        .await
        {
            tracing::error!("live translation ended: {error:#}");
        }
        if let Some(overlay) = handle.get_webview_window("live-translation-overlay") {
            let _ = overlay.emit(
                crate::events::LIVE_TRANSLATION_FRAME.name,
                crate::events::LiveTranslationFrame {
                    scale_factor: 1.0,
                    lines: Vec::new(),
                },
            );
            let _ = overlay.hide();
        }
        let mut running = app.live_cancel.lock();
        if running
            .as_ref()
            .is_some_and(|(active, _)| *active == generation)
        {
            *running = None;
        }
        let _ = handle.emit(crate::events::LIVE_TRANSLATION_STATUS.name, false);
    });
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn stop_live_translation(state: State<'_, Arc<App>>) {
    if let Some((_, cancel)) = state.live_cancel.lock().as_ref() {
        cancel.cancel();
    }
}
