//! Background processing of the queue.

use eframe::egui;
use mediaanon_core::{CancelToken, Edits, Options, Processor, Report};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// One file to process.
#[derive(Debug, Clone)]
pub struct Task {
    pub input: PathBuf,
    pub output: PathBuf,
    /// Per-section settings (videos) and faces left visible.
    pub edits: Edits,
}

/// Messages from the worker to the UI.
#[derive(Debug)]
pub enum Event {
    /// The processor is ready; carries the inference device (if any).
    Ready(Option<String>),
    Started(PathBuf),
    Progress(PathBuf, Option<f32>),
    Done(PathBuf, Result<Box<Report>, String>),
    Skipped(PathBuf, String),
    /// The run could not start (invalid options, device unavailable...).
    Fatal(String),
    Finished,
}

/// Handle to a running batch.
pub struct Worker {
    pub events: Receiver<Event>,
    cancel: CancelToken,
}

/// Minimum interval between two progress messages for the same file.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

impl Worker {
    pub fn start(tasks: Vec<Task>, options: Options, overwrite: bool, ctx: egui::Context) -> Self {
        let (tx, events) = channel();
        let cancel = CancelToken::new();
        let token = cancel.clone();
        std::thread::Builder::new()
            .name("mediaanon-worker".into())
            .spawn(move || {
                let send = |e: Event| {
                    // The UI may already be gone at shutdown: ignore.
                    let _ = tx.send(e);
                    ctx.request_repaint();
                };
                run(tasks, options, overwrite, &token, &send, &tx, &ctx);
                send(Event::Finished);
            })
            .expect("cannot spawn worker thread");
        Self { events, cancel }
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
    }
}

fn run(
    tasks: Vec<Task>,
    options: Options,
    overwrite: bool,
    cancel: &CancelToken,
    send: &dyn Fn(Event),
    tx: &Sender<Event>,
    ctx: &egui::Context,
) {
    let processor = match Processor::new(options) {
        Ok(p) => p,
        Err(e) => return send(Event::Fatal(e.to_string())),
    };
    send(Event::Ready(processor.inference_device().map(String::from)));
    for task in tasks {
        if cancel.is_cancelled() {
            break;
        }
        if task.output.exists() && task.output != task.input && !overwrite {
            send(Event::Skipped(task.input, "output exists (enable overwrite)".into()));
            continue;
        }
        if let Some(dir) = task.output.parent() {
            if let Err(e) = std::fs::create_dir_all(dir) {
                send(Event::Done(task.input, Err(format!("cannot create {}: {e}", dir.display()))));
                continue;
            }
        }
        send(Event::Started(task.input.clone()));
        // The progress callback must be `Sync`; the sender is not, so guard it.
        let last = Mutex::new((Instant::now(), tx.clone()));
        let input = task.input.clone();
        let on_progress = |p: mediaanon_core::Progress| {
            let Ok(mut guard) = last.lock() else { return };
            if guard.0.elapsed() >= PROGRESS_INTERVAL {
                guard.0 = Instant::now();
                let _ = guard.1.send(Event::Progress(input.clone(), p.fraction()));
                ctx.request_repaint();
            }
        };
        let result = processor.process_file_with(&task.input, &task.output, &task.edits, &on_progress, cancel);
        send(Event::Done(task.input, result.map(Box::new).map_err(|e| e.to_string())));
    }
}
