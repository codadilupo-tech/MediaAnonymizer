//! YuNet inference: preprocessing, session management and output decoding.
//!
//! The model predicts, for every cell of three feature maps (strides 8, 16
//! and 32), a classification score, an objectness score, a box and five
//! landmarks. Decoding follows OpenCV's `FaceDetectorYN` implementation.

use super::geometry::{nms, Face, Point};
use super::runtime;
use super::RgbFrame;
use crate::options::InferenceDevice;
use crate::{Error, Result};
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Tensor;
use std::sync::Arc;

const STRIDES: [usize; 3] = [8, 16, 32];
/// The network downsamples by 32, so inputs are padded to a multiple of it.
const ALIGN: u32 = 32;
const NMS_IOU: f32 = 0.3;

/// A loaded YuNet session.
pub struct YuNet {
    session: Session,
    input: String,
    /// Kept to rebuild the session on the CPU if the accelerator fails.
    model: Arc<[u8]>,
    device: InferenceDevice,
    intra_threads: usize,
}

/// Raw outputs, indexed `[stride][cls, obj, bbox, kps]`.
type Outputs = [[Vec<f32>; 4]; 3];
const OUTPUT_NAMES: [&str; 4] = ["cls", "obj", "bbox", "kps"];

impl std::fmt::Debug for YuNet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("YuNet").field("input", &self.input).field("device", &self.device).finish_non_exhaustive()
    }
}

impl YuNet {
    /// Creates a session on `device` (already resolved, never `Auto`).
    pub fn new(model: &[u8], device: InferenceDevice, gpu: u32, intra_threads: usize) -> Result<Self> {
        let model: Arc<[u8]> = model.into();
        let session = Self::session(&model, device, gpu, intra_threads)?;
        let input = session.inputs().first().map(|i| i.name().to_string()).unwrap_or_else(|| "input".into());
        Ok(Self { session, input, model, device, intra_threads })
    }

