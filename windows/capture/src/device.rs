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

use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
    DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
    DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME, DisplayConfigGetDeviceInfo,
    GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
};
use windows::Win32::Foundation::{HMODULE, LUID};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
    ID3D11DeviceContext, ID3D11Multithread,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_INVALID_CALL, IDXGIAdapter1, IDXGIDevice, IDXGIFactory1,
    IDXGIOutput,
};
use windows::Win32::Graphics::Gdi::{DISPLAY_DEVICEW, EnumDisplayDevicesW};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::core::Interface;

use crate::backend::CaptureError;
use crate::trace;

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

/// One attached output, as DXGI enumerates it, paired with the name Windows
/// shows for the monitor on the other end of it.
///
/// DXGI identifies an output by a position in a list and a GDI device name,
/// and neither survives plugging a monitor in: attaching a display renumbers
/// `\\.\DISPLAYn` and shifts every index after it. The monitor's own name
/// does survive, which is the only reason a benchmark can name its source
/// once and still be measuring it a week later.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputInfo {
    /// The index [`CaptureDevice::open`] takes, valid only until the set of
    /// attached displays changes.
    pub index: u32,
    /// `\\.\DISPLAY10`, and just as perishable as the index.
    pub device_name: String,
    /// What Windows calls the monitor, e.g. `LG ULTRAWIDE`. Empty for a
    /// display with no EDID name of its own, which includes indirect
    /// displays: an IddCx monitor is named by its driver, not by itself.
    pub monitor_name: String,
    /// The GDI adapter behind the output, e.g. `LanPlay IDD-LAB 1080p120`
    /// or `NVIDIA GeForce RTX 4060 Ti`. For an indirect display this is the
    /// only distinctive name there is.
    pub adapter_name: String,
    pub width: u32,
    pub height: u32,
}

impl fmt::Display for OutputInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} {}x{} \"{}\" via \"{}\"",
            self.index,
            self.device_name,
            self.width,
            self.height,
            self.monitor_name,
            self.adapter_name
        )
    }
}

/// Every output of the first adapter that has any, in the order
/// [`CaptureDevice::open`] indexes them.
pub fn outputs() -> Result<Vec<OutputInfo>, CaptureError> {
    // SAFETY: every call takes valid pointers and its result is checked
    // before use; the COM objects are refcounted by `windows`.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(|e| CaptureError::Api {
            call: "CreateDXGIFactory1",
            hresult: e.code().0,
        })?;
        let mut found = Vec::new();
        for adapter_index in 0.. {
            let Ok(adapter) = factory.EnumAdapters1(adapter_index) else {
                break;
            };
            for index in 0.. {
                let Ok(output) = adapter.EnumOutputs(index) else {
                    break;
                };
                let desc = output.GetDesc().map_err(|e| CaptureError::Api {
                    call: "IDXGIOutput::GetDesc",
                    hresult: e.code().0,
                })?;
                let device_name = String::from_utf16_lossy(trim_nul(&desc.DeviceName));
                found.push(OutputInfo {
                    index,
                    monitor_name: monitor_name(&desc.DeviceName),
                    adapter_name: adapter_name(&desc.DeviceName),
                    device_name,
                    width: (desc.DesktopCoordinates.right - desc.DesktopCoordinates.left) as u32,
                    height: (desc.DesktopCoordinates.bottom - desc.DesktopCoordinates.top) as u32,
                });
            }
            // The first adapter with outputs is the one `open` will pick, so
            // listing a second adapter's outputs would list indices `open`
            // cannot reach.
            if !found.is_empty() {
                break;
            }
        }
        Ok(found)
    }
}

/// Resolves a name fragment to the index `open` takes, matching the monitor
/// name, the adapter name or the GDI device name.
///
/// Ambiguity is an error rather than a first match: a sweep that silently
/// changed which display it measured is the failure this exists to prevent.
pub fn output_named(fragment: &str) -> Result<u32, CaptureError> {
    let all = outputs()?;
    let matched: Vec<&OutputInfo> = all
        .iter()
        .filter(|o| {
            o.monitor_name.contains(fragment)
                || o.adapter_name.contains(fragment)
                || o.device_name.contains(fragment)
        })
        .collect();
    let listing = || {
        all.iter()
            .map(|o| o.to_string())
            .collect::<Vec<_>>()
            .join("; ")
    };
    match matched.as_slice() {
        [one] => Ok(one.index),
        [] => Err(CaptureError::Unsupported(format!(
            "no output matches {fragment:?}; attached: {}",
            listing()
        ))),
        many => Err(CaptureError::Unsupported(format!(
            "{} outputs match {fragment:?}; attached: {}",
            many.len(),
            listing()
        ))),
    }
}

