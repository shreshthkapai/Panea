use super::*;

const RED: RenderColor = RenderColor::rgb(255, 0, 0);
const GREEN: RenderColor = RenderColor::rgb(0, 255, 0);
const BLUE: RenderColor = RenderColor::rgb(0, 0, 255);
const WHITE: RenderColor = RenderColor::rgb(255, 255, 255);
const YELLOW: RenderColor = RenderColor::rgb(255, 255, 0);

#[test]
fn gpu_backend_preferences_map_to_portable_wgpu_backend_sets() {
    assert_eq!(
        instance_backends(GpuBackendPreference::Auto),
        wgpu::Backends::all()
    );
    assert_eq!(
        instance_backends(GpuBackendPreference::Vulkan),
        wgpu::Backends::VULKAN
    );
    assert_eq!(
        instance_backends(GpuBackendPreference::Metal),
        wgpu::Backends::METAL
    );
    assert_eq!(
        instance_backends(GpuBackendPreference::Dx12),
        wgpu::Backends::DX12
    );
    assert_eq!(
        instance_backends(GpuBackendPreference::Gl),
        wgpu::Backends::GL
    );
}

#[test]
fn auto_backend_candidates_are_platform_and_transparency_aware() {
    assert_eq!(
        backend_candidates_for(
            NativeBackendFamily::Windows,
            GpuBackendPreference::Auto,
            false,
        ),
        vec![
            GpuBackendPreference::Dx12,
            GpuBackendPreference::Vulkan,
            GpuBackendPreference::Gl,
        ]
    );
    assert_eq!(
        backend_candidates_for(
            NativeBackendFamily::Windows,
            GpuBackendPreference::Auto,
            true,
        ),
        vec![
            GpuBackendPreference::Vulkan,
            GpuBackendPreference::Dx12,
            GpuBackendPreference::Gl,
        ]
    );
    assert_eq!(
        backend_candidates_for(NativeBackendFamily::Apple, GpuBackendPreference::Auto, true,),
        vec![
            GpuBackendPreference::Metal,
            GpuBackendPreference::Vulkan,
            GpuBackendPreference::Gl,
        ]
    );
    assert_eq!(
        backend_candidates_for(NativeBackendFamily::Unix, GpuBackendPreference::Auto, false,),
        vec![GpuBackendPreference::Vulkan, GpuBackendPreference::Gl]
    );
}

#[test]
fn explicit_backend_selection_never_silently_falls_back() {
    assert_eq!(
        backend_candidates_for(
            NativeBackendFamily::Windows,
            GpuBackendPreference::Metal,
            false,
        ),
        vec![GpuBackendPreference::Metal]
    );
}

#[test]
fn renderer_startup_timings_report_all_initialization_phases() {
    let timings = RendererStartupTimings {
        instance_and_surface: Duration::from_millis(1),
        adapter_request: Duration::from_millis(2),
        device_request: Duration::from_millis(3),
        surface_configuration: Duration::from_millis(4),
        pipeline_creation: Duration::from_millis(5),
        total: Duration::from_millis(15),
    };

    assert_eq!(timings.accounted(), timings.total);
}

fn damage_covers(regions: &[DamageRegion], expected: RenderRect) -> bool {
    regions.iter().any(|region| {
        region.x <= expected.x
            && region.y <= expected.y
            && i64::from(region.x) + i64::from(region.width)
                >= i64::from(expected.x) + i64::from(expected.width)
            && i64::from(region.y) + i64::from(region.height)
                >= i64::from(expected.y) + i64::from(expected.height)
    })
}

struct TestFrame {
    clears: Vec<(RenderRect, RenderColor)>,
    quads: Vec<(RenderRect, RenderColor)>,
}

impl TestFrame {
    fn quadrants(colors: [RenderColor; 4]) -> Self {
        Self {
            clears: Vec::new(),
            quads: vec![
                (
                    RenderRect {
                        x: 0,
                        y: 0,
                        width: 8,
                        height: 8,
                    },
                    colors[0],
                ),
                (
                    RenderRect {
                        x: 8,
                        y: 0,
                        width: 8,
                        height: 8,
                    },
                    colors[1],
                ),
                (
                    RenderRect {
                        x: 0,
                        y: 8,
                        width: 8,
                        height: 8,
                    },
                    colors[2],
                ),
                (
                    RenderRect {
                        x: 8,
                        y: 8,
                        width: 8,
                        height: 8,
                    },
                    colors[3],
                ),
            ],
        }
    }

    fn damage(bounds: RenderRect, color: RenderColor) -> Self {
        Self {
            clears: vec![(bounds, color)],
            quads: Vec::new(),
        }
    }
}

struct TestPixels {
    width: u32,
    pixels: Vec<u8>,
}

impl TestPixels {
    fn at(&self, x: u32, y: u32) -> [u8; 4] {
        let index = usize::try_from((y * self.width + x) * 4).expect("pixel index");
        self.pixels[index..index + 4]
            .try_into()
            .expect("RGBA pixel")
    }
}

fn render_retained_sequence(
    first: TestFrame,
    second: TestFrame,
    clear_color: wgpu::Color,
) -> Result<Option<TestPixels>, String> {
    const WIDTH: u32 = 16;
    const HEIGHT: u32 = 16;

    let instance = wgpu::Instance::default();
    let Some(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
    else {
        eprintln!("retained-frame test skipped: no WGPU adapter is available");
        return Ok(None);
    };
    let adapter_info = adapter.get_info();
    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("panea-retained-frame-test-device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
        },
        None,
    ))
    .map_err(|error| {
        format!(
            "failed to create retained-frame test device for {} ({:?}): {error}",
            adapter_info.name, adapter_info.backend
        )
    })?;
    eprintln!(
        "retained-frame test adapter={} backend={:?}",
        adapter_info.name, adapter_info.backend
    );

    let format = wgpu::TextureFormat::Rgba8Unorm;
    let retained = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("panea-retained-frame-test-target"),
        size: wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let retained_view = retained.create_view(&wgpu::TextureViewDescriptor::default());
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("panea-retained-frame-test-shader"),
        source: wgpu::ShaderSource::Wgsl(BATCH_SHADER.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("panea-retained-frame-test-layout"),
        bind_group_layouts: &[],
        push_constant_ranges: &[],
    });
    let pipeline = create_batch_pipeline(
        &device,
        &layout,
        &shader,
        format,
        "panea-retained-frame-test-pipeline",
        "fs_color_unorm_target",
    );
    let clear_pipeline = create_replacement_pipeline(
        &device,
        &layout,
        &shader,
        format,
        "panea-retained-frame-test-clear-pipeline",
        "fs_color_unorm_target",
    );
    let upload_context = GpuUploadContext {
        device: &device,
        queue: &queue,
        width: WIDTH,
        height: HEIGHT,
    };
    let mut gpu_batches = PersistentBatchBuffers::default();

    let encode_test_frame = |frame: TestFrame,
                             load_previous: bool,
                             gpu_batches: &mut PersistentBatchBuffers|
     -> wgpu::CommandBuffer {
        let damage_regions = if frame.clears.is_empty() {
            vec![RenderRect {
                x: 0,
                y: 0,
                width: WIDTH,
                height: HEIGHT,
            }]
        } else {
            frame.clears.iter().map(|(bounds, _)| *bounds).collect()
        };
        let mut batch = QuadBatch::new(QuadBatchKind::Background);
        for (bounds, color) in frame.quads {
            push_solid_quad(&mut batch, bounds, color);
        }
        let mut clears = QuadBatch::new(QuadBatchKind::Background);
        for (bounds, color) in frame.clears {
            push_solid_quad(&mut clears, bounds, color);
        }
        gpu_batches.damage_clear.upload(
            &upload_context,
            "retained-frame-test-clear",
            &clears.vertices,
            &clears.indices,
        );
        gpu_batches.background.upload(
            &upload_context,
            "retained-frame-test-background",
            &batch.vertices,
            &batch.indices,
        );
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("panea-retained-frame-test-encoder"),
        });
        encode_retained_frame(
            &mut encoder,
            &retained_view,
            load_previous,
            clear_color,
            GpuFrameDraw {
                clear_pipeline: &clear_pipeline,
                quad_pipeline: &pipeline,
                glyph_pipeline: &pipeline,
                glyph_bind_group: None,
                logo_bind_group: None,
                batches: gpu_batches,
                damage_regions: &damage_regions,
                target_width: WIDTH,
                target_height: HEIGHT,
            },
            None,
        );
        encoder.finish()
    };

    queue.submit(Some(encode_test_frame(first, false, &mut gpu_batches)));
    queue.submit(Some(encode_test_frame(second, true, &mut gpu_batches)));

    let unpadded_row_bytes = WIDTH * 4;
    let padded_row_bytes = unpadded_row_bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("panea-retained-frame-test-readback"),
        size: u64::from(padded_row_bytes * HEIGHT),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("panea-retained-frame-test-readback-encoder"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture: &retained,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &readback,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(padded_row_bytes),
                rows_per_image: Some(HEIGHT),
            },
        },
        wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));

    let (sender, receiver) = mpsc::channel();
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result.map_err(|error| error.to_string()));
        });
    device.poll(wgpu::Maintain::Wait);
    receiver.recv().map_err(|error| error.to_string())??;

    let mapped = readback.slice(..).get_mapped_range();
    let mut pixels = Vec::with_capacity(usize::try_from(WIDTH * HEIGHT * 4).unwrap_or(0));
    for row in mapped.chunks_exact(usize::try_from(padded_row_bytes).unwrap_or(0)) {
        pixels.extend_from_slice(&row[..usize::try_from(unpadded_row_bytes).unwrap_or(0)]);
    }
    drop(mapped);
    readback.unmap();

    Ok(Some(TestPixels {
        width: WIDTH,
        pixels,
    }))
}

#[test]
fn retained_frame_preserves_unchanged_pixels_and_replaces_damage() {
    let first = TestFrame::quadrants([RED, GREEN, BLUE, WHITE]);
    let second = TestFrame::damage(
        RenderRect {
            x: 0,
            y: 0,
            width: 8,
            height: 8,
        },
        YELLOW,
    );
    let Some(pixels) =
        render_retained_sequence(first, second, wgpu::Color::TRANSPARENT).expect("GPU sequence")
    else {
        return;
    };

    assert_eq!(pixels.at(2, 2), [255, 255, 0, 255]);
    assert_eq!(pixels.at(12, 2), [0, 255, 0, 255]);
    assert_eq!(pixels.at(2, 12), [0, 0, 255, 255]);
    assert_eq!(pixels.at(12, 12), [255, 255, 255, 255]);
}

#[test]
fn retained_frame_scissors_full_bounds_draws_to_partial_damage() {
    let first = TestFrame::quadrants([RED, GREEN, BLUE, WHITE]);
    let damage = RenderRect {
        x: 0,
        y: 0,
        width: 8,
        height: 8,
    };
    let second = TestFrame {
        clears: vec![(damage, YELLOW)],
        quads: vec![(
            RenderRect {
                x: 0,
                y: 0,
                width: 16,
                height: 16,
            },
            RED,
        )],
    };
    let Some(pixels) =
        render_retained_sequence(first, second, wgpu::Color::TRANSPARENT).expect("GPU sequence")
    else {
        return;
    };

    assert_eq!(pixels.at(2, 2), [255, 0, 0, 255]);
    assert_eq!(pixels.at(12, 2), [0, 255, 0, 255]);
    assert_eq!(pixels.at(2, 12), [0, 0, 255, 255]);
    assert_eq!(pixels.at(12, 12), [255, 255, 255, 255]);
}

#[test]
fn translucent_background_replaces_clear_without_alpha_compounding() {
    let background = RenderColor {
        red: 30,
        green: 30,
        blue: 46,
        alpha: 235,
    };
    let first = TestFrame {
        clears: Vec::new(),
        quads: vec![(
            RenderRect {
                x: 0,
                y: 0,
                width: 8,
                height: 8,
            },
            background,
        )],
    };
    let second = TestFrame {
        clears: Vec::new(),
        quads: Vec::new(),
    };
    let Some(pixels) = render_retained_sequence(
        first,
        second,
        surface_clear_color(background, wgpu::TextureFormat::Rgba8Unorm),
    )
    .expect("GPU sequence") else {
        return;
    };

    assert_eq!(pixels.at(2, 2), [30, 30, 46, 235]);
    assert_eq!(pixels.at(12, 12), [30, 30, 46, 235]);
}

#[test]
fn retained_damage_status_is_explicit() {
    assert_eq!(
        retained_damage_status(false, true),
        RetainedDamageStatus::DisabledByConfig
    );
    assert!(matches!(
        retained_damage_status(true, false),
        RetainedDamageStatus::Unsupported { .. }
    ));
    assert_eq!(
        retained_damage_status(true, true),
        RetainedDamageStatus::Enabled
    );
    assert!(
        retained_damage_status(true, false)
            .to_string()
            .contains("cannot receive")
    );
}

#[test]
fn automatic_present_mode_defaults_to_fifo() {
    assert_eq!(
        select_present_mode(
            PresentMode::Auto,
            &[wgpu::PresentMode::Fifo, wgpu::PresentMode::Mailbox],
        ),
        wgpu::PresentMode::Fifo
    );
    assert_eq!(
        select_present_mode(PresentMode::Auto, &[wgpu::PresentMode::Fifo]),
        wgpu::PresentMode::Fifo
    );
}

#[test]
fn explicit_present_modes_use_cross_backend_fallbacks() {
    let fifo_and_mailbox = [wgpu::PresentMode::Fifo, wgpu::PresentMode::Mailbox];
    assert_eq!(
        select_present_mode(PresentMode::Fifo, &fifo_and_mailbox),
        wgpu::PresentMode::Fifo
    );
    assert_eq!(
        select_present_mode(PresentMode::Mailbox, &fifo_and_mailbox),
        wgpu::PresentMode::Mailbox
    );
    assert_eq!(
        select_present_mode(PresentMode::Immediate, &fifo_and_mailbox),
        wgpu::PresentMode::Mailbox
    );
}

#[test]
fn renderer_keeps_only_one_frame_queued_for_input_latency() {
    assert_eq!(DESIRED_MAXIMUM_FRAME_LATENCY, 1);
}

#[test]
fn retained_frame_invalidation_forces_fresh_full_frame() {
    let mut retained = RetainedFrameState {
        texture: None,
        view: None,
        size: Some((80, 24)),
        initialized: true,
    };

    retained.invalidate();

    assert_eq!(retained.size, None);
    assert!(!retained.initialized);
    assert!(!should_load_retained_frame(true, true));
}

#[test]
fn retained_damage_clear_covers_removed_content_regions() {
    let regions = vec![
        RenderRect {
            x: 8,
            y: 16,
            width: 8,
            height: 16,
        },
        RenderRect {
            x: 32,
            y: 48,
            width: 24,
            height: 16,
        },
    ];

    let batch = prepare_damage_clear_batch(&regions, RenderColor::rgb(12, 12, 12));

    assert_eq!(batch.quad_count(), 2);
    assert_eq!(batch.vertices[0].position_px, [8.0, 16.0]);
    assert_eq!(batch.vertices[4].position_px, [32.0, 48.0]);
}

#[test]
fn full_frame_clear_covers_the_entire_surface() {
    let background = RenderColor {
        red: 30,
        green: 30,
        blue: 46,
        alpha: 235,
    };

    let batch = prepare_frame_clear_batch(false, &[], 1920, 1080, background);

    assert_eq!(batch.quad_count(), 1);
    assert_eq!(batch.vertices[0].position_px, [0.0, 0.0]);
    assert_eq!(batch.vertices[2].position_px, [1920.0, 1080.0]);
    assert_eq!(batch.vertices[0].color[3], 235.0 / 255.0);
}

#[test]
fn glyph_atlas_uvs_cover_only_the_padded_glyph_interior() {
    let (x0, y0, x1, y1) = atlas_uv_bounds(
        AtlasEntry {
            x: 10,
            y: 20,
            width: 4,
            height: 6,
        },
        (100, 200),
    );

    assert_eq!(x0, 10.0 / 100.0);
    assert_eq!(y0, 20.0 / 200.0);
    assert_eq!(x1, 14.0 / 100.0);
    assert_eq!(y1, 26.0 / 200.0);
}

#[test]
fn monochrome_glyph_masks_use_texel_exact_sampling() {
    let sampler = glyph_sampler_descriptor();

    assert_eq!(sampler.mag_filter, wgpu::FilterMode::Nearest);
    assert_eq!(sampler.min_filter, wgpu::FilterMode::Nearest);
    assert_eq!(sampler.mipmap_filter, wgpu::FilterMode::Nearest);
}

#[test]
fn scaled_chrome_logo_uses_linear_sampling() {
    let sampler = logo_sampler_descriptor();

    assert_eq!(sampler.mag_filter, wgpu::FilterMode::Linear);
    assert_eq!(sampler.min_filter, wgpu::FilterMode::Linear);
}

