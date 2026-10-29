//! Face detection.
//!
//! The detector is [YuNet](https://github.com/ShiqiYu/libfacedetection), a
//! tiny (230 KB) yet accurate face detector that also predicts five facial
//! landmarks (eyes, nose, mouth corners). It runs on ONNX Runtime, which
//! provides the CPU and GPU back-ends (CUDA, TensorRT, DirectML, Core ML,
//! OpenVINO, ROCm).
//!
//! Detection works on a downscaled RGB copy of the frame ([`RgbFrame`]);
//! callers map the resulting coordinates back to full resolution.

mod geometry;
mod runtime;
mod yunet;

pub use geometry::{Face, Point};
pub use runtime::{available_devices, DeviceInfo};
pub use yunet::YuNet;

use crate::options::{ComputeOptions, FaceOptions, InferenceDevice};
use crate::Result;
use std::sync::{Condvar, Mutex};

/// A packed 8-bit RGB image used as detector input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbFrame {
    pub width: u32,
    pub height: u32,
    /// `width * height * 3` bytes, row-major, no padding.
    pub data: Vec<u8>,
}

impl RgbFrame {
    /// Builds a frame of size `dst_w`×`dst_h` by box-filtering (averaging)
    /// the source pixels returned by `sample(x, y)`.
    ///
    /// Area averaging preserves small faces much better than nearest
    /// neighbour sampling when shrinking large photos.
    pub fn downscale<F>(src_w: u32, src_h: u32, dst_w: u32, dst_h: u32, sample: F) -> Self
    where
        F: Fn(u32, u32) -> [u8; 3] + Sync,
    {
        use rayon::prelude::*;
        let mut data = vec![0u8; dst_w as usize * dst_h as usize * 3];
        let sx = src_w as f32 / dst_w as f32;
        let sy = src_h as f32 / dst_h as f32;
        // Cap the number of taps per axis: beyond 4 the result is visually identical.
        let taps_x = (sx.ceil() as u32).clamp(1, 4);
        let taps_y = (sy.ceil() as u32).clamp(1, 4);
        data.par_chunks_mut(dst_w as usize * 3).enumerate().for_each(|(y, row)| {
            for x in 0..dst_w as usize {
                let mut acc = [0u32; 3];
                for ty in 0..taps_y {
                    let py = (((y as f32 + (ty as f32 + 0.5) / taps_y as f32) * sy) as u32).min(src_h - 1);
                    for tx in 0..taps_x {
                        let px = (((x as f32 + (tx as f32 + 0.5) / taps_x as f32) * sx) as u32).min(src_w - 1);
                        let p = sample(px, py);
                        acc[0] += p[0] as u32;
                        acc[1] += p[1] as u32;
                        acc[2] += p[2] as u32;
                    }
                }
                let n = taps_x * taps_y;
                row[x * 3..x * 3 + 3].copy_from_slice(&[(acc[0] / n) as u8, (acc[1] / n) as u8, (acc[2] / n) as u8]);
            }
        });
        RgbFrame { width: dst_w, height: dst_h, data }
    }
}

/// Size of the detector input for a `w`×`h` source whose longest side is
/// limited to `max_side` (never upscales).
pub fn detection_dims(w: u32, h: u32, max_side: u32) -> (u32, u32) {
    let longest = w.max(h);
    if longest <= max_side {
        return (w.max(1), h.max(1));
    }
    let s = max_side as f32 / longest as f32;
    (((w as f32 * s).round() as u32).max(1), ((h as f32 * s).round() as u32).max(1))
}

/// Thread-safe pool of detectors.
///
/// ONNX Runtime sessions need exclusive access while running, so parallel
/// callers (batch photo processing, video frames) each borrow a detector.
/// On a GPU a single session is usually optimal; on a CPU several sessions
/// with fewer intra-op threads each scale better than one big session.
pub struct DetectorPool {
    idle: Mutex<Vec<YuNet>>,
    available: Condvar,
    size: usize,
    device: String,
}

impl std::fmt::Debug for DetectorPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DetectorPool").field("device", &self.device).field("size", &self.size).finish()
    }
}

