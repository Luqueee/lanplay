//! Device, swap chain and the one shader that draws the whole picture.
//!
//! The device is built on the adapter that owns the requested monitor, the
//! same rule `lanplay_capture::device` follows, so the producer and the
//! capturer are never on different GPUs sharing frames across the PCIe bus.
//! The swap chain is `FLIP_DISCARD` with `SyncInterval` 0 because the point is
//! to be able to outrun the panel: at `--fps 240` on a 120 Hz display, vsync
//! would silently halve the rate the operator asked for and the capture
//! numbers would be taken against a source that is not the one requested.

#![cfg(windows)]

use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct3D::Fxc::{
    D3DCOMPILE_ENABLE_STRICTNESS, D3DCOMPILE_OPTIMIZATION_LEVEL3, D3DCompile,
};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
    D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, ID3DBlob,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_CONSTANT_BUFFER, D3D11_BUFFER_DESC, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_SDK_VERSION, D3D11_USAGE_DEFAULT, D3D11_VIEWPORT, D3D11CreateDevice, ID3D11Buffer,
    ID3D11Device, ID3D11DeviceContext, ID3D11PixelShader, ID3D11RenderTargetView, ID3D11Texture2D,
    ID3D11VertexShader,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_FEATURE_PRESENT_ALLOW_TEARING, DXGI_MWA_NO_ALT_ENTER, DXGI_PRESENT,
    DXGI_PRESENT_ALLOW_TEARING, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1,
    DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING, DXGI_SWAP_EFFECT_FLIP_DISCARD,
    DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIFactory2, IDXGIFactory5, IDXGISwapChain1,
};
use windows::core::{BOOL, Interface, PCSTR, s};

use crate::{Error, api};

/// Compiled every run rather than shipped as bytecode. `fxc` only exists in
/// the Windows SDK, and this workspace is developed on a machine that has no
/// SDK to run it with, so a build script would have made the crate
/// unbuildable off Windows. `d3dcompiler_47.dll` is present on every Windows
/// 10 and 11 install, the shader is a few dozen instructions, and it is
/// compiled once at startup and never on the frame path.
const SOURCE_HLSL: &str = include_str!("source.hlsl");

/// Bytes of [`Tick`]. Constant buffers are allocated in 16-byte registers, so
/// the padding field is not slack, it is the rest of register zero.
const TICK_BYTES: u32 = 16;

/// What the shader is told about the frame it is drawing.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Tick {
    frame_index: u32,
    width: u32,
    height: u32,
    reserved: u32,
}

/// Where a monitor sits on the virtual desktop, in the desktop's own
/// coordinates. Needed in full-screen mode: a borderless window has to be
/// placed at the monitor's origin, which is negative for a display left of the
/// primary one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Monitor {
    pub name: String,
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

/// The adapter's device, the factory that made it, and the monitor it drives.
pub struct Gpu {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    factory: IDXGIFactory2,
    monitor: Monitor,
    adapter_name: String,
    /// Whether the swap chain may present without waiting for a vblank.
    ///
    /// Without it, a windowed flip chain with `SyncInterval` 0 still stalls in
    /// `Present` once its buffers are all queued, so DWM ends up pacing the
    /// producer at the refresh rate. That would silently turn `--fps 240` on a
    /// 120 Hz panel into 120 fps: the one case this tool exists to produce.
    tearing: bool,
}

impl Gpu {
    /// Opens a device on the adapter owning output `monitor_index`.
    pub fn open(monitor_index: u32) -> Result<Gpu, Error> {
        // SAFETY: every call is given valid pointers, and every result is
        // checked before the value behind it is used. The COM objects are
        // reference counted by the `windows` crate.
        unsafe {
            let factory: IDXGIFactory2 =
                CreateDXGIFactory1().map_err(api("CreateDXGIFactory1"))?;

            let mut chosen = None;
            for index in 0.. {
                let Ok(adapter) = factory.EnumAdapters1(index) else {
                    break;
                };
                if let Ok(output) = adapter.EnumOutputs(monitor_index) {
                    chosen = Some((adapter, output));
                    break;
                }
            }
            let (adapter, output) = chosen.ok_or_else(|| {
                Error::Unsupported(format!("no adapter has a monitor {monitor_index}"))
            })?;

            let adapter_desc = adapter
                .GetDesc1()
                .map_err(api("IDXGIAdapter1::GetDesc1"))?;
            let output_desc = output.GetDesc().map_err(api("IDXGIOutput::GetDesc"))?;
            let bounds = output_desc.DesktopCoordinates;
            let monitor = Monitor {
                name: String::from_utf16_lossy(trim_nul(&output_desc.DeviceName)),
                left: bounds.left,
                top: bounds.top,
                width: (bounds.right - bounds.left) as u32,
                height: (bounds.bottom - bounds.top) as u32,
            };

            let mut device = None;
            let mut context = None;
            let mut level = D3D_FEATURE_LEVEL::default();
            // BGRA support because the swap chain is pinned to
            // B8G8R8A8_UNORM, matching what both capture backends are pinned
            // to.
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
            .map_err(api("D3D11CreateDevice"))?;

            let device = device.ok_or_else(|| {
                Error::Unsupported("D3D11CreateDevice returned no device".into())
            })?;
            let context = context.ok_or_else(|| {
                Error::Unsupported("D3D11CreateDevice returned no context".into())
            })?;

            Ok(Gpu {
                device,
                context,
                tearing: tearing_supported(&factory),
                factory,
                monitor,
                adapter_name: String::from_utf16_lossy(trim_nul(&adapter_desc.Description)),
            })
        }
    }

