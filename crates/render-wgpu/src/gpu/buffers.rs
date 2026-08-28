// Persistent GPU instance buffers, upload capacity, and render-pass encoding.

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuQuadInstance {
    positions: [[f32; 2]; 4],
    uv_bounds: [f32; 4],
    color: [f32; 4],
}

impl GpuQuadInstance {
    const ATTRIBUTES: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![
        0 => Float32x2,
        1 => Float32x2,
        2 => Float32x2,
        3 => Float32x2,
        4 => Float32x4,
        5 => Float32x4
    ];

    fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &Self::ATTRIBUTES,
        }
    }
}

#[derive(Debug, Default)]
struct GpuBatchBuffers {
    instances: Option<wgpu::Buffer>,
    instance_capacity: u64,
    instance_count: u32,
    staging_instances: Vec<GpuQuadInstance>,
}

impl GpuBatchBuffers {
    fn upload(
        &mut self,
        context: &GpuUploadContext<'_>,
        label: &'static str,
        vertices: &[BatchVertex],
        indices: &[u32],
    ) {
        if vertices.is_empty() || indices.is_empty() {
            self.instance_count = 0;
            return;
        }

        debug_assert_eq!(vertices.len() % 4, 0);
        debug_assert_eq!(indices.len(), vertices.len() / 4 * 6);
        self.staging_instances.clear();
        self.staging_instances.extend(
            vertices
                .chunks_exact(4)
                .map(|quad| quad_instance_from_vertices(quad, context.width, context.height)),
        );
        let instance_bytes = bytemuck::cast_slice(&self.staging_instances);
        ensure_buffer_capacity(
            context.device,
            &mut self.instances,
            &mut self.instance_capacity,
            instance_bytes.len() as u64,
            wgpu::BufferUsages::VERTEX,
            label,
        );
        if let Some(buffer) = &self.instances {
            context.queue.write_buffer(buffer, 0, instance_bytes);
        }
        self.instance_count = self.staging_instances.len().min(u32::MAX as usize) as u32;
    }
}

struct GpuUploadContext<'a> {
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    width: u32,
    height: u32,
}

fn ensure_buffer_capacity(
    device: &wgpu::Device,
    buffer: &mut Option<wgpu::Buffer>,
    capacity: &mut u64,
    required: u64,
    usage: wgpu::BufferUsages,
    label: &str,
) {
    if required == 0 || (*capacity >= required && buffer.is_some()) {
        return;
    }
    let new_capacity = buffer_capacity(required);
    *buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: new_capacity,
        usage: usage | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    }));
    *capacity = new_capacity;
}

fn buffer_capacity(required: u64) -> u64 {
    required.next_power_of_two().max(256)
}

#[derive(Debug, Default)]
struct PersistentBatchBuffers {
    damage_clear: GpuBatchBuffers,
    background: GpuBatchBuffers,
    glyphs: GpuBatchBuffers,
    logo_glyphs: GpuBatchBuffers,
    overlay_glyphs: GpuBatchBuffers,
    decorations: GpuBatchBuffers,
    cursor_effects: GpuBatchBuffers,
    cursor_trail: GpuBatchBuffers,
    window_chrome: GpuBatchBuffers,
    selections: GpuBatchBuffers,
    cursor: GpuBatchBuffers,
    cursor_image: GpuBatchBuffers,
}

fn prepare_damage_clear_batch(damage_regions: &[DamageRegion], color: RenderColor) -> QuadBatch {
    let mut batch = QuadBatch::new(QuadBatchKind::Background);
    for region in damage_regions {
        push_solid_quad(&mut batch, *region, color);
    }
    batch
}

fn prepare_frame_clear_batch(
    load_previous: bool,
    damage_regions: &[DamageRegion],
    width: u32,
    height: u32,
    color: RenderColor,
) -> QuadBatch {
    if load_previous {
        return prepare_damage_clear_batch(damage_regions, color);
    }

    prepare_damage_clear_batch(
        &[DamageRegion {
            x: 0,
            y: 0,
            width,
            height,
        }],
        color,
    )
}

struct GpuFrameDraw<'a> {
    clear_pipeline: &'a wgpu::RenderPipeline,
    quad_pipeline: &'a wgpu::RenderPipeline,
    glyph_pipeline: &'a wgpu::RenderPipeline,
    glyph_bind_group: Option<&'a wgpu::BindGroup>,
    logo_bind_group: Option<&'a wgpu::BindGroup>,
    batches: &'a PersistentBatchBuffers,
    damage_regions: &'a [DamageRegion],
    target_width: u32,
    target_height: u32,
}

struct GpuCursorOverlayDraw<'a> {
    quad_pipeline: &'a wgpu::RenderPipeline,
    cursor_image_pipeline: Option<&'a wgpu::RenderPipeline>,
    cursor_image_bind_group: Option<&'a wgpu::BindGroup>,
    batches: &'a PersistentBatchBuffers,
    cursor_image_active: bool,
}