#[test]
fn glyph_atlas_allocates_a_duplicate_edge_padding_border() {
    let mut atlas = GlyphAtlas::new(16, 16);
    let key_a = GlyphCacheKey::new(1, 1, 13.0, false, false);
    let key_b = GlyphCacheKey::new(1, 2, 13.0, false, false);
    let bitmap = GlyphBitmap::missing(4.0, 4);

    let first = atlas.allocate(key_a, &bitmap).expect("first atlas entry");
    let second = atlas.allocate(key_b, &bitmap).expect("second atlas entry");

    assert_eq!((first.x, first.y), (1, 1));
    assert_eq!((first.width, first.height), (4, 4));
    assert_eq!((second.x, second.y), (7, 1));
    assert!(second.x >= first.x + first.width + GLYPH_ATLAS_PADDING * 2);
}

#[test]
fn frame_clear_draw_call_is_instrumented_only_when_used() {
    assert_eq!(frame_clear_extra_draw_calls(false, 0), 1);
    assert_eq!(frame_clear_extra_draw_calls(true, 0), 0);
    assert_eq!(frame_clear_extra_draw_calls(true, 2), 1);
}

fn metrics() -> CellMetrics {
    CellMetrics {
        font_size: 13.0,
        cell_width: 8.0,
        cell_height: 16.0,
        ascent: 11.0,
        descent: -3.0,
        line_gap: 1.0,
        baseline: 12.0,
        underline_position: 14.0,
        strikethrough_position: 7.0,
        decoration_thickness: 1.0,
    }
}

#[test]
fn text_decorations_follow_font_metrics() {
    let mut batch = QuadBatch::new(QuadBatchKind::Decoration);
    let mut styled = cell(0, 0, "x");
    styled.style.underline = true;
    styled.style.strikethrough = true;
    let metrics = CellMetrics {
        underline_position: 12.0,
        strikethrough_position: 5.0,
        decoration_thickness: 2.0,
        ..metrics()
    };

    push_text_decorations(
        &mut batch,
        &styled,
        metrics,
        RenderRect {
            x: 0,
            y: 20,
            width: 8,
            height: 16,
        },
        None,
    );

    let mut y_values = batch
        .vertices
        .chunks_exact(4)
        .map(|quad| quad[0].position_px[1] as i32)
        .collect::<Vec<_>>();
    y_values.sort_unstable();
    assert_eq!(y_values, [25, 32]);
    assert!(
        batch
            .vertices
            .chunks_exact(4)
            .all(|quad| (quad[2].position_px[1] - quad[0].position_px[1]) == 2.0)
    );
}

#[test]
fn clipped_cell_keeps_decoration_at_the_unclipped_metric_offset() {
    let mut batch = QuadBatch::new(QuadBatchKind::Decoration);
    let mut styled = cell(0, 0, "x");
    styled.style.underline = true;
    let metrics = CellMetrics {
        underline_position: 12.0,
        decoration_thickness: 2.0,
        ..metrics()
    };
    push_text_decorations(
        &mut batch,
        &styled,
        metrics,
        RenderRect {
            x: 0,
            y: 20,
            width: 8,
            height: 16,
        },
        Some(RenderRect {
            x: 0,
            y: 24,
            width: 8,
            height: 12,
        }),
    );

    assert_eq!(batch.quad_count(), 1);
    assert_eq!(batch.vertices[0].position_px[1], 32.0);
}

#[test]
fn cpu_frame_size_math_rejects_u32_overflow() {
    assert_eq!(rgba_buffer_len(4, 3), Some(48));
    assert_eq!(rgb_buffer_len(4, 3), Some(36));
    assert_eq!(rgba_buffer_len(u32::MAX, u32::MAX), None);
    assert_eq!(rgb_buffer_len(u32::MAX, u32::MAX), None);
}

#[test]
fn persistent_buffer_growth_is_geometric_and_never_shrinks() {
    assert_eq!(buffer_capacity(1), 256);
    assert_eq!(buffer_capacity(300), 512);
    assert_eq!(buffer_capacity(4096), 4096);
}

#[test]
fn gpu_quads_use_one_compact_instance_instead_of_four_heavy_vertices() {
    assert_eq!(std::mem::size_of::<GpuQuadInstance>(), 64);
    let color = RenderColor::rgb(10, 20, 30);
    let mut batch = QuadBatch::new(QuadBatchKind::Background);
    push_solid_quad(
        &mut batch,
        RenderRect {
            x: 2,
            y: 3,
            width: 4,
            height: 5,
        },
        color,
    );
    let instance = quad_instance_from_vertices(&batch.vertices, 10, 10);
    assert!((instance.positions[0][0] + 0.6).abs() < 0.0001);
    assert!((instance.positions[0][1] - 0.4).abs() < 0.0001);
    assert!((instance.positions[2][0] - 0.2).abs() < 0.0001);
    assert!((instance.positions[2][1] + 0.6).abs() < 0.0001);
    assert_eq!(instance.uv_bounds, [0.0; 4]);
    assert_eq!(instance.color, color_to_f32(color));
}

#[test]
fn cursor_image_shader_and_array_bindings_validate_on_available_adapter() {
    let instance = wgpu::Instance::default();
    let Some(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
    else {
        return;
    };
    let Ok((device, _queue)) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("panea-cursor-image-test-device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
        },
        None,
    )) else {
        return;
    };
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("panea-cursor-image-test-shader"),
        source: wgpu::ShaderSource::Wgsl(CURSOR_IMAGE_SHADER.into()),
    });
    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("panea-cursor-image-test-layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2Array,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("panea-cursor-image-test-pipeline-layout"),
        bind_group_layouts: &[&bind_group_layout],
        push_constant_ranges: &[],
    });
    let _pipeline = create_batch_pipeline(
        &device,
        &layout,
        &shader,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        "panea-cursor-image-test-pipeline",
        "fs_cursor_image_srgb_target",
    );
    device.poll(wgpu::Maintain::Wait);
    let error = pollster::block_on(device.pop_error_scope());
    assert!(
        error.is_none(),
        "cursor image pipeline validation failed: {error:?}"
    );
}

fn cell(row: i64, col: u16, text: &str) -> RenderCell {
    RenderCell {
        position: CellPosition { row, col },
        text: text.into(),
        foreground: RenderColor::rgb(230, 230, 230),
        background: RenderColor::rgb(12, 12, 12),
        style: RenderCellStyle::default(),
    }
}

#[test]
fn full_frame_fallback_honors_config_and_surface_capability() {
    assert!(!RendererOptions::default().damage_tracking);
    assert!(should_prepare_full_frame(true, true, true));
    assert!(should_prepare_full_frame(false, false, true));
    assert!(should_prepare_full_frame(false, true, false));
    assert!(!should_prepare_full_frame(false, true, true));
}

#[test]
fn surface_clear_color_matches_scene_color_space() {
    let color = RenderColor {
        red: 12,
        green: 64,
        blue: 255,
        alpha: 128,
    };
    let unorm = surface_clear_color(color, wgpu::TextureFormat::Bgra8Unorm);
    let srgb = surface_clear_color(color, wgpu::TextureFormat::Bgra8UnormSrgb);

    assert!((unorm.r - 12.0 / 255.0).abs() < f64::EPSILON);
    assert!(srgb.r < unorm.r);
    assert!(srgb.g < unorm.g);
    assert!((srgb.b - 1.0).abs() < f64::EPSILON);
    assert!((srgb.a - 128.0 / 255.0).abs() < f64::EPSILON);
}

#[test]
fn premultiplied_surface_clear_scales_rgb_by_alpha() {
    let color = RenderColor {
        red: 200,
        green: 100,
        blue: 50,
        alpha: 128,
    };
    let clear = surface_clear_color_for_alpha_mode(
        color,
        wgpu::TextureFormat::Rgba8Unorm,
        wgpu::CompositeAlphaMode::PreMultiplied,
    );
    let alpha = 128.0 / 255.0;

    assert!((clear.r - (200.0 / 255.0) * alpha).abs() < f64::EPSILON);
    assert!((clear.g - (100.0 / 255.0) * alpha).abs() < f64::EPSILON);
    assert!((clear.b - (50.0 / 255.0) * alpha).abs() < f64::EPSILON);
    assert!((clear.a - alpha).abs() < f64::EPSILON);
}

#[test]
fn transparent_surface_prefers_premultiplied_compositing_and_blending() {
    let modes = [
        wgpu::CompositeAlphaMode::PostMultiplied,
        wgpu::CompositeAlphaMode::PreMultiplied,
        wgpu::CompositeAlphaMode::Opaque,
    ];
    let selected = select_composite_alpha_mode(true, &modes);

    assert_eq!(selected, wgpu::CompositeAlphaMode::PreMultiplied);
    assert_eq!(
        blend_state_for_alpha_mode(selected),
        wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING
    );
}

#[test]
fn text_gamma_adjustment_is_identity_by_default_and_strengthens_coverage_when_raised() {
    let adjusted = adjust_text_coverage(0.25, 1.2);

    assert!(adjusted > 0.25);
    assert!(adjusted < 1.0);
    assert_eq!(adjust_text_coverage(0.25, 1.0), 0.25);
    assert_eq!(RendererOptions::default().text_gamma_adjustment, 1.0);
}

#[test]
fn transparent_surface_clear_uses_configured_background_alpha() {
    let background = RenderColor {
        red: 30,
        green: 30,
        blue: 46,
        alpha: 235,
    };

    assert_eq!(surface_background_color(true, background), background);
    assert_eq!(
        surface_background_color(false, background),
        RenderColor {
            alpha: u8::MAX,
            ..background
        }
    );
}

#[test]
fn pane_border_decorations_render_behind_terminal_glyphs() {
    assert!(overlay_draws_behind_terminal_text(OverlayKind::Decoration));
}

#[test]
fn clipped_glyph_quad_preserves_atlas_uvs_at_pane_boundary() {
    let mut batch = GlyphBatch {
        vertices: Vec::new(),
        indices: Vec::new(),
        glyph_count: 0,
    };
    push_clipped_glyph_quad(
        &mut batch,
        RenderRect {
            x: 10,
            y: 20,
            width: 10,
            height: 10,
        },
        AtlasEntry {
            x: 20,
            y: 40,
            width: 10,
            height: 10,
        },
        (100, 100),
        RenderColor::rgb(255, 255, 255),
        false,
        Some(RenderRect {
            x: 12,
            y: 22,
            width: 4,
            height: 5,
        }),
    );

    assert_eq!(batch.glyph_count, 1);
    assert_eq!(batch.vertices[0].position_px, [12.0, 22.0]);
    assert_eq!(batch.vertices[2].position_px, [16.0, 27.0]);
    for (actual, expected) in batch.vertices[0].uv.into_iter().zip([0.22, 0.42]) {
        assert!((actual - expected).abs() < 0.000_001);
    }
    for (actual, expected) in batch.vertices[2].uv.into_iter().zip([0.26, 0.47]) {
        assert!((actual - expected).abs() < 0.000_001);
    }
}

#[test]
fn damage_scissors_clip_each_region_to_the_surface() {
    let scissors = damage_scissor_rects(
        &[
            RenderRect {
                x: -4,
                y: 3,
                width: 10,
                height: 5,
            },
            RenderRect {
                x: 18,
                y: 8,
                width: 10,
                height: 10,
            },
            RenderRect {
                x: 40,
                y: 40,
                width: 2,
                height: 2,
            },
        ],
        20,
        12,
    );

    assert_eq!(
        scissors,
        vec![
            RenderRect {
                x: 0,
                y: 3,
                width: 6,
                height: 5,
            },
            RenderRect {
                x: 18,
                y: 8,
                width: 2,
                height: 4,
            },
        ]
    );
}

#[test]
fn damage_rows_bucket_horizontal_cell_spans_once_per_row() {
    let damage = [
        RenderRect {
            x: 16,
            y: 16,
            width: 8,
            height: 16,
        },
        RenderRect {
            x: 40,
            y: 16,
            width: 8,
            height: 16,
        },
    ];
    let rows = DamageRows::new(
        &damage,
        metrics(),
        render_core::RenderOffset::default(),
        0,
        3,
    );

    assert_eq!(rows.span(0), None);
    assert_eq!(rows.span(1), Some((2, 5)));
    assert_eq!(rows.span(2), None);
    assert!(rows.intersects_cell(CellPosition { row: 1, col: 4 }));
    assert!(!rows.intersects_cell(CellPosition { row: 0, col: 4 }));
}

#[test]
fn merged_damage_regions_are_sorted_after_horizontal_sweep() {
    let merged = merge_regions(vec![
        RenderRect {
            x: 40,
            y: 0,
            width: 8,
            height: 8,
        },
        RenderRect {
            x: 8,
            y: 0,
            width: 8,
            height: 8,
        },
        RenderRect {
            x: 0,
            y: 0,
            width: 8,
            height: 8,
        },
    ]);

    assert_eq!(
        merged,
        vec![
            RenderRect {
                x: 0,
                y: 0,
                width: 16,
                height: 8,
            },
            RenderRect {
                x: 40,
                y: 0,
                width: 8,
                height: 8,
            },
        ]
    );
}

#[test]
fn partial_damage_keeps_the_full_intersecting_glyph_for_gpu_scissoring() {
    let mut batch = GlyphBatch {
        vertices: Vec::new(),
        indices: Vec::new(),
        glyph_count: 0,
    };
    push_scissor_culled_glyph_quad(
        &mut batch,
        RenderRect {
            x: 8,
            y: 0,
            width: 16,
            height: 16,
        },
        AtlasEntry {
            x: 1,
            y: 1,
            width: 16,
            height: 16,
        },
        (64, 64),
        RenderColor::rgb(255, 255, 255),
        false,
        GlyphQuadClip {
            content: None,
            damage_regions: Some(&[RenderRect {
                x: 12,
                y: 0,
                width: 2,
                height: 16,
            }]),
        },
    );

    assert_eq!(batch.glyph_count, 1);
    assert_eq!(batch.vertices[0].position_px, [8.0, 0.0]);
    assert_eq!(batch.vertices[2].position_px, [24.0, 16.0]);
}

#[test]
fn full_frame_rendering_never_loads_stale_retained_pixels() {
    for (damage_tracking_enabled, retained_damage_supported) in
        [(false, false), (false, true), (true, false)]
    {
        let full_frame =
            should_prepare_full_frame(false, damage_tracking_enabled, retained_damage_supported);
        let retained_damage_enabled = damage_tracking_enabled && retained_damage_supported;
        assert!(full_frame);
        assert!(!should_load_retained_frame(
            retained_damage_enabled,
            full_frame
        ));
    }

    let retained_damage_enabled = true;
    let full_frame = should_prepare_full_frame(false, true, true);
    assert!(!full_frame);
    assert!(should_load_retained_frame(
        retained_damage_enabled,
        full_frame
    ));
}

fn scene(cells: Vec<RenderCell>) -> RenderScene {
    RenderScene {
        grid: RenderGrid {
            columns: 4,
            rows: 2,
            cells,
        },
        cursor: Some(CursorVisual {
            position: CellPosition { row: 0, col: 0 },
            shape: RenderCursorShape::Block,
            color: RenderColor::rgb(255, 255, 255),
            text_color: None,
            visible: true,
            thickness_percent: 15,
            corner_radius_px: 0,
            inactive: false,
        }),
        ..RenderScene::default()
    }
}

#[test]
fn adjacent_ascii_cells_are_shaped_as_one_style_run() {
    let runs = terminal_text_runs(&[
        cell(0, 0, "="),
        cell(0, 1, ">"),
        cell(0, 2, " "),
        cell(1, 0, "x"),
    ]);
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].text, "=> ");
    assert_eq!(runs[1].text, "x");
}

#[test]
fn non_ascii_cells_remain_separate_text_runs() {
    let runs = terminal_text_runs(&[cell(0, 0, "a"), cell(0, 1, "界"), cell(0, 3, "b")]);

    assert_eq!(
        runs.iter().map(|run| run.text.as_str()).collect::<Vec<_>>(),
        ["a", "界", "b"]
    );
}

#[test]
fn powerline_cells_are_text_run_boundaries() {
    let runs = terminal_text_runs(&[cell(0, 0, "a"), cell(0, 1, "\u{e0b0}"), cell(0, 2, "b")]);

    assert_eq!(
        runs.iter().map(|run| run.text.as_str()).collect::<Vec<_>>(),
        ["a", "\u{e0b0}", "b"]
    );
}

#[test]
fn shaped_run_cache_evicts_the_least_recently_used_entry() {
    let mut fonts = FontSystem::new(font_system::FontConfig::default());
    let mut planner = RenderBatchPlanner {
        max_glyph_runs: 2,
        ..RenderBatchPlanner::default()
    };

    for text in ["a", "b", "a", "c"] {
        planner
            .prepare_full(&scene_without_cursor(vec![cell(0, 0, text)]), &mut fonts)
            .expect("shape cache probe");
    }

    let cache_state = planner
        .glyph_runs
        .iter()
        .map(|(key, cached)| (key.text.as_str(), cached.last_used))
        .collect::<Vec<_>>();
    assert!(
        planner.glyph_runs.keys().any(|key| key.text == "a"),
        "cache state: {cache_state:?}"
    );
    assert!(!planner.glyph_runs.keys().any(|key| key.text == "b"));
    assert!(planner.glyph_runs.keys().any(|key| key.text == "c"));
}

