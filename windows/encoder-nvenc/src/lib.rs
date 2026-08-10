//! NVIDIA NVENC session access for the Windows host.
//!
//! The driver is loaded at runtime. A machine without `nvEncodeAPI64.dll`
//! therefore remains a valid non-NVIDIA build, while a session is opened only
//! after a real D3D11 device is available. This crate intentionally exposes
//! the capability query first: API presence is not proof that a D3D11 encode
//! session can be opened.

#![cfg(windows)]

use core::ffi::{c_char, c_void};
use core::fmt;
use core::mem;
use core::ptr;

use nvenc_sys::{
    GUID, NV_ENC_BUFFER_FORMAT, NV_ENC_BUFFER_USAGE, NV_ENC_CODEC_H264_GUID, NV_ENC_CONFIG_VER,
    NV_ENC_CREATE_BITSTREAM_BUFFER, NV_ENC_CREATE_BITSTREAM_BUFFER_VER, NV_ENC_DEVICE_TYPE,
    NV_ENC_H264_PROFILE_HIGH_GUID, NV_ENC_INITIALIZE_PARAMS, NV_ENC_INITIALIZE_PARAMS_VER,
    NV_ENC_INPUT_RESOURCE_TYPE, NV_ENC_LOCK_BITSTREAM, NV_ENC_LOCK_BITSTREAM_VER,
    NV_ENC_MAP_INPUT_RESOURCE, NV_ENC_MAP_INPUT_RESOURCE_VER, NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS,
    NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER, NV_ENC_PARAMS_RC_MODE, NV_ENC_PIC_FLAGS,
    NV_ENC_PIC_PARAMS, NV_ENC_PIC_PARAMS_VER, NV_ENC_PIC_STRUCT, NV_ENC_PRESET_CONFIG,
    NV_ENC_PRESET_CONFIG_VER, NV_ENC_PRESET_P1_GUID, NV_ENC_REGISTER_RESOURCE,
    NV_ENC_REGISTER_RESOURCE_VER, NV_ENC_REGISTERED_PTR, NV_ENC_TUNING_INFO,
    NV_ENCODE_API_FUNCTION_LIST, NV_ENCODE_API_FUNCTION_LIST_VER, NVENC_INFINITE_GOPLENGTH,
    NVENCAPI_VERSION, NVENCSTATUS, NvEncodeApiCreateInstanceFn,
};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::core::Interface;

#[allow(non_snake_case)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryW(file_name: *const u16) -> *mut c_void;
    fn FreeLibrary(module: *mut c_void) -> i32;
    fn GetProcAddress(module: *mut c_void, proc_name: *const c_char) -> *mut c_void;
}

const NVENC_DLL: &str = "nvEncodeAPI64.dll";
const CREATE_INSTANCE: &[u8] = b"NvEncodeAPICreateInstance\0";

