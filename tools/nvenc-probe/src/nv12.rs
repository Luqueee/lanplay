//! D3D11 video-processor conversion from benchmark BGRA textures to NV12.
//!
//! The encoder sees one persistent NV12 texture per pool slot. Conversion is
//! queued on the same immediate context before NVENC maps that slot, so no raw
//! frame crosses the CPU and D3D11 preserves the producer/consumer ordering.

use core::mem::{ManuallyDrop, size_of};

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_QUERY_DATA_TIMESTAMP_DISJOINT,
    D3D11_QUERY_DESC, D3D11_QUERY_TIMESTAMP, D3D11_QUERY_TIMESTAMP_DISJOINT,
    D3D11_RESOURCE_MISC_FLAG, D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, D3D11_VIDEO_PROCESSOR_COLOR_SPACE,
    D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT,
    D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_STREAM,
    D3D11_VIDEO_USAGE_PLAYBACK_NORMAL, D3D11_VPIV_DIMENSION_TEXTURE2D,
    D3D11_VPOV_DIMENSION_TEXTURE2D, ID3D11Device, ID3D11DeviceContext, ID3D11Query,
    ID3D11Texture2D, ID3D11VideoContext, ID3D11VideoProcessor, ID3D11VideoProcessorInputView,
    ID3D11VideoProcessorOutputView,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};
use windows::core::{BOOL, Interface};

struct Slot {
    texture: ID3D11Texture2D,
    input: ID3D11VideoProcessorInputView,
    output: ID3D11VideoProcessorOutputView,
    disjoint: ID3D11Query,
    start: ID3D11Query,
    end: ID3D11Query,
    pending_frame: Option<u64>,
}

pub struct Converter {
    immediate: ID3D11DeviceContext,
    context: ID3D11VideoContext,
    processor: ID3D11VideoProcessor,
    slots: Vec<Slot>,
}