#[test]
fn rounded_rectangles_emit_one_sdf_quad() {
    let mut fill = QuadBatch::new(QuadBatchKind::Decoration);
    push_rounded_quads(
        &mut fill,
        RenderRect {
            x: 10,
            y: 20,
            width: 200,
            height: 120,
        },
        12,
        RenderColor::rgb(20, 40, 60),
    );
    let mut stroke = QuadBatch::new(QuadBatchKind::Decoration);
    push_rounded_stroke_quads(
        &mut stroke,
        RenderRect {
            x: 10,
            y: 20,
            width: 200,
            height: 120,
        },
        2,
        12,
        RenderColor::rgb(20, 40, 60),
    );

    assert_eq!(fill.quad_count(), 1);
    assert_eq!(stroke.quad_count(), 1);
    assert!(
        fill.vertices[0].color[3] < 0.0,
        "negative alpha tags SDF metadata"
    );
}

#[test]
fn alpha_atlas_upload_keeps_one_byte_per_padded_texel() {
    let upload = AtlasUpload {
        key: AtlasCacheKey::Glyph(GlyphCacheKey::new(1, 1, 13.0, false, false)),
        entry: AtlasEntry {
            x: GLYPH_ATLAS_PADDING,
            y: GLYPH_ATLAS_PADDING,
            width: 2,
            height: 1,
        },
        pixels: vec![10, 20],
        format: GlyphBitmapFormat::Alpha,
    };

    let padded = padded_atlas_upload(&upload).expect("valid padded alpha upload");

    assert_eq!(padded.bytes_per_row, 2 + GLYPH_ATLAS_PADDING * 2);
    assert_eq!(padded.pixels.len(), padded.bytes_per_row as usize * 3);
    assert_eq!(&padded.pixels[..4], &[10, 10, 20, 20]);
}

#[test]
fn wide_text_run_uses_its_display_cell_width() {
    let rect = text_run_region(&cell(0, 3, "界"), metrics());

    assert_eq!(rect.x, 24);
    assert_eq!(rect.width, 16);
}

#[test]
fn repeated_ascii_live_input_emits_one_monotonic_glyph_quad_per_character() {
    let text = "hhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhheeeeeeeeeeee";
    let mut fonts = FontSystem::new(font_system::FontConfig::default());
    let metrics = fonts.cell_metrics().expect("default font metrics");
    let cells = text
        .chars()
        .enumerate()
        .map(|(column, character)| cell(0, column as u16, &character.to_string()))
        .collect::<Vec<_>>();
    let mut test_scene = scene_without_cursor(cells);
    test_scene.grid.columns = 80;
    test_scene.grid.rows = 2;
    test_scene.damage_regions = vec![scene_grid_region(&test_scene, metrics)];
    let mut planner = RenderBatchPlanner::default();

    let batches = planner
        .prepare(&test_scene, &mut fonts)
        .expect("repeated input should prepare");
    let origins = batches
        .glyphs
        .vertices
        .chunks_exact(4)
        .map(|quad| quad[0].position_px[0])
        .collect::<Vec<_>>();

    assert_eq!(batches.glyphs.glyph_count, text.chars().count());
    assert_eq!(origins.len(), text.chars().count());
    assert!(
        origins.windows(2).all(|pair| pair[0] < pair[1]),
        "glyph origins must not overlap or repeat: {origins:?}"
    );
}

#[test]
fn partial_damage_selects_only_affected_rows_for_text_shaping() {
    let cells = (0..40_i64)
        .flat_map(|row| (0..80_u16).map(move |col| cell(row, col, "x")))
        .collect::<Vec<_>>();
    let damage = [RenderRect {
        x: 24 * 8,
        y: 17 * 16,
        width: 8,
        height: 16,
    }];

    let selected = damaged_terminal_text_runs_with_stats(
        &cells,
        &damage,
        metrics(),
        render_core::RenderOffset::default(),
        &[],
    );

    assert_eq!(selected.source_cells, 80);
    assert_eq!(selected.runs.len(), 1);
    assert_eq!(selected.runs[0].cell.position.row, 17);
    assert_eq!(selected.runs[0].cell.text.len(), 80);
}

#[test]
fn cpu_rasterizer_blends_color_glyph_pixels_without_terminal_tint() {
    let mut frame = CpuFrame {
        width: 1,
        height: 1,
        pixels: vec![0, 0, 0, 255],
    };
    let bitmap = GlyphBitmap {
        width: 1,
        height: 1,
        offset_x: 0,
        offset_y: 0,
        advance_width: 1.0,
        pixels: vec![240, 20, 80, 255],
        format: GlyphBitmapFormat::Rgba,
    };
    draw_glyph(&mut frame, 0, 0, &bitmap, RenderColor::rgb(0, 255, 0));
    assert_eq!(&frame.pixels[..3], &[240, 20, 80]);
}

#[test]
fn cpu_glyph_rasterization_respects_pane_clip() {
    let mut frame = CpuFrame {
        width: 4,
        height: 1,
        pixels: vec![0; 4 * 4],
    };
    let bitmap = GlyphBitmap {
        width: 4,
        height: 1,
        offset_x: 0,
        offset_y: 0,
        advance_width: 4.0,
        pixels: vec![255; 4],
        format: GlyphBitmapFormat::Alpha,
    };

    draw_glyph_clipped(
        &mut frame,
        0,
        0,
        &bitmap,
        RenderColor::rgb(255, 255, 255),
        Some(RenderRect {
            x: 1,
            y: 0,
            width: 2,
            height: 1,
        }),
    );

    assert_eq!(&frame.pixels[0..3], &[0, 0, 0]);
    assert_eq!(&frame.pixels[4..7], &[255, 255, 255]);
    assert_eq!(&frame.pixels[8..11], &[255, 255, 255]);
    assert_eq!(&frame.pixels[12..15], &[0, 0, 0]);
}

fn scene_without_cursor(cells: Vec<RenderCell>) -> RenderScene {
    RenderScene {
        cursor: None,
        ..scene(cells)
    }
}

#[test]
fn atlas_reports_full_without_invalidating_existing_entries() {
    let mut atlas = GlyphAtlas::new(16, 8);
    let key_a = GlyphCacheKey::new(1, u16::from(b'a'), 13.0, false, false);
    let key_b = GlyphCacheKey::new(1, u16::from(b'b'), 13.0, false, false);
    let key_c = GlyphCacheKey::new(1, u16::from(b'c'), 13.0, false, false);
    let bitmap = GlyphBitmap::missing(4.0, 4);

    assert!(atlas.allocate(key_a, &bitmap).is_some());
    assert!(atlas.allocate(key_b, &bitmap).is_some());
    assert!(atlas.allocate(key_c, &bitmap).is_none());
    assert_eq!(atlas.len(), 2);
    assert!(atlas.entry(key_a).is_some());
    assert!(atlas.entry(key_b).is_some());
}

#[test]
fn batch_prepare_restarts_after_a_stale_atlas_fills() {
    let mut fonts = FontSystem::new(font_system::FontConfig::default());
    let mut planner = RenderBatchPlanner::new(4096, 256, 64);
    let bitmap = GlyphBitmap::missing(8.0, 8);
    let mut reached_capacity = false;
    for glyph_id in 0..u16::MAX {
        let key = GlyphCacheKey::new(99, glyph_id, 13.0, false, false);
        if planner.atlas.allocate(key, &bitmap).is_none() {
            reached_capacity = true;
            break;
        }
    }
    assert!(reached_capacity, "test atlas must begin full");
    planner.atlas_font_generation = Some(fonts.generation_id());

    let batches = planner
        .prepare_full(&scene_without_cursor(vec![cell(0, 0, "W")]), &mut fonts)
        .expect("a full stale atlas should be cleared and prepared again");

    assert!(batches.glyphs.glyph_count > 0);
    assert!(batches.instrumentation.glyphs.atlas_uploads > 0);
}

#[test]
fn font_generation_change_evicts_old_atlas_sizes() {
    let mut fonts = FontSystem::new(font_system::FontConfig::default());
    let mut planner = RenderBatchPlanner::default();
    let test_scene = scene_without_cursor(vec![cell(0, 0, "abc")]);
    planner
        .prepare_full(&test_scene, &mut fonts)
        .expect("initial font generation should prepare");
    assert!(!planner.atlas.is_empty());

    assert!(fonts.set_scale_factor(1.5));
    let zoomed = planner
        .prepare_full(&test_scene, &mut fonts)
        .expect("zoomed font generation should prepare");

    assert_eq!(
        planner.atlas.len(),
        zoomed.atlas_uploads.len(),
        "the atlas must contain only entries uploaded for the current font generation"
    );
}

#[test]
fn damage_tracks_changed_cell_and_cursor() {
    let mut tracker = DamageTracker::new();
    let first = scene(vec![cell(0, 0, "a"), cell(0, 1, "b")]);
    let initial = tracker.update(&first, metrics());
    assert_eq!(initial.len(), 1);

    let mut second = scene(vec![cell(0, 0, "a"), cell(0, 1, "c")]);
    second.cursor = Some(CursorVisual {
        position: CellPosition { row: 1, col: 1 },
        shape: RenderCursorShape::Block,
        color: RenderColor::rgb(255, 255, 255),
        text_color: None,
        visible: true,
        thickness_percent: 15,
        corner_radius_px: 0,
        inactive: false,
    });

    let damage = tracker.update(&second, metrics());
    assert!(damage_covers(
        &damage,
        RenderRect {
            x: 8,
            y: 0,
            width: 8,
            height: 16
        }
    ));
    assert!(damage_covers(
        &damage,
        RenderRect {
            x: 0,
            y: 0,
            width: 8,
            height: 16
        }
    ));
    assert!(damage_covers(
        &damage,
        RenderRect {
            x: 8,
            y: 16,
            width: 8,
            height: 16
        }
    ));
}

#[test]
fn pane_clip_geometry_changes_force_a_full_retained_frame_redraw() {
    let mut tracker = DamageTracker::new();
    let mut first = scene_without_cursor(vec![cell(0, 0, "a"), cell(0, 1, "b")]);
    first.content_clips = vec![
        render_core::RenderContentClip {
            bounds: RenderRect {
                x: 0,
                y: 0,
                width: 16,
                height: 32,
            },
            cells: render_core::RenderItemRange::new(0, 1),
            search_highlights: render_core::RenderItemRange::default(),
            semantic_overlays: render_core::RenderItemRange::default(),
            selections: render_core::RenderItemRange::default(),
        },
        render_core::RenderContentClip {
            bounds: RenderRect {
                x: 16,
                y: 0,
                width: 16,
                height: 32,
            },
            cells: render_core::RenderItemRange::new(1, 2),
            search_highlights: render_core::RenderItemRange::default(),
            semantic_overlays: render_core::RenderItemRange::default(),
            selections: render_core::RenderItemRange::default(),
        },
    ];
    let _ = tracker.update(&first, metrics());

    let mut resized = first.clone();
    resized.content_clips[0].bounds.width = 8;
    resized.content_clips[1].bounds.x = 8;
    resized.content_clips[1].bounds.width = 24;

    assert_eq!(
        tracker.update(&resized, metrics()),
        vec![RenderRect {
            x: 0,
            y: 0,
            width: 32,
            height: 32,
        }],
        "changing a pane viewport must invalidate stale retained pixels"
    );
}

#[test]
fn content_damage_includes_local_ligature_context() {
    let mut tracker = DamageTracker::new();
    let first = scene_without_cursor(vec![
        cell(0, 0, "a"),
        cell(0, 1, "b"),
        cell(0, 2, "c"),
        cell(0, 3, "d"),
    ]);
    let _ = tracker.update(&first, metrics());
    let second = scene_without_cursor(vec![
        cell(0, 0, "a"),
        cell(0, 1, "b"),
        cell(0, 2, "x"),
        cell(0, 3, "d"),
    ]);

    let damage = tracker.update(&second, metrics());

    for col in 0..4 {
        assert!(
            damage_covers(
                &damage,
                RenderRect {
                    x: col * 8,
                    y: 0,
                    width: 8,
                    height: 16
                }
            ),
            "column {col} should be repainted for shaping context"
        );
    }
}

#[test]
fn damage_tracks_removed_cells_and_removed_overlays() {
    let mut tracker = DamageTracker::new();
    let mut first = scene(vec![cell(0, 0, "a"), cell(0, 1, "b")]);
    first.semantic_overlays.push(OverlayPrimitive {
        kind: OverlayKind::CommandBlock,
        bounds: RenderRect {
            x: 0,
            y: 16,
            width: 16,
            height: 16,
        },
        color: RenderColor::rgb(20, 20, 20),
        border_color: None,
        border_width_px: 0,
        corner_radius_px: 0,
        z_index: 0,
        label: None,
        label_color: None,
    });
    let _ = tracker.update(&first, metrics());

    let second = scene(vec![cell(0, 0, "a")]);
    let damage = tracker.update(&second, metrics());

    assert!(damage_covers(
        &damage,
        RenderRect {
            x: 8,
            y: 0,
            width: 8,
            height: 16
        }
    ));
    assert!(damage_covers(
        &damage,
        RenderRect {
            x: 0,
            y: 16,
            width: 16,
            height: 16
        }
    ));
}

#[test]
fn cursor_animation_damage_does_not_redraw_static_semantic_overlays() {
    let cursor_animation = |x| AnimationHandle {
        id: 1,
        kind: AnimationKind::CursorTrail,
        affected_region: RenderRect {
            x,
            y: 0,
            width: 12,
            height: 20,
        },
        start_region: RenderRect {
            x,
            y: 0,
            width: 2,
            height: 16,
        },
        end_region: RenderRect {
            x: x + 8,
            y: 0,
            width: 2,
            height: 16,
        },
        color: RenderColor::rgb(245, 224, 220),
        quad: None,
        elapsed: Duration::ZERO,
        remaining: None,
    };
    let static_overlay = OverlayPrimitive {
        kind: OverlayKind::CommandBlock,
        bounds: RenderRect {
            x: 0,
            y: 120,
            width: 1_000,
            height: 400,
        },
        color: RenderColor::rgb(20, 20, 20),
        border_color: None,
        border_width_px: 0,
        corner_radius_px: 0,
        z_index: 0,
        label: None,
        label_color: None,
    };
    let mut tracker = DamageTracker::new();
    let mut first = scene(vec![cell(0, 0, "a")]);
    first.semantic_overlays.push(static_overlay.clone());
    first.animations.push(cursor_animation(0));
    let _ = tracker.update(&first, metrics());

    let mut second = first.clone();
    second.animations = vec![cursor_animation(4)];
    let damage = tracker.update(&second, metrics());

    assert!(damage_covers(&damage, cursor_animation(0).affected_region));
    assert!(damage_covers(&damage, cursor_animation(4).affected_region));
    assert!(
        damage.iter().all(|region| region.y < 120),
        "an unchanged command-block overlay must not be damaged by cursor motion: {damage:?}"
    );
}

#[test]
fn frame_scheduler_stays_idle_without_work() {
    let mut scheduler = FrameScheduler::new();
    assert!(!scheduler.has_pending_frame());
    assert_eq!(scheduler.next_frame(), FrameDecision::NoFrameNeeded);

    scheduler.terminal_content_changed();
    assert!(scheduler.has_pending_frame());
    assert_eq!(
        scheduler.next_frame(),
        FrameDecision::FrameNeeded(FrameRequestReason::TerminalContentChanged)
    );
    assert!(!scheduler.has_pending_frame());
    assert_eq!(scheduler.next_frame(), FrameDecision::NoFrameNeeded);
}

#[test]
fn frame_scheduler_never_lets_animation_displace_terminal_content() {
    let mut scheduler = FrameScheduler::new();

    scheduler.terminal_content_changed();
    scheduler.animation_changed();

    assert_eq!(
        scheduler.next_frame(),
        FrameDecision::FrameNeeded(FrameRequestReason::TerminalContentChanged),
        "cursor animation must never postpone newly echoed terminal content"
    );

    scheduler.animation_changed();
    scheduler.terminal_content_changed();
    assert_eq!(
        scheduler.next_frame(),
        FrameDecision::FrameNeeded(FrameRequestReason::TerminalContentChanged)
    );
}

#[test]
fn animation_frame_pacer_waits_for_its_deadline_without_drifting() {
    let started = Instant::now();
    let interval = Duration::from_millis(8);
    let deadline = started + interval;
    let mut pacer = AnimationFramePacer::new();

    assert_eq!(
        pacer.poll(started, Some(interval)),
        AnimationFramePacerDecision::WaitUntil(deadline)
    );
    assert_eq!(
        pacer.poll(started + Duration::from_millis(3), Some(interval)),
        AnimationFramePacerDecision::WaitUntil(deadline)
    );
    assert_eq!(
        pacer.poll(deadline, Some(interval)),
        AnimationFramePacerDecision::FrameDue
    );
    assert_eq!(
        pacer.poll(deadline, Some(interval)),
        AnimationFramePacerDecision::WaitUntil(deadline + interval)
    );
}