/// A failure before or during a real NVENC session.
#[derive(Debug)]
pub enum NvencError {
    Unavailable(&'static str),
    NotInitialized,
    Api { call: &'static str, status: i32 },
    InvalidApiFunction(&'static str),
    InvalidCount(&'static str, u32),
    InvalidOutput(&'static str),
}

impl fmt::Display for NvencError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NvencError::Unavailable(why) => write!(f, "NVENC unavailable: {why}"),
            NvencError::NotInitialized => f.write_str("NVENC encoder is not initialized"),
            NvencError::Api { call, status } => {
                write!(f, "{call} failed with status {status}")
            }
            NvencError::InvalidApiFunction(name) => write!(f, "NVENC function missing: {name}"),
            NvencError::InvalidCount(name, count) => {
                write!(f, "NVENC returned unreasonable {name} count {count}")
            }
            NvencError::InvalidOutput(what) => write!(f, "NVENC returned invalid output: {what}"),
        }
    }
}

impl core::error::Error for NvencError {}

fn status(status: NVENCSTATUS, call: &'static str) -> Result<(), NvencError> {
    if status == NVENCSTATUS::NV_ENC_SUCCESS {
        Ok(())
    } else {
        Err(NvencError::Api {
            call,
            status: status as i32,
        })
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(core::iter::once(0)).collect()
}

/// The loaded function table and its owning DLL handle.
struct Api {
    module: *mut c_void,
    functions: NV_ENCODE_API_FUNCTION_LIST,
}

impl Api {
    fn load() -> Result<Api, NvencError> {
        let library = wide(NVENC_DLL);
        // SAFETY: the string is NUL terminated and the returned module is
        // owned by this value until Drop.
        let module = unsafe { LoadLibraryW(library.as_ptr()) };
        if module.is_null() {
            return Err(NvencError::Unavailable("nvEncodeAPI64.dll is not loadable"));
        }

        // SAFETY: the module is live and the symbol name is NUL terminated.
        let symbol = unsafe { GetProcAddress(module, CREATE_INSTANCE.as_ptr().cast()) };
        if symbol.is_null() {
            // SAFETY: this handle came from LoadLibraryW above.
            unsafe { FreeLibrary(module) };
            return Err(NvencError::InvalidApiFunction("NvEncodeAPICreateInstance"));
        }
        // SAFETY: NVIDIA publishes this symbol with this calling convention
        // and signature on Windows. The function table is zeroed before the
        // driver fills it.
        let create: NvEncodeApiCreateInstanceFn = unsafe { mem::transmute(symbol) };
        let mut functions = NV_ENCODE_API_FUNCTION_LIST {
            version: NV_ENCODE_API_FUNCTION_LIST_VER,
            ..Default::default()
        };
        let result = unsafe { create(&mut functions) };
        if let Err(error) = status(result, "NvEncodeAPICreateInstance") {
            // SAFETY: this handle came from LoadLibraryW above.
            unsafe { FreeLibrary(module) };
            return Err(error);
        }

        Ok(Api { module, functions })
    }

    fn has_open_session(&self) -> bool {
        self.functions.nvEncOpenEncodeSessionEx.is_some()
    }
}
impl Drop for Api {
    fn drop(&mut self) {
        // The session owns the function table, so the table is dropped only
        // after NvencSession has destroyed its encoder handle.
        if !self.module.is_null() {
            // SAFETY: this handle came from LoadLibraryW and is released once.
            unsafe { FreeLibrary(self.module) };
        }
    }
}

/// One synchronous H.264 result. The bytes are the Annex-B payload returned
/// by NVENC; the network boundary will convert NAL boundaries to AVCC/RTP.
#[derive(Debug)]
pub struct EncodedFrame {
    pub frame_index: u64,
    pub is_idr: bool,
    pub data: Vec<u8>,
}

pub struct SubmittedFrame<'a> {
    session: &'a NvencSession,
    mapped: *mut c_void,
    frame_index: u64,
    is_idr: bool,
}

/// A D3D11 texture registered for this session.
///
/// Registration persists for the pool slot. Mapping is per submission, so
/// D3D11 may write the texture again after the completed frame is unlocked.
pub struct RegisteredInput<'a> {
    session: &'a NvencSession,
    registered: NV_ENC_REGISTERED_PTR,
}

/// A reusable system-memory bitstream buffer owned by this session.
pub struct BitstreamBuffer<'a> {
    session: &'a NvencSession,
    output: *mut c_void,
}

/// An opened NVENC session bound to one D3D11 device.
///
/// This type does not copy or retain a frame. Resource registration and
/// encode submission are the next layer; keeping that distinction explicit
/// prevents a borrowed capture surface from accidentally outliving its API
/// ownership.
pub struct NvencSession {
    api: Api,
    encoder: *mut c_void,
    initialized: bool,
    width: u32,
    height: u32,
}

