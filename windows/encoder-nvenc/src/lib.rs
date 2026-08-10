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
    GUID, NV_ENC_BUFFER_FORMAT, NV_ENC_DEVICE_TYPE, NV_ENC_INPUT_RESOURCE_TYPE,
    NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS, NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER,
    NV_ENCODE_API_FUNCTION_LIST, NV_ENCODE_API_FUNCTION_LIST_VER, NVENCAPI_VERSION, NVENCSTATUS,
    NvEncodeApiCreateInstanceFn,
};
use windows::Win32::Graphics::Direct3D11::ID3D11Device;
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
    Api { call: &'static str, status: i32 },
    InvalidApiFunction(&'static str),
    InvalidCount(&'static str, u32),
}

impl fmt::Display for NvencError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NvencError::Unavailable(why) => write!(f, "NVENC unavailable: {why}"),
            NvencError::Api { call, status } => {
                write!(f, "{call} failed with status {status}")
            }
            NvencError::InvalidApiFunction(name) => write!(f, "NVENC function missing: {name}"),
            NvencError::InvalidCount(name, count) => {
                write!(f, "NVENC returned unreasonable {name} count {count}")
            }
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

/// An opened NVENC session bound to one D3D11 device.
///
/// This type does not copy or retain a frame. Resource registration and
/// encode submission are the next layer; keeping that distinction explicit
/// prevents a borrowed capture surface from accidentally outliving its API
/// ownership.
pub struct NvencSession {
    api: Api,
    encoder: *mut c_void,
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

        Ok(NvencSession { api, encoder })
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

    /// Returns whether a registered resource description is the D3D11 BGRA
    /// format used by the capture backends.
    pub fn directx_bgra_resource_type() -> NV_ENC_INPUT_RESOURCE_TYPE {
        NV_ENC_INPUT_RESOURCE_TYPE::NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX
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