#[test]
fn animation_frame_pacer_cancels_a_pending_wake_when_idle() {
    let started = Instant::now();
    let mut pacer = AnimationFramePacer::new();

    assert!(matches!(
        pacer.poll(started, Some(Duration::from_millis(8))),
        AnimationFramePacerDecision::WaitUntil(_)
    ));
    assert_eq!(
        pacer.poll(started + Duration::from_millis(1), None),
        AnimationFramePacerDecision::Idle
    );
    assert_eq!(
        pacer.poll(started + Duration::from_millis(20), None),
        AnimationFramePacerDecision::Idle
    );
}

#[test]
fn cpu_snapshot_changes_when_content_changes() {
    let mut fonts = FontSystem::new(font_system::FontConfig::default());
    let mut rasterizer = TerminalRasterizer::default();

    let Ok(first) = rasterizer.rasterize(&scene_without_cursor(vec![cell(0, 0, "a")]), &mut fonts)
    else {
        return;
    };
    let second = rasterizer
        .rasterize(&scene_without_cursor(vec![cell(0, 0, "b")]), &mut fonts)
        .expect("same resolved font should render second snapshot");

    assert_ne!(first.snapshot_hash(), second.snapshot_hash());
}

#[test]
fn batch_planner_groups_cells_into_few_draws() {
    let mut fonts = FontSystem::new(font_system::FontConfig::default());
    let mut planner = RenderBatchPlanner::default();
    let Ok(batches) = planner.prepare_full(
        &scene(vec![cell(0, 0, "a"), cell(0, 1, "b"), cell(1, 0, "c")]),
        &mut fonts,
    ) else {
        return;
    };

    assert_eq!(batches.background.quad_count(), 3);
    assert_eq!(batches.glyphs.glyph_count, 3);
    assert!(batches.instrumentation.draw_call_count <= 3);
    assert!(batches.instrumentation.glyphs.atlas_uploads > 0);
}

#[test]
fn solid_powerline_caps_fill_exactly_one_terminal_cell() {
    let mut fonts = FontSystem::new(font_system::FontConfig::default());
    let metrics = fonts.cell_metrics().expect("cell metrics");
    let mut planner = RenderBatchPlanner::default();
    let batches = planner
        .prepare_full(
            &scene_without_cursor(vec![cell(0, 0, "\u{e0b6}"), cell(0, 1, "\u{e0b4}")]),
            &mut fonts,
        )
        .expect("prepare powerline caps");

    assert_eq!(batches.glyphs.glyph_count, 2);
    for (index, position) in [
        CellPosition { row: 0, col: 0 },
        CellPosition { row: 0, col: 1 },
    ]
    .into_iter()
    .enumerate()
    {
        let expected = cell_region(position, metrics);
        let vertices = &batches.glyphs.vertices[index * 4..index * 4 + 4];
        let actual = RenderRect {
            x: vertices[0].position_px[0].round() as i32,
            y: vertices[0].position_px[1].round() as i32,
            width: (vertices[2].position_px[0] - vertices[0].position_px[0]).round() as u32,
            height: (vertices[2].position_px[1] - vertices[0].position_px[1]).round() as u32,
        };
        assert_eq!(actual, expected, "powerline cap {index} escaped its cell");
    }

    assert_eq!(batches.atlas_uploads.len(), 2);
    let left = &batches.atlas_uploads[0];
    let right = &batches.atlas_uploads[1];
    for (upload, col) in [(left, 0), (right, 1)] {
        let expected = cell_region(CellPosition { row: 0, col }, metrics);
        assert_eq!(upload.format, GlyphBitmapFormat::Alpha);
        assert_eq!(upload.entry.width, expected.width);
        assert_eq!(upload.entry.height, expected.height);
    }
    let alpha_at =
        |upload: &AtlasUpload, x: u32, y: u32| upload.pixels[(y * upload.entry.width + x) as usize];
    let left_last_x = left.entry.width - 1;
    let right_last_x = right.entry.width - 1;
    let middle_y = left.entry.height / 2;
    assert!(alpha_at(left, 0, 0) < 128);
    assert!(alpha_at(left, left_last_x, 0) > 128);
    assert!(alpha_at(left, 0, middle_y) > 128);
    assert!(alpha_at(right, right_last_x, 0) < 128);
    assert!(alpha_at(right, 0, 0) > 128);
    assert!(alpha_at(right, right_last_x, middle_y) > 128);
}

#[test]
fn solid_powerline_triangles_fill_exactly_one_terminal_cell() {
    let mut fonts = FontSystem::new(font_system::FontConfig::default());
    let metrics = fonts.cell_metrics().expect("cell metrics");
    let mut planner = RenderBatchPlanner::default();
    let batches = planner
        .prepare_full(
            &scene_without_cursor(vec![cell(0, 0, "\u{e0b2}"), cell(0, 1, "\u{e0b0}")]),
            &mut fonts,
        )
        .expect("prepare powerline triangles");

    assert_eq!(batches.glyphs.glyph_count, 2);
    for (index, position) in [
        CellPosition { row: 0, col: 0 },
        CellPosition { row: 0, col: 1 },
    ]
    .into_iter()
    .enumerate()
    {
        let expected = cell_region(position, metrics);
        let vertices = &batches.glyphs.vertices[index * 4..index * 4 + 4];
        let actual = RenderRect {
            x: vertices[0].position_px[0].round() as i32,
            y: vertices[0].position_px[1].round() as i32,
            width: (vertices[2].position_px[0] - vertices[0].position_px[0]).round() as u32,
            height: (vertices[2].position_px[1] - vertices[0].position_px[1]).round() as u32,
        };
        assert_eq!(
            actual, expected,
            "powerline triangle {index} escaped its cell"
        );
    }

    assert_eq!(batches.atlas_uploads.len(), 2);
    for (upload, col) in batches.atlas_uploads.iter().zip(0_u16..=1) {
        let expected = cell_region(CellPosition { row: 0, col }, metrics);
        assert_eq!(upload.format, GlyphBitmapFormat::Alpha);
        assert_eq!(upload.entry.width, expected.width);
        assert_eq!(upload.entry.height, expected.height);
    }
}

#[test]
fn semantic_command_blocks_draw_behind_text_and_badges_get_overlay_glyphs() {
    let mut fonts = FontSystem::new(font_system::FontConfig::default());
    let mut planner = RenderBatchPlanner::default();
    let mut test_scene = scene_without_cursor(vec![cell(0, 0, "p"), cell(0, 1, "w")]);
    test_scene.semantic_overlays = vec![
        OverlayPrimitive {
            kind: OverlayKind::CommandBlock,
            bounds: RenderRect {
                x: 0,
                y: 0,
                width: 64,
                height: 32,
            },
            color: RenderColor {
                red: 40,
                green: 48,
                blue: 56,
                alpha: 96,
            },
            border_color: Some(RenderColor::rgb(43, 185, 115)),
            border_width_px: 1,
            corner_radius_px: 4,
            z_index: 10,
            label: None,
            label_color: None,
        },
        OverlayPrimitive {
            kind: OverlayKind::Badge,
            bounds: RenderRect {
                x: 16,
                y: 2,
                width: 16,
                height: 14,
            },
            color: RenderColor {
                red: 43,
                green: 185,
                blue: 115,
                alpha: 148,
            },
            border_color: None,
            border_width_px: 0,
            corner_radius_px: 3,
            z_index: 30,
            label: Some("ok".to_owned()),
            label_color: None,
        },
    ];

    let Ok(batches) = planner.prepare_full(&test_scene, &mut fonts) else {
        return;
    };

    assert!(
        batches.background.quad_count() > test_scene.grid.cells.len(),
        "command block overlay should be batched behind terminal glyphs"
    );
    assert!(
        batches.decorations.quad_count() >= 1,
        "badge rectangle should be an overlay decoration"
    );
    assert_eq!(batches.overlay_glyphs.glyph_count, 2);
}

#[test]
fn collapsed_content_masks_render_above_terminal_glyphs() {
    assert!(!overlay_draws_behind_terminal_text(
        OverlayKind::ContentMask
    ));

    let mut batch = QuadBatch::new(QuadBatchKind::Decoration);
    push_rounded_stroke_quads(
        &mut batch,
        RenderRect {
            x: 0,
            y: 0,
            width: 80,
            height: 32,
        },
        3,
        6,
        RenderColor::rgb(255, 255, 255),
    );
    assert_eq!(batch.quad_count(), 1);
}

#[test]
fn batch_planner_reuses_cached_glyphs_and_atlas_entries() {
    let mut fonts = FontSystem::new(font_system::FontConfig::default());
    let mut planner = RenderBatchPlanner::default();
    let test_scene = scene_without_cursor(vec![cell(0, 0, "panea")]);
    if planner.prepare_full(&test_scene, &mut fonts).is_err() {
        return;
    }
    let second = planner
        .prepare_full(&test_scene, &mut fonts)
        .expect("same resolved font should prepare second batch");

    assert!(second.instrumentation.glyphs.cache_hits > 0);
    assert_eq!(second.instrumentation.glyphs.atlas_uploads, 0);
}

#[test]
fn batch_planner_reuses_cpu_geometry_storage_between_frames() {
    let mut fonts = FontSystem::new(font_system::FontConfig::default());
    let mut planner = RenderBatchPlanner::default();
    let test_scene = scene_without_cursor(vec![cell(0, 0, "panea")]);
    let Ok(first) = planner.prepare_full(&test_scene, &mut fonts) else {
        return;
    };
    let background_ptr = first.background.vertices.as_ptr();

    let second = planner
        .prepare_full_reusing(&test_scene, &mut fonts, Some(first))
        .expect("same resolved font should prepare a recycled frame");

    assert_eq!(second.background.vertices.as_ptr(), background_ptr);
    assert!(second.background.vertices.capacity() > 0);
}

#[test]
fn shaped_terminal_run_stays_aligned_with_cursor_cell() {
    const TEXT: &str = "panea-grid-cursor-check";
    let mut fonts = FontSystem::new_with_scale_factor(font_system::FontConfig::default(), 1.25);
    let metrics = fonts.cell_metrics().expect("cell metrics");
    let cells = TEXT
        .chars()
        .enumerate()
        .map(|(col, ch)| cell(0, col as u16, &ch.to_string()))
        .collect::<Vec<_>>();
    let mut test_scene = scene(cells);
    test_scene.cursor = Some(CursorVisual {
        position: CellPosition {
            row: 0,
            col: TEXT.len() as u16,
        },
        shape: RenderCursorShape::Beam,
        color: RenderColor::rgb(255, 255, 255),
        text_color: None,
        visible: true,
        thickness_percent: 15,
        corner_radius_px: 0,
        inactive: false,
    });
    let mut planner = RenderBatchPlanner::default();
    let batches = planner
        .prepare_full(&test_scene, &mut fonts)
        .expect("prepare terminal run");

    let text_right = batches
        .glyphs
        .vertices
        .iter()
        .map(|vertex| vertex.position_px[0])
        .fold(f32::NEG_INFINITY, f32::max);
    let cursor_left = batches
        .cursor
        .vertices
        .iter()
        .map(|vertex| vertex.position_px[0])
        .fold(f32::INFINITY, f32::min);
    assert!(
        text_right <= cursor_left + metrics.cell_width,
        "text geometry escaped its terminal cells: text_right={text_right}, cursor_left={cursor_left}, cell_width={}",
        metrics.cell_width
    );
}

#[test]
fn cursor_text_color_changes_color_without_changing_glyph_geometry() {
    let cells = vec![cell(0, 0, "a"), cell(0, 1, "b"), cell(0, 2, "c")];
    let mut base = scene_without_cursor(cells);
    let mut with_cursor = base.clone();
    with_cursor.cursor = Some(CursorVisual {
        position: CellPosition { row: 0, col: 1 },
        shape: RenderCursorShape::Block,
        color: RenderColor::rgb(255, 255, 255),
        text_color: Some(RenderColor::rgb(1, 2, 3)),
        visible: true,
        thickness_percent: 15,
        corner_radius_px: 0,
        inactive: false,
    });
    let mut fonts = FontSystem::new(font_system::FontConfig::default());
    let mut planner = RenderBatchPlanner::default();
    let Ok(without_batches) = planner.prepare_full(&base, &mut fonts) else {
        return;
    };
    base.damage_regions.clear();
    let with_batches = planner
        .prepare_full(&with_cursor, &mut fonts)
        .expect("cursor-colored run should prepare");

    let without_positions = without_batches
        .glyphs
        .vertices
        .iter()
        .map(|vertex| vertex.position_px)
        .collect::<Vec<_>>();
    let with_positions = with_batches
        .glyphs
        .vertices
        .iter()
        .map(|vertex| vertex.position_px)
        .collect::<Vec<_>>();
    assert_eq!(with_positions, without_positions);
    assert!(planner.glyph_runs.keys().any(|key| key.text == "abc"));
    assert!(with_batches.glyphs.vertices.iter().any(|vertex| {
        vertex.color[0] < 0.01 && vertex.color[1] < 0.02 && vertex.color[2] < 0.02
    }));
}

#[test]
fn prepared_terminal_glyph_uses_the_centered_cell_baseline() {
    let mut fonts = FontSystem::new(font_system::FontConfig {
        line_height: 1.5,
        ..font_system::FontConfig::default()
    });
    let metrics = fonts.cell_metrics().expect("cell metrics");
    let shaped = fonts.shape_text("H", false, false).expect("shape glyph");
    let glyph = shaped.glyphs[0];
    let bitmap = fonts.rasterize_glyph(glyph.key).expect("rasterize glyph");
    let expected_top = (metrics.baseline - glyph.y_offset).round() + bitmap.offset_y as f32;
    let mut planner = RenderBatchPlanner::default();
    let batches = planner
        .prepare_full(&scene_without_cursor(vec![cell(0, 0, "H")]), &mut fonts)
        .expect("prepare terminal glyph");
    let actual_top = batches
        .glyphs
        .vertices
        .iter()
        .map(|vertex| vertex.position_px[1])
        .fold(f32::INFINITY, f32::min);

    assert_eq!(actual_top, expected_top);
}

#[test]
fn resetting_gpu_resident_glyphs_reuploads_cached_glyphs() {
    let mut fonts = FontSystem::new(font_system::FontConfig::default());
    let mut planner = RenderBatchPlanner::default();
    let test_scene = scene_without_cursor(vec![cell(0, 0, "panea")]);
    if planner.prepare_full(&test_scene, &mut fonts).is_err() {
        return;
    }
    let cached = planner
        .prepare_full(&test_scene, &mut fonts)
        .expect("same resolved font should prepare cached batch");
    assert_eq!(cached.instrumentation.glyphs.atlas_uploads, 0);

    planner.reset_gpu_resident_glyphs();
    let recovered = planner
        .prepare_full(&test_scene, &mut fonts)
        .expect("cached glyph bitmaps should re-upload after atlas reset");

    assert!(recovered.instrumentation.glyphs.cache_hits > 0);
    assert!(recovered.instrumentation.glyphs.atlas_uploads > 0);
}

#[test]
fn device_lost_callback_mapping_ignores_intentional_teardown() {
    assert_eq!(
        map_device_lost_reason(wgpu::DeviceLostReason::Unknown),
        Some(RenderRecoveryReason::DeviceLost)
    );
    assert_eq!(
        map_device_lost_reason(wgpu::DeviceLostReason::DeviceInvalid),
        Some(RenderRecoveryReason::DeviceLost)
    );
    assert_eq!(
        map_device_lost_reason(wgpu::DeviceLostReason::Dropped),
        None
    );
    assert_eq!(
        map_device_lost_reason(wgpu::DeviceLostReason::ReplacedCallback),
        None
    );
}

#[test]
fn non_submitted_frames_require_the_next_frame_to_redraw_fully() {
    assert!(!present_outcome_requires_full_redraw(
        PresentOutcome::Submitted
    ));
    assert!(present_outcome_requires_full_redraw(
        PresentOutcome::Timeout
    ));
    assert!(present_outcome_requires_full_redraw(
        PresentOutcome::Skipped
    ));
    assert!(present_outcome_requires_full_redraw(
        PresentOutcome::SurfaceReconfigured(RenderRecoveryReason::SurfaceOutdated)
    ));
}

#[test]
fn surface_size_decision_skips_zero_and_reconfigures_changed_sizes() {
    assert_eq!(
        surface_size_action((80, 24), (0, 24)),
        SurfaceSizeAction::Skip
    );
    assert_eq!(
        surface_size_action((80, 24), (80, 24)),
        SurfaceSizeAction::Keep
    );
    assert_eq!(
        surface_size_action((80, 24), (120, 40)),
        SurfaceSizeAction::Reconfigure(120, 40)
    );
}

#[test]
fn uncaptured_validation_error_becomes_a_device_loss_signal() {
    let error = wgpu::Error::Validation {
        source: Box::new(std::io::Error::other("validation source")),
        description: "bad render command".to_owned(),
    };

    let signal = device_loss_signal_from_uncaptured(&error);

    assert_eq!(signal.reason, RenderRecoveryReason::DeviceLost);
    assert!(signal.message.contains("bad render command"));
}

