//! Person attributes: what kind of person a face belongs to (civilian,
//! police, carabinieri, military...) and what they wear or carry (helmet,
//! shield, balaclava, glasses...).
//!
//! The model (see `models/attributes/`) is an ONNX file chosen by the user;
//! it is not bundled. It classifies a square *person crop* around each face:
//! the head with its headwear and the upper body. Its metadata
//! (`mediaanon.attributes`, JSON) lists the attributes with two thresholds
//! each and describes the crop, so the attribute set can change without
//! changing this code.

use super::{runtime, Face, RgbFrame};
use crate::options::{ComputeOptions, InferenceDevice};
use crate::{Error, Result};
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Tensor;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;

/// Metadata key of the attribute model description.
const METADATA_KEY: &str = "mediaanon.attributes";
/// Crops classified per inference call.
const BATCH: usize = 16;

/// Kind of attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AttributeGroup {
    /// What kind of person: exactly one role applies (scores sum to 1).
    Role,
    /// Something worn or carried: any number apply.
    Gear,
}

/// One attribute the model predicts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttributeInfo {
    /// Identifier, e.g. `police`.
    pub name: String,
    /// Display name, e.g. `Police`.
    pub label: String,
    pub group: AttributeGroup,
    /// Score from which the attribute is considered present (chosen for a
    /// high precision on the validation data).
    pub present: f32,
    /// Score below which it is considered absent (chosen so that few people
    /// who have it fall below). In between, the model is unsure.
    pub absent: f32,
}

/// Geometry of the crop the model expects.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
struct Crop {
    /// Side of the square, in face heights.
    side: f32,
    /// Space above the top of the face box, in face heights.
    above: f32,
    input_size: u32,
    mean: [f32; 3],
    std: [f32; 3],
    fill: [u8; 3],
}

#[derive(Debug, Deserialize)]
struct Metadata {
    attributes: Vec<AttributeInfo>,
    crop: Crop,
}

/// A loaded attribute model.
pub struct AttributeModel {
    session: Mutex<Session>,
    input: String,
    attributes: Vec<AttributeInfo>,
    crop: Crop,
    device: String,
}

impl std::fmt::Debug for AttributeModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AttributeModel")
            .field("attributes", &self.attributes.iter().map(|a| &a.name).collect::<Vec<_>>())
            .field("device", &self.device)
            .finish_non_exhaustive()
    }
}

impl AttributeModel {
    /// Loads the model at `path` on the inference device of `compute`
    /// (falling back to the CPU when `auto` picks an accelerator that fails).
    pub fn load(path: &Path, compute: &ComputeOptions) -> Result<Self> {
        let bytes = std::fs::read(path).map_err(|e| Error::io(path, e))?;
        let (device, warning) = runtime::resolve_device(compute.device)?;
        if let Some(w) = warning {
            tracing::warn!("{w}");
        }
        let threads = compute.threads.unwrap_or_else(super::num_threads).max(1);
        let (session, device) = match session(&bytes, device, compute.gpu_index, threads) {
            Ok(s) => (s, device),
            Err(e) if compute.device == InferenceDevice::Auto && device != InferenceDevice::Cpu => {
                tracing::warn!("{device} is not usable for the attribute model ({e}); using the CPU");
                (session(&bytes, InferenceDevice::Cpu, 0, threads)?, InferenceDevice::Cpu)
            }
            Err(e) => return Err(e),
        };
        let meta = session.metadata().ok().and_then(|m| m.custom(METADATA_KEY)).ok_or_else(|| {
            Error::Detection(format!(
                "{} is not a MediaAnonymizer attribute model (no `{METADATA_KEY}` metadata)",
                path.display()
            ))
        })?;
        let meta: Metadata = serde_json::from_str(&meta)
            .map_err(|e| Error::Detection(format!("invalid attribute model metadata in {}: {e}", path.display())))?;
        if meta.attributes.is_empty() || meta.crop.input_size == 0 || meta.crop.side <= 0.0 {
            return Err(Error::Detection(format!("invalid attribute model metadata in {}", path.display())));
        }
        let input = session.inputs().first().map(|i| i.name().to_string()).unwrap_or_else(|| "crops".into());
        let device = runtime::describe(device, compute.gpu_index);
        tracing::info!(device = %device, attributes = meta.attributes.len(), "attribute model ready");
        Ok(Self { session: Mutex::new(session), input, attributes: meta.attributes, crop: meta.crop, device })
    }

    /// The attributes, in the order of the scores returned by
    /// [`AttributeModel::classify`].
    pub fn attributes(&self) -> &[AttributeInfo] {
        &self.attributes
    }

    /// Device running the model, e.g. `cpu`.
    pub fn device(&self) -> &str {
        &self.device
    }