fn encode_retained_frame<'a>(
    encoder: &'a mut wgpu::CommandEncoder,
    retained: &'a wgpu::TextureView,
    load_previous: bool,
    clear_color: wgpu::Color,
    draw: GpuFrameDraw<'a>,
    timestamp_writes: Option<wgpu::RenderPassTimestampWrites<'a>>,
) -> bool {
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("panea-batch-pass"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: retained,
            resolve_target: None,
            ops: wgpu::Operations {
                load: if load_previous {
                    wgpu::LoadOp::Load
                } else {
                    wgpu::LoadOp::Clear(clear_color)
                },
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        occlusion_query_set: None,
        timestamp_writes,
    });

    for scissor in damage_scissor_rects(draw.damage_regions, draw.target_width, draw.target_height)
    {
        pass.set_scissor_rect(
            scissor.x as u32,
            scissor.y as u32,
            scissor.width,
            scissor.height,
        );

        pass.set_pipeline(draw.clear_pipeline);
        draw_buffers(&mut pass, &draw.batches.damage_clear);

        // Cell backgrounds replace the surface clear so their configured alpha is
        // not compounded with the translucent window background beneath them.
        pass.set_pipeline(draw.clear_pipeline);
        draw_buffers(&mut pass, &draw.batches.background);

        pass.set_pipeline(draw.quad_pipeline);
        draw_buffers(&mut pass, &draw.batches.selections);
        draw_buffers(&mut pass, &draw.batches.cursor);

        if let Some(glyph_bind_group) = draw.glyph_bind_group {
            pass.set_pipeline(draw.glyph_pipeline);
            pass.set_bind_group(0, glyph_bind_group, &[]);
            draw_buffers(&mut pass, &draw.batches.glyphs);
        }

        pass.set_pipeline(draw.quad_pipeline);
        draw_buffers(&mut pass, &draw.batches.decorations);
        draw_buffers(&mut pass, &draw.batches.window_chrome);
        if let Some(logo_bind_group) = draw.logo_bind_group {
            pass.set_pipeline(draw.glyph_pipeline);
            pass.set_bind_group(0, logo_bind_group, &[]);
            draw_buffers(&mut pass, &draw.batches.logo_glyphs);
        }
        if let Some(glyph_bind_group) = draw.glyph_bind_group {
            pass.set_pipeline(draw.glyph_pipeline);
            pass.set_bind_group(0, glyph_bind_group, &[]);
            draw_buffers(&mut pass, &draw.batches.overlay_glyphs);
        }
    }
    load_previous
}

fn damage_scissor_rects(
    damage_regions: &[DamageRegion],
    target_width: u32,
    target_height: u32,
) -> Vec<RenderRect> {
    let surface = RenderRect {
        x: 0,
        y: 0,
        width: target_width,
        height: target_height,
    };
    damage_regions
        .iter()
        .filter_map(|region| rect_intersection(*region, surface))
        .collect()
}

fn encode_cursor_overlay<'a>(
    encoder: &'a mut wgpu::CommandEncoder,
    target: &'a wgpu::TextureView,
    draw: GpuCursorOverlayDraw<'a>,
) {
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("panea-cursor-overlay-pass"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Load,
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        occlusion_query_set: None,
        timestamp_writes: None,
    });

    pass.set_pipeline(draw.quad_pipeline);
    draw_buffers(&mut pass, &draw.batches.cursor_trail);
    draw_buffers(&mut pass, &draw.batches.cursor_effects);
    if draw.cursor_image_active
        && let Some(cursor_image_pipeline) = draw.cursor_image_pipeline
        && let Some(cursor_image_bind_group) = draw.cursor_image_bind_group
    {
        pass.set_pipeline(cursor_image_pipeline);
        pass.set_bind_group(0, cursor_image_bind_group, &[]);
        draw_buffers(&mut pass, &draw.batches.cursor_image);
    }
}

fn draw_buffers<'a>(pass: &mut wgpu::RenderPass<'a>, buffers: &'a GpuBatchBuffers) {
    let Some(instances) = &buffers.instances else {
        return;
    };
    if buffers.instance_count == 0 {
        return;
    }

    pass.set_vertex_buffer(0, instances.slice(..));
    pass.draw(0..6, 0..buffers.instance_count);
}

fn quad_instance_from_vertices(
    vertices: &[BatchVertex],
    surface_width: u32,
    surface_height: u32,
) -> GpuQuadInstance {
    debug_assert_eq!(vertices.len(), 4);
    let width = surface_width.max(1) as f32;
    let height = surface_height.max(1) as f32;
    GpuQuadInstance {
        positions: std::array::from_fn(|index| {
            let position = vertices[index].position_px;
            [
                (position[0] / width) * 2.0 - 1.0,
                1.0 - (position[1] / height) * 2.0,
            ]
        }),
        uv_bounds: [
            vertices[0].uv[0],
            vertices[0].uv[1],
            vertices[2].uv[0],
            vertices[2].uv[1],
        ],
        color: vertices[0].color,
    }
}