#[test]
fn cursor_damage_only_batches_cursor_region() {
    let mut fonts = FontSystem::new(font_system::FontConfig::default());
    let mut planner = RenderBatchPlanner::default();
    let mut test_scene = scene(vec![cell(0, 0, "a"), cell(0, 1, "b")]);
    let Ok(font_metrics) = fonts.cell_metrics() else {
        return;
    };
    test_scene.damage_regions = vec![cell_region(CellPosition { row: 0, col: 0 }, font_metrics)];
    let Ok(batches) = planner.prepare(&test_scene, &mut fonts) else {
        return;
    };

    assert!(batches.background.quad_count() <= 1);
    assert_eq!(batches.glyphs.glyph_count, 1);
    assert_eq!(batches.cursor.quad_count(), 1);
    assert_eq!(batches.damage_regions.len(), 1);
}

#[test]
fn adjacent_cell_damage_coalesces_into_one_clear_region() {
    let cell_width = 8;
    let regions = (0..40)
        .map(|col| RenderRect {
            x: col * cell_width,
            y: 0,
            width: cell_width as u32,
            height: 16,
        })
        .collect();

    let merged = merge_regions(regions);

    assert_eq!(
        merged,
        vec![RenderRect {
            x: 0,
            y: 0,
            width: 320,
            height: 16
        }]
    );
}

#[test]
fn incremental_batch_shapes_with_complete_run_context_but_emits_only_damaged_glyphs() {
    let mut fonts = FontSystem::new(font_system::FontConfig::default());
    let mut planner = RenderBatchPlanner::default();
    let mut test_scene = scene_without_cursor(vec![
        cell(0, 0, "a"),
        cell(0, 1, "b"),
        cell(0, 2, "c"),
        cell(0, 3, "d"),
    ]);
    let Ok(font_metrics) = fonts.cell_metrics() else {
        return;
    };
    test_scene.damage_regions = vec![cell_region(CellPosition { row: 0, col: 3 }, font_metrics)];
    let Ok(batches) = planner.prepare(&test_scene, &mut fonts) else {
        return;
    };

    assert_eq!(batches.background.quad_count(), 1);
    assert!(
        (1..4).contains(&batches.glyphs.glyph_count),
        "only glyphs intersecting the damaged cell, including fractional overhang, should be emitted"
    );
    assert!(
        planner.glyph_runs.keys().any(|key| key.text == "abcd"),
        "incremental rendering must not reshape a damaged suffix without its line context"
    );
    assert!(!planner.glyph_runs.keys().any(|key| key.text == "d"));
    for quad in batches.glyphs.vertices.chunks_exact(4) {
        let glyph_bounds = quad_bounds([
            quad[0].position_px,
            quad[1].position_px,
            quad[2].position_px,
            quad[3].position_px,
        ]);
        assert!(
            batches
                .damage_regions
                .iter()
                .any(|damage| rect_contains(*damage, glyph_bounds)),
            "an alpha-blended glyph must not escape the region cleared for this retained frame: glyph={glyph_bounds:?} damage={:?}",
            batches.damage_regions
        );
    }
}

#[test]
fn disabled_cursor_animations_add_no_scene_work() {
    let mut runtime = CursorAnimationRuntime::new();
    let mut test_scene = scene(vec![cell(0, 0, "a")]);

    runtime.record_typing();
    runtime.populate_scene(
        &mut test_scene,
        metrics(),
        CursorAnimationSettings::default(),
    );

    assert!(test_scene.animations.is_empty());
    assert!(test_scene.damage_regions.is_empty());
    assert!(!runtime.needs_frame());
}

#[test]
fn panea_cursor_motion_tilts_and_extends_right() {
    let settings = CursorAnimationSettings::panea(165, 4, 250_000);
    let beam = |col| CursorVisual {
        position: CellPosition { row: 0, col },
        shape: RenderCursorShape::Beam,
        color: RenderColor::rgb(120, 190, 255),
        text_color: None,
        visible: true,
        thickness_percent: 22,
        corner_radius_px: 0,
        inactive: false,
    };
    let mut runtime = CursorAnimationRuntime::new();
    let mut initial = scene(vec![cell(0, 0, "a")]);
    initial.cursor = Some(beam(0));

    runtime.populate_scene(&mut initial, metrics(), settings);

    assert!(initial.animations.is_empty(), "startup must remain static");
    let mut moved = scene(vec![cell(0, 0, "a")]);
    moved.cursor = Some(beam(1));
    runtime.populate_scene(&mut moved, metrics(), settings);

    let tilt = moved
        .animations
        .iter()
        .find(|animation| animation.kind == AnimationKind::CursorTilt)
        .expect("rightward motion should produce the Panea tilt overlay");
    assert!(
        moved
            .animations
            .iter()
            .all(|animation| animation.kind != AnimationKind::CursorTrail),
        "the Panea preset must not extend geometry across previous cells"
    );
    let corners = animation_quad_pixels(tilt.quad.expect("tilt requires explicit geometry"));
    assert!(
        corners[0][0] > corners[3][0] && corners[1][0] > corners[2][0],
        "rightward movement should lean the cursor like /"
    );
    assert!(
        corners[0][0] - corners[3][0] >= metrics().cell_width * 0.85,
        "the Panea lean should be clearly visible"
    );
    let extension = moved
        .animations
        .iter()
        .find(|animation| animation.kind == AnimationKind::CursorElasticExtension)
        .expect("rightward motion should stretch behind the destination cursor");
    let extension_corners = animation_quad_pixels(
        extension
            .quad
            .expect("elastic extension requires explicit geometry"),
    );
    assert!(
        extension_corners
            .iter()
            .any(|corner| corner[0] < tilt.end_region.x as f32 - metrics().cell_width * 0.5),
        "the elastic quad should extend toward the previous cursor cell"
    );
    assert!(extension.start_region.x < extension.end_region.x);
    assert_eq!(extension.remaining, Some(Duration::from_millis(90)));
    assert_eq!(tilt.start_region, tilt.end_region);
    assert_eq!(tilt.end_region.x, metrics().cell_width.round() as i32);
    assert!(runtime.needs_frame());
    assert!(
        tilt.affected_region.width <= metrics().cell_width.ceil() as u32 + 8,
        "Panea motion must not damage unrelated terminal columns: width={}",
        tilt.affected_region.width
    );
    assert!(
        tilt.affected_region.height <= metrics().cell_height.ceil() as u32 + 8,
        "Panea motion must not damage unrelated terminal rows"
    );
}

#[test]
fn panea_cursor_motion_tilts_and_extends_left() {
    let settings = CursorAnimationSettings::panea(165, 4, 250_000);
    let beam = |col| CursorVisual {
        position: CellPosition { row: 0, col },
        shape: RenderCursorShape::Beam,
        color: RenderColor::rgb(120, 190, 255),
        text_color: None,
        visible: true,
        thickness_percent: 22,
        corner_radius_px: 0,
        inactive: false,
    };
    let mut runtime = CursorAnimationRuntime::new();
    let mut initial = scene(vec![cell(0, 0, "a")]);
    initial.cursor = Some(beam(1));
    runtime.populate_scene(&mut initial, metrics(), settings);

    let mut moved = scene(vec![cell(0, 0, "a")]);
    moved.cursor = Some(beam(0));
    runtime.populate_scene(&mut moved, metrics(), settings);

    let tilt = moved
        .animations
        .iter()
        .find(|animation| animation.kind == AnimationKind::CursorTilt)
        .expect("leftward motion should produce the Panea tilt overlay");
    let corners = animation_quad_pixels(tilt.quad.expect("tilt requires explicit geometry"));
    assert!(
        corners[0][0] < corners[3][0] && corners[1][0] < corners[2][0],
        "leftward movement should lean the cursor like \\"
    );
    assert!(
        corners[3][0] - corners[0][0] >= metrics().cell_width * 0.85,
        "the reverse lean should be clearly visible"
    );
    let extension = moved
        .animations
        .iter()
        .find(|animation| animation.kind == AnimationKind::CursorElasticExtension)
        .expect("leftward motion should stretch behind the destination cursor");
    let extension_corners = animation_quad_pixels(
        extension
            .quad
            .expect("elastic extension requires explicit geometry"),
    );
    assert!(
        extension_corners
            .iter()
            .any(|corner| { corner[0] > tilt.end_region.x as f32 + metrics().cell_width * 0.5 }),
        "the reverse elastic quad should extend toward the previous cursor cell"
    );
    assert!(extension.start_region.x > extension.end_region.x);
    assert_eq!(tilt.remaining, Some(Duration::from_millis(90)));
    assert_eq!(tilt.start_region, tilt.end_region);
}

#[test]
fn panea_motion_batches_tilt_and_extension_without_legacy_trail() {
    let settings = CursorAnimationSettings::panea(165, 4, 250_000);
    let beam = |col| CursorVisual {
        position: CellPosition { row: 0, col },
        shape: RenderCursorShape::Beam,
        color: RenderColor::rgb(120, 190, 255),
        text_color: None,
        visible: true,
        thickness_percent: 22,
        corner_radius_px: 0,
        inactive: false,
    };
    let mut runtime = CursorAnimationRuntime::new();
    let mut initial = scene(vec![cell(0, 0, "a")]);
    initial.cursor = Some(beam(0));
    runtime.populate_scene(&mut initial, metrics(), settings);

    let mut moved = scene(vec![cell(0, 0, "a")]);
    moved.cursor = Some(beam(1));
    runtime.populate_scene(&mut moved, metrics(), settings);
    let mut fonts = FontSystem::new(font_system::FontConfig::default());
    let mut planner = RenderBatchPlanner::default();
    let batches = planner
        .prepare(&moved, &mut fonts)
        .expect("tilted cursor frame should prepare");

    assert!(
        batches.cursor.is_empty(),
        "the straight cursor must not render beneath the tilted cursor"
    );
    assert_eq!(batches.cursor_effects.quad_count(), 2);
    assert!(batches.cursor_trail.is_empty());
}

#[test]
fn cursor_blink_runtime_is_bounded_and_activity_restores_visibility() {
    let mut runtime = CursorBlinkRuntime::new();
    let started = runtime.phase_started;
    let interval = Duration::from_millis(500);

    assert!(!runtime.update_at(started, true, interval));
    assert!(runtime.visible());
    assert!(runtime.update_at(started + interval, true, interval));
    assert!(!runtime.visible());
    assert!(runtime.record_activity());
    assert!(runtime.visible());
    assert!(!runtime.update(false, interval));
    assert!(runtime.next_frame_after().is_none());
}

#[test]
fn rounded_static_cursor_stays_in_the_cursor_batch() {
    let mut fonts = FontSystem::new(font_system::FontConfig::default());
    let mut planner = RenderBatchPlanner::default();
    let mut test_scene = scene(vec![cell(0, 0, "a")]);
    test_scene.cursor = Some(CursorVisual {
        position: CellPosition { row: 0, col: 0 },
        shape: RenderCursorShape::Block,
        color: RenderColor::rgb(255, 255, 255),
        text_color: None,
        visible: true,
        thickness_percent: 15,
        corner_radius_px: 4,
        inactive: false,
    });

    let batches = planner
        .prepare_full(&test_scene, &mut fonts)
        .expect("rounded cursor should prepare");

    assert_eq!(batches.cursor.quad_count(), 1);
    assert_eq!(batches.instrumentation.draw_call_count, 3);
}

#[test]
fn content_offset_applies_to_bounded_text_context_damage() {
    let mut tracker = DamageTracker::new();
    let mut first = scene(vec![cell(0, 0, "a")]);
    first.content_offset = render_core::RenderOffset { x: 12, y: 8 };
    let initial = tracker.update(&first, metrics());
    assert_eq!(initial[0].x, 0);
    assert_eq!(initial[0].y, 0);

    let mut second = first.clone();
    second.grid.cells[0].text = "b".into();
    let damage = tracker.update(&second, metrics());
    assert_eq!(damage.len(), 1);
    for index in 0..3 {
        assert!(damage_covers(
            &damage,
            RenderRect {
                x: 12 + index * 8,
                y: 8,
                width: 8,
                height: 16
            }
        ));
    }
}

#[test]
fn cursor_animations_damage_only_cursor_regions() {
    let mut runtime = CursorAnimationRuntime::new();
    let settings = CursorAnimationSettings {
        enabled: true,
        tilt: false,
        smooth_movement: true,
        typing_pulse: true,
        typing_stretch: true,
        trail: true,
        trail_delay: Duration::ZERO,
        trail_start_threshold_cells: 0,
        trail_decay_fast: Duration::from_millis(100),
        trail_decay_slow: Duration::from_millis(400),
        blink_easing: false,
        short_lived_glow: true,
        shadow: true,
        fps: 60,
        max_active_animations: 8,
        max_animated_region_pixels: 250_000,
    };
    let mut first = scene(vec![cell(0, 0, "a")]);
    runtime.populate_scene(&mut first, metrics(), settings);

    let mut second = scene(vec![cell(0, 0, "a")]);
    second.cursor = Some(CursorVisual {
        position: CellPosition { row: 1, col: 1 },
        shape: RenderCursorShape::Block,
        color: RenderColor::rgb(255, 255, 255),
        text_color: None,
        visible: true,
        thickness_percent: 15,
        corner_radius_px: 0,
        inactive: false,
    });
    runtime.record_typing();
    runtime.populate_scene(&mut second, metrics(), settings);

    assert!(
        second
            .animations
            .iter()
            .any(|animation| animation.kind == AnimationKind::CursorSmoothMovement)
    );
    assert!(
        second
            .animations
            .iter()
            .any(|animation| animation.kind == AnimationKind::CursorTypingPulse)
    );
    assert!(second.damage_regions.iter().all(|region| {
        region.width <= 32 && region.height <= 48 && region.x <= 12 && region.y <= 20
    }));
    assert!(runtime.needs_frame());
}

#[test]
fn cursor_trail_ignores_single_cell_typing_moves() {
    let mut runtime = CursorAnimationRuntime::new();
    let settings = CursorAnimationSettings {
        enabled: true,
        trail: true,
        trail_delay: Duration::ZERO,
        ..CursorAnimationSettings::default()
    };
    let beam = |col| CursorVisual {
        position: CellPosition { row: 0, col },
        shape: RenderCursorShape::Beam,
        color: RenderColor::rgb(255, 255, 255),
        text_color: None,
        visible: true,
        thickness_percent: 8,
        corner_radius_px: 0,
        inactive: false,
    };
    let mut first = scene(vec![cell(0, 0, "a")]);
    first.cursor = Some(beam(0));
    runtime.populate_scene(&mut first, metrics(), settings);

    let mut second = scene(vec![cell(0, 0, "a")]);
    second.cursor = Some(beam(1));
    runtime.populate_scene(&mut second, metrics(), settings);

    assert!(
        second
            .animations
            .iter()
            .all(|animation| animation.kind != AnimationKind::CursorTrail),
        "single-cell typing should not leave a cursor trail"
    );

    std::thread::sleep(Duration::from_millis(2));
    let mut stable = scene(vec![cell(0, 0, "a")]);
    stable.cursor = Some(beam(1));
    runtime.populate_scene(&mut stable, metrics(), settings);
    assert!(
        stable
            .animations
            .iter()
            .all(|animation| animation.kind != AnimationKind::CursorTrail),
        "beam width must not be mistaken for terminal cell width"
    );
}

#[test]
fn cursor_trail_connects_an_immediate_cursor_across_one_typed_cell() {
    let mut runtime = CursorAnimationRuntime::new();
    let settings = CursorAnimationSettings {
        enabled: true,
        trail: true,
        trail_delay: Duration::ZERO,
        trail_start_threshold_cells: 0,
        ..CursorAnimationSettings::default()
    };
    let beam = |col| CursorVisual {
        position: CellPosition { row: 0, col },
        shape: RenderCursorShape::Beam,
        color: RenderColor::rgb(245, 224, 220),
        text_color: None,
        visible: true,
        thickness_percent: 22,
        corner_radius_px: 0,
        inactive: false,
    };
    let mut initial = scene(vec![cell(0, 0, "a")]);
    initial.cursor = Some(beam(0));
    runtime.populate_scene(&mut initial, metrics(), settings);

    let mut typed = scene(vec![cell(0, 0, "a")]);
    typed.cursor = Some(beam(1));
    runtime.populate_scene(&mut typed, metrics(), settings);
    let trail = typed
        .animations
        .iter()
        .find(|animation| animation.kind == AnimationKind::CursorTrail)
        .expect("one-cell typing must start the configured trail");
    typed.damage_regions = vec![trail.affected_region];

    let mut fonts = FontSystem::new(font_system::FontConfig::default());
    let mut planner = RenderBatchPlanner::default();
    let batches = planner
        .prepare(&typed, &mut fonts)
        .expect("one-cell cursor frame should prepare");
    let target_left = cell_region(CellPosition { row: 0, col: 1 }, metrics()).x as f32;

    assert!(
        !batches.cursor.is_empty(),
        "destination cursor is immediate"
    );
    assert!(
        batches
            .cursor_trail
            .vertices
            .iter()
            .any(|vertex| (vertex.position_px[0] - target_left).abs() < 0.01),
        "elastic trail must remain connected to the destination cursor"
    );
    assert!(
        batches
            .cursor_trail
            .vertices
            .iter()
            .any(|vertex| { vertex.position_px[0] < target_left - metrics().cell_width * 0.5 }),
        "one-cell movement must retain visibly displaced trailing geometry"
    );
}