/// The GDI adapter string for a display device name.
///
/// This is the driver's own name for the thing driving the output, which is
/// what identifies an indirect display: an IddCx monitor exposes no EDID
/// name, so the adapter is the only place `LanPlay IDD-LAB 1080p120` appears.
fn adapter_name(device_name: &[u16; 32]) -> String {
    let wanted = trim_nul(device_name);
    for index in 0.. {
        let mut adapter = DISPLAY_DEVICEW {
            cb: core::mem::size_of::<DISPLAY_DEVICEW>() as u32,
            ..Default::default()
        };
        // SAFETY: `adapter` is correctly sized and the null device name asks
        // for adapter enumeration, which is what the loop index is for.
        let ok = unsafe { EnumDisplayDevicesW(None, index, &mut adapter, 0) };
        if !ok.as_bool() {
            break;
        }
        if trim_nul(&adapter.DeviceName) == wanted {
            return String::from_utf16_lossy(trim_nul(&adapter.DeviceString));
        }
    }
    String::new()
}

/// The monitor's friendly name for a GDI device name, e.g. `LG ULTRAWIDE`
/// for `\\.\DISPLAY1`.
///
/// `EnumDisplayDevices` is the obvious call and the wrong one: it answers
/// `Generic PnP Monitor` for everything with a standard driver, which is
/// every monitor worth telling apart. The name a user sees comes from the
/// display topology, which has to be walked from source to target.
fn monitor_name(device_name: &[u16; 32]) -> String {
    let wanted = trim_nul(device_name);
    // SAFETY: sizes come from the same call that fills the buffers, every
    // request packet is zeroed and carries its own size, and nothing escapes.
    unsafe {
        let mut paths = 0u32;
        let mut modes = 0u32;
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut paths, &mut modes).is_err() {
            return String::new();
        }
        let mut path_buf = vec![DISPLAYCONFIG_PATH_INFO::default(); paths as usize];
        let mut mode_buf = vec![DISPLAYCONFIG_MODE_INFO::default(); modes as usize];
        if QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut paths,
            path_buf.as_mut_ptr(),
            &mut modes,
            mode_buf.as_mut_ptr(),
            None,
        )
        .is_err()
        {
            return String::new();
        }

        for path in &path_buf[..paths as usize] {
            let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                    size: core::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                    adapterId: path.sourceInfo.adapterId,
                    id: path.sourceInfo.id,
                },
                ..Default::default()
            };
            if DisplayConfigGetDeviceInfo(&mut source.header) != 0
                || trim_nul(&source.viewGdiDeviceName) != wanted
            {
                continue;
            }
            let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
                    size: core::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32,
                    adapterId: path.targetInfo.adapterId,
                    id: path.targetInfo.id,
                },
                ..Default::default()
            };
            if DisplayConfigGetDeviceInfo(&mut target.header) == 0 {
                return String::from_utf16_lossy(trim_nul(&target.monitorFriendlyDeviceName));
            }
        }
        String::new()
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
        // Process-wide, and deliberately here rather than left to each binary.
        // `IDXGIOutput5::DuplicateOutput1` is documented to fail with
        // DXGI_ERROR_UNSUPPORTED for a process that is not per-monitor DPI
        // aware, and the failure names the pixel format, so a caller that
        // forgets spends its time looking at the format list instead. It also
        // stops a scaled display handing back a stretched surface. Idempotent,
        // and an error means something already set a context, which is fine.
        //
        // SAFETY: no arguments to get wrong, and no failure mode that matters.
        unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }

        // SAFETY: every call below takes valid pointers and its result is
        // checked before use; the COM objects are refcounted by `windows`.
        unsafe {
            let span = trace::begin("create_factory", "api=CreateDXGIFactory1");
            let factory: IDXGIFactory1 = match CreateDXGIFactory1() {
                Ok(factory) => {
                    span.ok("api=CreateDXGIFactory1");
                    factory
                }
                Err(error) => {
                    span.error(error.code().0, "api=CreateDXGIFactory1");
                    return Err(CaptureError::Api {
                        call: "CreateDXGIFactory1",
                        hresult: error.code().0,
                    });
                }
            };

            let mut chosen = None;
            for index in 0.. {
                let span = trace::begin("enumerate_adapter", format_args!("index={index}"));
                let adapter = match factory.EnumAdapters1(index) {
                    Ok(adapter) => {
                        span.ok(format_args!("index={index}"));
                        adapter
                    }
                    Err(error) => {
                        span.error(error.code().0, format_args!("index={index}"));
                        break;
                    }
                };
                let span = trace::begin(
                    "enumerate_output",
                    format_args!("adapter_index={index} output_index={output_index}"),
                );
                match adapter.EnumOutputs(output_index) {
                    Ok(output) => {
                        span.ok(format_args!(
                            "adapter_index={index} output_index={output_index}"
                        ));
                        chosen = Some((adapter, output));
                        break;
                    }
                    Err(error) => {
                        span.error(
                            error.code().0,
                            format_args!("adapter_index={index} output_index={output_index}"),
                        );
                    }
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
            let span = trace::begin(
                "d3d11_create_device",
                format_args!("adapter_luid={}", luid_as_i64(adapter_desc.AdapterLuid)),
            );
            let outcome = D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                Some(&mut level),
                Some(&mut context),
            );
            if let Err(error) = outcome {
                span.error(
                    error.code().0,
                    format_args!("adapter_luid={}", luid_as_i64(adapter_desc.AdapterLuid)),
                );
                return Err(CaptureError::Api {
                    call: "D3D11CreateDevice",
                    hresult: error.code().0,
                });
            }
            span.ok(format_args!(
                "adapter_luid={} feature_level=0x{:X}",
                luid_as_i64(adapter_desc.AdapterLuid),
                level.0
            ));

            let device = device.ok_or_else(|| {
                CaptureError::Unsupported("D3D11CreateDevice returned no device".into())
            })?;
            let context = context.ok_or_else(|| {
                CaptureError::Unsupported("D3D11CreateDevice returned no context".into())
            })?;

            let dxgi_device = device
                .cast::<IDXGIDevice>()
                .map_err(|error| CaptureError::Api {
                    call: "ID3D11Device::QueryInterface(IDXGIDevice)",
                    hresult: error.code().0,
                })?;
            let device_adapter = dxgi_device
                .GetAdapter()
                .map_err(|error| CaptureError::Api {
                    call: "IDXGIDevice::GetAdapter",
                    hresult: error.code().0,
                })?
                .cast::<IDXGIAdapter1>()
                .map_err(|error| CaptureError::Api {
                    call: "IDXGIAdapter::QueryInterface(IDXGIAdapter1)",
                    hresult: error.code().0,
                })?;
            let device_adapter_desc =
                device_adapter
                    .GetDesc1()
                    .map_err(|error| CaptureError::Api {
                        call: "IDXGIAdapter1::GetDesc1(device adapter)",
                        hresult: error.code().0,
                    })?;
            let output_luid = luid_as_i64(adapter_desc.AdapterLuid);
            let device_luid = luid_as_i64(device_adapter_desc.AdapterLuid);
            let span = trace::begin(
                "validate_adapter",
                format_args!(
                    "output_adapter_luid={output_luid} d3d11_device_adapter_luid={device_luid}"
                ),
            );
            if output_luid != device_luid {
                span.error(
                    DXGI_ERROR_INVALID_CALL.0,
                    format_args!(
                        "output_adapter_luid={output_luid} d3d11_device_adapter_luid={device_luid} match=no"
                    ),
                );
                return Err(CaptureError::Unsupported(format!(
                    "output adapter LUID {output_luid} does not match D3D11 device adapter LUID {device_luid}"
                )));
            }
            span.ok(format_args!(
                "output_adapter_luid={output_luid} d3d11_device_adapter_luid={device_luid} match=yes"
            ));

            // The device is handed to two threads: whichever one captures and
            // copies, and whichever one drives the encoder. A D3D11 device is
            // free-threaded but its immediate context is not, and NVENC takes
            // that context internally on calls the caller never sees - the
            // bitstream lock and unlock among them. Without this flag the two
            // eventually meet inside the driver and simply stop: one thread
            // parked in `AcquireNextFrame`, the other in
            // `NvEncUnlockBitstream`, neither holding a lock the other can
            // see. NVIDIA's own samples set it for exactly this reason.
            //
            // Serialising the context by hand instead would mean a mutex the
            // driver already has, held across calls whose duration we do not
            // control.
            let multithread =
                context
                    .cast::<ID3D11Multithread>()
                    .map_err(|error| CaptureError::Api {
                        call: "ID3D11DeviceContext::QueryInterface(ID3D11Multithread)",
                        hresult: error.code().0,
                    })?;
            let span = trace::begin("set_multithread_protected", "api=ID3D11Multithread");
            // Returns the previous setting, which nothing here needs.
            let _ = multithread.SetMultithreadProtected(true);
            if !multithread.GetMultithreadProtected().as_bool() {
                span.error(
                    DXGI_ERROR_INVALID_CALL.0,
                    "api=ID3D11Multithread protected=no",
                );
                return Err(CaptureError::Unsupported(
                    "the D3D11 device refused multithread protection".into(),
                ));
            }
            span.ok("api=ID3D11Multithread protected=yes");

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
