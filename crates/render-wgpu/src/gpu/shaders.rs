// GPU pipeline construction and embedded WGSL shaders.

#[cfg(test)]
fn create_batch_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    label: &'static str,
    fragment_entry: &'static str,
) -> wgpu::RenderPipeline {
    create_batch_pipeline_with_blend(
        device,
        layout,
        shader,
        format,
        label,
        fragment_entry,
        Some(wgpu::BlendState::ALPHA_BLENDING),
    )
}

fn create_composited_batch_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    label: &'static str,
    fragment_entry: &'static str,
    alpha_mode: wgpu::CompositeAlphaMode,
) -> wgpu::RenderPipeline {
    create_batch_pipeline_with_blend(
        device,
        layout,
        shader,
        format,
        label,
        fragment_entry,
        Some(blend_state_for_alpha_mode(alpha_mode)),
    )
}

#[derive(Clone, Copy)]
struct GlyphPipelineOptions {
    alpha_mode: wgpu::CompositeAlphaMode,
    text_gamma_adjustment: f32,
}

fn create_glyph_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    label: &'static str,
    fragment_entry: &'static str,
    options: GlyphPipelineOptions,
) -> wgpu::RenderPipeline {
    let constants = HashMap::from([(
        "text_gamma_adjustment".to_owned(),
        f64::from(options.text_gamma_adjustment.clamp(1.0, 2.0)),
    )]);
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: "vs_batch",
            buffers: &[GpuQuadInstance::layout()],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: fragment_entry,
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(blend_state_for_alpha_mode(options.alpha_mode)),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions {
                constants: &constants,
                ..wgpu::PipelineCompilationOptions::default()
            },
        }),
        primitive: quad_primitive_state(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
    })
}

fn create_replacement_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    label: &'static str,
    fragment_entry: &'static str,
) -> wgpu::RenderPipeline {
    create_batch_pipeline_with_blend(device, layout, shader, format, label, fragment_entry, None)
}

fn glyph_sampler_descriptor() -> wgpu::SamplerDescriptor<'static> {
    wgpu::SamplerDescriptor {
        label: Some("panea-glyph-sampler"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        mipmap_filter: wgpu::FilterMode::Nearest,
        ..wgpu::SamplerDescriptor::default()
    }
}

fn logo_sampler_descriptor() -> wgpu::SamplerDescriptor<'static> {
    wgpu::SamplerDescriptor {
        label: Some("panea-logo-sampler"),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::FilterMode::Linear,
        ..wgpu::SamplerDescriptor::default()
    }
}

fn create_batch_pipeline_with_blend(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    label: &'static str,
    fragment_entry: &'static str,
    blend: Option<wgpu::BlendState>,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: "vs_batch",
            buffers: &[GpuQuadInstance::layout()],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: fragment_entry,
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: quad_primitive_state(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
    })
}

fn quad_primitive_state() -> wgpu::PrimitiveState {
    wgpu::PrimitiveState::default()
}

const BATCH_SHADER: &str = r#"
override text_gamma_adjustment: f32 = 1.2;

struct VertexIn {
    @location(0) position_0: vec2<f32>,
    @location(1) position_1: vec2<f32>,
    @location(2) position_2: vec2<f32>,
    @location(3) position_3: vec2<f32>,
    @location(4) uv_bounds: vec4<f32>,
    @location(5) color: vec4<f32>,
};

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_batch(in: VertexIn, @builtin(vertex_index) vertex_index: u32) -> VertexOut {
    var position = in.position_0;
    var uv = in.uv_bounds.xy;
    switch vertex_index {
        case 1u: {
            position = in.position_1;
            uv = in.uv_bounds.zy;
        }
        case 2u: {
            position = in.position_2;
            uv = in.uv_bounds.zw;
        }
        case 3u: {}
        case 4u: {
            position = in.position_2;
            uv = in.uv_bounds.zw;
        }
        case 5u: {
            position = in.position_3;
            uv = in.uv_bounds.xw;
        }
        default: {}
    }
    var out: VertexOut;
    out.position = vec4<f32>(position, 0.0, 1.0);
    out.uv = uv;
    out.color = in.color;
    return out;
}