    fn session(model: &[u8], device: InferenceDevice, gpu: u32, intra_threads: usize) -> Result<Session> {
        let env = runtime::environment()?;
        let mut builder = Session::builder(env)?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| Error::Detection(e.to_string()))?
            .with_intra_threads(intra_threads)
            .map_err(|e| Error::Detection(e.to_string()))?
            // Busy-waiting threads would steal CPU from the FFmpeg processes
            // and the other pipeline stages.
            .with_intra_op_spinning(false)
            .map_err(|e| Error::Detection(e.to_string()))?;
        match runtime::provider(device, gpu) {
            Some(ep) => {
                builder = builder
                    .with_execution_providers([ep])
                    .map_err(|e| Error::Hardware(format!("cannot initialise {device} for inference: {e}")))?;
            }
            // Without an explicit provider, ort lets ONNX Runtime pick one with
            // a "max efficiency" policy, which on macOS silently registers
            // Core ML. A CPU session must really run on the CPU.
            None => {
                builder = builder
                    .with_auto_device(ort::session::builder::AutoDevicePolicy::PreferCPU)
                    .map_err(|e| Error::Detection(e.to_string()))?;
            }
        }
        Ok(builder.commit_from_memory(model)?)
    }

    /// Runs the network and copies the outputs out of ONNX Runtime.
    fn infer(&mut self, shape: [usize; 4], tensor: &[f32]) -> Result<Outputs> {
        let input = Tensor::from_array((shape, tensor.to_vec()))?;
        let outputs = self.session.run(ort::inputs![self.input.as_str() => input])?;
        let mut out: Outputs = Default::default();
        for (si, stride) in STRIDES.iter().enumerate() {
            for (ni, name) in OUTPUT_NAMES.iter().enumerate() {
                let key = format!("{name}_{stride}");
                let value = outputs
                    .get(key.as_str())
                    .ok_or_else(|| Error::Detection(format!("model output `{key}` missing: not a YuNet model?")))?;
                out[si][ni] = value.try_extract_tensor::<f32>()?.1.to_vec();
            }
        }
        Ok(out)
    }

    /// Runs the network; if an accelerator fails at inference time (some
    /// drivers reject shapes they accepted when the session was created),
    /// switches this session to the CPU for good and retries.
    fn infer_or_fallback(&mut self, shape: [usize; 4], tensor: &[f32]) -> Result<Outputs> {
        match self.infer(shape, tensor) {
            Err(e) if self.device != InferenceDevice::Cpu => {
                tracing::warn!("face detection on {} failed ({e}); falling back to the CPU", self.device);
                self.session = Self::session(&self.model, InferenceDevice::Cpu, 0, self.intra_threads)?;
                self.device = InferenceDevice::Cpu;
                self.infer(shape, tensor)
            }
            other => other,
        }
    }

    /// Detects faces; coordinates are in `frame` space.
    pub fn detect(&mut self, frame: &RgbFrame, min_confidence: f32) -> Result<Vec<Face>> {
        let (w, h) = (frame.width as usize, frame.height as usize);
        let pw = (frame.width.div_ceil(ALIGN) * ALIGN) as usize;
        let ph = (frame.height.div_ceil(ALIGN) * ALIGN) as usize;
        let plane = pw * ph;

        // RGB interleaved u8 -> BGR planar f32 in 0..255, zero padded.
        let mut tensor = vec![0f32; plane * 3];
        let (b, rest) = tensor.split_at_mut(plane);
        let (g, r) = rest.split_at_mut(plane);
        for y in 0..h {
            let src = &frame.data[y * w * 3..(y + 1) * w * 3];
            let row = y * pw;
            for (x, px) in src.as_chunks::<3>().0.iter().enumerate() {
                r[row + x] = px[0] as f32;
                g[row + x] = px[1] as f32;
                b[row + x] = px[2] as f32;
            }
        }

        let outputs = self.infer_or_fallback([1, 3, ph, pw], &tensor)?;

        let mut faces = Vec::new();
        for (si, stride) in STRIDES.into_iter().enumerate() {
            let [cls, obj, bbox, kps] = &outputs[si];
            let cols = pw / stride;
            let rows = ph / stride;
            if cls.len() < rows * cols || bbox.len() < rows * cols * 4 || kps.len() < rows * cols * 10 {
                return Err(Error::Detection("unexpected YuNet output size".into()));
            }
            let s = stride as f32;
            for idx in 0..rows * cols {
                let score = (cls[idx].clamp(0.0, 1.0) * obj[idx].clamp(0.0, 1.0)).sqrt();
                if score < min_confidence {
                    continue;
                }
                let (c, r) = ((idx % cols) as f32, (idx / cols) as f32);
                let bb = &bbox[idx * 4..idx * 4 + 4];
                let cx = (c + bb[0]) * s;
                let cy = (r + bb[1]) * s;
                let bw = bb[2].exp() * s;
                let bh = bb[3].exp() * s;
                let k = &kps[idx * 10..idx * 10 + 10];
                let landmarks = std::array::from_fn(|i| Point::new((k[2 * i] + c) * s, (k[2 * i + 1] + r) * s));
                faces.push(Face { x: cx - bw / 2.0, y: cy - bh / 2.0, width: bw, height: bh, landmarks, score });
            }
        }

        Ok(nms(faces, NMS_IOU).into_iter().filter_map(|f| clip(f, w as f32, h as f32)).collect())
    }
}

/// Clips a face to the frame; drops it if nothing is left.
fn clip(mut f: Face, w: f32, h: f32) -> Option<Face> {
    let x0 = f.x.max(0.0);
    let y0 = f.y.max(0.0);
    let x1 = (f.x + f.width).min(w);
    let y1 = (f.y + f.height).min(h);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    (f.x, f.y, f.width, f.height) = (x0, y0, x1 - x0, y1 - y0);
    Some(f)
}
