// The producer's whole job is to make consecutive frames genuinely different.
// A capture backend pointed at an unchanging desktop reports frames it never
// had to move, so every region of this image is a function of the frame index.

cbuffer Tick : register(b0)
{
    uint frame_index;
    uint surface_width;
    uint surface_height;
    uint reserved;
};

// Fullscreen triangle generated from the vertex id: no vertex buffer, no input
// layout, nothing between the frame counter and the pixels.
float4 vs_main(uint id : SV_VertexID) : SV_Position
{
    float2 uv = float2((id << 1) & 2, id & 2);
    return float4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
}

// Knuth's multiplicative hash, so neighbouring frame indices are far apart in
// colour. A ramp would make two adjacent frames nearly identical, which is the
// failure this producer exists to avoid.
float3 field_colour(uint f)
{
    uint h = f * 2654435761u;
    float3 rgb = float3(float((h >> 16) & 255u), float((h >> 8) & 255u), float(h & 255u)) / 255.0;
    // Dimmed, so the counter and the sweep read clearly on top of it.
    return rgb * 0.30 + 0.04;
}

float4 ps_main(float4 pos : SV_Position) : SV_Target
{
    uint px = (uint)pos.x;
    uint py = (uint)pos.y;

    uint band = max(surface_height / 10u, 8u);

    // The frame number in binary, most significant bit on the left, as 32
    // blocks. Blocks rather than glyphs because a font would need machinery
    // this tool has no other use for, and because a capture of this band can
    // be thresholded back into the frame number it came from.
    if (py < band)
    {
        uint cell = max(surface_width / 32u, 1u);
        uint index = min(px / cell, 31u);
        uint inset = px - index * cell;
        if (inset < 3u || py < 3u || py + 3u >= band)
        {
            return float4(0.0, 0.0, 0.0, 1.0);
        }
        uint bit = 31u - index;
        float on = ((frame_index >> bit) & 1u) == 1u ? 1.0 : 0.06;
        return float4(on, on, on, 1.0);
    }

    // A one-pixel-period grating shifted by one pixel per frame: the highest
    // spatial frequency the surface can carry, at the smallest displacement a
    // capture can resolve. Anything that resamples or drops frames shows here
    // first.
    uint bars_top = (surface_height * 78u) / 100u;
    uint bars_bottom = (surface_height * 92u) / 100u;
    if (py >= bars_top && py < bars_bottom)
    {
        float v = ((px + frame_index) & 1u) == 1u ? 1.0 : 0.0;
        return float4(v, v, v, 1.0);
    }

    float3 colour = field_colour(frame_index);

    // Full-height bar crossing the frame every 256 frames. The distance is
    // measured on a wrapped axis so the bar re-enters on the left without a
    // second draw or a branch for the seam.
    float sweep = float(frame_index % 256u) / 256.0;
    float x = (float(px) + 0.5) / float(max(surface_width, 1u));
    float offset = abs(frac(x - sweep + 0.5) - 0.5);
    if (offset < 0.06)
    {
        colour = float3(1.0, 0.85, 0.10);
    }

    return float4(colour, 1.0);
}
