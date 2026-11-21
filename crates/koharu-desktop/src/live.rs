//! Continuous desktop translation, using Koharu's page detector, OCR and
//! translator for each changed screen capture.

use std::{io::Cursor, sync::Arc, time::Duration};

use anyhow::Context as _;
use image::{DynamicImage, ImageFormat};
use koharu_pipeline::{Event, Outcome, Pipeline, Request, Stage, Target};
use koharu_scene::{Background, Edit, Hash, Op, PageId, ProjectId, Ref, Session};
use parking_lot::Mutex;
use serde::Serialize;
use specta::Type;
use tauri::{AppHandle, Emitter as _, Manager as _, WebviewUrl, WebviewWindowBuilder};
use tauri_runtime_cef::CefRuntime;
use tokio::sync::broadcast::error::RecvError;
use tokio_util::sync::CancellationToken;
use xcap::Monitor;

use crate::{app::App, events::LiveTranslationFrame};

const OVERLAY: &str = "live-translation-overlay";
const FRAME_DELAY: Duration = Duration::from_millis(900);
const SESSION_FRAMES: usize = 30;

struct LiveSession {
    pipeline: Pipeline,
    project: ProjectId,
    session: Arc<Mutex<Session>>,
    _directory: tempfile::TempDir,
    page: PageId,
    size: (u32, u32),
    frames: usize,
}

impl LiveSession {
    fn new(image: Vec<u8>, size: (u32, u32), pipeline: Pipeline) -> anyhow::Result<Self> {
        let directory = tempfile::tempdir().context("failed to create a temporary project")?;
        let mut session = Session::create(directory.path().join("frame.khr"))
            .context("failed to create a frame")?;
        let background = Hash::of(&image);
        session.apply(Edit {
            label: "live frame".to_owned(),
            ops: vec![
                Op::AddFile { bytes: image },
                Op::AddPage {
                    id: None,
                    index: 0,
                    size: None,
                    background: Some(background),
                },
            ],
        })?;
        let page = session
            .project()
            .pages()
            .next()
            .context("the captured frame has no page")?
            .id;
        let project = session.id();
        Ok(Self {
            pipeline,
            project,
            session: Arc::new(Mutex::new(session)),
            _directory: directory,
            page,
            size,
            frames: 1,
        })
    }

    fn replace(&mut self, image: Vec<u8>) -> anyhow::Result<()> {
        let background = Hash::of(&image);
        self.session.lock().apply(Edit {
            label: "live frame".to_owned(),
            ops: vec![
                Op::AddFile { bytes: image },
                Op::SetBackground {
                    page: Ref::from(self.page),
                    background: Some(Background {
                        file: background,
                        visible: true,
                    }),
                },
            ],
        })?;
        self.frames += 1;
        Ok(())
    }
}

