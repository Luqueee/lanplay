/// The whole render path, compiled from source at startup.
///
/// Source compilation costs a few milliseconds once and buys the crate freedom
/// from a build script, an `xcrun metal` toolchain dependency and a `.metallib`
/// that has to be found at runtime relative to a binary that may live anywhere.
///
/// The vertex stage emits one oversized triangle rather than a quad: two
/// triangles share an edge, and fragments on that edge get rasterised twice.
pub(crate) const NV12_TO_RGB: &str = r#"
#include <metal_stdlib>
using namespace metal;

struct Varyings {
    float4 position [[position]];
    float2 uv;
};

vertex Varyings nv12_vertex(uint vertex_id [[vertex_id]]) {
    // (0,0), (2,0), (0,2) in texture space; the triangle covers the viewport
    // and everything outside [0,1] is clipped away.
    float2 uv = float2((vertex_id << 1) & 2, vertex_id & 2);
    Varyings out;
    out.position = float4(uv * float2(2.0, -2.0) + float2(-1.0, 1.0), 0.0, 1.0);
    out.uv = uv;
    return out;
}

fragment float4 nv12_fragment(Varyings in [[stage_in]],
                              texture2d<float> luma [[texture(0)]],
                              texture2d<float> chroma [[texture(1)]]) {
    constexpr sampler bilinear(filter::linear, mip_filter::none, address::clamp_to_edge);

    float y = luma.sample(bilinear, in.uv).r;
    float2 cbcr = chroma.sample(bilinear, in.uv).rg;

    // BT.709 video range: luma occupies 16..235 and chroma 16..240 of 255, so
    // the offsets come off before the matrix and the gain is 255/219.
    float3 ycbcr = float3(y - 16.0 / 255.0, cbcr - float2(128.0 / 255.0));
    float3 rgb = float3(
        dot(ycbcr, float3(1.16438356,  0.00000000,  1.79274107)),
        dot(ycbcr, float3(1.16438356, -0.21324861, -0.53290933)),
        dot(ycbcr, float3(1.16438356,  2.11240179,  0.00000000)));

    return float4(saturate(rgb), 1.0);
}
"#;
