use core::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_core_foundation::CFRetained;
use objc2_core_video::{
    CVMetalTexture, CVMetalTextureCache, CVMetalTextureGetTexture, CVPixelBuffer,
    CVPixelBufferGetHeightOfPlane, CVPixelBufferGetWidthOfPlane, kCVReturnSuccess,
};
use objc2_metal::{
    MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue, MTLDevice, MTLDrawable, MTLLibrary,
    MTLLoadAction, MTLPixelFormat, MTLPrimitiveType, MTLRenderCommandEncoder,
    MTLRenderPassDescriptor, MTLRenderPipelineDescriptor, MTLRenderPipelineState, MTLStoreAction,
};
use objc2_quartz_core::CAMetalDrawable;

use crate::error::RendererError;
use crate::shader;
use crate::slot::SurfaceFrame;

/// Frames whose GPU resources are kept alive after submission.
///
/// Metal retains the textures a command buffer references, but nothing retains
/// the *pixel buffer* they alias; releasing it at once would return the
/// IOSurface to the decoder's pool while the GPU is still sampling it, and the
/// next decode would overwrite the picture on screen. Three matches
/// `maximumDrawableCount`: by the time slot `n` is reused, three later
/// drawables have been vended, which cannot happen until the command buffer
/// that used slot `n` has completed and recycled its own drawable.
const IN_FLIGHT: usize = 3;

/// One submitted frame's resources, held only to delay their release.
struct Submission {
    _frame: SurfaceFrame,
    _luma: CFRetained<CVMetalTexture>,
    _chroma: CFRetained<CVMetalTexture>,
}

/// The Metal half of the presenter: everything built once, plus the per-frame
/// encode.
pub(crate) struct Gpu {
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    pipeline: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    texture_cache: CFRetained<CVMetalTextureCache>,
    /// Reused rather than rebuilt per frame; only its texture changes.
    pass: Retained<MTLRenderPassDescriptor>,
    in_flight: [Option<Submission>; IN_FLIGHT],
    next_slot: usize,
}

impl Gpu {
    pub(crate) fn new(device: &ProtocolObject<dyn MTLDevice>) -> Result<Gpu, RendererError> {
        let queue = device
            .newCommandQueue()
            .ok_or(RendererError::NoCommandQueue)?;

        let source = objc2_foundation::NSString::from_str(shader::NV12_TO_RGB);
        let library = device
            .newLibraryWithSource_options_error(&source, None)
            .map_err(|error| {
                RendererError::ShaderCompile(error.localizedDescription().to_string())
            })?;

        let vertex = library
            .newFunctionWithName(&objc2_foundation::NSString::from_str("nv12_vertex"))
            .ok_or(RendererError::MissingShaderFunction("nv12_vertex"))?;
        let fragment = library
            .newFunctionWithName(&objc2_foundation::NSString::from_str("nv12_fragment"))
            .ok_or(RendererError::MissingShaderFunction("nv12_fragment"))?;

        let descriptor = MTLRenderPipelineDescriptor::new();
        descriptor.setVertexFunction(Some(&vertex));
        descriptor.setFragmentFunction(Some(&fragment));
        // SAFETY: attachment 0 always exists on a fresh descriptor.
        unsafe { descriptor.colorAttachments().objectAtIndexedSubscript(0) }
            .setPixelFormat(MTLPixelFormat::BGRA8Unorm);

        let pipeline = device
            .newRenderPipelineStateWithDescriptor_error(&descriptor)
            .map_err(|error| {
                RendererError::PipelineCreate(error.localizedDescription().to_string())
            })?;

        let mut raw_cache: *mut CVMetalTextureCache = core::ptr::null_mut();
        // SAFETY: `raw_cache` is a live local; passing no attribute
        // dictionaries means there are no dictionary generics to get wrong.
        let status = unsafe {
            CVMetalTextureCache::create(None, None, device, None, NonNull::from(&mut raw_cache))
        };
        let cache = NonNull::new(raw_cache)
            .filter(|_| status == kCVReturnSuccess)
            .ok_or(RendererError::TextureCacheCreate(status))?;
        // SAFETY: `CVMetalTextureCacheCreate` follows the create rule, so this
        // is the only reference and `CFRetained` becomes its owner.
        let texture_cache = unsafe { CFRetained::from_raw(cache) };

        let pass = MTLRenderPassDescriptor::new();
        // SAFETY: attachment 0 always exists on a fresh descriptor.
        let attachment = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
        // The triangle covers every pixel of the drawable, so there is nothing
        // worth loading; skipping the load saves a full-surface clear.
        attachment.setLoadAction(MTLLoadAction::DontCare);
        attachment.setStoreAction(MTLStoreAction::Store);

        Ok(Gpu {
            queue,
            pipeline,
            texture_cache,
            pass,
            in_flight: [const { None }; IN_FLIGHT],
            next_slot: 0,
        })
    }