fn srgb_to_linear(color: vec3<f32>) -> vec3<f32> {
    let low = color / 12.92;
    let high = pow((color + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(high, low, color <= vec3<f32>(0.04045));
}

fn linear_to_srgb(color: vec3<f32>) -> vec3<f32> {
    let low = color * 12.92;
    let high = 1.055 * pow(color, vec3<f32>(1.0 / 2.4)) - vec3<f32>(0.055);
    return select(high, low, color <= vec3<f32>(0.0031308));
}

struct QuadSample {
    color: vec4<f32>,
    coverage: f32,
};

fn rounded_box_distance(point: vec2<f32>, half_size: vec2<f32>, radius: f32) -> f32 {
    let corner = abs(point) - max(half_size - vec2<f32>(radius), vec2<f32>(0.0));
    return length(max(corner, vec2<f32>(0.0))) + min(max(corner.x, corner.y), 0.0) - radius;
}

fn distance_coverage(distance: f32) -> f32 {
    let antialias = max(fwidth(distance), 0.75);
    return clamp(0.5 - distance / antialias, 0.0, 1.0);
}

fn quad_sample(in: VertexOut) -> QuadSample {
    if in.color.a >= 0.0 {
        return QuadSample(in.color, 1.0);
    }
    let size = floor(in.color.rg * 0.5);
    let radius = floor(in.color.b * 0.5);
    let payload = -in.color.a - 1.0;
    let line = floor(payload * 0.5);
    let alpha = payload - line * 2.0;
    let color = vec4<f32>(
        in.color.r - size.x * 2.0,
        in.color.g - size.y * 2.0,
        in.color.b - radius * 2.0,
        alpha,
    );
    let half_size = size * 0.5;
    let point = in.uv - half_size;
    let outer = distance_coverage(rounded_box_distance(point, half_size, radius));
    var coverage = outer;
    if line > 0.0 {
        let inner_half = max(half_size - vec2<f32>(line), vec2<f32>(0.0));
        let inner_radius = max(radius - line, 0.0);
        let inner = distance_coverage(rounded_box_distance(point, inner_half, inner_radius));
        coverage = outer * (1.0 - inner);
    }
    return QuadSample(color, coverage);
}

@fragment
fn fs_color_srgb_target(in: VertexOut) -> @location(0) vec4<f32> {
    let sample = quad_sample(in);
    return vec4<f32>(srgb_to_linear(sample.color.rgb), sample.color.a * sample.coverage);
}

@fragment
fn fs_color_unorm_target(in: VertexOut) -> @location(0) vec4<f32> {
    let sample = quad_sample(in);
    return vec4<f32>(sample.color.rgb, sample.color.a * sample.coverage);
}

@fragment
fn fs_color_srgb_target_premultiplied(in: VertexOut) -> @location(0) vec4<f32> {
    let sample = quad_sample(in);
    let alpha = sample.color.a * sample.coverage;
    return vec4<f32>(srgb_to_linear(sample.color.rgb) * alpha, alpha);
}

@fragment
fn fs_color_unorm_target_premultiplied(in: VertexOut) -> @location(0) vec4<f32> {
    let sample = quad_sample(in);
    let alpha = sample.color.a * sample.coverage;
    return vec4<f32>(sample.color.rgb * alpha, alpha);
}

@group(0) @binding(0) var glyph_mask_atlas: texture_2d<f32>;
@group(0) @binding(1) var glyph_color_atlas: texture_2d<f32>;
@group(0) @binding(2) var glyph_sampler: sampler;

fn adjusted_glyph_coverage(coverage: f32) -> f32 {
    return pow(clamp(coverage, 0.0, 1.0), 1.0 / text_gamma_adjustment);
}

@fragment
fn fs_glyph_srgb_target(in: VertexOut) -> @location(0) vec4<f32> {
    let mask = textureSample(glyph_mask_atlas, glyph_sampler, in.uv).r;
    let sample = textureSample(glyph_color_atlas, glyph_sampler, in.uv);
    if in.color.a < 0.0 {
        return vec4<f32>(sample.rgb, sample.a * -in.color.a);
    }
    return vec4<f32>(srgb_to_linear(in.color.rgb), in.color.a * adjusted_glyph_coverage(mask));
}

@fragment
fn fs_glyph_unorm_target(in: VertexOut) -> @location(0) vec4<f32> {
    let mask = textureSample(glyph_mask_atlas, glyph_sampler, in.uv).r;
    let sample = textureSample(glyph_color_atlas, glyph_sampler, in.uv);
    if in.color.a < 0.0 {
        return vec4<f32>(linear_to_srgb(sample.rgb), sample.a * -in.color.a);
    }
    return vec4<f32>(in.color.rgb, in.color.a * adjusted_glyph_coverage(mask));
}

@fragment
fn fs_glyph_srgb_target_premultiplied(in: VertexOut) -> @location(0) vec4<f32> {
    let mask = textureSample(glyph_mask_atlas, glyph_sampler, in.uv).r;
    let sample = textureSample(glyph_color_atlas, glyph_sampler, in.uv);
    if in.color.a < 0.0 {
        let alpha = sample.a * -in.color.a;
        return vec4<f32>(sample.rgb * alpha, alpha);
    }
    let alpha = in.color.a * adjusted_glyph_coverage(mask);
    return vec4<f32>(srgb_to_linear(in.color.rgb) * alpha, alpha);
}

@fragment
fn fs_glyph_unorm_target_premultiplied(in: VertexOut) -> @location(0) vec4<f32> {
    let mask = textureSample(glyph_mask_atlas, glyph_sampler, in.uv).r;
    let sample = textureSample(glyph_color_atlas, glyph_sampler, in.uv);
    if in.color.a < 0.0 {
        let alpha = sample.a * -in.color.a;
        return vec4<f32>(linear_to_srgb(sample.rgb) * alpha, alpha);
    }
    let alpha = in.color.a * adjusted_glyph_coverage(mask);
    return vec4<f32>(in.color.rgb * alpha, alpha);
}

"#;

const CURSOR_IMAGE_SHADER: &str = r#"
struct VertexIn {
    @location(0) position_0: vec2<f32>,
    @location(1) position_1: vec2<f32>,
    @location(2) position_2: vec2<f32>,
    @location(3) position_3: vec2<f32>,
    @location(4) uv_bounds: vec4<f32>,
    @location(5) color: vec4<f32>,
};

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_batch(in: VertexIn, @builtin(vertex_index) vertex_index: u32) -> VertexOut {
    var position = in.position_0;
    var uv = in.uv_bounds.xy;
    switch vertex_index {
        case 1u: {
            position = in.position_1;
            uv = in.uv_bounds.zy;
        }
        case 2u: {
            position = in.position_2;
            uv = in.uv_bounds.zw;
        }
        case 3u: {}
        case 4u: {
            position = in.position_2;
            uv = in.uv_bounds.zw;
        }
        case 5u: {
            position = in.position_3;
            uv = in.uv_bounds.xw;
        }
        default: {}
    }
    var out: VertexOut;
    out.position = vec4<f32>(position, 0.0, 1.0);
    out.uv = uv;
    out.color = in.color;
    return out;
}

fn linear_to_srgb(color: vec3<f32>) -> vec3<f32> {
    let low = color * 12.92;
    let high = 1.055 * pow(color, vec3<f32>(1.0 / 2.4)) - vec3<f32>(0.055);
    return select(high, low, color <= vec3<f32>(0.0031308));
}

@group(0) @binding(2) var cursor_images: texture_2d_array<f32>;
@group(0) @binding(3) var cursor_image_sampler: sampler;

@fragment
fn fs_cursor_image_srgb_target(in: VertexOut) -> @location(0) vec4<f32> {
    let frame = i32(round(in.color.r));
    let sample = textureSample(cursor_images, cursor_image_sampler, in.uv, frame);
    return vec4<f32>(sample.rgb, sample.a * in.color.g);
}

@fragment
fn fs_cursor_image_unorm_target(in: VertexOut) -> @location(0) vec4<f32> {
    let frame = i32(round(in.color.r));
    let sample = textureSample(cursor_images, cursor_image_sampler, in.uv, frame);
    return vec4<f32>(linear_to_srgb(sample.rgb), sample.a * in.color.g);
}

@fragment
fn fs_cursor_image_srgb_target_premultiplied(in: VertexOut) -> @location(0) vec4<f32> {
    let frame = i32(round(in.color.r));
    let sample = textureSample(cursor_images, cursor_image_sampler, in.uv, frame);
    let alpha = sample.a * in.color.g;
    return vec4<f32>(sample.rgb * alpha, alpha);
}

@fragment
fn fs_cursor_image_unorm_target_premultiplied(in: VertexOut) -> @location(0) vec4<f32> {
    let frame = i32(round(in.color.r));
    let sample = textureSample(cursor_images, cursor_image_sampler, in.uv, frame);
    let alpha = sample.a * in.color.g;
    return vec4<f32>(linear_to_srgb(sample.rgb) * alpha, alpha);
}
"#;
