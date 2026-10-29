//! # mediaanon-mobile
//!
//! A small C ABI over [`mediaanon_core`] for the Flutter app in `mobile/`,
//! which calls it through `dart:ffi`. See `include/mediaanon.h` for the
//! declarations.
//!
//! Conventions:
//!
//! * Strings are NUL-terminated UTF-8. Every `char *` returned by this library
//!   must be released with [`mediaanon_string_free`].
//! * Calls that can fail return a JSON document: `{"ok": <value>}` on success,
//!   `{"error": {"kind": "...", "message": "..."}}` otherwise. Panics are
//!   caught and reported as errors of kind `panic`.
//! * Options are the JSON form of [`mediaanon_core::Options`], exactly as the
//!   CLI's `--json` and the desktop app's settings use them.
//! * Every call is blocking and thread-safe; the app runs them on background
//!   isolates. Long operations can be cancelled through a cancel handle.
//!
//! Only photos are supported: the video pipeline drives `ffmpeg`/`ffprobe`
//! processes, which mobile platforms cannot spawn.

#![warn(missing_debug_implementations, rust_2018_idioms)]

use mediaanon_core::{CancelToken, Error, ImageFormat, MediaKind, Options, Processor};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::ffi::{c_char, CStr, CString};
use std::io::{Read, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

/// Failure reported to the app as `{"error": {...}}`.
#[derive(Debug, Serialize)]
struct Failure {
    /// Stable machine-readable category (`cancelled`, `unsupported`, `io`...).
    kind: &'static str,
    message: String,
}

impl Failure {
    fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }
}

impl From<Error> for Failure {
    fn from(e: Error) -> Self {
        let kind = match &e {
            Error::Io { .. } => "io",
            Error::Unsupported(_) => "unsupported",
            Error::Malformed { .. } => "malformed",
            Error::Codec(_) => "codec",
            Error::Detection(_) => "detection",
            Error::Hardware(_) => "hardware",
            Error::Ffmpeg(_) => "ffmpeg",
            Error::InvalidOptions(_) => "invalid-options",
            Error::Cancelled => "cancelled",
            _ => "other",
        };
        Failure::new(kind, e.to_string())
    }
}

type Outcome = Result<Value, Failure>;

/// Runs `f`, catching panics, and encodes the outcome as a JSON C string.
fn respond(f: impl FnOnce() -> Outcome) -> *mut c_char {
    let body = match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(value)) => json!({ "ok": value }),
        Ok(Err(failure)) => json!({ "error": failure }),
        Err(panic) => {
            let message = panic
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".into());
            json!({ "error": Failure::new("panic", format!("internal error: {message}")) })
        }
    };
    into_c_string(body.to_string())
}

fn into_c_string(s: String) -> *mut c_char {
    // JSON and our own strings never contain NUL bytes; strip them defensively.
    CString::new(s.replace('\0', "")).unwrap_or_default().into_raw()
}

/// Borrows a C string argument.
///
/// # Safety
/// `ptr` must be null or point to a NUL-terminated string that outlives the call.
unsafe fn arg<'a>(ptr: *const c_char, name: &str) -> Result<&'a str, Failure> {
    if ptr.is_null() {
        return Err(Failure::new("invalid-argument", format!("`{name}` is null")));
    }
    CStr::from_ptr(ptr).to_str().map_err(|_| Failure::new("invalid-argument", format!("`{name}` is not valid UTF-8")))
}

fn parse_options(json: &str) -> Result<Options, Failure> {
    let options: Options = serde_json::from_str(json)
        .map_err(|e| Failure::new("invalid-options", format!("cannot parse options: {e}")))?;
    options.validate()?;
    Ok(options)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Returns a processor for `options`, reusing the previous one when the
/// options did not change so the face detector is loaded only once.
fn processor(options: Options) -> Result<Arc<Processor>, Failure> {
    static CACHE: Mutex<Option<Arc<Processor>>> = Mutex::new(None);
    let mut cache = lock(&CACHE);
    if let Some(p) = cache.as_ref().filter(|p| *p.options() == options) {
        return Ok(Arc::clone(p));
    }
    let p = Arc::new(Processor::new(options)?);
    *cache = Some(Arc::clone(&p));
    Ok(p)
}

/// Checks that `path` is a photo this build can process and returns its format.
///
/// HEIC/HEIF photos (the iPhone default) and videos are ISO-BMFF files, which
/// the core would treat as videos; they get a precise message instead.
fn photo_format(path: &Path) -> Result<ImageFormat, Failure> {
    let mut head = [0u8; 16];
    let n =
        std::fs::File::open(path).and_then(|mut f| f.read(&mut head)).map_err(|e| Failure::from(Error::io(path, e)))?;
    let head = &head[..n];
    if head.len() >= 12 && &head[4..8] == b"ftyp" {
        let brand = &head[8..12];
        if [b"heic", b"heix", b"heim", b"heis", b"hevc", b"hevx", b"mif1", b"msf1", b"avif"].iter().any(|b| *b == brand)
        {
            return Err(Failure::new(
                "unsupported",
                "HEIC/HEIF/AVIF photos are not supported yet: export the photo as JPEG, or set your camera to the \
                 \"Most Compatible\" format",
            ));
        }
    }
    match mediaanon_core::media::detect(path)? {
        MediaKind::Image(format) => Ok(format),
        MediaKind::Video => Err(Failure::new("unsupported", "videos are not supported by the mobile app yet")),
    }
}

/// Live cancel handles, by id.
fn cancel_tokens() -> &'static Mutex<HashMap<u64, CancelToken>> {
    static TOKENS: std::sync::OnceLock<Mutex<HashMap<u64, CancelToken>>> = std::sync::OnceLock::new();
    TOKENS.get_or_init(Default::default)
}