impl NvencSession {
    /// Opens NVENC against a live D3D11 device.
    pub fn open(device: &ID3D11Device) -> Result<NvencSession, NvencError> {
        let api = Api::load()?;
        if !api.has_open_session() {
            return Err(NvencError::InvalidApiFunction("nvEncOpenEncodeSessionEx"));
        }

        let mut encoder = ptr::null_mut();
        let mut params = NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS {
            version: NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER,
            deviceType: NV_ENC_DEVICE_TYPE::NV_ENC_DEVICE_TYPE_DIRECTX,
            device: device.as_raw(),
            apiVersion: NVENCAPI_VERSION,
            ..Default::default()
        };
        let open = api
            .functions
            .nvEncOpenEncodeSessionEx
            .ok_or(NvencError::InvalidApiFunction("nvEncOpenEncodeSessionEx"))?;
        // SAFETY: params is fully initialized, the D3D11 interface is live,
        // and the driver writes one session handle to a valid out-pointer.
        let result = unsafe { open(&mut params, &mut encoder) };
        status(result, "nvEncOpenEncodeSessionEx")?;
        if encoder.is_null() {
            return Err(NvencError::Unavailable(
                "the driver opened no encode session handle",
            ));
        }

        Ok(NvencSession {
            api,
            encoder,
            initialized: false,
            width: 0,
            height: 0,
        })
    }

    /// Enumerates codec GUIDs supported by this concrete session.
    pub fn encode_guids(&self) -> Result<Vec<GUID>, NvencError> {
        let mut count = 0u32;
        let get_count = self
            .api
            .functions
            .nvEncGetEncodeGUIDCount
            .ok_or(NvencError::InvalidApiFunction("nvEncGetEncodeGUIDCount"))?;
        status(
            unsafe { get_count(self.encoder, &mut count) },
            "nvEncGetEncodeGUIDCount",
        )?;
        if count > 64 {
            return Err(NvencError::InvalidCount("codec GUID", count));
        }
        let mut guids = vec![GUID::default(); count as usize];
        let mut actual = 0u32;
        let get_guids = self
            .api
            .functions
            .nvEncGetEncodeGUIDs
            .ok_or(NvencError::InvalidApiFunction("nvEncGetEncodeGUIDs"))?;
        status(
            unsafe { get_guids(self.encoder, guids.as_mut_ptr(), count, &mut actual) },
            "nvEncGetEncodeGUIDs",
        )?;
        if actual > count {
            return Err(NvencError::InvalidCount("returned codec GUID", actual));
        }
        guids.truncate(actual as usize);
        Ok(guids)
    }

    /// Enumerates input formats accepted for a codec GUID.
    pub fn input_formats(&self, codec: GUID) -> Result<Vec<NV_ENC_BUFFER_FORMAT>, NvencError> {
        let mut count = 0u32;
        let get_count = self
            .api
            .functions
            .nvEncGetInputFormatCount
            .ok_or(NvencError::InvalidApiFunction("nvEncGetInputFormatCount"))?;
        status(
            unsafe { get_count(self.encoder, codec, &mut count) },
            "nvEncGetInputFormatCount",
        )?;
        if count > 64 {
            return Err(NvencError::InvalidCount("input format", count));
        }
        let mut formats =
            vec![NV_ENC_BUFFER_FORMAT::NV_ENC_BUFFER_FORMAT_UNDEFINED; count as usize];
        let mut actual = 0u32;
        let get_formats = self
            .api
            .functions
            .nvEncGetInputFormats
            .ok_or(NvencError::InvalidApiFunction("nvEncGetInputFormats"))?;
        status(
            unsafe {
                get_formats(
                    self.encoder,
                    codec,
                    formats.as_mut_ptr(),
                    count,
                    &mut actual,
                )
            },
            "nvEncGetInputFormats",
        )?;
        if actual > count {
            return Err(NvencError::InvalidCount("returned input format", actual));
        }
        formats.truncate(actual as usize);
        Ok(formats)
    }