impl DetectorPool {
    /// Creates the pool, loading the model once per session.
    pub fn new(faces: &FaceOptions, compute: &ComputeOptions) -> Result<Self> {
        let threads = compute.threads.unwrap_or_else(num_threads).max(1);
        let (mut resolved, warning) = runtime::resolve_device(compute.device)?;
        if let Some(w) = warning {
            tracing::warn!("{w}");
        }
        let model = runtime::model_bytes(faces.model_path.as_deref())?;
        let sessions = match Self::sessions(&model, resolved, compute.gpu_index, threads) {
            Ok(s) => s,
            // `auto` picks providers that are compiled in and present in ONNX
            // Runtime, but the hardware may still be missing (e.g. DirectML on
            // a machine without a DirectX 12 GPU): fall back to the CPU.
            // An explicitly requested device stays an error.
            Err(e) if compute.device == InferenceDevice::Auto && resolved != InferenceDevice::Cpu => {
                tracing::warn!("{resolved} is not usable ({e}); face detection falls back to the CPU");
                resolved = InferenceDevice::Cpu;
                Self::sessions(&model, resolved, compute.gpu_index, threads)?
            }
            Err(e) => return Err(e),
        };
        let size = sessions.len();
        let device = runtime::describe(resolved, compute.gpu_index);
        tracing::info!(device = %device, sessions = size, "face detector ready");
        Ok(Self { idle: Mutex::new(sessions), available: Condvar::new(), size, device })
    }

    /// Creates the detector sessions for `device`.
    fn sessions(model: &[u8], device: InferenceDevice, gpu: u32, threads: usize) -> Result<Vec<YuNet>> {
        let (count, intra) = match device {
            InferenceDevice::Cpu => {
                // The model is small: running several frames/photos at once
                // with 2 threads each scales better than one wide session.
                let sessions = (threads / 2).clamp(1, 8);
                (sessions, (threads / sessions).max(1))
            }
            _ => (2, 2),
        };
        (0..count).map(|_| YuNet::new(model, device, gpu, intra)).collect()
    }

    /// Human readable description of the device in use (e.g. `cuda:0`).
    pub fn device(&self) -> &str {
        &self.device
    }

    /// Number of sessions, i.e. useful detection parallelism.
    pub fn size(&self) -> usize {
        self.size
    }

    /// Runs `f` with exclusive access to one detector, waiting if all are busy.
    pub fn with<R>(&self, f: impl FnOnce(&mut YuNet) -> R) -> R {
        let mut det = {
            let mut idle = self.idle.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if let Some(d) = idle.pop() {
                    break d;
                }
                idle = self.available.wait(idle).unwrap_or_else(|e| e.into_inner());
            }
        };
        // Return the detector even if `f` panics so other workers do not deadlock.
        struct Return<'a>(&'a DetectorPool, Option<YuNet>);
        impl Drop for Return<'_> {
            fn drop(&mut self) {
                if let Some(d) = self.1.take() {
                    self.0.idle.lock().unwrap_or_else(|e| e.into_inner()).push(d);
                    self.0.available.notify_one();
                }
            }
        }
        let mut guard = Return(self, None);
        let out = f(&mut det);
        guard.1 = Some(det);
        out
    }

    /// Detects faces in `frame` (coordinates in frame space).
    pub fn detect(&self, frame: &RgbFrame, min_confidence: f32) -> Result<Vec<Face>> {
        self.with(|d| d.detect(frame, min_confidence))
    }
}

pub(crate) fn num_threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dims_never_upscale() {
        assert_eq!(detection_dims(640, 480, 1000), (640, 480));
        assert_eq!(detection_dims(4000, 3000, 1000), (1000, 750));
        assert_eq!(detection_dims(1080, 1920, 960), (540, 960));
    }

    #[test]
    fn downscale_averages() {
        let f = RgbFrame::downscale(4, 4, 2, 2, |x, _| if x < 2 { [0, 0, 0] } else { [200, 100, 50] });
        assert_eq!(f.data, vec![0, 0, 0, 200, 100, 50, 0, 0, 0, 200, 100, 50]);
    }
}