#[test]
fn cursor_overlay_frame_prepares_without_terminal_cells_fonts_or_atlas_work() {
    let cursor = CursorVisual {
        position: CellPosition { row: 4, col: 12 },
        shape: RenderCursorShape::Beam,
        color: RenderColor::rgb(245, 224, 220),
        text_color: None,
        visible: true,
        thickness_percent: 22,
        corner_radius_px: 0,
        inactive: false,
    };
    let start = cursor_visual_region(
        CursorVisual {
            position: CellPosition { row: 4, col: 11 },
            ..cursor
        },
        metrics(),
    );
    let end = cursor_visual_region(cursor, metrics());
    let overlay = CursorOverlayFrame {
        cursor: Some(cursor),
        animations: vec![AnimationHandle {
            id: 1,
            kind: AnimationKind::CursorTrail,
            affected_region: cursor_animation_region(union_region(start, end)),
            start_region: start,
            end_region: end,
            color: cursor.color,
            quad: Some(animation_quad([
                [start.x as f32, start.y as f32],
                [end.x as f32, end.y as f32],
                [end.x as f32, (end.y + end.height as i32) as f32],
                [start.x as f32, (start.y + start.height as i32) as f32],
            ])),
            elapsed: Duration::ZERO,
            remaining: None,
        }],
        content_offset: render_core::RenderOffset { x: 10, y: 8 },
    };

    let batches = prepare_cursor_overlay(&overlay, metrics());

    assert!(!batches.cursor.is_empty());
    assert!(!batches.cursor_trail.is_empty());
    assert_eq!(batches.draw_call_count(), 2);
    assert!(batches.animated_pixels > 0);
}

#[test]
fn cursor_overlay_reuses_cpu_geometry_storage_between_frames() {
    let overlay = CursorOverlayFrame {
        cursor: Some(cursor(0, 2, RenderCursorShape::Beam, false)),
        animations: vec![AnimationHandle {
            id: 1,
            kind: AnimationKind::CursorTrail,
            affected_region: RenderRect {
                x: 0,
                y: 0,
                width: 24,
                height: 20,
            },
            start_region: RenderRect {
                x: 0,
                y: 0,
                width: 2,
                height: 20,
            },
            end_region: RenderRect {
                x: 16,
                y: 0,
                width: 2,
                height: 20,
            },
            color: RenderColor::rgb(120, 190, 255),
            quad: None,
            elapsed: Duration::from_millis(16),
            remaining: None,
        }],
        content_offset: render_core::RenderOffset::default(),
    };

    let first = prepare_cursor_overlay(&overlay, metrics());
    let trail_ptr = first.cursor_trail.vertices.as_ptr();
    let cursor_ptr = first.cursor.vertices.as_ptr();
    let second = prepare_cursor_overlay_reusing(&overlay, metrics(), Some(first));

    assert_eq!(second.cursor_trail.vertices.as_ptr(), trail_ptr);
    assert_eq!(second.cursor.vertices.as_ptr(), cursor_ptr);
}

#[test]
fn cursor_overlay_frame_is_a_small_value_detached_from_the_terminal_grid() {
    assert!(
        std::mem::size_of::<CursorOverlayFrame>() <= 128,
        "cursor animation frames must not retain or clone terminal grids"
    );
}

#[test]
fn cursor_overlay_fast_path_requires_the_same_cursor_baked_into_the_retained_frame() {
    let cursor = CursorVisual {
        position: CellPosition { row: 2, col: 7 },
        shape: RenderCursorShape::Beam,
        color: RenderColor::rgb(245, 224, 220),
        text_color: None,
        visible: true,
        thickness_percent: 22,
        corner_radius_px: 0,
        inactive: false,
    };
    let frame = CursorOverlayFrame {
        cursor: Some(cursor),
        animations: Vec::new(),
        content_offset: render_core::RenderOffset::default(),
    };

    assert!(can_present_cursor_overlay(true, true, Some(cursor), &frame));
    assert!(!can_present_cursor_overlay(
        false,
        true,
        Some(cursor),
        &frame
    ));
    assert!(!can_present_cursor_overlay(
        true,
        false,
        Some(cursor),
        &frame
    ));
    assert!(!can_present_cursor_overlay(true, true, None, &frame));
    assert!(!can_present_cursor_overlay(
        true,
        true,
        Some(CursorVisual {
            position: CellPosition { row: 2, col: 6 },
            ..cursor
        }),
        &frame,
    ));
}

#[test]
fn cursor_animation_refresh_reuses_scene_without_accumulating_visuals() {
    let mut runtime = CursorAnimationRuntime::new();
    let settings = CursorAnimationSettings {
        enabled: true,
        trail: true,
        trail_delay: Duration::ZERO,
        trail_start_threshold_cells: 0,
        ..CursorAnimationSettings::default()
    };
    let beam = |col| CursorVisual {
        position: CellPosition { row: 0, col },
        shape: RenderCursorShape::Beam,
        color: RenderColor::rgb(245, 224, 220),
        text_color: None,
        visible: true,
        thickness_percent: 22,
        corner_radius_px: 0,
        inactive: false,
    };
    let mut retained = scene(vec![cell(0, 0, "a"), cell(0, 1, "b")]);
    retained.cursor = Some(beam(0));
    runtime.populate_scene(&mut retained, metrics(), settings);
    retained.cursor = Some(beam(1));
    runtime.refresh_retained_scene(&mut retained, metrics(), settings);
    let first_count = retained
        .animations
        .iter()
        .filter(|animation| animation.kind == AnimationKind::CursorTrail)
        .count();

    runtime.refresh_retained_scene(&mut retained, metrics(), settings);

    assert_eq!(retained.grid.cells.len(), 2);
    assert_eq!(first_count, 1);
    assert_eq!(
        retained
            .animations
            .iter()
            .filter(|animation| animation.kind == AnimationKind::CursorTrail)
            .count(),
        1,
        "retained animation frames must replace, not append, cursor visuals"
    );
}

#[test]
fn cursor_trail_uses_cursor_shape_and_local_damage() {
    let mut runtime = CursorAnimationRuntime::new();
    let settings = CursorAnimationSettings {
        enabled: true,
        trail: true,
        trail_delay: Duration::ZERO,
        ..CursorAnimationSettings::default()
    };
    let beam = |col| CursorVisual {
        position: CellPosition { row: 0, col },
        shape: RenderCursorShape::Beam,
        color: RenderColor::rgb(255, 255, 255),
        text_color: None,
        visible: true,
        thickness_percent: 8,
        corner_radius_px: 0,
        inactive: false,
    };
    let mut first = scene(vec![cell(0, 0, "a")]);
    first.cursor = Some(beam(0));
    runtime.populate_scene(&mut first, metrics(), settings);

    let mut second = scene(vec![cell(0, 0, "a")]);
    second.cursor = Some(beam(4));
    runtime.populate_scene(&mut second, metrics(), settings);

    let trail = second
        .animations
        .iter()
        .find(|animation| animation.kind == AnimationKind::CursorTrail)
        .expect("large cursor jump should create a trail");
    assert_eq!(trail.start_region.width, 1);
    assert_eq!(trail.end_region.width, 1);
    let quad = trail
        .quad
        .expect("persistent trail should expose its GPU quad");
    let bounds = quad_bounds(animation_quad_pixels(quad));
    assert!(trail.affected_region.x <= bounds.x);
    assert!(
        trail.affected_region.x + trail.affected_region.width as i32
            >= bounds.x + bounds.width as i32
    );
    assert_eq!(
        trail.affected_region,
        cursor_animation_region(union_region(trail.start_region, trail.end_region)),
        "damage must cover exactly the local trail bridge and its clear margin"
    );
}

#[test]
fn cursor_trail_moves_leading_corners_faster_than_trailing_corners() {
    let start = RenderRect {
        x: 0,
        y: 0,
        width: 2,
        height: 16,
    };
    let end = RenderRect {
        x: 80,
        y: 32,
        width: 2,
        height: 16,
    };

    let initial = cursor_trail_quad(start, end, Duration::ZERO);
    assert_eq!(initial, rect_quad(start));

    let moving = cursor_trail_quad(start, end, Duration::from_millis(100));
    let target = rect_quad(end);
    let distance = |point: [f32; 2], goal: [f32; 2]| (point[0] - goal[0]).hypot(point[1] - goal[1]);
    assert!(
        distance(moving[2], target[2]) < distance(moving[0], target[0]),
        "the leading corner should catch the target before the trailing corner"
    );
    assert_ne!(moving[0][0], moving[3][0]);

    let settled = cursor_trail_quad(start, end, Duration::from_millis(400));
    assert!(
        settled
            .iter()
            .zip(target)
            .all(|(point, goal)| distance(*point, goal) < 0.2),
        "all corners should settle within a subpixel after the slow decay"
    );
}

#[test]
fn cursor_trail_preserves_inflight_geometry_when_retargeted() {
    let settings = CursorAnimationSettings {
        enabled: true,
        trail: true,
        trail_start_threshold_cells: 0,
        ..CursorAnimationSettings::default()
    };
    let start = RenderRect {
        x: 0,
        y: 0,
        width: 2,
        height: 16,
    };
    let first_target = RenderRect { x: 8, ..start };
    let second_target = RenderRect { x: 16, ..start };
    let mut trail = PersistentCursorTrail::default();
    trail.retarget(start, RenderColor::rgb(120, 190, 255), [8, 16], settings);
    trail.retarget(
        first_target,
        RenderColor::rgb(120, 190, 255),
        [8, 16],
        settings,
    );
    trail.advance(Duration::from_millis(16), settings);
    let inflight = trail.corners;

    trail.retarget(
        second_target,
        RenderColor::rgb(120, 190, 255),
        [8, 16],
        settings,
    );

    assert_eq!(trail.corners, inflight);
    assert!(trail.needs_frame());
}

#[test]
fn cursor_trail_waits_for_a_stable_target_before_animating() {
    let settings = CursorAnimationSettings {
        enabled: true,
        trail: true,
        trail_start_threshold_cells: 2,
        ..CursorAnimationSettings::default()
    };
    let block = |col| CursorVisual {
        position: CellPosition { row: 0, col },
        shape: RenderCursorShape::Block,
        color: RenderColor::rgb(255, 80, 120),
        text_color: None,
        visible: true,
        thickness_percent: 100,
        corner_radius_px: 0,
        inactive: false,
    };
    let mut runtime = CursorAnimationRuntime::new();

    let mut initial = scene(vec![cell(0, 0, "a")]);
    initial.cursor = Some(block(0));
    runtime.populate_scene(&mut initial, metrics(), settings);

    let mut first_observation = scene(vec![cell(0, 0, "a")]);
    first_observation.cursor = Some(block(4));
    runtime.populate_scene(&mut first_observation, metrics(), settings);
    assert!(
        first_observation
            .animations
            .iter()
            .all(|animation| animation.kind != AnimationKind::CursorTrail),
        "a newly observed cursor target must not animate immediately"
    );

    std::thread::sleep(Duration::from_millis(2));
    let mut stable_observation = scene(vec![cell(0, 0, "a")]);
    stable_observation.cursor = Some(block(4));
    runtime.populate_scene(&mut stable_observation, metrics(), settings);
    assert!(
        stable_observation
            .animations
            .iter()
            .any(|animation| animation.kind == AnimationKind::CursorTrail),
        "a stable cursor jump larger than the threshold should animate"
    );
}

#[test]
fn persistent_cursor_trail_converges_without_fixed_duration_redraws() {
    let settings = CursorAnimationSettings {
        enabled: true,
        trail: true,
        trail_start_threshold_cells: 0,
        ..CursorAnimationSettings::default()
    };
    let start = RenderRect {
        x: 0,
        y: 0,
        width: 2,
        height: 16,
    };
    let target = RenderRect { x: 8, ..start };
    let mut trail = PersistentCursorTrail::default();
    trail.retarget(start, RenderColor::rgb(120, 190, 255), [8, 16], settings);
    trail.retarget(target, RenderColor::rgb(120, 190, 255), [8, 16], settings);

    for _ in 0..50 {
        trail.advance(Duration::from_millis(8), settings);
        if !trail.needs_frame() {
            break;
        }
    }

    assert!(!trail.needs_frame());
    assert_eq!(trail.corners, rect_quad(target));
}

#[test]
fn cursor_trail_batches_one_clipped_deformed_gpu_polygon() {
    let start = RenderRect {
        x: 0,
        y: 0,
        width: 2,
        height: 16,
    };
    let end = RenderRect {
        x: 80,
        y: 32,
        width: 2,
        height: 16,
    };
    let affected_region = cursor_animation_region(union_region(start, end));
    let expected = cursor_trail_quad(start, end, Duration::from_millis(100));
    let animation = AnimationHandle {
        id: 1,
        kind: AnimationKind::CursorTrail,
        affected_region,
        start_region: start,
        end_region: end,
        color: RenderColor::rgb(120, 190, 255),
        quad: Some(animation_quad(expected)),
        elapsed: Duration::from_millis(100),
        remaining: Some(Duration::from_millis(300)),
    };
    let mut effects = QuadBatch::new(QuadBatchKind::Decoration);
    let mut trail = QuadBatch::new(QuadBatchKind::CursorTrail);
    let offset = render_core::RenderOffset { x: 12, y: 8 };

    push_animation_quads(
        &mut effects,
        &mut trail,
        &[animation],
        &[offset_region(affected_region, offset)],
        offset,
        false,
    );

    assert!(effects.is_empty());
    assert!(!trail.is_empty());
    let target_edge = (end.x + offset.x) as f32;
    assert!(
        trail
            .vertices
            .iter()
            .all(|vertex| vertex.position_px[0] <= target_edge),
        "the GPU trail must stop before the active cursor"
    );
}

#[test]
fn cursor_trail_batches_separately_from_generic_decorations() {
    let mut fonts = FontSystem::new(font_system::FontConfig::default());
    let mut planner = RenderBatchPlanner::default();
    let start = RenderRect {
        x: 0,
        y: 0,
        width: 2,
        height: 16,
    };
    let end = RenderRect { x: 24, ..start };
    let affected_region = cursor_animation_region(union_region(start, end));
    let mut test_scene = scene(Vec::new());
    test_scene.cursor = None;
    test_scene.animations = vec![AnimationHandle {
        id: 1,
        kind: AnimationKind::CursorTrail,
        affected_region,
        start_region: start,
        end_region: end,
        color: RenderColor::rgb(120, 190, 255),
        quad: Some(animation_quad([
            [start.x as f32, start.y as f32],
            [(end.x + end.width as i32) as f32, end.y as f32],
            [
                (end.x + end.width as i32) as f32,
                (end.y + end.height as i32) as f32,
            ],
            [start.x as f32, (start.y + start.height as i32) as f32],
        ])),
        elapsed: Duration::ZERO,
        remaining: None,
    }];
    test_scene.damage_regions = vec![affected_region];

    let batches = planner.prepare(&test_scene, &mut fonts).unwrap();

    assert!(!batches.cursor_trail.is_empty());
    assert!(
        batches.decorations.is_empty(),
        "cursor motion must not share the generic overlay batch"
    );
}

#[test]
fn cursor_effects_are_not_baked_into_static_terminal_decorations() {
    let mut fonts = FontSystem::new(font_system::FontConfig::default());
    let mut planner = RenderBatchPlanner::default();
    let bounds = RenderRect {
        x: 16,
        y: 16,
        width: 8,
        height: 16,
    };
    let mut test_scene = scene(Vec::new());
    test_scene.cursor = None;
    test_scene.animations = vec![AnimationHandle {
        id: 1,
        kind: AnimationKind::CursorTypingPulse,
        affected_region: cursor_animation_region(bounds),
        start_region: bounds,
        end_region: bounds,
        color: RenderColor::rgb(120, 190, 255),
        quad: None,
        elapsed: Duration::from_millis(20),
        remaining: Some(Duration::from_millis(120)),
    }];
    test_scene.damage_regions = vec![cursor_animation_region(bounds)];

    let batches = planner.prepare(&test_scene, &mut fonts).unwrap();

    assert!(batches.decorations.is_empty());
    assert!(!batches.cursor_effects.is_empty());
}

#[test]
fn partial_cell_redraw_cannot_overwrite_a_divider_outside_the_scissor() {
    let mut fonts = FontSystem::new(font_system::FontConfig::default());
    let metrics = fonts.cell_metrics().expect("font metrics");
    let mut planner = RenderBatchPlanner::default();
    let mut test_scene = scene_without_cursor(vec![cell(1, 0, "a")]);
    let cell_bounds = cell_region(CellPosition { row: 1, col: 0 }, metrics);
    test_scene.decorations.push(RenderDecoration {
        bounds: RenderRect {
            x: 0,
            y: cell_bounds.y,
            width: 32,
            height: 1,
        },
        color: RenderColor::rgb(80, 150, 255),
        border_color: None,
    });
    test_scene.damage_regions = vec![RenderRect {
        x: cell_bounds.x,
        y: cell_bounds.y + 1,
        width: cell_bounds.width,
        height: 1,
    }];

    let batches = planner
        .prepare(&test_scene, &mut fonts)
        .expect("partial cell frame should prepare");

    assert_eq!(batches.background.quad_count(), 1);
    assert_eq!(
        batches.damage_regions, test_scene.damage_regions,
        "damage must remain pixel-exact for the GPU scissor"
    );
    assert_eq!(
        batches.decorations.quad_count(),
        0,
        "the divider lies outside damage and the scissor protects it from the full cell quad"
    );
    assert_eq!(
        damage_scissor_rects(&batches.damage_regions, 32, 32).len(),
        1,
        "the partial redraw must encode exactly one scissor"
    );
}

