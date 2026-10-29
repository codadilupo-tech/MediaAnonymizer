//! The list of files to process and their status.

use mediaanon_core::{media, Edits, FaceTrack, MediaKind, Report, VideoInfo};
use std::path::{Path, PathBuf};

/// Processing state of one file.
#[derive(Debug, Clone)]
pub enum Status {
    Pending,
    Running(Option<f32>),
    Done(Box<Report>),
    Skipped(String),
    Failed(String),
}

impl Status {
    pub fn is_finished(&self) -> bool {
        matches!(self, Status::Done(_) | Status::Skipped(_) | Status::Failed(_))
    }
}

/// A file in the queue.
#[derive(Debug, Clone)]
pub struct Item {
    pub path: PathBuf,
    /// `None` when the type could not be determined.
    pub kind: Option<MediaKind>,
    pub status: Status,
    /// Per-section settings and cuts (videos), faces left visible.
    pub edits: Edits,
    /// Videos: where the faces marked visible are followed, and for which
    /// edits and options this was computed.
    pub tracks: Option<Tracks>,
    /// Videos: playhead position in seconds.
    pub position: f64,
    /// Videos: duration and frame rate, loaded in the background.
    pub info: Option<Result<VideoInfo, String>>,
}

impl Item {
    pub fn name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    pub fn is_image(&self) -> bool {
        matches!(self.kind, Some(MediaKind::Image(_)))
    }

    pub fn is_video(&self) -> bool {
        matches!(self.kind, Some(MediaKind::Video))
    }

    pub fn kind_label(&self) -> &'static str {
        match self.kind {
            Some(MediaKind::Image(f)) => f.name(),
            Some(MediaKind::Video) => "Video",
            None => "?",
        }
    }
}

/// Result of a tracking pass of the visible faces of a video.
#[derive(Debug, Clone)]
pub struct Tracks {
    /// What the pass was computed for: `(edits, options)`.
    pub key: (Edits, mediaanon_core::Options),
    pub result: Result<Vec<FaceTrack>, String>,
}

/// Ordered list of files, without duplicates.
#[derive(Debug, Default)]
pub struct Queue {
    pub items: Vec<Item>,
}

impl Queue {
    /// Adds files, and the supported files found (recursively) in folders.
    /// Returns the number of files added.
    pub fn add_paths(&mut self, paths: impl IntoIterator<Item = PathBuf>) -> usize {
        let before = self.items.len();
        for p in paths {
            if p.is_dir() {
                let files = walkdir::WalkDir::new(&p)
                    .sort_by_file_name()
                    .into_iter()
                    .filter_map(Result::ok)
                    .filter(|e| e.file_type().is_file() && media::has_supported_extension(e.path()))
                    .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
                    .map(|e| e.into_path());
                for f in files.collect::<Vec<_>>() {
                    self.add_file(f);
                }
            } else if p.is_file() {
                self.add_file(p);
            }
        }
        self.items.len() - before
    }

    fn add_file(&mut self, path: PathBuf) {
        if self.items.iter().any(|i| i.path == path) {
            return;
        }
        let kind = media::detect(&path).ok();
        self.items.push(Item {
            path,
            kind,
            status: Status::Pending,
            edits: Edits::default(),
            tracks: None,
            position: 0.0,
            info: None,
        });
    }

    /// Whether the item can be queued again (it failed or was skipped).
    pub fn can_retry(item: &Item) -> bool {
        matches!(item.status, Status::Failed(_) | Status::Skipped(_))
    }

    /// Whether Start processes the item: anything not done yet, including
    /// failed and skipped files, so that Start also retries them.
    pub fn will_run(item: &Item) -> bool {
        !matches!(item.status, Status::Done(_) | Status::Running(_))
    }

    /// Puts a failed or skipped item back to pending.
    pub fn retry(&mut self, idx: usize) {
        if let Some(i) = self.items.get_mut(idx).filter(|i| Self::can_retry(i)) {
            i.status = Status::Pending;
        }
    }

    /// Puts every failed item back to pending; returns how many.
    pub fn retry_failed(&mut self) -> usize {
        let mut n = 0;
        for i in self.items.iter_mut().filter(|i| matches!(i.status, Status::Failed(_))) {
            i.status = Status::Pending;
            n += 1;
        }
        n
    }

    pub fn clear_finished(&mut self) {
        self.items.retain(|i| !i.status.is_finished());
    }

    pub fn index_of(&self, path: &Path) -> Option<usize> {
        self.items.iter().position(|i| i.path == path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(status: Status) -> Item {
        Item {
            path: PathBuf::from("x.jpg"),
            kind: None,
            status,
            edits: Edits::default(),
            tracks: None,
            position: 0.0,
            info: None,
        }
    }

    #[test]
    fn retry_resets_failed_and_skipped_only() {
        let mut q = Queue {
            items: vec![
                item(Status::Failed("boom".into())),
                item(Status::Skipped("exists".into())),
                item(Status::Pending),
            ],
        };
        q.retry(1);
        assert!(matches!(q.items[1].status, Status::Pending));
        assert_eq!(q.retry_failed(), 1);
        assert!(q.items.iter().all(|i| matches!(i.status, Status::Pending)));
    }

    #[test]
    fn start_reruns_everything_but_done() {
        assert!(Queue::will_run(&item(Status::Pending)));
        assert!(Queue::will_run(&item(Status::Failed("boom".into()))));
        assert!(Queue::will_run(&item(Status::Skipped("exists".into()))));
        assert!(!Queue::will_run(&item(Status::Running(None))));
    }
}
