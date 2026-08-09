//! One D3D11 device, shared by both backends.
//!
//! Two devices would be two sets of driver state, two allocators and two
//! scheduling contexts, and the difference between the backends would then
//! include a difference we invented. Desktop Duplication additionally requires
//! that the device belong to the adapter owning the output being duplicated,
//! so the adapter is chosen first and the device is made from it.
//!
//! Everything identifying that choice is recorded, because a capture benchmark
//! that does not say which GPU and driver produced it is a number without a
//! subject.

#![cfg(windows)]

use core::fmt;

use windows::Win32::Foundation::{HMODULE, LUID};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
    ID3D11DeviceContext,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput,
};
use windows::core::Interface;

use crate::backend::CaptureError;

/// Which GPU, which driver, which output. Printed with every result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceIdentity {
    pub adapter: String,
    pub luid: i64,
    pub vendor_id: u32,
    pub device_id: u32,
    pub dedicated_vram_mb: u64,
    pub feature_level: u32,
    pub output: String,
    pub output_width: u32,
    pub output_height: u32,
}

impl fmt::Display for DeviceIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} ({:04x}:{:04x}, {} MB VRAM, feature level {}.{}) driving {} at {}x{}",
            self.adapter,
            self.vendor_id,
            self.device_id,
            self.dedicated_vram_mb,
            self.feature_level >> 12,
            (self.feature_level >> 8) & 0xf,
            self.output,
            self.output_width,
            self.output_height
        )
    }
}

/// The device both backends run on, and the adapter and output it belongs to.
pub struct CaptureDevice {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    adapter: IDXGIAdapter1,
    output: IDXGIOutput,
    identity: DeviceIdentity,
}

impl CaptureDevice {
    /// Picks the adapter owning the requested output and builds the device on
    /// it.
    ///
    /// `output` indexes the outputs of the first adapter that has any. A
    /// machine with two GPUs and a display on the second one is not a case
    /// this benchmark needs to serve, and guessing would be worse than being
    /// explicit about the limit.
    pub fn open(output_index: u32) -> Result<CaptureDevice, CaptureError> {
        // SAFETY: every call below takes valid pointers and its result is
        // checked before use; the COM objects are refcounted by `windows`.
        unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(|e| CaptureError::Api {
                call: "CreateDXGIFactory1",
                hresult: e.code().0,
            })?;

            let mut chosen = None;
            for index in 0.. {
                let Ok(adapter) = factory.EnumAdapters1(index) else {
                    break;
                };
                if let Ok(output) = adapter.EnumOutputs(output_index) {
                    chosen = Some((adapter, output));
                    break;
                }
            }
            let (adapter, output) = chosen.ok_or_else(|| {
                CaptureError::Unsupported(format!("no adapter has an output {output_index}"))
            })?;

            let adapter_desc = adapter.GetDesc1().map_err(|e| CaptureError::Api {
                call: "IDXGIAdapter1::GetDesc1",
                hresult: e.code().0,
            })?;
            let output_desc = output.GetDesc().map_err(|e| CaptureError::Api {
                call: "IDXGIOutput::GetDesc",
                hresult: e.code().0,
            })?;

            let mut device = None;
            let mut context = None;
            let mut level = D3D_FEATURE_LEVEL::default();
            // BGRA support is required: both backends are pinned to
            // B8G8R8A8_UNORM so that the comparison is not between formats.
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                Some(&mut level),
                Some(&mut context),
            )
            .map_err(|e| CaptureError::Api {
                call: "D3D11CreateDevice",
                hresult: e.code().0,
            })?;

            let device = device.ok_or_else(|| {
                CaptureError::Unsupported("D3D11CreateDevice returned no device".into())
            })?;
            let context = context.ok_or_else(|| {
                CaptureError::Unsupported("D3D11CreateDevice returned no context".into())
            })?;

            let identity = DeviceIdentity {
                adapter: String::from_utf16_lossy(trim_nul(&adapter_desc.Description)),
                luid: luid_as_i64(adapter_desc.AdapterLuid),
                vendor_id: adapter_desc.VendorId,
                device_id: adapter_desc.DeviceId,
                dedicated_vram_mb: (adapter_desc.DedicatedVideoMemory / (1024 * 1024)) as u64,
                feature_level: level.0 as u32,
                output: String::from_utf16_lossy(trim_nul(&output_desc.DeviceName)),
                output_width: (output_desc.DesktopCoordinates.right
                    - output_desc.DesktopCoordinates.left) as u32,
                output_height: (output_desc.DesktopCoordinates.bottom
                    - output_desc.DesktopCoordinates.top) as u32,
            };

            Ok(CaptureDevice {
                device,
                context,
                adapter,
                output,
                identity,
            })
        }
    }

    pub fn device(&self) -> &ID3D11Device {
        &self.device
    }

    pub fn context(&self) -> &ID3D11DeviceContext {
        &self.context
    }

    pub fn adapter(&self) -> &IDXGIAdapter1 {
        &self.adapter
    }

    pub fn output(&self) -> &IDXGIOutput {
        &self.output
    }

    pub fn identity(&self) -> &DeviceIdentity {
        &self.identity
    }

    /// The output as the interface Desktop Duplication needs.
    pub fn output_as<T: Interface>(&self) -> Result<T, CaptureError> {
        self.output.cast::<T>().map_err(|e| {
            CaptureError::Unsupported(format!(
                "this output does not support {}: 0x{:08X}",
                core::any::type_name::<T>(),
                e.code().0 as u32
            ))
        })
    }
}

fn trim_nul(text: &[u16]) -> &[u16] {
    let end = text.iter().position(|c| *c == 0).unwrap_or(text.len());
    &text[..end]
}

fn luid_as_i64(luid: LUID) -> i64 {
    ((luid.HighPart as i64) << 32) | (luid.LowPart as i64)
}
