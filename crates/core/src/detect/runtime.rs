//! ONNX Runtime environment and execution-provider selection.

use crate::options::InferenceDevice;
use crate::{Error, Result};
use ort::environment::Environment;
#[allow(unused_imports)]
use ort::ep::{self, ExecutionProvider, ExecutionProviderDispatch};
use serde::Serialize;
use std::path::Path;
use std::sync::OnceLock;

/// The bundled YuNet model (MIT licensed, see `models/LICENSE-YuNet`).
static BUNDLED_MODEL: &[u8] = include_bytes!("../../../../models/yunet_2023mar_dynamic.onnx");

/// GPU providers tried by [`InferenceDevice::Auto`], best first.
///
/// Core ML is deliberately absent: it fails at run time on some macOS
/// versions with YuNet's dynamic input shapes, and Apple-silicon CPUs run
/// this small model fast anyway. It remains available when chosen explicitly.
const AUTO_ORDER: &[InferenceDevice] = &[
    InferenceDevice::TensorRt,
    InferenceDevice::Cuda,
    InferenceDevice::DirectMl,
    InferenceDevice::Rocm,
    InferenceDevice::OpenVino,
];

/// Returns the process-wide ONNX Runtime environment, creating it on first use.
pub(crate) fn environment() -> Result<&'static Environment> {
    static ENV: OnceLock<std::result::Result<Environment, String>> = OnceLock::new();
    ENV.get_or_init(|| {
        load_library()?;
        ort::init().with_name("mediaanon").build().map_err(|e| e.to_string())
    })
    .as_ref()
    .map_err(|e| Error::Detection(format!("cannot initialise ONNX Runtime: {e}")))
}

/// With the `ort-dynamic` feature the shared library is loaded at runtime:
/// from `ORT_DYLIB_PATH`, from next to the executable, or from the system
/// library search path, in that order.
#[cfg(feature = "ort-dynamic")]
fn load_library() -> std::result::Result<(), String> {
    use std::path::PathBuf;
    let file = format!("{}onnxruntime{}", std::env::consts::DLL_PREFIX, std::env::consts::DLL_SUFFIX);
    let candidates: Vec<PathBuf> = std::env::var_os("ORT_DYLIB_PATH")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .into_iter()
        .chain(std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join(&file))))
        .collect();
    let path = candidates.into_iter().find(|p| p.exists()).unwrap_or_else(|| PathBuf::from(&file));
    std::panic::catch_unwind(|| ort::init_from(&path).map(|_| ()))
        .map_err(|_| format!("failed to load {}", path.display()))?
        .map_err(|e| format!("failed to load {}: {e}. Set ORT_DYLIB_PATH to the ONNX Runtime library", path.display()))
}

#[cfg(not(feature = "ort-dynamic"))]
fn load_library() -> std::result::Result<(), String> {
    Ok(())
}

pub(crate) fn model_bytes(custom: Option<&Path>) -> Result<std::borrow::Cow<'static, [u8]>> {
    match custom {
        None => Ok(BUNDLED_MODEL.into()),
        Some(p) => std::fs::read(p).map(Into::into).map_err(|e| Error::io(p, e)),
    }
}

/// Whether ONNX Runtime and this build both support `device`.
fn is_available(device: InferenceDevice) -> bool {
    macro_rules! check {
        ($feature:literal, $ep:ty) => {{
            #[cfg(feature = $feature)]
            {
                <$ep as ExecutionProvider>::is_available().unwrap_or(false)
            }
            #[cfg(not(feature = $feature))]
            {
                false
            }
        }};
    }
    match device {
        InferenceDevice::Auto | InferenceDevice::Cpu => true,
        InferenceDevice::Cuda => check!("cuda", ep::CUDA),
        InferenceDevice::TensorRt => check!("tensorrt", ep::TensorRT),
        InferenceDevice::DirectMl => check!("directml", ep::DirectML),
        InferenceDevice::CoreMl => check!("coreml", ep::CoreML),
        InferenceDevice::OpenVino => check!("openvino", ep::OpenVINO),
        InferenceDevice::Rocm => check!("rocm", ep::ROCm),
    }
}