    /// Configures H.264 P1 ultra-low-latency CBR with no B-frames.
    ///
    /// Synchronous mode is deliberate for the first isolated benchmark: it
    /// makes encode completion measurable without Windows event plumbing.
    pub fn initialize_h264(
        &mut self,
        width: u32,
        height: u32,
        fps_num: u32,
        fps_den: u32,
        bitrate: u32,
    ) -> Result<(), NvencError> {
        if self.initialized {
            return Err(NvencError::InvalidOutput("session initialized twice"));
        }
        if width == 0 || height == 0 || fps_num == 0 || fps_den == 0 || bitrate == 0 {
            return Err(NvencError::InvalidOutput(
                "zero encoder configuration value",
            ));
        }

        let tuning = NV_ENC_TUNING_INFO::NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY;
        let mut preset = NV_ENC_PRESET_CONFIG {
            version: NV_ENC_PRESET_CONFIG_VER,
            presetCfg: nvenc_sys::NV_ENC_CONFIG {
                version: NV_ENC_CONFIG_VER,
                ..Default::default()
            },
            ..Default::default()
        };
        let get_preset = self.api.functions.nvEncGetEncodePresetConfigEx.ok_or(
            NvencError::InvalidApiFunction("nvEncGetEncodePresetConfigEx"),
        )?;
        status(
            unsafe {
                get_preset(
                    self.encoder,
                    NV_ENC_CODEC_H264_GUID,
                    NV_ENC_PRESET_P1_GUID,
                    tuning,
                    &mut preset,
                )
            },
            "nvEncGetEncodePresetConfigEx",
        )?;

        let config = &mut preset.presetCfg;
        config.version = NV_ENC_CONFIG_VER;
        config.profileGUID = NV_ENC_H264_PROFILE_HIGH_GUID;
        config.gopLength = NVENC_INFINITE_GOPLENGTH;
        config.frameIntervalP = 1;
        config.rcParams.rateControlMode = NV_ENC_PARAMS_RC_MODE::NV_ENC_PARAMS_RC_CBR;
        config.rcParams.averageBitRate = bitrate;
        config.rcParams.maxBitRate = bitrate;
        config.rcParams.vbvBufferSize = bitrate.saturating_mul(fps_den) / fps_num;
        config.rcParams.vbvInitialDelay = config.rcParams.vbvBufferSize;

        let mut params = NV_ENC_INITIALIZE_PARAMS {
            version: NV_ENC_INITIALIZE_PARAMS_VER,
            encodeGUID: NV_ENC_CODEC_H264_GUID,
            presetGUID: NV_ENC_PRESET_P1_GUID,
            encodeWidth: width,
            encodeHeight: height,
            darWidth: width,
            darHeight: height,
            frameRateNum: fps_num,
            frameRateDen: fps_den,
            enableEncodeAsync: 0,
            enablePTD: 1,
            encodeConfig: config,
            maxEncodeWidth: width,
            maxEncodeHeight: height,
            tuningInfo: tuning,
            ..Default::default()
        };
        let initialize = self
            .api
            .functions
            .nvEncInitializeEncoder
            .ok_or(NvencError::InvalidApiFunction("nvEncInitializeEncoder"))?;
        status(
            unsafe { initialize(self.encoder, &mut params) },
            "nvEncInitializeEncoder",
        )?;
        self.initialized = true;
        self.width = width;
        self.height = height;
        Ok(())
    }