    /// Binds `frame`'s two planes as Metal textures, converts them into
    /// `drawable` and commits. Returns once the work is queued; the GPU is
    /// still busy.
    ///
    /// Nothing here reads or writes pixels on the CPU: both textures alias the
    /// frame's IOSurface through the texture cache, and the only data the
    /// command buffer carries is the pipeline state and two texture bindings.
    pub(crate) fn draw(
        &mut self,
        frame: SurfaceFrame,
        drawable: &ProtocolObject<dyn CAMetalDrawable>,
    ) -> Result<(), RendererError> {
        let luma = self.plane_texture(&frame.pixel_buffer, 0, MTLPixelFormat::R8Unorm)?;
        let chroma = self.plane_texture(&frame.pixel_buffer, 1, MTLPixelFormat::RG8Unorm)?;

        let luma_texture = CVMetalTextureGetTexture(&luma).ok_or(RendererError::TextureBind {
            plane: 0,
            status: 0,
        })?;
        let chroma_texture =
            CVMetalTextureGetTexture(&chroma).ok_or(RendererError::TextureBind {
                plane: 1,
                status: 0,
            })?;

        // SAFETY: attachment 0 always exists on this descriptor.
        unsafe { self.pass.colorAttachments().objectAtIndexedSubscript(0) }
            .setTexture(Some(&drawable.texture()));

        let command_buffer = self
            .queue
            .commandBuffer()
            .ok_or(RendererError::NoCommandQueue)?;
        let encoder = command_buffer
            .renderCommandEncoderWithDescriptor(&self.pass)
            .ok_or(RendererError::NoCommandQueue)?;
        encoder.setRenderPipelineState(&self.pipeline);
        // SAFETY: the fragment function declares exactly these two texture
        // bindings, at indices 0 and 1.
        unsafe {
            encoder.setFragmentTexture_atIndex(Some(&luma_texture), 0);
            encoder.setFragmentTexture_atIndex(Some(&chroma_texture), 1);
            // The vertex function reads only `vertex_id`, so three vertices
            // with no buffers bound is the whole draw.
            encoder.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::Triangle, 0, 3);
        }
        encoder.endEncoding();

        let presentable: &ProtocolObject<dyn MTLDrawable> = ProtocolObject::from_ref(drawable);
        command_buffer.presentDrawable(presentable);
        command_buffer.commit();

        self.retire(Submission {
            _frame: frame,
            _luma: luma,
            _chroma: chroma,
        });

        // The cache keeps every texture it has vended alive until it is
        // flushed. Without this, the IOSurfaces behind them stay referenced,
        // the decoder's pixel buffer pool runs dry within a few frames and
        // decoding stalls waiting for a surface that will never come back.
        self.texture_cache.flush(0);

        Ok(())
    }

    fn plane_texture(
        &self,
        pixel_buffer: &CVPixelBuffer,
        plane: usize,
        format: MTLPixelFormat,
    ) -> Result<CFRetained<CVMetalTexture>, RendererError> {
        let width = CVPixelBufferGetWidthOfPlane(pixel_buffer, plane);
        let height = CVPixelBufferGetHeightOfPlane(pixel_buffer, plane);

        let mut raw: *mut CVMetalTexture = core::ptr::null_mut();
        // SAFETY: `raw` is a live local and no attribute dictionary is passed.
        let status = unsafe {
            CVMetalTextureCache::create_texture_from_image(
                None,
                &self.texture_cache,
                pixel_buffer,
                None,
                format,
                width,
                height,
                plane,
                NonNull::from(&mut raw),
            )
        };
        let texture = NonNull::new(raw)
            .filter(|_| status == kCVReturnSuccess)
            .ok_or(RendererError::TextureBind { plane, status })?;
        // SAFETY: the create rule again; we own the returned reference.
        Ok(unsafe { CFRetained::from_raw(texture) })
    }

    fn retire(&mut self, submission: Submission) {
        // Overwriting the slot is what finally releases the frame three
        // submissions ago, outside any lock and off the encode path.
        self.in_flight[self.next_slot] = Some(submission);
        self.next_slot = (self.next_slot + 1) % IN_FLIGHT;
    }
}