impl Converter {
    pub fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        width: u32,
        height: u32,
        fps: u32,
        bgra: &[ID3D11Texture2D],
    ) -> Result<Self, String> {
        let video_device = device
            .cast::<windows::Win32::Graphics::Direct3D11::ID3D11VideoDevice>()
            .map_err(|error| format!("ID3D11VideoDevice: {error}"))?;
        let video_context = context
            .cast::<ID3D11VideoContext>()
            .map_err(|error| format!("ID3D11VideoContext: {error}"))?;
        let rate = DXGI_RATIONAL {
            Numerator: fps,
            Denominator: 1,
        };
        let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: width,
            InputHeight: height,
            OutputFrameRate: rate,
            OutputWidth: width,
            OutputHeight: height,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };

        // SAFETY: all descriptors and out-pointers are fully initialised and
        // every resource remains owned by the returned converter.
        unsafe {
            let enumerator = video_device
                .CreateVideoProcessorEnumerator(&raw const content)
                .map_err(|error| format!("CreateVideoProcessorEnumerator: {error}"))?;
            let bgra_support = enumerator
                .CheckVideoProcessorFormat(DXGI_FORMAT_B8G8R8A8_UNORM)
                .map_err(|error| format!("CheckVideoProcessorFormat(BGRA): {error}"))?;
            let nv12_support = enumerator
                .CheckVideoProcessorFormat(DXGI_FORMAT_NV12)
                .map_err(|error| format!("CheckVideoProcessorFormat(NV12): {error}"))?;
            if bgra_support & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0 as u32 == 0 {
                return Err("D3D11 video processor does not accept BGRA input".into());
            }
            if nv12_support & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0 as u32 == 0 {
                return Err("D3D11 video processor does not produce NV12 output".into());
            }
            let processor = video_device
                .CreateVideoProcessor(&enumerator, 0)
                .map_err(|error| format!("CreateVideoProcessor: {error}"))?;

            let rect = RECT {
                left: 0,
                top: 0,
                right: width as i32,
                bottom: height as i32,
            };
            video_context.VideoProcessorSetStreamFrameFormat(
                &processor,
                0,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            );
            video_context.VideoProcessorSetStreamSourceRect(
                &processor,
                0,
                true,
                Some(&raw const rect),
            );
            video_context.VideoProcessorSetStreamDestRect(
                &processor,
                0,
                true,
                Some(&raw const rect),
            );
            video_context.VideoProcessorSetOutputTargetRect(
                &processor,
                true,
                Some(&raw const rect),
            );
            video_context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);

            // Full-range RGB input, Rec.709 studio-range YCbCr output.
            let rgb = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 0 };
            let yuv709_limited = D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
                _bitfield: (1 << 2) | (1 << 4),
            };
            video_context.VideoProcessorSetStreamColorSpace(&processor, 0, &rgb);
            video_context.VideoProcessorSetOutputColorSpace(&processor, &yuv709_limited);

            let mut slots = Vec::with_capacity(bgra.len());
            for source in bgra {
                let texture_desc = D3D11_TEXTURE2D_DESC {
                    Width: width,
                    Height: height,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_NV12,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                    CPUAccessFlags: 0,
                    MiscFlags: D3D11_RESOURCE_MISC_FLAG(0).0 as u32,
                };
                let mut texture = None;
                device
                    .CreateTexture2D(&texture_desc, None, Some(&mut texture))
                    .map_err(|error| format!("CreateTexture2D(NV12): {error}"))?;
                let texture =
                    texture.ok_or_else(|| "CreateTexture2D(NV12) returned null".to_owned())?;

                let input_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                    FourCC: 0,
                    ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                        Texture2D: D3D11_TEX2D_VPIV {
                            MipSlice: 0,
                            ArraySlice: 0,
                        },
                    },
                };
                let mut input = None;
                video_device
                    .CreateVideoProcessorInputView(
                        source,
                        &enumerator,
                        &raw const input_desc,
                        Some(&raw mut input),
                    )
                    .map_err(|error| format!("CreateVideoProcessorInputView: {error}"))?;
                let input = input
                    .ok_or_else(|| "CreateVideoProcessorInputView returned null".to_owned())?;

                let output_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                    ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                        Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
                    },
                };
                let mut output = None;
                video_device
                    .CreateVideoProcessorOutputView(
                        &texture,
                        &enumerator,
                        &raw const output_desc,
                        Some(&raw mut output),
                    )
                    .map_err(|error| format!("CreateVideoProcessorOutputView: {error}"))?;
                let output = output
                    .ok_or_else(|| "CreateVideoProcessorOutputView returned null".to_owned())?;
                let disjoint = create_query(device, D3D11_QUERY_TIMESTAMP_DISJOINT)?;
                let start = create_query(device, D3D11_QUERY_TIMESTAMP)?;
                let end = create_query(device, D3D11_QUERY_TIMESTAMP)?;
                slots.push(Slot {
                    texture,
                    input,
                    output,
                    disjoint,
                    start,
                    end,
                    pending_frame: None,
                });
            }

            Ok(Self {
                immediate: context.clone(),
                context: video_context,
                processor,
                slots,
            })
        }
    }

    pub fn texture(&self, slot: usize) -> &ID3D11Texture2D {
        &self.slots[slot].texture
    }

    /// Queues one conversion and returns the prior GPU duration for this slot.
    ///
    /// A slot only becomes reusable after NVENC signals completion, so its
    /// prior timestamp query is ready here without stalling the producer.
    pub fn convert(&mut self, slot_index: usize, frame: u64) -> Result<Option<(u64, u64)>, String> {
        let completed = self.resolve(slot_index)?;
        let slot = &mut self.slots[slot_index];
        let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: BOOL(1),
            pInputSurface: ManuallyDrop::new(Some(slot.input.clone())),
            ..Default::default()
        };
        // SAFETY: the queries, views and processor share one device. The
        // stream owns one temporary COM reference, dropped after submission.
        let result = unsafe {
            self.immediate.Begin(&slot.disjoint);
            self.immediate.End(&slot.start);
            let result = self.context.VideoProcessorBlt(
                &self.processor,
                &slot.output,
                frame as u32,
                core::slice::from_ref(&stream),
            );
            self.immediate.End(&slot.end);
            self.immediate.End(&slot.disjoint);
            result
        };
        unsafe { ManuallyDrop::drop(&mut stream.pInputSurface) };
        result.map_err(|error| format!("VideoProcessorBlt frame {frame}: {error}"))?;
        slot.pending_frame = Some(frame);
        Ok(completed)
    }

    pub fn finish_timings(&mut self) -> Result<Vec<(u64, u64)>, String> {
        (0..self.slots.len())
            .filter_map(|slot| self.resolve(slot).transpose())
            .collect()
    }

    fn resolve(&mut self, slot_index: usize) -> Result<Option<(u64, u64)>, String> {
        let slot = &mut self.slots[slot_index];
        let Some(frame) = slot.pending_frame.take() else {
            return Ok(None);
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let disjoint = loop {
            let mut value = D3D11_QUERY_DATA_TIMESTAMP_DISJOINT::default();
            // SAFETY: the output buffer has the query's exact data size.
            unsafe {
                self.immediate
                    .GetData(
                        &slot.disjoint,
                        Some((&raw mut value).cast()),
                        size_of::<D3D11_QUERY_DATA_TIMESTAMP_DISJOINT>() as u32,
                        0,
                    )
                    .map_err(|error| format!("GetData(disjoint) frame {frame}: {error}"))?;
            }
            if value.Frequency != 0 {
                break value;
            }
            if std::time::Instant::now() >= deadline {
                return Err(format!("GPU timestamp timeout for frame {frame}"));
            }
            std::thread::yield_now();
        };
        let read_timestamp = |query: &ID3D11Query, name: &str| -> Result<u64, String> {
            loop {
                let mut value = u64::MAX;
                // SAFETY: the output buffer has the timestamp query's exact
                // data size.
                unsafe {
                    self.immediate
                        .GetData(
                            query,
                            Some((&raw mut value).cast()),
                            size_of::<u64>() as u32,
                            0,
                        )
                        .map_err(|error| format!("GetData({name}) frame {frame}: {error}"))?;
                }
                if value != u64::MAX {
                    return Ok(value);
                }
                if std::time::Instant::now() >= deadline {
                    return Err(format!("GPU {name} timestamp timeout for frame {frame}"));
                }
                std::thread::yield_now();
            }
        };
        let start = read_timestamp(&slot.start, "start")?;
        let end = read_timestamp(&slot.end, "end")?;
        if disjoint.Disjoint.as_bool() || disjoint.Frequency == 0 || end < start {
            return Err(format!("invalid GPU timestamp interval for frame {frame}"));
        }
        let nanos =
            (u128::from(end - start) * 1_000_000_000u128 / u128::from(disjoint.Frequency)) as u64;
        Ok(Some((frame, nanos)))
    }
}

fn create_query(
    device: &ID3D11Device,
    kind: windows::Win32::Graphics::Direct3D11::D3D11_QUERY,
) -> Result<ID3D11Query, String> {
    let desc = D3D11_QUERY_DESC {
        Query: kind,
        MiscFlags: 0,
    };
    let mut query = None;
    // SAFETY: `desc` and the out-pointer are live for the call.
    unsafe { device.CreateQuery(&raw const desc, Some(&raw mut query)) }
        .map_err(|error| format!("CreateQuery({kind:?}): {error}"))?;
    query.ok_or_else(|| format!("CreateQuery({kind:?}) returned null"))
}
