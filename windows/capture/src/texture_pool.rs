//! Textures the streamer owns, for measuring what ownership costs.
//!
//! Phase 4 will have to decide whether the encoder holds the capture API's own
//! surface until it finishes, or whether we pay for a GPU copy into something
//! that is ours. That decision needs a price, and the price is measurable now,
//! before any encoder exists: copy the acquired frame into a texture from this
//! pool, release the source, and see what the copy cost and how long the
//! source was held.
//!
//! The pool is deliberately small and deliberately not a queue. A slot is
//! either free or in flight; a caller that cannot get one has learned
//! something worth reporting rather than something to paper over by waiting.

#![cfg(windows)]

use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_RESOURCE_MISC_SHARED,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, ID3D11Device, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};

use crate::backend::CaptureError;

/// A texture belonging to us, and whether anything is using it.
struct Slot {
    texture: ID3D11Texture2D,
    in_flight: bool,
}

pub struct TexturePool {
    slots: Vec<Slot>,
    width: u32,
    height: u32,
    /// Acquires that found every slot busy. A number worth reporting: it is
    /// the pool being too small for the rate, which is the thing phase 4 needs
    /// to size.
    starved: u64,
}

impl TexturePool {
    /// Allocates `count` textures matching the capture format.
    ///
    /// Shared and shader-readable because that is what the encoder and any
    /// colour conversion will want; allocating them differently now would
    /// measure a copy the product will not perform.
    pub fn new(
        device: &ID3D11Device,
        count: u32,
        width: u32,
        height: u32,
    ) -> Result<TexturePool, CaptureError> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: D3D11_RESOURCE_MISC_SHARED.0 as u32,
        };

        let mut slots = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let mut texture = None;
            // SAFETY: the description is fully initialised and the out-pointer
            // is valid for the duration of the call.
            unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture)) }.map_err(|e| {
                CaptureError::Api {
                    call: "ID3D11Device::CreateTexture2D",
                    hresult: e.code().0,
                }
            })?;
            let texture = texture.ok_or_else(|| {
                CaptureError::Unsupported("CreateTexture2D returned no texture".into())
            })?;
            slots.push(Slot {
                texture,
                in_flight: false,
            });
        }

        Ok(TexturePool {
            slots,
            width,
            height,
            starved: 0,
        })
    }

    /// Takes a free slot, or reports that there was none.
    pub fn take(&mut self) -> Option<PoolHandle> {
        match self.slots.iter().position(|slot| !slot.in_flight) {
            Some(index) => {
                self.slots[index].in_flight = true;
                Some(PoolHandle { index })
            }
            None => {
                self.starved += 1;
                None
            }
        }
    }

    pub fn texture(&self, handle: &PoolHandle) -> &ID3D11Texture2D {
        &self.slots[handle.index].texture
    }

    pub fn release(&mut self, handle: PoolHandle) {
        self.slots[handle.index].in_flight = false;
    }

    pub fn starved(&self) -> u64 {
        self.starved
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    pub fn in_flight(&self) -> usize {
        self.slots.iter().filter(|slot| slot.in_flight).count()
    }

    /// True when the pool no longer matches the output, which happens on a
    /// resolution change and means it must be rebuilt.
    pub fn matches(&self, width: u32, height: u32) -> bool {
        self.width == width && self.height == height
    }
}

/// A claim on one pool slot. Not `Copy`, so it cannot be released twice by
/// accident, and carries no lifetime because the pool outlives every claim.
#[derive(Debug, PartialEq, Eq)]
pub struct PoolHandle {
    index: usize,
}