    pub fn context(&self) -> &ID3D11DeviceContext {
        &self.context
    }

    pub fn monitor(&self) -> &Monitor {
        &self.monitor
    }

    pub fn adapter_name(&self) -> &str {
        &self.adapter_name
    }

    /// Whether presents will be allowed to tear, and so to exceed the refresh
    /// rate. Worth printing: a run without it cannot honour an `--fps` above
    /// the panel's.
    pub fn tearing(&self) -> bool {
        self.tearing
    }

    /// Builds the flip-model swap chain for `hwnd`.
    pub fn swap_chain(&self, hwnd: HWND, width: u32, height: u32) -> Result<SwapChain, Error> {
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: width,
            Height: height,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            Stereo: false.into(),
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            // Three, not the flip model's minimum of two: at more than one
            // present per vblank the producer would otherwise block waiting
            // for the single spare buffer, and the rate it reports would be
            // the panel's rather than the one asked for.
            BufferCount: 3,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            Flags: if self.tearing {
                DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING.0 as u32
            } else {
                0
            },
        };

        // Tearing has to be asked for on the chain and again on every present;
        // asking on only one of the two is an error, not a downgrade.
        let present_flags = if self.tearing {
            DXGI_PRESENT_ALLOW_TEARING
        } else {
            DXGI_PRESENT(0)
        };

        // SAFETY: `hwnd` is a live window owned by the caller, the description
        // is fully initialised, and the returned interfaces are checked.
        unsafe {
            let chain = self
                .factory
                .CreateSwapChainForHwnd(&self.device, hwnd, &desc, None, None)
                .map_err(api("IDXGIFactory2::CreateSwapChainForHwnd"))?;

            // Alt+Enter would hand the window to DXGI's own full-screen
            // transition, changing the display mode underneath a measurement.
            self.factory
                .MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER)
                .map_err(api("IDXGIFactory2::MakeWindowAssociation"))?;

            let back_buffer: ID3D11Texture2D = chain
                .GetBuffer(0)
                .map_err(api("IDXGISwapChain1::GetBuffer"))?;
            let mut target = None;
            self.device
                .CreateRenderTargetView(&back_buffer, None, Some(&mut target))
                .map_err(api("ID3D11Device::CreateRenderTargetView"))?;
            let target = target.ok_or_else(|| {
                Error::Unsupported("CreateRenderTargetView returned no view".into())
            })?;

            Ok(SwapChain {
                chain,
                target,
                width,
                height,
                present_flags,
            })
        }
    }

    /// Compiles the shader and allocates the constant buffer.
    pub fn pipeline(&self) -> Result<Pipeline, Error> {
        let vertex_code = compile(s!("vs_main"), s!("vs_5_0"))?;
        let pixel_code = compile(s!("ps_main"), s!("ps_5_0"))?;

        let constants_desc = D3D11_BUFFER_DESC {
            ByteWidth: TICK_BYTES,
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
            StructureByteStride: 0,
        };

        // SAFETY: the bytecode slices are owned by the blobs held alive for
        // the length of these calls, the description is fully initialised, and
        // every out-pointer is checked.
        unsafe {
            let mut vertex = None;
            self.device
                .CreateVertexShader(bytecode(&vertex_code), None, Some(&mut vertex))
                .map_err(api("ID3D11Device::CreateVertexShader"))?;
            let mut pixel = None;
            self.device
                .CreatePixelShader(bytecode(&pixel_code), None, Some(&mut pixel))
                .map_err(api("ID3D11Device::CreatePixelShader"))?;
            let mut constants = None;
            self.device
                .CreateBuffer(&constants_desc, None, Some(&mut constants))
                .map_err(api("ID3D11Device::CreateBuffer"))?;

            match (vertex, pixel, constants) {
                (Some(vertex), Some(pixel), Some(constants)) => Ok(Pipeline {
                    vertex,
                    pixel,
                    constants,
                }),
                _ => Err(Error::Unsupported(
                    "the device accepted the shaders but returned nothing".into(),
                )),
            }
        }
    }
}

/// The back buffer chain and the view the shader writes through.
pub struct SwapChain {
    chain: IDXGISwapChain1,
    target: ID3D11RenderTargetView,
    width: u32,
    height: u32,
    present_flags: DXGI_PRESENT,
}