fn cargo_feature(device: InferenceDevice) -> &'static str {
    match device {
        InferenceDevice::Cuda => "cuda",
        InferenceDevice::TensorRt => "tensorrt",
        InferenceDevice::DirectMl => "directml",
        InferenceDevice::CoreMl => "coreml",
        InferenceDevice::OpenVino => "openvino",
        InferenceDevice::Rocm => "rocm",
        InferenceDevice::Auto | InferenceDevice::Cpu => "",
    }
}

/// Turns `Auto` into a concrete device and validates explicit choices.
/// Returns an optional warning to surface to the user.
pub(crate) fn resolve_device(device: InferenceDevice) -> Result<(InferenceDevice, Option<String>)> {
    environment()?;
    match device {
        InferenceDevice::Cpu => Ok((InferenceDevice::Cpu, None)),
        InferenceDevice::Auto => {
            Ok((AUTO_ORDER.iter().copied().find(|d| is_available(*d)).unwrap_or(InferenceDevice::Cpu), None))
        }
        d if is_available(d) => Ok((d, None)),
        d => Err(Error::Hardware(format!(
            "inference device `{d}` is not available: build with `--features {}` and use an ONNX Runtime \
             build that includes it (see docs/HARDWARE_ACCELERATION.md), or choose `--device cpu`",
            cargo_feature(d)
        ))),
    }
}

/// Builds the execution provider for a resolved device (`None` = CPU).
#[allow(unused_variables)]
pub(crate) fn provider(device: InferenceDevice, gpu: u32) -> Option<ExecutionProviderDispatch> {
    let gpu = gpu as i32;
    let ep: ExecutionProviderDispatch = match device {
        #[cfg(feature = "cuda")]
        InferenceDevice::Cuda => ep::CUDA::default().with_device_id(gpu).build(),
        #[cfg(feature = "tensorrt")]
        InferenceDevice::TensorRt => ep::TensorRT::default().with_device_id(gpu).build(),
        #[cfg(feature = "directml")]
        InferenceDevice::DirectMl => ep::DirectML::default().with_device_id(gpu).build(),
        #[cfg(feature = "coreml")]
        // The legacy NeuralNetwork format (ort's default) fails at run time
        // on our dynamic input shapes; ML Program handles them.
        InferenceDevice::CoreMl => ep::CoreML::default()
            .with_model_format(ep::coreml::ModelFormat::MLProgram)
            .with_compute_units(ep::coreml::ComputeUnits::All)
            .build(),
        #[cfg(feature = "openvino")]
        InferenceDevice::OpenVino => ep::OpenVINO::default().build(),
        #[cfg(feature = "rocm")]
        InferenceDevice::Rocm => ep::ROCm::default().with_device_id(gpu).build(),
        _ => return None,
    };
    #[allow(unreachable_code)]
    Some(ep.error_on_failure())
}

pub(crate) fn describe(device: InferenceDevice, gpu: u32) -> String {
    match device {
        InferenceDevice::Cpu | InferenceDevice::Auto | InferenceDevice::CoreMl | InferenceDevice::OpenVino => {
            device.to_string()
        }
        _ => format!("{device}:{gpu}"),
    }
}

/// One inference device and whether it can be used.
#[derive(Debug, Clone, Serialize)]
pub struct DeviceInfo {
    pub device: InferenceDevice,
    /// Compiled into this build *and* supported by the loaded ONNX Runtime.
    pub available: bool,
    /// Cargo feature that enables it (empty for the CPU).
    pub feature: &'static str,
}

/// Lists every inference device and its availability.
pub fn available_devices() -> Result<Vec<DeviceInfo>> {
    environment()?;
    Ok(InferenceDevice::ALL
        .iter()
        .filter(|d| **d != InferenceDevice::Auto)
        .map(|&device| DeviceInfo { device, available: is_available(device), feature: cargo_feature(device) })
        .collect())
}