#[test]
fn cursor_trail_geometry_does_not_cover_the_active_cursor() {
    for (start_x, end_x) in [(0, 24), (24, 0)] {
        let mut fonts = FontSystem::new(font_system::FontConfig::default());
        let mut planner = RenderBatchPlanner::default();
        let start = RenderRect {
            x: start_x,
            y: 0,
            width: 2,
            height: 16,
        };
        let end = RenderRect { x: end_x, ..start };
        let affected_region = cursor_animation_region(union_region(start, end));
        let left = start.x.min(end.x) as f32;
        let right = (start.x.max(end.x) + end.width as i32) as f32;
        let mut test_scene = scene(Vec::new());
        test_scene.cursor = None;
        test_scene.animations = vec![AnimationHandle {
            id: 1,
            kind: AnimationKind::CursorTrail,
            affected_region,
            start_region: start,
            end_region: end,
            color: RenderColor::rgb(120, 190, 255),
            quad: Some(animation_quad([
                [left, 0.0],
                [right, 0.0],
                [right, 16.0],
                [left, 16.0],
            ])),
            elapsed: Duration::ZERO,
            remaining: None,
        }];
        test_scene.damage_regions = vec![affected_region];

        let batches = planner.prepare(&test_scene, &mut fonts).unwrap();
        assert!(!batches.cursor_trail.is_empty());
        if end.x > start.x {
            assert!(
                batches
                    .cursor_trail
                    .vertices
                    .iter()
                    .all(|vertex| vertex.position_px[0] <= end.x as f32)
            );
        } else {
            let active_cursor_right = (end.x + end.width as i32) as f32;
            assert!(
                batches
                    .cursor_trail
                    .vertices
                    .iter()
                    .all(|vertex| vertex.position_px[0] >= active_cursor_right)
            );
        }
    }
}

#[test]
fn cursor_trail_geometry_stays_attached_to_the_active_cursor_edge() {
    let settings = CursorAnimationSettings {
        enabled: true,
        trail: true,
        trail_start_threshold_cells: 0,
        trail_decay_fast: Duration::from_millis(100),
        trail_decay_slow: Duration::from_millis(400),
        ..CursorAnimationSettings::default()
    };
    for (start_x, end_x) in [(0, 80), (80, 0)] {
        let start = RenderRect {
            x: start_x,
            y: 0,
            width: 2,
            height: 16,
        };
        let end = RenderRect { x: end_x, ..start };
        let mut persistent = PersistentCursorTrail::default();
        persistent.retarget(start, RenderColor::rgb(120, 190, 255), [8, 16], settings);
        persistent.retarget(end, RenderColor::rgb(120, 190, 255), [8, 16], settings);
        persistent.advance(Duration::from_millis(8), settings);
        let visual = persistent
            .visual(settings)
            .expect("trail should remain active");
        assert!(
            damage_covers(
                &[visual.affected_region],
                union_region(visual.start_region, visual.end_region)
            ),
            "declared trail damage must cover every anchored GPU pixel"
        );
        let mut batch = QuadBatch::new(QuadBatchKind::CursorTrail);

        push_clipped_cursor_trail(
            &mut batch,
            animation_quad_pixels(visual.quad.expect("trail quad")),
            visual.start_region,
            visual.end_region,
            visual.color,
        );

        let active_edge = if end.x > start.x {
            end.x as f32
        } else {
            (end.x + end.width as i32) as f32
        };
        assert!(
            batch
                .vertices
                .iter()
                .any(|vertex| (vertex.position_px[0] - active_edge).abs() < 0.01),
            "the elastic trail must remain connected to the immediate caret"
        );
    }
}

#[test]
fn cursor_trail_uses_the_cursor_opacity() {
    let animation = AnimationHandle {
        id: 1,
        kind: AnimationKind::CursorTrail,
        affected_region: RenderRect {
            x: 0,
            y: 0,
            width: 10,
            height: 20,
        },
        start_region: RenderRect {
            x: 0,
            y: 0,
            width: 2,
            height: 20,
        },
        end_region: RenderRect {
            x: 8,
            y: 0,
            width: 2,
            height: 20,
        },
        color: RenderColor {
            red: 245,
            green: 224,
            blue: 220,
            alpha: 255,
        },
        quad: None,
        elapsed: Duration::ZERO,
        remaining: None,
    };

    assert_eq!(animation_color(animation).alpha, 255);
}

#[test]
fn cursor_trail_damage_clears_previous_and_current_geometry() {
    let previous = RenderRect {
        x: 0,
        y: 0,
        width: 2,
        height: 16,
    };
    let current = RenderRect { x: 6, ..previous };
    let animation = |rect| AnimationHandle {
        id: 1,
        kind: AnimationKind::CursorTrail,
        affected_region: cursor_animation_region(rect),
        start_region: rect,
        end_region: current,
        color: RenderColor::rgb(120, 190, 255),
        quad: Some(animation_quad(rect_quad(rect))),
        elapsed: Duration::ZERO,
        remaining: None,
    };
    let mut tracker = DamageTracker::new();
    let mut first = scene(vec![cell(0, 0, "a")]);
    first.animations = vec![animation(previous)];
    let _ = tracker.update(&first, metrics());

    let mut second = first.clone();
    second.animations = vec![animation(current)];
    let damage = tracker.update(&second, metrics());

    assert!(damage_covers(&damage, cursor_animation_region(previous)));
    assert!(damage_covers(&damage, cursor_animation_region(current)));
}

#[test]
fn cursor_animation_quads_are_batched_separately_from_cells() {
    let mut fonts = FontSystem::new(font_system::FontConfig::default());
    let mut planner = RenderBatchPlanner::default();
    let mut test_scene = scene(vec![cell(0, 0, "a")]);
    let region = RenderRect {
        x: 0,
        y: 0,
        width: 24,
        height: 24,
    };
    test_scene.animations = vec![AnimationHandle {
        id: 1,
        kind: AnimationKind::CursorGlow,
        affected_region: region,
        start_region: region,
        end_region: region,
        color: RenderColor::rgb(120, 190, 255),
        quad: None,
        elapsed: Duration::from_millis(20),
        remaining: Some(Duration::from_millis(100)),
    }];
    test_scene.damage_regions = vec![region];

    let Ok(batches) = planner.prepare(&test_scene, &mut fonts) else {
        return;
    };

    assert!(batches.cursor_effects.quad_count() >= 1);
    assert!(
        batches.decorations.is_empty(),
        "cursor animations must remain isolated from static terminal decorations"
    );
    assert_eq!(batches.instrumentation.animated_region_count, 1);
}

#[test]
fn animated_cursor_image_header_decode_is_bounded() {
    let gif = [
        b"GIF89a".as_slice(),
        &[2, 0, 3, 0],
        &[0x21, 0xF9, 0x04, 0, 0, 0, 0, 0],
        &[0x21, 0xF9, 0x04, 0, 0, 0, 0, 0],
    ]
    .concat();

    let decoded = decode_cursor_image_header(&gif).expect("valid GIF header");

    assert_eq!(decoded, (2, 3, 2));
}

#[test]
fn panea_vector_cursor_format_is_bounded_and_batches_primitives() {
    let bytes = br#"{
            "version": 1,
            "primitives": [
                {"x": 0, "y": 0, "width": 250, "height": 1000, "corner_radius": 0},
                {"x": 250, "y": 400, "width": 750, "height": 200, "corner_radius": 0,
                 "color": [10, 20, 30, 255]}
            ]
        }"#;
    let primitives = decode_cursor_vector(bytes).expect("valid vector cursor");
    assert_eq!(primitives.len(), 2);
    let visual = CursorVectorVisual {
        asset: Arc::new(CursorVectorAsset {
            id: 1,
            primitives: primitives.into(),
        }),
        bounds: RenderRect {
            x: 10,
            y: 20,
            width: 20,
            height: 40,
        },
        color: RenderColor::rgb(255, 255, 255),
        opacity: 255,
    };
    let mut batch = QuadBatch::new(QuadBatchKind::Cursor);
    push_cursor_vector_quads(
        &mut batch,
        &visual,
        &[RenderRect {
            x: 0,
            y: 0,
            width: 100,
            height: 100,
        }],
        render_core::RenderOffset::default(),
    );
    assert_eq!(batch.quad_count(), 2);
}

#[test]
fn panea_vector_cursor_rejects_unknown_or_out_of_bounds_data() {
    let unknown = br#"{"version":1,"unknown":true,"primitives":[]}"#;
    assert!(decode_cursor_vector(unknown).is_err());
    let outside = br#"{
            "version": 1,
            "primitives": [{"x": 900, "y": 0, "width": 200, "height": 10}]
        }"#;
    assert!(decode_cursor_vector(outside).is_err());
}

#[test]
fn animated_gif_frames_decode_to_cached_rgba_with_limits() {
    let mut encoded = Vec::new();
    {
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut encoded);
        for color in [[255, 0, 0, 255], [0, 255, 0, 180]] {
            let image = image::RgbaImage::from_pixel(2, 3, image::Rgba(color));
            encoder
                .encode_frame(image::Frame::new(image))
                .expect("test GIF frame should encode");
        }
    }

    let decoded =
        decode_cursor_image_frames(&encoded, 32, 8, 4096).expect("test GIF should decode");
    assert_eq!((decoded.width, decoded.height), (2, 3));
    assert_eq!(decoded.frames.len(), 2);
    assert_eq!(decoded.frames[0].pixels.len(), 24);
    assert!(decode_cursor_image_frames(&encoded, 32, 1, 4096).is_err());
    assert!(decode_cursor_image_frames(&encoded, 1, 8, 4096).is_err());
}

#[test]
fn static_png_cursor_decodes_as_one_frame() {
    let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        3,
        2,
        image::Rgba([12, 34, 56, 200]),
    ));
    let mut encoded = Cursor::new(Vec::new());
    image
        .write_to(&mut encoded, image::ImageFormat::Png)
        .expect("test PNG should encode");

    let decoded =
        decode_cursor_image_frames(encoded.get_ref(), 32, 8, 4096).expect("test PNG should decode");
    assert_eq!((decoded.width, decoded.height), (3, 2));
    assert_eq!(decoded.frames.len(), 1);
    assert_eq!(decoded.frames[0].pixels[3], 200);
}

#[test]
fn image_cursor_is_one_batched_quad_and_suppresses_static_cursor() {
    let mut fonts = FontSystem::new(font_system::FontConfig::default());
    let mut planner = RenderBatchPlanner::default();
    let mut test_scene = scene(vec![cell(0, 0, "a")]);
    let asset = test_cursor_image_asset(2);
    test_scene.cursor_image = Some(CursorImageVisual {
        asset: Arc::clone(&asset),
        frame_index: 1,
        bounds: RenderRect {
            x: 0,
            y: 0,
            width: 8,
            height: 16,
        },
        opacity: 220,
    });

    let batches = planner
        .prepare_full(&test_scene, &mut fonts)
        .expect("cursor image scene should prepare");
    assert_eq!(batches.cursor.quad_count(), 0);
    assert_eq!(batches.cursor_image.quad_count(), 1);
    assert_eq!(
        batches.cursor_image_asset.as_ref().map(|asset| asset.id),
        Some(asset.id)
    );
}

#[test]
fn image_cursor_frame_changes_damage_only_its_bounds() {
    let asset = test_cursor_image_asset(2);
    let bounds = RenderRect {
        x: 8,
        y: 16,
        width: 8,
        height: 16,
    };
    let mut first = scene(vec![cell(0, 0, "a")]);
    first.cursor_image = Some(CursorImageVisual {
        asset: Arc::clone(&asset),
        frame_index: 0,
        bounds,
        opacity: 255,
    });
    let mut tracker = DamageTracker::new();
    let _ = tracker.update(&first, metrics());

    let mut second = first.clone();
    second
        .cursor_image
        .as_mut()
        .expect("cursor image")
        .frame_index = 1;
    let damage = tracker.update(&second, metrics());
    assert_eq!(damage, vec![bounds]);
}

#[test]
fn vector_cursor_is_batched_and_damages_only_old_and_new_bounds() {
    let asset = Arc::new(CursorVectorAsset {
        id: 9,
        primitives: vec![CursorVectorPrimitive {
            x: 0,
            y: 0,
            width: 1000,
            height: 1000,
            corner_radius: 0,
            color: None,
        }]
        .into(),
    });
    let first_bounds = RenderRect {
        x: 8,
        y: 16,
        width: 8,
        height: 16,
    };
    let mut first = scene(vec![cell(0, 0, "a")]);
    first.cursor_vector = Some(CursorVectorVisual {
        asset: Arc::clone(&asset),
        bounds: first_bounds,
        color: RenderColor::rgb(40, 80, 120),
        opacity: 255,
    });
    let mut fonts = FontSystem::new(font_system::FontConfig::default());
    let batches = RenderBatchPlanner::default()
        .prepare_full(&first, &mut fonts)
        .expect("vector cursor scene should prepare");
    assert!(batches.cursor.quad_count() >= 1);
    assert!(batches.cursor_image.is_empty());

    let mut tracker = DamageTracker::new();
    let _ = tracker.update(&first, metrics());
    let mut second = first.clone();
    let second_bounds = RenderRect {
        x: 12,
        ..first_bounds
    };
    second.cursor_vector.as_mut().expect("cursor vector").bounds = second_bounds;
    let damage = tracker.update(&second, metrics());
    assert_eq!(
        damage,
        vec![RenderRect {
            x: 8,
            y: 16,
            width: 12,
            height: 16,
        }]
    );
}

#[test]
fn image_cursor_runtime_schedules_only_visible_multiframe_assets() {
    let asset = test_cursor_image_asset(2);
    let image = DecodedCursorImage {
        path: PathBuf::from("cursor.gif"),
        width: 2,
        height: 2,
        frame_count: 2,
        fps: 24,
        size_kb: 1,
        warnings: Vec::new(),
        asset,
    };
    let mut runtime = AnimatedCursorImageRuntime::new();
    runtime.set_image(&image);
    let mut test_scene = scene(vec![cell(0, 0, "a")]);
    runtime.populate_scene(&mut test_scene, metrics());
    assert!(test_scene.cursor_image.is_some());
    assert_eq!(
        runtime.next_frame_after(),
        Some(Duration::from_micros(1_000_000 / 24))
    );

    runtime.clear();
    assert!(runtime.next_frame_after().is_none());
}

#[test]
fn cursor_animation_runtime_enforces_active_and_pixel_budgets() {
    let mut runtime = CursorAnimationRuntime::new();
    let settings = CursorAnimationSettings {
        enabled: true,
        smooth_movement: true,
        typing_pulse: true,
        typing_stretch: true,
        max_active_animations: 1,
        max_animated_region_pixels: 1024,
        ..CursorAnimationSettings::default()
    };
    let mut first = scene(vec![cell(0, 0, "a")]);
    runtime.populate_scene(&mut first, metrics(), settings);
    let mut second = first.clone();
    second.cursor.as_mut().expect("cursor").position = CellPosition { row: 1, col: 1 };
    runtime.record_typing();
    runtime.populate_scene(&mut second, metrics(), settings);
    assert_eq!(second.animations.len(), 1);

    let mut blocked = CursorAnimationRuntime::new();
    let mut blocked_scene = scene(vec![cell(0, 0, "a")]);
    blocked.record_typing();
    blocked.populate_scene(
        &mut blocked_scene,
        metrics(),
        CursorAnimationSettings {
            max_animated_region_pixels: 1,
            ..settings
        },
    );
    assert!(blocked_scene.animations.is_empty());
    assert!(blocked.next_frame_after(settings).is_none());
}

fn test_cursor_image_asset(frame_count: usize) -> Arc<CursorImageAsset> {
    let frames = (0..frame_count)
        .map(|index| CursorImageFrame {
            pixels: [index as u8, 40, 80, 255].repeat(4).into(),
        })
        .collect::<Vec<_>>();
    Arc::new(CursorImageAsset {
        id: 42,
        width: 2,
        height: 2,
        frames: frames.into(),
    })
}

#[test]
fn screenshot_fixtures_cover_required_categories() {
    let fixtures = screenshot_fixtures();
    let names = fixtures
        .iter()
        .map(|fixture| fixture.name)
        .collect::<std::collections::HashSet<_>>();

    for expected in [
        "ascii-grid",
        "truecolor-grid",
        "text-styles",
        "cjk-wide",
        "emoji",
        "cursor-states",
        "cursor-image",
        "selection-states",
        "prompt-decorations",
        "command-blocks",
        "multiple-panes",
        "transparency-opacity",
        "fullscreen-chrome-hidden",
        "fullscreen-chrome-half",
        "fullscreen-chrome-visible",
        "fullscreen-chrome-close-hover",
        "fullscreen-chrome-no-controls",
    ] {
        assert!(names.contains(expected), "missing fixture {expected}");
    }
}

