@group(0) @binding(0) var<uniform> screen_size: vec4<f32>;

@group(1) @binding(0) var image_tex: texture_2d<f32>;
@group(1) @binding(1) var image_sampler: sampler;

struct VsInput {
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
}

struct VsOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(in: VsInput) -> VsOutput {
    var out: VsOutput;
    let ndc = (2.0 * in.pos / screen_size.xy - 1.0) * vec2<f32>(1.0, -1.0);
    out.position = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = in.uv;
    return out;
}

@fragment
fn fs_main(in: VsOutput) -> @location(0) vec4<f32> {
    let color = textureSample(image_tex, image_sampler, in.uv);
    // Pre-multiply RGB by alpha for compositor transparency.
    return vec4<f32>(color.rgb * color.a, color.a);
}