    /// Scores in `0..=1` of every attribute for each face of `frame` (face
    /// coordinates in frame space).
    pub fn classify(&self, frame: &RgbFrame, faces: &[Face]) -> Result<Vec<Vec<f32>>> {
        let n = self.crop.input_size as usize;
        let k = self.attributes.len();
        let mut out = Vec::with_capacity(faces.len());
        for chunk in faces.chunks(BATCH) {
            let mut tensor = vec![0f32; chunk.len() * 3 * n * n];
            for (face, dst) in chunk.iter().zip(tensor.chunks_mut(3 * n * n)) {
                self.crop_into(frame, face, dst);
            }
            let input = Tensor::from_array(([chunk.len(), 3, n, n], tensor))?;
            let mut session = self.session.lock().unwrap_or_else(|e| e.into_inner());
            let outputs = session.run(ort::inputs![self.input.as_str() => input])?;
            let scores = outputs[0].try_extract_tensor::<f32>()?.1;
            if scores.len() != chunk.len() * k {
                return Err(Error::Detection(format!(
                    "the attribute model returned {} scores for {} crops of {k} attributes",
                    scores.len(),
                    chunk.len()
                )));
            }
            out.extend(scores.chunks(k).map(|s| s.iter().map(|v| v.clamp(0.0, 1.0)).collect()));
        }
        Ok(out)
    }

    /// Writes the normalised person crop of `face` (planar RGB) into `dst`.
    fn crop_into(&self, frame: &RgbFrame, face: &Face, dst: &mut [f32]) {
        let c = &self.crop;
        let n = c.input_size as usize;
        let side = c.side * face.height.max(1.0);
        let x0 = face.x + face.width / 2.0 - side / 2.0;
        let y0 = face.y - c.above * face.height;
        let scale = side / n as f32;
        let (w, h) = (frame.width as i64, frame.height as i64);
        let pixel = |x: i64, y: i64| -> [f32; 3] {
            if x < 0 || y < 0 || x >= w || y >= h {
                return c.fill.map(f32::from);
            }
            let i = (y as usize * frame.width as usize + x as usize) * 3;
            [frame.data[i] as f32, frame.data[i + 1] as f32, frame.data[i + 2] as f32]
        };
        // Shrinking: average up to 4×4 taps per output pixel. Enlarging
        // (small faces): bilinear interpolation.
        let taps = (scale.ceil() as usize).clamp(1, 4);
        let plane = n * n;
        for oy in 0..n {
            for ox in 0..n {
                let mut acc = [0f32; 3];
                if scale > 1.0 {
                    for ty in 0..taps {
                        for tx in 0..taps {
                            let sx = x0 + (ox as f32 + (tx as f32 + 0.5) / taps as f32) * scale;
                            let sy = y0 + (oy as f32 + (ty as f32 + 0.5) / taps as f32) * scale;
                            let p = pixel(sx.floor() as i64, sy.floor() as i64);
                            (0..3).for_each(|ch| acc[ch] += p[ch]);
                        }
                    }
                    acc.iter_mut().for_each(|v| *v /= (taps * taps) as f32);
                } else {
                    let sx = x0 + (ox as f32 + 0.5) * scale - 0.5;
                    let sy = y0 + (oy as f32 + 0.5) * scale - 0.5;
                    let (fx, fy) = (sx.floor(), sy.floor());
                    let (ax, ay) = (sx - fx, sy - fy);
                    let (ix, iy) = (fx as i64, fy as i64);
                    let (p00, p10, p01, p11) =
                        (pixel(ix, iy), pixel(ix + 1, iy), pixel(ix, iy + 1), pixel(ix + 1, iy + 1));
                    for ch in 0..3 {
                        let top = p00[ch] * (1.0 - ax) + p10[ch] * ax;
                        let bottom = p01[ch] * (1.0 - ax) + p11[ch] * ax;
                        acc[ch] = top * (1.0 - ay) + bottom * ay;
                    }
                }
                for ch in 0..3 {
                    dst[ch * plane + oy * n + ox] = (acc[ch] / 255.0 - c.mean[ch]) / c.std[ch];
                }
            }
        }
    }
}

fn session(model: &[u8], device: InferenceDevice, gpu: u32, threads: usize) -> Result<Session> {
    let env = runtime::environment()?;
    let mut builder = Session::builder(env)?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(|e| Error::Detection(e.to_string()))?
        .with_intra_threads(threads)
        .map_err(|e| Error::Detection(e.to_string()))?
        .with_intra_op_spinning(false)
        .map_err(|e| Error::Detection(e.to_string()))?;
    match runtime::provider(device, gpu) {
        Some(ep) => {
            builder = builder
                .with_execution_providers([ep])
                .map_err(|e| Error::Hardware(format!("cannot initialise {device} for inference: {e}")))?;
        }
        None => {
            builder = builder
                .with_auto_device(ort::session::builder::AutoDevicePolicy::PreferCPU)
                .map_err(|e| Error::Detection(e.to_string()))?;
        }
    }
    Ok(builder.commit_from_memory(model)?)
}
