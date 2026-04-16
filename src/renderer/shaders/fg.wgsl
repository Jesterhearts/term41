@group(0) @binding(0) var<uniform> screen_size: vec4<f32>;

@group(1) @binding(0) var atlas_tex: texture_2d<f32>;
@group(1) @binding(1) var atlas_sampler: sampler;
@group(1) @binding(2) var<uniform> atlas_size: vec2<f32>;

struct VsInput {
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: u32,
    @location(3) flags: u32,
}

struct VsOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) color: u32,
    @location(2) @interpolate(flat) flags: u32,
}

fn unpack_color(c: u32) -> vec4<f32> {
    return vec4<f32>(
        f32(c >> 24u) / 255.0,
        f32((c >> 16u) & 0xFFu) / 255.0,
        f32((c >> 8u) & 0xFFu) / 255.0,
        f32(c & 0xFFu) / 255.0,
    );
}

@vertex
fn vs_main(in: VsInput) -> VsOutput {
    var out: VsOutput;
    let ndc = (2.0 * in.pos / screen_size.xy - 1.0) * vec2<f32>(1.0, -1.0);
    out.position = vec4<f32>(ndc, 0.0, 1.0);
    // Normalize UV to atlas texture coordinates.
    out.uv = in.uv / atlas_size;
    out.color = in.color;
    out.flags = in.flags;
    return out;
}

@fragment
fn fs_main(in: VsOutput) -> @location(0) vec4<f32> {
    let sampled = textureSample(atlas_tex, atlas_sampler, in.uv);
    let fg = unpack_color(in.color);
    let is_color = (in.flags & 1u) != 0u;

    // Color path: sample atlas RGBA directly, keep fg.a as a tint alpha.
    // Alpha path: atlas alpha is a coverage mask; tint fg.rgb by it.
    let color_rgb = sampled.rgb * fg.a;
    let color_a = sampled.a * fg.a;
    let alpha_a = sampled.a * fg.a;
    let alpha_rgb = fg.rgb * alpha_a;

    let rgb = select(alpha_rgb, color_rgb, is_color);
    let a = select(alpha_a, color_a, is_color);
    return vec4<f32>(rgb, a);
}