impl Drop for LiveSession {
    fn drop(&mut self) {
        self.pipeline.close(self.project);
    }
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct LiveMonitor {
    pub index: usize,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub scale_factor: f64,
    pub is_primary: bool,
}

pub(super) fn live_monitors() -> anyhow::Result<Vec<LiveMonitor>> {
    Monitor::all()
        .context("failed to list displays")?
        .into_iter()
        .enumerate()
        .map(|(index, monitor)| {
            Ok(LiveMonitor {
                index,
                name: monitor
                    .friendly_name()
                    .context("failed to read display name")?,
                width: monitor.width().context("failed to read display width")?,
                height: monitor.height().context("failed to read display height")?,
                scale_factor: monitor
                    .scale_factor()
                    .context("failed to read display scale")? as f64,
                is_primary: monitor
                    .is_primary()
                    .context("failed to read primary display")?,
            })
        })
        .collect()
}

pub(super) async fn run(
    handle: AppHandle<CefRuntime>,
    app: Arc<App>,
    monitor_index: usize,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let (scale, width, height, x, y, name) = {
        let monitors = Monitor::all().context("failed to list displays")?;
        let monitor = monitors
            .get(monitor_index)
            .with_context(|| format!("display {monitor_index} no longer exists"))?;
        (
            monitor
                .scale_factor()
                .context("failed to read display scale")? as f64,
            monitor.width().context("failed to read display width")?,
            monitor.height().context("failed to read display height")?,
            monitor.x().context("failed to read display position")?,
            monitor.y().context("failed to read display position")?,
            monitor
                .friendly_name()
                .context("failed to read display name")?,
        )
    };

    let overlay = match handle.get_webview_window(OVERLAY) {
        Some(window) => window,
        None => WebviewWindowBuilder::new(
            &handle,
            OVERLAY,
            WebviewUrl::App(if cfg!(debug_assertions) {
                "stream-overlay".into()
            } else {
                "stream-overlay.html".into()
            }),
        )
        .title("Koharu Live Translation")
        .position(x as f64 / scale, y as f64 / scale)
        .inner_size(width as f64 / scale, height as f64 / scale)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .content_protected(true)
        .focused(false)
        .visible(false)
        .build()
        .context("failed to create translation overlay")?,
    };
    overlay.set_position(tauri::LogicalPosition::new(
        x as f64 / scale,
        y as f64 / scale,
    ))?;
    overlay.set_size(tauri::LogicalSize::new(
        width as f64 / scale,
        height as f64 / scale,
    ))?;
    overlay.set_ignore_cursor_events(true)?;
    overlay.show()?;
    overlay.set_always_on_top(true)?;

    let mut previous = None;
    let mut live_session: Option<LiveSession> = None;
    tracing::info!(%name, width, height, "live translation started");
    loop {
        let captured = tokio::task::spawn_blocking({
            move || {
                let monitor = Monitor::all()
                    .context("failed to list displays")?
                    .into_iter()
                    .nth(monitor_index)
                    .with_context(|| format!("display {monitor_index} no longer exists"))?;
                monitor.capture_image().context("failed to capture display")
            }
        })
        .await??;
        if cancel.is_cancelled() {
            let _ = overlay.hide();
            return Ok(());
        }
        let digest = blake3::hash(captured.as_raw());
        if previous != Some(digest) {
            previous = Some(digest);
            let mut encoded = Cursor::new(Vec::new());
            let thumbnail = DynamicImage::ImageRgba8(captured).thumbnail(1600, 1600);
            let overlay_scale = scale * thumbnail.width() as f64 / width as f64;
            let size = (thumbnail.width(), thumbnail.height());
            overlay.emit(
                crate::events::LIVE_TRANSLATION_FRAME.name,
                LiveTranslationFrame {
                    scale_factor: overlay_scale,
                    lines: Vec::new(),
                },
            )?;
            thumbnail
                .write_to(&mut encoded, ImageFormat::Png)
                .context("failed to encode captured display")?;
            let image = encoded.into_inner();
            if live_session
                .as_ref()
                .is_some_and(|session| session.frames >= SESSION_FRAMES || session.size != size)
            {
                live_session = None;
            }
            if let Some(session) = &mut live_session {
                session.replace(image)?;
            } else {
                live_session = Some(LiveSession::new(image, size, app.pipeline.clone())?);
            }
            let frame = live_session.as_ref().expect("a frame session was created");
            let page = frame.page;
            let session = Arc::clone(&frame.session);
            let mut events = app.pipeline.events();
            let run = app.pipeline.run(
                Arc::clone(&session),
                Request {
                    target: Target::Pages(vec![page]),
                    stages: [Stage::Detect, Stage::Recognize, Stage::Translate]
                        .into_iter()
                        .collect(),
                    config: None,
                },
            )?;

            let mut stopping = false;
            loop {
                tokio::select! {
                    _ = cancel.cancelled(), if !stopping => {
                        app.pipeline.stop_run(run);
                        stopping = true;
                    },
                    event = events.recv() => match event {
                        Ok(Event::Ended { run: ended, outcome }) if ended == run => {
                            if stopping || cancel.is_cancelled() {
                                overlay.emit(crate::events::LIVE_TRANSLATION_FRAME.name, LiveTranslationFrame {
                                    scale_factor: overlay_scale,
                                    lines: Vec::new(),
                                })?;
                                let _ = overlay.hide();
                                return Ok(());
                            } else {
                                match outcome {
                                    Outcome::Failed { reason, .. } => {
                                        tracing::warn!("live frame translation failed: {reason}");
                                        overlay.emit(crate::events::LIVE_TRANSLATION_FRAME.name, LiveTranslationFrame {
                                            scale_factor: overlay_scale,
                                            lines: Vec::new(),
                                        })?;
                                    }
                                    Outcome::Done => {
                                        let lines = session.lock().project().blocks(page)?
                                            .into_iter()
                                            .filter_map(|(_, block)| {
                                                let translated = block.translation.as_ref()?;
                                                let bounds = translated.bounds()?;
                                                let text = translated.content.text();
                                                (!text.trim().is_empty()).then_some(crate::events::LiveTranslationLine {
                                                    text,
                                                    x: bounds.x0,
                                                    y: bounds.y0,
                                                    width: bounds.width(),
                                                    height: bounds.height(),
                                                })
                                            })
                                            .collect();
                                        overlay.emit(crate::events::LIVE_TRANSLATION_FRAME.name, LiveTranslationFrame {
                                            scale_factor: overlay_scale,
                                            lines,
                                        })?;
                                    }
                                    Outcome::Stopped => return Ok(()),
                                }
                            }
                            break;
                        }
                        Ok(_) | Err(RecvError::Lagged(_)) => {}
                        Err(RecvError::Closed) => anyhow::bail!("the translation pipeline stopped"),
                    }
                }
            }
        }

        tokio::select! {
            _ = cancel.cancelled() => {
                overlay.emit(crate::events::LIVE_TRANSLATION_FRAME.name, LiveTranslationFrame {
                    scale_factor: scale,
                    lines: Vec::new(),
                })?;
                let _ = overlay.hide();
                return Ok(());
            }
            () = tokio::time::sleep(FRAME_DELAY) => {}
        }
    }
}
