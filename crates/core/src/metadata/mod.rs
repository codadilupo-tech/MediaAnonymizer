//! Lossless metadata removal for still images.
//!
//! All functions here operate on the *container* level (JPEG segments, PNG
//! chunks, RIFF chunks, TIFF directories). Pixel data is copied byte-for-byte,
//! so stripping metadata never degrades image quality.
//!
//! Which blocks are removed for each [`MetadataPolicy`] is documented in
//! `docs/PRIVACY.md`; keep that document in sync when changing the rules.

pub mod exif;
pub mod irb;
pub mod jpeg;
pub mod png;
pub mod webp;

use crate::options::MetadataPolicy;

/// What the metadata layer must achieve for one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    pub policy: MetadataPolicy,
    /// Faces are being redacted: every embedded preview, thumbnail or
    /// secondary image must go, because it still shows the unredacted faces.
    pub faces: bool,
}

impl Plan {
    pub fn new(policy: MetadataPolicy, faces: bool) -> Self {
        Self { policy, faces }
    }

    /// Remove everything that is not needed to render the image.
    pub fn strip_all(self) -> bool {
        self.policy == MetadataPolicy::All
    }

    /// Remove personally identifying fields (implied by [`Self::strip_all`]).
    pub fn strip_sensitive(self) -> bool {
        self.policy != MetadataPolicy::Keep
    }

    /// Remove thumbnails, previews, depth/gain maps and other secondary
    /// images. They can reveal faces we redact or content that was cropped
    /// out of the main image.
    pub fn strip_previews(self) -> bool {
        self.faces || self.policy != MetadataPolicy::Keep
    }

    /// Remove opaque blocks we cannot inspect (vendor segments, unknown
    /// chunks): they may contain any of the above.
    pub fn strip_unknown(self) -> bool {
        self.strip_previews()
    }

    /// Nothing to do at the metadata level.
    pub fn is_noop(self) -> bool {
        !self.strip_previews()
    }
}

/// Human readable log of what was changed, surfaced in the final report.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Changes {
    pub removed: Vec<String>,
    pub warnings: Vec<String>,
}

impl Changes {
    pub(crate) fn removed(&mut self, what: impl Into<String>) {
        self.removed.push(what.into());
    }

    pub(crate) fn warn(&mut self, what: impl Into<String>) {
        self.warnings.push(what.into());
    }
}

/// Formats a byte count for reports (`512 B`, `4.2 KiB`, `1.3 MiB`).
pub(crate) fn human_size(n: usize) -> String {
    const KIB: f64 = 1024.0;
    let n = n as f64;
    if n < KIB {
        format!("{n} B")
    } else if n < KIB * KIB {
        format!("{:.1} KiB", n / KIB)
    } else {
        format!("{:.1} MiB", n / KIB / KIB)
    }
}

/// One metadata block found by [`inspect`](crate::inspect).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Finding {
    /// Container-level block (e.g. `APP1 Exif`, `tEXt`, `EXIF`).
    pub block: String,
    /// Detail line (e.g. `GPS latitude`).
    pub detail: String,
    /// Whether the policy `sensitive` would remove it.
    pub sensitive: bool,
}