    /// Registers one pool-owned BGRA texture and keeps it mapped.
    pub fn register_bgra<'a>(
        &'a self,
        texture: &ID3D11Texture2D,
    ) -> Result<RegisteredInput<'a>, NvencError> {
        if !self.initialized {
            return Err(NvencError::NotInitialized);
        }
        let mut register = NV_ENC_REGISTER_RESOURCE {
            version: NV_ENC_REGISTER_RESOURCE_VER,
            resourceType: NV_ENC_INPUT_RESOURCE_TYPE::NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX,
            width: self.width,
            height: self.height,
            resourceToRegister: texture.as_raw(),
            bufferFormat: NV_ENC_BUFFER_FORMAT::NV_ENC_BUFFER_FORMAT_ARGB,
            bufferUsage: NV_ENC_BUFFER_USAGE::NV_ENC_INPUT_IMAGE,
            ..Default::default()
        };
        let register_fn = self
            .api
            .functions
            .nvEncRegisterResource
            .ok_or(NvencError::InvalidApiFunction("nvEncRegisterResource"))?;
        status(
            unsafe { register_fn(self.encoder, &mut register) },
            "nvEncRegisterResource",
        )?;
        if register.registeredResource.is_null() {
            return Err(NvencError::InvalidOutput("null registered resource"));
        }

        Ok(RegisteredInput {
            session: self,
            registered: register.registeredResource,
        })
    }

    /// Allocates one reusable output buffer.
    pub fn create_bitstream_buffer(&self) -> Result<BitstreamBuffer<'_>, NvencError> {
        if !self.initialized {
            return Err(NvencError::NotInitialized);
        }
        let mut create = NV_ENC_CREATE_BITSTREAM_BUFFER {
            version: NV_ENC_CREATE_BITSTREAM_BUFFER_VER,
            ..Default::default()
        };
        let create_fn = self
            .api
            .functions
            .nvEncCreateBitstreamBuffer
            .ok_or(NvencError::InvalidApiFunction("nvEncCreateBitstreamBuffer"))?;
        status(
            unsafe { create_fn(self.encoder, &mut create) },
            "nvEncCreateBitstreamBuffer",
        )?;
        if create.bitstreamBuffer.is_null() {
            return Err(NvencError::InvalidOutput("null bitstream buffer"));
        }
        Ok(BitstreamBuffer {
            session: self,
            output: create.bitstreamBuffer,
        })
    }

    /// Maps and submits one texture without locking its output buffer.
    pub fn submit_bgra<'a>(
        &'a self,
        input: &RegisteredInput<'_>,
        output: &BitstreamBuffer<'_>,
        frame_index: u64,
        force_idr: bool,
    ) -> Result<SubmittedFrame<'a>, NvencError> {
        if !core::ptr::eq(self, input.session) || !core::ptr::eq(self, output.session) {
            return Err(NvencError::InvalidOutput(
                "resource belongs to another NVENC session",
            ));
        }
        let mut map = NV_ENC_MAP_INPUT_RESOURCE {
            version: NV_ENC_MAP_INPUT_RESOURCE_VER,
            registeredResource: input.registered,
            ..Default::default()
        };
        let map_fn = self
            .api
            .functions
            .nvEncMapInputResource
            .ok_or(NvencError::InvalidApiFunction("nvEncMapInputResource"))?;
        status(
            unsafe { map_fn(self.encoder, &mut map) },
            "nvEncMapInputResource",
        )?;
        if map.mappedResource.is_null() {
            return Err(NvencError::InvalidOutput("null mapped resource"));
        }

        let flags = if force_idr {
            NV_ENC_PIC_FLAGS::NV_ENC_PIC_FLAG_FORCEIDR as u32
                | NV_ENC_PIC_FLAGS::NV_ENC_PIC_FLAG_OUTPUT_SPSPPS as u32
        } else {
            0
        };
        let mut picture = NV_ENC_PIC_PARAMS {
            version: NV_ENC_PIC_PARAMS_VER,
            inputWidth: self.width,
            inputHeight: self.height,
            inputPitch: self.width,
            encodePicFlags: flags,
            frameIdx: frame_index as u32,
            inputTimeStamp: frame_index,
            inputDuration: 1,
            inputBuffer: map.mappedResource,
            outputBitstream: output.output,
            bufferFmt: map.mappedBufferFmt,
            pictureStruct: NV_ENC_PIC_STRUCT::NV_ENC_PIC_STRUCT_FRAME,
            ..Default::default()
        };
        let encode = self
            .api
            .functions
            .nvEncEncodePicture
            .ok_or(NvencError::InvalidApiFunction("nvEncEncodePicture"))?;
        if let Err(error) = status(
            unsafe { encode(self.encoder, &mut picture) },
            "nvEncEncodePicture",
        ) {
            if let Some(unmap) = self.api.functions.nvEncUnmapInputResource {
                unsafe { unmap(self.encoder, map.mappedResource) };
            }
            return Err(error);
        }
        Ok(SubmittedFrame {
            session: self,
            mapped: map.mappedResource,
            frame_index,
            is_idr: force_idr,
        })
    }

    /// Waits for one submitted output and copies its Annex-B bytes.
    pub fn lock_bitstream(
        &self,
        output: &BitstreamBuffer<'_>,
        submitted: SubmittedFrame<'_>,
    ) -> Result<EncodedFrame, NvencError> {
        if !core::ptr::eq(self, output.session) || !core::ptr::eq(self, submitted.session) {
            return Err(NvencError::InvalidOutput(
                "bitstream belongs to another NVENC session",
            ));
        }
        let mut lock = NV_ENC_LOCK_BITSTREAM {
            version: NV_ENC_LOCK_BITSTREAM_VER,
            outputBitstream: output.output,
            ..Default::default()
        };
        let lock_fn = self
            .api
            .functions
            .nvEncLockBitstream
            .ok_or(NvencError::InvalidApiFunction("nvEncLockBitstream"))?;
        status(
            unsafe { lock_fn(self.encoder, &mut lock) },
            "nvEncLockBitstream",
        )?;
        let result = if lock.bitstreamBufferPtr.is_null() {
            Err(NvencError::InvalidOutput("null locked bitstream"))
        } else {
            let bytes = unsafe {
                core::slice::from_raw_parts(
                    lock.bitstreamBufferPtr.cast::<u8>(),
                    lock.bitstreamSizeInBytes as usize,
                )
            };
            Ok(EncodedFrame {
                frame_index: submitted.frame_index,
                is_idr: submitted.is_idr,
                data: bytes.to_vec(),
            })
        };
        let unlock = self
            .api
            .functions
            .nvEncUnlockBitstream
            .ok_or(NvencError::InvalidApiFunction("nvEncUnlockBitstream"))?;
        status(
            unsafe { unlock(self.encoder, output.output) },
            "nvEncUnlockBitstream",
        )?;
        result
    }

    /// Convenience path for callers that do not need separate submit timing.
    pub fn encode_bgra(
        &self,
        input: &RegisteredInput<'_>,
        output: &BitstreamBuffer<'_>,
        frame_index: u64,
        force_idr: bool,
    ) -> Result<EncodedFrame, NvencError> {
        let submitted = self.submit_bgra(input, output, frame_index, force_idr)?;
        self.lock_bitstream(output, submitted)
    }

    pub fn h264_codec_guid() -> GUID {
        NV_ENC_CODEC_H264_GUID
    }

    /// Returns whether a registered resource description is the D3D11 BGRA
    /// format used by the capture backends.
    pub fn directx_bgra_resource_type() -> NV_ENC_INPUT_RESOURCE_TYPE {
        NV_ENC_INPUT_RESOURCE_TYPE::NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX
    }
}