#[test]
fn cpu_rasterizer_draws_labels_for_behind_text_overlays() {
    let with_label = transparency_scene();
    let mut without_label = with_label.clone();
    without_label.semantic_overlays[0].label = None;
    let mut fonts = FontSystem::new(font_system::FontConfig::default());
    let mut rasterizer = TerminalRasterizer::default();

    let labeled = rasterizer
        .rasterize(&with_label, &mut fonts)
        .expect("rasterize labeled overlay");
    let unlabeled = rasterizer
        .rasterize(&without_label, &mut fonts)
        .expect("rasterize unlabeled overlay");

    assert!(
        labeled
            .pixels
            .iter()
            .zip(&unlabeled.pixels)
            .any(|(labeled, unlabeled)| labeled != unlabeled),
        "an overlay label must remain visible when its geometry is behind terminal text"
    );
}

#[test]
fn fullscreen_chrome_fixtures_preserve_terminal_layout_below_chrome() {
    let fixtures = screenshot_fixtures();
    let hidden = fixtures
        .iter()
        .find(|fixture| fixture.name == "fullscreen-chrome-hidden")
        .expect("hidden fullscreen chrome fixture");
    let visible = fixtures
        .iter()
        .find(|fixture| fixture.name == "fullscreen-chrome-visible")
        .expect("visible fullscreen chrome fixture");

    assert_eq!(hidden.scene.grid, visible.scene.grid);
    assert_eq!(hidden.scene.content_offset, visible.scene.content_offset);

    let hidden = capture_screenshot_fixture(hidden.name).expect("hidden fixture capture");
    let visible = capture_screenshot_fixture(visible.name).expect("visible fixture capture");
    assert_eq!(hidden.frame.width, visible.frame.width);
    assert_eq!(hidden.frame.height, visible.frame.height);

    let chrome_height = 36_u32.min(hidden.frame.height);
    let first_unchanged_byte = (chrome_height * hidden.frame.width * u32::from(4_u8)) as usize;
    assert_eq!(
        &hidden.frame.pixels[first_unchanged_byte..],
        &visible.frame.pixels[first_unchanged_byte..],
        "fullscreen chrome must not reflow or alter terminal pixels outside its overlay bounds"
    );
}

#[test]
fn cpu_frame_ppm_round_trip_preserves_pixels() {
    let frame = CpuFrame {
        width: 2,
        height: 1,
        pixels: vec![1, 2, 3, 255, 40, 50, 60, 255],
    };

    let decoded = CpuFrame::decode_ppm(&frame.encode_ppm()).expect("valid PPM");

    assert_eq!(decoded, frame);
}

#[test]
fn screenshot_diff_separates_exact_antialias_and_layout_changes() {
    let base = CpuFrame {
        width: 20,
        height: 20,
        pixels: [10, 20, 30, 255].repeat(400),
    };
    let exact = compare_screenshots(&base, &base, ScreenshotTolerance::default());
    assert_eq!(exact.kind, ScreenshotDiffKind::Exact);
    assert!(exact.passed);

    let mut small = base.clone();
    small.pixels[0] = 11;
    let antialias = compare_screenshots(&base, &small, ScreenshotTolerance::default());
    assert_eq!(
        antialias.kind,
        ScreenshotDiffKind::AntialiasingWithinTolerance
    );
    assert!(antialias.passed);

    let mut layout = base.clone();
    for pixel in layout.pixels.chunks_exact_mut(4).take(30) {
        pixel[0] = 240;
        pixel[1] = 240;
        pixel[2] = 240;
    }
    let layout_diff = compare_screenshots(&base, &layout, ScreenshotTolerance::default());
    assert_eq!(layout_diff.kind, ScreenshotDiffKind::TextLayoutFailure);
    assert!(!layout_diff.passed);
}

#[test]
fn surface_overlay_damage_uses_surface_coordinates() {
    let mut tracker = DamageTracker::new();
    let mut first = scene(vec![cell(0, 0, "a")]);
    first.content_offset = render_core::RenderOffset { x: 30, y: 20 };
    first.surface_overlays.push(OverlayPrimitive {
        kind: OverlayKind::WindowChrome,
        bounds: RenderRect {
            x: 0,
            y: 0,
            width: 800,
            height: 36,
        },
        color: RenderColor::rgb(20, 20, 20),
        border_color: None,
        border_width_px: 0,
        corner_radius_px: 0,
        z_index: 100,
        label: None,
        label_color: None,
    });
    let initial = tracker.update(&first, metrics());
    assert!(initial.iter().any(|region| region.width >= 800));

    let mut hidden = first.clone();
    hidden.surface_overlays.clear();
    let damage = tracker.update(&hidden, metrics());
    assert!(damage.iter().any(|region| {
        region.x == 0 && region.y == 0 && region.width >= 800 && region.height >= 36
    }));
}

fn window_chrome_visual() -> render_core::WindowChromeVisual {
    use render_core::{WindowChromeControlKind, WindowChromeControlVisual};

    render_core::WindowChromeVisual {
        bounds: RenderRect {
            x: 0,
            y: 0,
            width: 800,
            height: 36,
        },
        opacity: u16::MAX,
        title: "Panea".to_owned(),
        show_logo: true,
        controls: vec![
            WindowChromeControlVisual {
                kind: WindowChromeControlKind::Minimize,
                bounds: RenderRect {
                    x: 656,
                    y: 0,
                    width: 48,
                    height: 36,
                },
                hovered: false,
                pressed: false,
            },
            WindowChromeControlVisual {
                kind: WindowChromeControlKind::LeaveFullscreen,
                bounds: RenderRect {
                    x: 704,
                    y: 0,
                    width: 48,
                    height: 36,
                },
                hovered: false,
                pressed: false,
            },
            WindowChromeControlVisual {
                kind: WindowChromeControlKind::Close,
                bounds: RenderRect {
                    x: 752,
                    y: 0,
                    width: 48,
                    height: 36,
                },
                hovered: true,
                pressed: false,
            },
        ],
    }
}

fn has_vertex_strictly_inside(batch: &QuadBatch, bounds: RenderRect) -> bool {
    let x0 = bounds.x as f32;
    let y0 = bounds.y as f32;
    let x1 = x0 + bounds.width as f32;
    let y1 = y0 + bounds.height as f32;
    batch.vertices.iter().any(|vertex| {
        vertex.position_px[0] > x0
            && vertex.position_px[0] < x1
            && vertex.position_px[1] > y0
            && vertex.position_px[1] < y1
    })
}

#[test]
fn window_chrome_is_one_batched_overlay_with_logo_title_and_controls() {
    let mut fonts = FontSystem::new(font_system::FontConfig::default());
    let mut planner = RenderBatchPlanner::default();
    let visual = window_chrome_visual();
    let mut test_scene = scene_without_cursor(Vec::new());
    test_scene.window_chrome = Some(visual.clone());
    test_scene.damage_regions = vec![visual.bounds];

    let first = planner
        .prepare(&test_scene, &mut fonts)
        .expect("window chrome should prepare");
    let second = planner
        .prepare(&test_scene, &mut fonts)
        .expect("unchanged window chrome should prepare");

    assert_eq!(first.window_chrome.kind, QuadBatchKind::Decoration);
    assert!(!first.window_chrome.is_empty());
    assert!(first.overlay_glyphs.glyph_count > 0, "title must be shaped");
    assert_eq!(first.logo_glyphs.glyph_count, 1);
    assert_eq!(
        first
            .atlas_uploads
            .iter()
            .filter(|upload| upload.key == AtlasCacheKey::PaneaLogo)
            .count(),
        1,
        "the built-in logo must be uploaded once"
    );
    assert_eq!(
        second
            .atlas_uploads
            .iter()
            .filter(|upload| upload.key == AtlasCacheKey::PaneaLogo)
            .count(),
        0,
        "the cached logo must not be uploaded again"
    );
    assert_eq!(first.overlay_glyphs, second.overlay_glyphs);
    assert_eq!(first.logo_glyphs, second.logo_glyphs);
    for control in &visual.controls {
        assert!(
            has_vertex_strictly_inside(&first.window_chrome, control.bounds),
            "{:?} control must contribute batched geometry",
            control.kind
        );
    }
    assert!(
        first.window_chrome.vertices.iter().any(|vertex| {
            vertex.color[0] > 0.7 && vertex.color[1] < 0.3 && vertex.color[2] < 0.3
        })
    );
}

#[test]
fn absent_window_chrome_has_zero_batch_and_upload_cost() {
    let mut fonts = FontSystem::new(font_system::FontConfig::default());
    let mut planner = RenderBatchPlanner::default();
    let test_scene = scene_without_cursor(Vec::new());

    let batches = planner
        .prepare(&test_scene, &mut fonts)
        .expect("empty scene should prepare");

    assert!(batches.window_chrome.is_empty());
    assert!(
        !batches
            .atlas_uploads
            .iter()
            .any(|upload| upload.key == AtlasCacheKey::PaneaLogo)
    );
}

#[test]
fn window_chrome_is_present_in_renderer_independent_cpu_snapshots() {
    let mut fonts = FontSystem::new(font_system::FontConfig::default());
    let mut rasterizer = TerminalRasterizer::default();
    let mut test_scene = scene_without_cursor(Vec::new());
    let visual = window_chrome_visual();
    test_scene.window_chrome = Some(visual.clone());

    let frame = rasterizer
        .rasterize(&test_scene, &mut fonts)
        .expect("window chrome snapshot should render");

    assert!(frame.width >= visual.bounds.width);
    assert!(frame.height >= visual.bounds.height);
    let index = ((2 * frame.width + 2) * 4) as usize;
    assert_ne!(&frame.pixels[index..index + 4], &[12, 12, 12, 255]);
}

#[test]
fn window_chrome_changes_damage_old_and_new_surface_bounds() {
    let mut tracker = DamageTracker::new();
    let mut first = scene_without_cursor(Vec::new());
    first.window_chrome = Some(window_chrome_visual());
    let _ = tracker.update(&first, metrics());

    let mut second = first.clone();
    let chrome = second.window_chrome.as_mut().expect("chrome visual");
    chrome.bounds.y = 12;
    chrome.opacity = u16::MAX / 2;
    let damage = tracker.update(&second, metrics());

    assert!(
        damage
            .iter()
            .any(|region| region.y == 0 && region.height >= 36)
    );
    assert!(
        damage
            .iter()
            .any(|region| region.y <= 12 && region.height >= 36)
    );
}

/// Bounds of every quad in a batch, for the coverage checks below.
fn batch_quad_bounds(batch: &QuadBatch) -> Vec<RenderRect> {
    batch
        .vertices
        .chunks_exact(4)
        .map(|quad| {
            quad_bounds([
                quad[0].position_px,
                quad[1].position_px,
                quad[2].position_px,
                quad[3].position_px,
            ])
        })
        .collect()
}

fn covered(damage: &[DamageRegion], rect: RenderRect) -> bool {
    damage.iter().any(|region| rect_contains(*region, rect))
}

/// Drives the real cursor runtime, damage tracker, and batch planner through a
/// held-Enter prompt redraw sequence under retained damage, and checks the two
/// invariants a partial redraw has to keep: everything drawn this frame lies in
/// this frame's damage, and everything drawn last frame is either drawn again
/// identically or lies in this frame's damage. A violation of the second is
/// ink from an old frame that the new frame never clears.
#[test]
fn held_enter_prompt_redraws_never_leave_cursor_ink_outside_damage() {
    let mut fonts = FontSystem::new(font_system::FontConfig::default());
    let Ok(font_metrics) = fonts.cell_metrics() else {
        return;
    };
    let settings = CursorAnimationSettings::panea(165, 4, 2_200_000);
    let mut runtime = CursorAnimationRuntime::new();
    let mut tracker = DamageTracker::new();
    let mut planner = RenderBatchPlanner::default();

    let beam = |row: i64, col: u16, visible: bool| CursorVisual {
        position: CellPosition { row, col },
        shape: RenderCursorShape::Beam,
        color: RenderColor::rgb(245, 224, 220),
        text_color: None,
        visible,
        thickness_percent: 22,
        corner_radius_px: 0,
        inactive: false,
    };
    let rows: i64 = 3;
    let make_scene = |prompt_rows: &[i64], cursor: CursorVisual| {
        let mut cells = Vec::new();
        for &row in prompt_rows {
            cells.push(cell(row, 0, "\u{276f}"));
            cells.push(cell(row, 1, " "));
        }
        let mut s = scene(cells);
        s.grid.columns = 12;
        s.grid.rows = rows as u16;
        s.cursor = Some(cursor);
        s
    };

    let mut previous_ink: Vec<RenderRect> = Vec::new();
    let mut frame_index = 0usize;
    let mut check = |label: &str,
                     scene: &RenderScene,
                     batches: &PreparedRenderBatches,
                     previous_ink: &mut Vec<RenderRect>,
                     full: bool| {
        let mut ink = batch_quad_bounds(&batches.cursor);
        ink.extend(batch_quad_bounds(&batches.cursor_effects));
        ink.extend(batch_quad_bounds(&batches.cursor_trail));
        if !full {
            for rect in &ink {
                assert!(
                    covered(&batches.damage_regions, *rect),
                    "frame {frame_index} ({label}): cursor ink {rect:?} drawn outside damage {:?}",
                    batches.damage_regions
                );
            }
            for old in previous_ink.iter() {
                let redrawn = ink.iter().any(|now| now == old);
                assert!(
                    redrawn || covered(&batches.damage_regions, *old),
                    "frame {frame_index} ({label}): ink from the previous frame at {old:?} is neither redrawn nor cleared; damage={:?} cursor={:?} animations={}",
                    batches.damage_regions,
                    scene.cursor.map(|c| c.position),
                    scene.animations.len()
                );
            }
        }
        *previous_ink = ink;
        frame_index += 1;
    };

    // First frame: full.
    let mut prompt_rows: Vec<i64> = vec![0];
    let mut current = make_scene(&prompt_rows, beam(0, 2, true));
    runtime.populate_scene(&mut current, font_metrics, settings);
    current.damage_regions = tracker.update(&current, font_metrics);
    let batches = planner
        .prepare_full(&current, &mut fonts)
        .expect("full frame");
    check("initial", &current, &batches, &mut previous_ink, true);

    let mut row = 0i64;
    for enter in 0..8 {
        // Enter: shell moves to the next line, cursor at column 0, then the
        // prompt is written and the cursor lands after it. The right-aligned
        // block sends the cursor far right in between, exactly as oh-my-posh
        // does, and every one of those positions can be caught by a frame.
        row = (row + 1) % rows;
        prompt_rows.retain(|r| *r != row);
        let steps: [(u16, bool, bool); 4] = [
            (0, false, false), // CR, cursor hidden while redrawing
            (11, true, false), // right prompt block written
            (0, true, false),  // back to the start of the line
            (2, true, true),   // prompt text landed, cursor after it
        ];
        for (col, visible, with_prompt) in steps {
            if with_prompt {
                prompt_rows.push(row);
            }
            let mut next = make_scene(&prompt_rows, beam(row, col, visible));
            runtime.populate_scene(&mut next, font_metrics, settings);
            next.damage_regions = tracker.update(&next, font_metrics);
            let batches = planner.prepare(&next, &mut fonts).expect("content frame");
            check("content", &next, &batches, &mut previous_ink, false);
            current = next;

            // Animation frames until the runtime is quiet, but on the odd
            // Enters interrupt them early: a held key does not wait.
            let mut ticks = 0;
            while runtime.needs_frame() {
                std::thread::sleep(Duration::from_millis(4));
                runtime.refresh_retained_scene(&mut current, font_metrics, settings);
                current.damage_regions = tracker.update_animations_only(&current, font_metrics);
                let batches = planner
                    .prepare(&current, &mut fonts)
                    .expect("animation frame");
                check("animation", &current, &batches, &mut previous_ink, false);
                ticks += 1;
                if enter % 2 == 1 && ticks >= 3 {
                    break;
                }
            }
        }
    }

    // Settle completely and confirm the static cursor is drawn where the
    // terminal says it is.
    while runtime.needs_frame() {
        std::thread::sleep(Duration::from_millis(4));
        runtime.refresh_retained_scene(&mut current, font_metrics, settings);
        current.damage_regions = tracker.update_animations_only(&current, font_metrics);
        let batches = planner.prepare(&current, &mut fonts).expect("settle frame");
        check("settle", &current, &batches, &mut previous_ink, false);
    }
    runtime.refresh_retained_scene(&mut current, font_metrics, settings);
    current.damage_regions = tracker.update_animations_only(&current, font_metrics);
    let final_batches = planner.prepare(&current, &mut fonts).expect("final frame");
    let cursor_cell = cell_region(current.cursor.expect("cursor").position, font_metrics);
    let drawn = batch_quad_bounds(&final_batches.cursor);
    assert!(
        drawn.iter().any(|rect| rect_contains(cursor_cell, *rect))
            || previous_ink
                .iter()
                .any(|rect| rect_contains(cursor_cell, *rect)),
        "after settling, the cursor must be drawn inside its own cell {cursor_cell:?}; drawn={drawn:?} previous={previous_ink:?}"
    );
}