fn cancel_token(id: u64) -> CancelToken {
    if id == 0 {
        return CancelToken::new();
    }
    lock(cancel_tokens()).get(&id).cloned().unwrap_or_default()
}

/// Version of the library, e.g. `0.1.0`. Free with [`mediaanon_string_free`].
#[no_mangle]
pub extern "C" fn mediaanon_version() -> *mut c_char {
    into_c_string(env!("CARGO_PKG_VERSION").to_string())
}

/// Releases a string returned by this library. Null is ignored.
///
/// # Safety
/// `s` must be null or a pointer returned by this library, freed only once.
#[no_mangle]
pub unsafe extern "C" fn mediaanon_string_free(s: *mut c_char) {
    if !s.is_null() {
        drop(CString::from_raw(s));
    }
}

/// Creates a cancel handle (never 0). Release it with [`mediaanon_cancel_free`].
#[no_mangle]
pub extern "C" fn mediaanon_cancel_new() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    lock(cancel_tokens()).insert(id, CancelToken::new());
    id
}

/// Requests cancellation of the operations using `handle`. Unknown handles are ignored.
#[no_mangle]
pub extern "C" fn mediaanon_cancel(handle: u64) {
    if let Some(token) = lock(cancel_tokens()).get(&handle) {
        token.cancel();
    }
}

/// Releases a cancel handle. Operations still using it keep working.
#[no_mangle]
pub extern "C" fn mediaanon_cancel_free(handle: u64) {
    lock(cancel_tokens()).remove(&handle);
}

/// Lists the metadata found in a photo.
///
/// Returns `{"ok": {"format": "JPEG", "findings": [{"block", "detail", "sensitive"}...]}}`.
///
/// # Safety
/// `path` must be a valid NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn mediaanon_inspect(path: *const c_char) -> *mut c_char {
    respond(|| {
        let path = Path::new(arg(path, "path")?);
        let format = photo_format(path)?;
        let findings = mediaanon_core::inspect(path)?;
        Ok(json!({ "format": format.name(), "findings": findings }))
    })
}

/// Processes the photo at `input` and writes the result to `output`
/// atomically. `cancel` is a handle from [`mediaanon_cancel_new`], or 0.
///
/// Returns `{"ok": <report>}`, see [`mediaanon_core::Report`].
///
/// # Safety
/// The string arguments must be valid NUL-terminated strings.
#[no_mangle]
pub unsafe extern "C" fn mediaanon_process(
    options: *const c_char,
    input: *const c_char,
    output: *const c_char,
    cancel: u64,
) -> *mut c_char {
    respond(|| {
        let options = parse_options(arg(options, "options")?)?;
        let input = Path::new(arg(input, "input")?);
        let output = Path::new(arg(output, "output")?);
        photo_format(input)?;
        let token = cancel_token(cancel);
        token.check()?;
        let report = processor(options)?.process_file(input, output, &|_| {}, &token)?;
        serde_json::to_value(report).map_err(|e| Failure::new("other", e.to_string()))
    })
}

/// Renders an upright preview of `input` (longest side `max_side`) with the
/// faces redacted and the timestamp drawn, and saves it as a JPEG to `output`.
/// Needs face redaction and/or the timestamp enabled in `options`.
///
/// Returns `{"ok": {"faces": n, "width": w, "height": h}}`.
///
/// # Safety
/// The string arguments must be valid NUL-terminated strings.
#[no_mangle]
pub unsafe extern "C" fn mediaanon_preview(
    options: *const c_char,
    input: *const c_char,
    output: *const c_char,
    max_side: u32,
) -> *mut c_char {
    respond(|| {
        let options = parse_options(arg(options, "options")?)?;
        let input = Path::new(arg(input, "input")?);
        let output = Path::new(arg(output, "output")?);
        photo_format(input)?;
        let (image, faces) = processor(options)?.preview(input, max_side.max(1))?;
        let rgb = image.to_rgb8();
        let mut file =
            std::io::BufWriter::new(std::fs::File::create(output).map_err(|e| Failure::from(Error::io(output, e)))?);
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut file, 90)
            .encode_image(&rgb)
            .map_err(|e| Failure::from(Error::from(e)))?;
        file.flush().map_err(|e| Failure::from(Error::io(output, e)))?;
        Ok(json!({ "faces": faces, "width": rgb.width(), "height": rgb.height() }))
    })
}