impl SwapChain {
    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Hands the back buffer to the compositor. `SyncInterval` is 0 and, where
    /// the adapter allows it, the present may tear: neither the panel's
    /// refresh nor DWM's cadence is permitted to set this producer's rate.
    pub fn present(&self) -> Result<(), Error> {
        // SAFETY: the chain is live for as long as this value is, and no
        // present parameters are being passed.
        unsafe { self.chain.Present1(0, self.present_flags, core::ptr::null()) }
            .ok()
            .map_err(api("IDXGISwapChain1::Present1"))
    }
}

/// The shaders and the one constant buffer they read.
pub struct Pipeline {
    vertex: ID3D11VertexShader,
    pixel: ID3D11PixelShader,
    constants: ID3D11Buffer,
}

impl Pipeline {
    /// Draws frame `frame_index` into `chain`'s back buffer.
    ///
    /// State is set every frame rather than once. It costs a handful of
    /// calls, and it means the picture cannot silently depend on state some
    /// earlier frame happened to leave behind.
    pub fn draw(&self, context: &ID3D11DeviceContext, chain: &SwapChain, frame_index: u32) {
        let tick = Tick {
            frame_index,
            width: chain.width,
            height: chain.height,
            reserved: 0,
        };
        let viewport = D3D11_VIEWPORT {
            TopLeftX: 0.0,
            TopLeftY: 0.0,
            Width: chain.width as f32,
            Height: chain.height as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        };

        // SAFETY: every interface belongs to the device that owns `context`,
        // and `tick` is a live 16-byte `#[repr(C)]` value matching the
        // constant buffer's declared width.
        unsafe {
            context.UpdateSubresource(
                &self.constants,
                0,
                None,
                (&raw const tick).cast(),
                TICK_BYTES,
                0,
            );
            context.OMSetRenderTargets(Some(&[Some(chain.target.clone())]), None);
            context.RSSetViewports(Some(&[viewport]));
            context.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            context.VSSetShader(&self.vertex, None);
            context.PSSetShader(&self.pixel, None);
            context.PSSetConstantBuffers(0, Some(&[Some(self.constants.clone())]));
            context.Draw(3, 0);
        }
    }
}

/// Whether this adapter and DWM will let a windowed flip chain present without
/// waiting for a vblank.
///
/// A pre-Windows-10-1703 factory has no `IDXGIFactory5` at all, so a failed
/// cast is a plain no, not an error worth stopping the run over.
fn tearing_supported(factory: &IDXGIFactory2) -> bool {
    let Ok(factory) = factory.cast::<IDXGIFactory5>() else {
        return false;
    };
    let mut allowed = BOOL(0);
    // SAFETY: the out-pointer is a live `BOOL` and the size passed matches it,
    // which is what `DXGI_FEATURE_PRESENT_ALLOW_TEARING` expects.
    let queried = unsafe {
        factory.CheckFeatureSupport(
            DXGI_FEATURE_PRESENT_ALLOW_TEARING,
            (&raw mut allowed).cast(),
            size_of::<BOOL>() as u32,
        )
    };
    queried.is_ok() && allowed.as_bool()
}

fn compile(entry: PCSTR, target: PCSTR) -> Result<ID3DBlob, Error> {
    // Not `WARNINGS_ARE_ERRORS`: a diagnostic that fxc considers cosmetic
    // would then refuse to start the producer at all. Warnings are printed
    // below instead, once, before any frame is drawn.
    let flags = D3DCOMPILE_ENABLE_STRICTNESS | D3DCOMPILE_OPTIMIZATION_LEVEL3;

    let mut code = None;
    let mut errors = None;
    // SAFETY: the source slice outlives the call, and both out-pointers are
    // valid `Option<ID3DBlob>` slots.
    let result = unsafe {
        D3DCompile(
            SOURCE_HLSL.as_ptr().cast(),
            SOURCE_HLSL.len(),
            s!("source.hlsl"),
            None,
            None,
            entry,
            target,
            flags,
            0,
            &mut code,
            Some(&mut errors),
        )
    };

    if let Err(error) = result {
        // The compiler's own diagnostics are the only useful thing here; the
        // HRESULT alone says nothing about which line was wrong.
        return match errors {
            Some(blob) => Err(Error::ShaderCompile(blob_text(&blob))),
            None => Err(api("D3DCompile")(error)),
        };
    }

    if let Some(blob) = errors {
        let warnings = blob_text(&blob);
        if !warnings.is_empty() {
            eprintln!("present-source: shader warnings:\n{warnings}");
        }
    }

    code.ok_or_else(|| Error::Unsupported("D3DCompile succeeded but produced no bytecode".into()))
}

fn bytecode(blob: &ID3DBlob) -> &[u8] {
    // SAFETY: a successful `D3DCompile` blob owns a contiguous buffer of the
    // reported size, and the borrow cannot outlive the blob.
    unsafe { core::slice::from_raw_parts(blob.GetBufferPointer().cast(), blob.GetBufferSize()) }
}

fn blob_text(blob: &ID3DBlob) -> String {
    String::from_utf8_lossy(bytecode(blob))
        .trim_end_matches(['\0', '\n', '\r'])
        .to_string()
}

fn trim_nul(text: &[u16]) -> &[u16] {
    let end = text.iter().position(|c| *c == 0).unwrap_or(text.len());
    &text[..end]
}