impl Drop for SubmittedFrame<'_> {
    fn drop(&mut self) {
        if let Some(unmap) = self.session.api.functions.nvEncUnmapInputResource {
            unsafe { unmap(self.session.encoder, self.mapped) };
        }
    }
}

impl Drop for RegisteredInput<'_> {
    fn drop(&mut self) {
        // A submitted frame borrows this registration, so Rust prevents this
        // destructor from running until its per-frame mapping is gone.
        if let Some(unregister) = self.session.api.functions.nvEncUnregisterResource {
            unsafe { unregister(self.session.encoder, self.registered) };
        }
    }
}

impl Drop for BitstreamBuffer<'_> {
    fn drop(&mut self) {
        if let Some(destroy) = self.session.api.functions.nvEncDestroyBitstreamBuffer {
            unsafe { destroy(self.session.encoder, self.output) };
        }
    }
}

impl Drop for NvencSession {
    fn drop(&mut self) {
        if !self.encoder.is_null() {
            if let Some(destroy) = self.api.functions.nvEncDestroyEncoder {
                // SAFETY: the handle was returned by nvEncOpenEncodeSessionEx
                // and this Drop is the unique owner.
                unsafe { destroy(self.encoder) };
            }
        }
    }
}

// The raw session handle is confined to the encode thread. Do not make this
// Send or Sync: NVENC's session functions are documented as thread-affine for
// several operations, and the benchmark owns one session per producer thread.
