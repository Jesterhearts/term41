@group(0) @binding(0) var<uniform> screen_size: vec4<f32>;
@group(1) @binding(0) var bg_tex: texture_2d<f32>;
@group(1) @binding(1) var bg_sampler: sampler;

struct VsInput {
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
    // Per-vertex (uniform per draw — same value on all 4 vertices) RGB
    // multiplier from the `background_opacity` config key. Carried as a
    // vertex attribute so we don't need a second uniform buffer for one
    // float; the redundant copies are cheap and the shader stays simple.
    @location(2) dim: f32,
}

struct VsOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) dim: f32,
}

@vertex
fn vs_main(in: VsInput) -> VsOutput {
    var out: VsOutput;
    let ndc = (2.0 * in.pos / screen_size.xy - 1.0) * vec2<f32>(1.0, -1.0);
    out.position = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = in.uv;
    out.dim = in.dim;
    return out;
}

@fragment
fn fs_main(in: VsOutput) -> @location(0) vec4<f32> {
    let color = textureSample(bg_tex, bg_sampler, in.uv);
    // Dim the RGB by `dim` (the configured background_opacity), preserve
    // the image's own alpha. Pre-multiply for compositor-friendly blending.
    let rgb = color.rgb * in.dim;
    return vec4<f32>(rgb * color.a, color.a);
}
