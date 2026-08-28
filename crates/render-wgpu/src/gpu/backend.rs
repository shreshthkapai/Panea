// WGPU surface/device lifecycle, frame submission, and device-loss recovery.

pub struct GpuTerminalRenderer {
    window: Arc<Window>,
    options: RendererOptions,
    backend: Option<GpuBackend>,
    rasterizer: TerminalRasterizer,
    recycled_batches: Option<PreparedRenderBatches>,
    recycled_cursor_overlay: Option<PreparedCursorOverlay>,
    last_instrumentation: RenderInstrumentation,
    recovery_status: RenderRecoveryStatus,
    recovery_attempts: u32,
    recovery_events: Vec<RenderRecoveryEvent>,
    requires_full_redraw: bool,
    retained_cursor: Option<CursorVisual>,
}

struct GpuBackend {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    clear_pipeline: wgpu::RenderPipeline,
    quad_pipeline: wgpu::RenderPipeline,
    glyph_pipeline: wgpu::RenderPipeline,
    glyph_bind_group_layout: wgpu::BindGroupLayout,
    glyph_sampler: wgpu::Sampler,
    logo_sampler: wgpu::Sampler,
    glyph_mask_atlas_texture: Option<wgpu::Texture>,
    glyph_color_atlas_texture: Option<wgpu::Texture>,
    glyph_atlas_size: Option<(u32, u32)>,
    glyph_bind_group: Option<wgpu::BindGroup>,
    logo_bind_group: Option<wgpu::BindGroup>,
    cursor_image_resources: Option<CursorImageGpuResources>,
    cursor_image_texture: Option<wgpu::Texture>,
    cursor_image_asset_id: Option<u64>,
    cursor_image_bind_group: Option<wgpu::BindGroup>,
    retained_frame: RetainedFrameState,
    surface_copy_supported: bool,
    batches: PersistentBatchBuffers,
    device_loss_signal: Arc<Mutex<Option<DeviceLossSignal>>>,
    gpu_timing: GpuTiming,
    transparent: bool,
    alpha_mode: wgpu::CompositeAlphaMode,
    background: RenderColor,
    startup_diagnostics: RendererStartupDiagnostics,
}

#[derive(Default)]
struct RetainedFrameState {
    texture: Option<wgpu::Texture>,
    view: Option<wgpu::TextureView>,
    size: Option<(u32, u32)>,
    initialized: bool,
}

impl RetainedFrameState {
    fn invalidate(&mut self) {
        self.texture = None;
        self.view = None;
        self.size = None;
        self.initialized = false;
    }
}

struct CursorImageGpuResources {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DeviceLossSignal {
    reason: RenderRecoveryReason,
    message: String,
}

struct GpuTiming {
    query_set: Option<wgpu::QuerySet>,
    resolve_buffer: Option<wgpu::Buffer>,
    readback_buffer: Option<wgpu::Buffer>,
    pending: Option<Receiver<Result<(), String>>>,
    timestamp_period_ns: f32,
    last_duration: Option<Duration>,
    status: GpuTimingStatus,
}

impl GpuTiming {
    const QUERY_COUNT: u32 = 2;
    const BUFFER_SIZE: u64 = std::mem::size_of::<u64>() as u64 * Self::QUERY_COUNT as u64;

    fn disabled() -> Self {
        Self {
            query_set: None,
            resolve_buffer: None,
            readback_buffer: None,
            pending: None,
            timestamp_period_ns: 0.0,
            last_duration: None,
            status: GpuTimingStatus::Disabled,
        }
    }

    fn unsupported() -> Self {
        Self {
            status: GpuTimingStatus::Unsupported,
            ..Self::disabled()
        }
    }

    fn enabled(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let query_set = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("panea-gpu-timestamp-query"),
            ty: wgpu::QueryType::Timestamp,
            count: Self::QUERY_COUNT,
        });
        let resolve_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("panea-gpu-timestamp-resolve"),
            size: Self::BUFFER_SIZE,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("panea-gpu-timestamp-readback"),
            size: Self::BUFFER_SIZE,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        Self {
            query_set: Some(query_set),
            resolve_buffer: Some(resolve_buffer),
            readback_buffer: Some(readback_buffer),
            pending: None,
            timestamp_period_ns: queue.get_timestamp_period(),
            last_duration: None,
            status: GpuTimingStatus::Pending,
        }
    }

    fn timing_status(&self) -> GpuTimingStatus {
        self.status
    }

    fn last_duration(&self) -> Option<Duration> {
        self.last_duration
    }

    fn can_write_this_frame(&self) -> bool {
        self.query_set.is_some() && self.pending.is_none()
    }

    fn render_pass_writes(&self) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        self.query_set
            .as_ref()
            .filter(|_| self.pending.is_none())
            .map(|query_set| wgpu::RenderPassTimestampWrites {
                query_set,
                beginning_of_pass_write_index: Some(0),
                end_of_pass_write_index: Some(1),
            })
    }

    fn resolve_after_pass(&self, encoder: &mut wgpu::CommandEncoder) {
        let (Some(query_set), Some(resolve_buffer), Some(readback_buffer)) = (
            self.query_set.as_ref(),
            self.resolve_buffer.as_ref(),
            self.readback_buffer.as_ref(),
        ) else {
            return;
        };

        encoder.resolve_query_set(query_set, 0..Self::QUERY_COUNT, resolve_buffer, 0);
        encoder.copy_buffer_to_buffer(resolve_buffer, 0, readback_buffer, 0, Self::BUFFER_SIZE);
    }

    fn start_readback(&mut self) {
        if self.query_set.is_none() || self.pending.is_some() {
            return;
        }
        let Some(readback_buffer) = self.readback_buffer.as_ref() else {
            return;
        };

        let (sender, receiver) = mpsc::channel();
        readback_buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result.map_err(|error| error.to_string()));
            });
        self.pending = Some(receiver);
        self.status = GpuTimingStatus::Pending;
    }

    fn poll(&mut self, device: &wgpu::Device) {
        let Some(receiver) = self.pending.as_ref() else {
            return;
        };
        device.poll(wgpu::Maintain::Poll);

        let Ok(result) = receiver.try_recv() else {
            self.status = GpuTimingStatus::Pending;
            return;
        };
        self.pending = None;

        let Some(readback_buffer) = self.readback_buffer.as_ref() else {
            self.status = GpuTimingStatus::Failed;
            self.last_duration = None;
            return;
        };

        match result {
            Ok(()) => {
                let slice = readback_buffer.slice(..);
                let mapped = slice.get_mapped_range();
                if mapped.len() < Self::BUFFER_SIZE as usize {
                    self.status = GpuTimingStatus::Failed;
                    self.last_duration = None;
                } else {
                    let start =
                        u64::from_le_bytes(mapped[0..8].try_into().expect("timestamp start width"));
                    let end =
                        u64::from_le_bytes(mapped[8..16].try_into().expect("timestamp end width"));
                    let delta_ticks = end.saturating_sub(start);
                    let nanos =
                        (delta_ticks as f64 * f64::from(self.timestamp_period_ns)).max(0.0) as u64;
                    self.last_duration = Some(Duration::from_nanos(nanos));
                    self.status = GpuTimingStatus::Available;
                }
                drop(mapped);
                readback_buffer.unmap();
            }
            Err(_) => {
                self.status = GpuTimingStatus::Failed;
                self.last_duration = None;
                readback_buffer.unmap();
            }
        }
    }
}

impl GpuTerminalRenderer {
    pub async fn new(window: Arc<Window>, options: RendererOptions) -> Result<Self, RendererError> {
        let backend = GpuBackend::new(Arc::clone(&window), options).await?;

        Ok(Self {
            window,
            options,
            backend: Some(backend),
            rasterizer: TerminalRasterizer::new(options.glyph_cache_entries.max(1), 2048, 2048),
            recycled_batches: None,
            recycled_cursor_overlay: None,
            last_instrumentation: RenderInstrumentation::default(),
            recovery_status: RenderRecoveryStatus::Ready,
            recovery_attempts: 0,
            recovery_events: Vec::new(),
            requires_full_redraw: true,
            retained_cursor: None,
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if let Some(backend) = self.backend.as_mut() {
            backend.resize(width, height);
        }
        self.requires_full_redraw = true;
        self.retained_cursor = None;
    }

    #[must_use]
    pub fn startup_diagnostics(&self) -> Option<&RendererStartupDiagnostics> {
        self.backend
            .as_ref()
            .map(|backend| &backend.startup_diagnostics)
    }

    pub fn set_glyph_cache_capacity(&mut self, entries: usize) {
        let entries = entries.max(1);
        if self.options.glyph_cache_entries == entries {
            return;
        }
        self.options.glyph_cache_entries = entries;
        self.rasterizer = TerminalRasterizer::new(entries, 2048, 2048);
        self.requires_full_redraw = true;
        self.retained_cursor = None;
    }

    pub fn set_background(&mut self, background: RenderColor) {
        if self.options.background == background {
            return;
        }
        self.options.background = background;
        if let Some(backend) = self.backend.as_mut() {
            backend.background = background;
        }
        self.requires_full_redraw = true;
    }

    /// Presents the configured background before the native window becomes
    /// visible, avoiding an OS-default flash while GPU resources initialize.
    pub fn present_startup_background(&mut self) -> Result<(), RendererError> {
        let outcome = self
            .backend
            .as_mut()
            .ok_or_else(|| {
                RendererError::DeviceUnavailable(
                    "renderer backend is unavailable until GPU recovery succeeds".to_owned(),
                )
            })?
            .present_background()?;
        match outcome {
            PresentOutcome::Submitted => Ok(()),
            PresentOutcome::SurfaceReconfigured(reason) => {
                self.record_surface_recovery(reason);
                let retry = self
                    .backend
                    .as_mut()
                    .ok_or_else(|| {
                        RendererError::DeviceUnavailable(
                            "renderer backend became unavailable during startup".to_owned(),
                        )
                    })?
                    .present_background()?;
                match retry {
                    PresentOutcome::Submitted => Ok(()),
                    PresentOutcome::SurfaceReconfigured(_) => Err(RendererError::Surface(
                        "startup surface remained unavailable after reconfiguration".to_owned(),
                    )),
                    PresentOutcome::Timeout | PresentOutcome::Skipped => Err(
                        RendererError::Surface("startup surface presentation timed out".to_owned()),
                    ),
                }
            }
            PresentOutcome::Timeout | PresentOutcome::Skipped => Err(RendererError::Surface(
                "startup surface presentation timed out".to_owned(),
            )),
        }
    }

    pub fn request_full_redraw(&mut self) {
        self.requires_full_redraw = true;
        self.retained_cursor = None;
    }

    #[must_use]
    pub fn transparency_active(&self) -> bool {
        self.backend
            .as_ref()
            .is_some_and(|backend| backend.transparent)
    }

    #[must_use]
    pub fn damage_tracking_active(&self) -> bool {
        self.retained_damage_status() == RetainedDamageStatus::Enabled
    }

    #[must_use]
    pub fn retained_damage_status(&self) -> RetainedDamageStatus {
        if !self.options.damage_tracking {
            return RetainedDamageStatus::DisabledByConfig;
        }
        self.backend.as_ref().map_or_else(
            || RetainedDamageStatus::Unverified {
                reason: "the GPU backend is unavailable while renderer recovery is pending"
                    .to_owned(),
            },
            |backend| {
                retained_damage_status(
                    self.options.damage_tracking,
                    backend.supports_retained_damage(),
                )
            },
        )
    }

    pub fn render_scene(
        &mut self,
        scene: &RenderScene,
        fonts: &mut FontSystem,
    ) -> Result<(), RendererError> {
        let Some(backend) = self.backend.as_mut() else {
            return Err(RendererError::DeviceUnavailable(
                "renderer backend is unavailable until GPU recovery succeeds".to_owned(),
            ));
        };
        if let Some(signal) = backend.take_device_loss_signal() {
            let error = RendererError::DeviceLost {
                reason: signal.reason,
                message: signal.message,
            };
            self.mark_backend_lost(&error);
            return Err(error);
        }

        let frame_started = Instant::now();
        backend.poll_gpu_timing();
        let retained_damage_enabled =
            self.options.damage_tracking && backend.supports_retained_damage();
        let prepare_full_frame = should_prepare_full_frame(
            self.requires_full_redraw,
            self.options.damage_tracking,
            backend.supports_retained_damage(),
        );
        if prepare_full_frame {
            backend.retained_frame.initialized = false;
        }
        let recycled = self.recycled_batches.take();
        let mut batches = if prepare_full_frame {
            self.rasterizer
                .prepare_full_batches_reusing(scene, fonts, recycled)?
        } else {
            self.rasterizer
                .prepare_batches_reusing(scene, fonts, recycled)?
        };
        batches.instrumentation.gpu_time = backend.gpu_timing.last_duration();
        batches.instrumentation.gpu_timing_status = backend.gpu_timing.timing_status();
        let gpu_started = Instant::now();
        backend.upload_atlas(&self.rasterizer, &batches);
        if let Some(asset) = batches.cursor_image_asset.as_deref() {
            backend.upload_cursor_image(asset);
        }
        let load_retained_frame =
            should_load_retained_frame(retained_damage_enabled, prepare_full_frame);
        batches.instrumentation.draw_call_count = batches
            .instrumentation
            .draw_call_count
            .saturating_add(frame_clear_extra_draw_calls(
                load_retained_frame,
                batches.damage_regions.len(),
            ));
        let result =
            backend.present_batches(&batches, retained_damage_enabled, load_retained_frame);
        batches.instrumentation.gpu_submit_time = Some(gpu_started.elapsed());
        batches.instrumentation.frame_time = frame_started.elapsed();
        self.last_instrumentation = batches.instrumentation;

        let outcome = match result {
            Ok(PresentOutcome::Submitted) => {
                self.requires_full_redraw = false;
                self.retained_cursor = if retained_damage_enabled && !batches.cursor.is_empty() {
                    scene.cursor
                } else {
                    None
                };
                Ok(())
            }
            Ok(PresentOutcome::Timeout | PresentOutcome::Skipped) => {
                self.request_full_redraw();
                Ok(())
            }
            Ok(PresentOutcome::SurfaceReconfigured(reason)) => {
                self.record_surface_recovery(reason);
                self.retry_present_after_surface_reconfigure(
                    &batches,
                    retained_damage_enabled,
                    load_retained_frame,
                )
            }
            Err(error @ RendererError::DeviceLost { .. }) => {
                self.mark_backend_lost(&error);
                Err(error)
            }
            Err(error) => Err(error),
        };
        self.recycled_batches = Some(batches);
        outcome
    }

    pub fn render_cursor_overlay(
        &mut self,
        frame: &CursorOverlayFrame,
        metrics: CellMetrics,
    ) -> Result<bool, RendererError> {
        let Some(backend) = self.backend.as_mut() else {
            return Ok(false);
        };
        if let Some(signal) = backend.take_device_loss_signal() {
            let error = RendererError::DeviceLost {
                reason: signal.reason,
                message: signal.message,
            };
            self.mark_backend_lost(&error);
            return Err(error);
        }
        let retained_damage_enabled =
            self.options.damage_tracking && backend.supports_retained_damage();
        if !can_present_cursor_overlay(
            retained_damage_enabled,
            backend.retained_frame.initialized,
            self.retained_cursor,
            frame,
        ) {
            return Ok(false);
        }

        let frame_started = Instant::now();
        let prepare_started = Instant::now();
        let batches =
            prepare_cursor_overlay_reusing(frame, metrics, self.recycled_cursor_overlay.take());
        let cpu_prepare_time = prepare_started.elapsed();
        let gpu_started = Instant::now();
        let result = backend.present_cursor_overlay(&batches);
        let gpu_submit_time = gpu_started.elapsed();
        self.last_instrumentation = RenderInstrumentation {
            frame_time: frame_started.elapsed(),
            cpu_prepare_time,
            gpu_submit_time: Some(gpu_submit_time),
            gpu_time: backend.gpu_timing.last_duration(),
            gpu_timing_status: backend.gpu_timing.timing_status(),
            damage_region_count: usize::from(batches.animated_pixels > 0),
            draw_call_count: batches.draw_call_count(),
            animated_region_count: frame.animations.len(),
            ..RenderInstrumentation::default()
        };
        self.recycled_cursor_overlay = Some(batches);

        match result? {
            PresentOutcome::Submitted => Ok(true),
            PresentOutcome::Timeout | PresentOutcome::Skipped => {
                self.request_full_redraw();
                Ok(false)
            }
            PresentOutcome::SurfaceReconfigured(reason) => {
                self.record_surface_recovery(reason);
                self.requires_full_redraw = true;
                self.retained_cursor = None;
                Ok(false)
            }
        }
    }

    #[must_use]
    pub const fn last_instrumentation(&self) -> RenderInstrumentation {
        self.last_instrumentation
    }

    #[must_use]
    pub fn status(&self) -> RenderSurfaceStatus {
        self.recovery_status.surface_status()
    }

    #[must_use]
    pub const fn recovery_status(&self) -> &RenderRecoveryStatus {
        &self.recovery_status
    }

    #[must_use]
    pub fn recovery_events(&self) -> &[RenderRecoveryEvent] {
        &self.recovery_events
    }

    pub async fn recover_from_device_loss(
        &mut self,
        reason: RenderRecoveryReason,
    ) -> Result<RenderRecoveryEvent, RendererError> {
        self.recovery_attempts = self.recovery_attempts.saturating_add(1);
        self.recovery_status = RenderRecoveryStatus::Recovering {
            reason,
            attempts: self.recovery_attempts,
        };
        self.backend = None;
        self.invalidate_gpu_resident_resources();

        match GpuBackend::new(Arc::clone(&self.window), self.options).await {
            Ok(backend) => {
                self.backend = Some(backend);
                self.requires_full_redraw = true;
                self.recovery_status = RenderRecoveryStatus::Ready;
                let event = RenderRecoveryEvent::success(reason, self.recovery_attempts);
                self.recovery_events.push(event.clone());
                Ok(event)
            }
            Err(error) => {
                let message = error.to_string();
                self.recovery_status = RenderRecoveryStatus::Failed {
                    reason,
                    message: message.clone(),
                };
                let event =
                    RenderRecoveryEvent::failure(reason, self.recovery_attempts, message.clone());
                self.recovery_events.push(event);
                Err(RendererError::RecoveryFailed(message))
            }
        }
    }

    fn invalidate_gpu_resident_resources(&mut self) {
        self.rasterizer.reset_gpu_resident_glyphs();
        self.last_instrumentation = RenderInstrumentation::default();
        self.requires_full_redraw = true;
        self.retained_cursor = None;
    }

    fn retry_present_after_surface_reconfigure(
        &mut self,
        batches: &PreparedRenderBatches,
        retained_damage_enabled: bool,
        load_retained_frame: bool,
    ) -> Result<(), RendererError> {
        if load_retained_frame {
            self.request_full_redraw();
            self.window.request_redraw();
            return Ok(());
        }
        let Some(backend) = self.backend.as_mut() else {
            return Err(RendererError::DeviceUnavailable(
                "renderer backend disappeared during surface recovery".to_owned(),
            ));
        };

        let result = backend.present_batches(batches, retained_damage_enabled, load_retained_frame);
        match result {
            Ok(PresentOutcome::Submitted) => Ok(()),
            Ok(outcome) if present_outcome_requires_full_redraw(outcome) => {
                self.request_full_redraw();
                self.window.request_redraw();
                Ok(())
            }
            Ok(_) => Ok(()),
            Err(error @ RendererError::DeviceLost { .. }) => {
                self.mark_backend_lost(&error);
                Err(error)
            }
            Err(error) => Err(error),
        }
    }

    fn record_surface_recovery(&mut self, reason: RenderRecoveryReason) {
        let event = RenderRecoveryEvent {
            reason,
            attempts: self.recovery_attempts,
            rebuilt_surface: true,
            rebuilt_device: false,
            rebuilt_pipelines: false,
            rebuilt_glyph_atlas: false,
            preserved_terminal_state: true,
            message: "surface was reconfigured after a recoverable surface event".to_owned(),
        };
        self.recovery_events.push(event);
        self.recovery_status = RenderRecoveryStatus::Ready;
    }

    fn mark_backend_lost(&mut self, error: &RendererError) {
        let (reason, message) = match error {
            RendererError::DeviceLost { reason, message } => (*reason, message.clone()),
            _ => (
                RenderRecoveryReason::BackendError,
                "renderer backend failed".to_owned(),
            ),
        };
        self.backend = None;
        self.invalidate_gpu_resident_resources();
        self.recovery_status = RenderRecoveryStatus::Lost { reason, message };
    }
}

fn should_prepare_full_frame(
    requires_full_redraw: bool,
    damage_tracking_enabled: bool,
    retained_damage_supported: bool,
) -> bool {
    requires_full_redraw || !damage_tracking_enabled || !retained_damage_supported
}

fn should_load_retained_frame(retained_damage_enabled: bool, prepare_full_frame: bool) -> bool {
    retained_damage_enabled && !prepare_full_frame
}

fn frame_clear_extra_draw_calls(load_previous: bool, damage_region_count: usize) -> u32 {
    u32::from(!load_previous || damage_region_count > 0)
}

impl GpuBackend {
    async fn new(window: Arc<Window>, options: RendererOptions) -> Result<Self, RendererError> {
        let overall_started = Instant::now();
        let requested = options.backend;
        let mut attempted_backends = Vec::new();
        let mut fallback_errors = Vec::new();

        for candidate in
            backend_candidates_for(native_backend_family(), requested, options.transparent)
        {
            attempted_backends.push(candidate);
            match Self::new_for_backend(Arc::clone(&window), options, candidate).await {
                Ok(mut backend) => {
                    backend.startup_diagnostics.requested_backend = requested;
                    backend.startup_diagnostics.attempted_backends = attempted_backends;
                    backend.startup_diagnostics.fallback_errors = fallback_errors;
                    backend.startup_diagnostics.timings.total = overall_started.elapsed();
                    return Ok(backend);
                }
                Err(RendererError::EmptySurface) => return Err(RendererError::EmptySurface),
                Err(error) if requested != GpuBackendPreference::Auto => return Err(error),
                Err(error) => {
                    let failure = format!("{}: {error}", candidate.as_str());
                    eprintln!("renderer backend fallback: {failure}");
                    fallback_errors.push(failure);
                }
            }
        }

        Err(RendererError::BackendSelection {
            requested,
            attempts: fallback_errors,
        })
    }

    async fn new_for_backend(
        window: Arc<Window>,
        options: RendererOptions,
        candidate: GpuBackendPreference,
    ) -> Result<Self, RendererError> {
        let startup_started = Instant::now();
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return Err(RendererError::EmptySurface);
        }

        let phase_started = Instant::now();
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: instance_backends(candidate),
            ..wgpu::InstanceDescriptor::default()
        });
        let surface = instance
            .create_surface(Arc::clone(&window))
            .map_err(|err| RendererError::SurfaceCreation(err.to_string()))?;
        let instance_and_surface = phase_started.elapsed();
        let phase_started = Instant::now();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .ok_or(RendererError::AdapterUnavailable {
                requested: candidate,
            })?;
        let adapter_request = phase_started.elapsed();
        let adapter_info = adapter.get_info();
        let adapter_features = adapter.features();
        let gpu_timestamps_supported = adapter_features.contains(wgpu::Features::TIMESTAMP_QUERY);
        let required_features = if options.gpu_timestamps && gpu_timestamps_supported {
            wgpu::Features::TIMESTAMP_QUERY
        } else {
            wgpu::Features::empty()
        };
        let phase_started = Instant::now();
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("panea-render-device"),
                    required_features,
                    required_limits: wgpu::Limits::default(),
                },
                None,
            )
            .await
            .map_err(|err| RendererError::DeviceCreation(err.to_string()))?;
        let device_request = phase_started.elapsed();
        let device_loss_signal = Arc::new(Mutex::new(None));
        let callback_signal = Arc::clone(&device_loss_signal);
        device.set_device_lost_callback(move |reason, message| {
            if let Some(reason) = map_device_lost_reason(reason)
                && let Ok(mut signal) = callback_signal.lock()
            {
                *signal = Some(DeviceLossSignal { reason, message });
            }
        });
        let uncaptured_signal = Arc::clone(&device_loss_signal);
        device.on_uncaptured_error(Box::new(move |error| {
            if let Ok(mut signal) = uncaptured_signal.lock() {
                *signal = Some(device_loss_signal_from_uncaptured(&error));
            }
        }));

        let phase_started = Instant::now();
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .unwrap_or(caps.formats[0]);
        let present_mode = select_present_mode(options.present_mode, &caps.present_modes);
        eprintln!(
            "renderer present mode: requested={:?} effective={present_mode:?} max_frame_latency={DESIRED_MAXIMUM_FRAME_LATENCY}",
            options.present_mode
        );
        let alpha_mode = select_composite_alpha_mode(options.transparent, &caps.alpha_modes);
        let surface_copy_supported = caps.usages.contains(wgpu::TextureUsages::COPY_DST);
        let config = wgpu::SurfaceConfiguration {
            usage: if surface_copy_supported {
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_DST
            } else {
                wgpu::TextureUsages::RENDER_ATTACHMENT
            },
            format,
            width: size.width,
            height: size.height,
            present_mode,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: DESIRED_MAXIMUM_FRAME_LATENCY,
        };
        surface.configure(&device, &config);
        let surface_configuration = phase_started.elapsed();

        let phase_started = Instant::now();
        let batch_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("panea-batch-shader"),
            source: wgpu::ShaderSource::Wgsl(BATCH_SHADER.into()),
        });
        let quad_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("panea-quad-pipeline-layout"),
            bind_group_layouts: &[],
            push_constant_ranges: &[],
        });
        let premultiplied_alpha = alpha_mode == wgpu::CompositeAlphaMode::PreMultiplied;
        let quad_pipeline = create_composited_batch_pipeline(
            &device,
            &quad_pipeline_layout,
            &batch_shader,
            format,
            "panea-quad-pipeline",
            if format.is_srgb() && premultiplied_alpha {
                "fs_color_srgb_target_premultiplied"
            } else if format.is_srgb() {
                "fs_color_srgb_target"
            } else if premultiplied_alpha {
                "fs_color_unorm_target_premultiplied"
            } else {
                "fs_color_unorm_target"
            },
            alpha_mode,
        );
        let clear_pipeline = create_replacement_pipeline(
            &device,
            &quad_pipeline_layout,
            &batch_shader,
            format,
            "panea-damage-clear-pipeline",
            if format.is_srgb() && premultiplied_alpha {
                "fs_color_srgb_target_premultiplied"
            } else if format.is_srgb() {
                "fs_color_srgb_target"
            } else if premultiplied_alpha {
                "fs_color_unorm_target_premultiplied"
            } else {
                "fs_color_unorm_target"
            },
        );
        let glyph_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("panea-glyph-bind-group-layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });
        let glyph_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("panea-glyph-pipeline-layout"),
                bind_group_layouts: &[&glyph_bind_group_layout],
                push_constant_ranges: &[],
            });
        let glyph_pipeline = create_glyph_pipeline(
            &device,
            &glyph_pipeline_layout,
            &batch_shader,
            format,
            "panea-glyph-pipeline",
            if format.is_srgb() && premultiplied_alpha {
                "fs_glyph_srgb_target_premultiplied"
            } else if format.is_srgb() {
                "fs_glyph_srgb_target"
            } else if premultiplied_alpha {
                "fs_glyph_unorm_target_premultiplied"
            } else {
                "fs_glyph_unorm_target"
            },
            GlyphPipelineOptions {
                alpha_mode,
                text_gamma_adjustment: options.text_gamma_adjustment,
            },
        );
        let glyph_sampler = device.create_sampler(&glyph_sampler_descriptor());
        let logo_sampler = device.create_sampler(&logo_sampler_descriptor());
        let gpu_timing = if options.gpu_timestamps {
            if gpu_timestamps_supported {
                GpuTiming::enabled(&device, &queue)
            } else {
                GpuTiming::unsupported()
            }
        } else {
            GpuTiming::disabled()
        };
        let pipeline_creation = phase_started.elapsed();
        let startup_diagnostics = RendererStartupDiagnostics {
            requested_backend: candidate,
            effective_backend: format!("{:?}", adapter_info.backend),
            adapter: adapter_info.name,
            attempted_backends: vec![candidate],
            fallback_errors: Vec::new(),
            timings: RendererStartupTimings {
                instance_and_surface,
                adapter_request,
                device_request,
                surface_configuration,
                pipeline_creation,
                total: startup_started.elapsed(),
            },
        };

        Ok(Self {
            window,
            surface,
            device,
            queue,
            config,
            clear_pipeline,
            quad_pipeline,
            glyph_pipeline,
            glyph_bind_group_layout,
            glyph_sampler,
            logo_sampler,
            glyph_mask_atlas_texture: None,
            glyph_color_atlas_texture: None,
            glyph_atlas_size: None,
            glyph_bind_group: None,
            logo_bind_group: None,
            cursor_image_resources: None,
            cursor_image_texture: None,
            cursor_image_asset_id: None,
            cursor_image_bind_group: None,
            retained_frame: RetainedFrameState::default(),
            surface_copy_supported,
            batches: PersistentBatchBuffers::default(),
            device_loss_signal,
            gpu_timing,
            transparent: options.transparent && alpha_mode != wgpu::CompositeAlphaMode::Opaque,
            alpha_mode,
            background: options.background,
            startup_diagnostics,
        })
    }

    fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }

        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.retained_frame.invalidate();
    }

    fn configure_surface_from_window(&mut self, force: bool) -> SurfaceSizeAction {
        let size = self.window.inner_size();
        let action = surface_size_action(
            (self.config.width, self.config.height),
            (size.width, size.height),
        );
        match action {
            SurfaceSizeAction::Skip => SurfaceSizeAction::Skip,
            SurfaceSizeAction::Keep if !force => SurfaceSizeAction::Keep,
            SurfaceSizeAction::Keep => {
                self.surface.configure(&self.device, &self.config);
                self.retained_frame.invalidate();
                SurfaceSizeAction::Keep
            }
            SurfaceSizeAction::Reconfigure(width, height) => {
                self.config.width = width;
                self.config.height = height;
                self.surface.configure(&self.device, &self.config);
                self.retained_frame.invalidate();
                SurfaceSizeAction::Reconfigure(width, height)
            }
        }
    }

    fn supports_retained_damage(&self) -> bool {
        self.surface_copy_supported
    }

    fn ensure_retained_frame(&mut self) {
        if !self.surface_copy_supported
            || self.retained_frame.size == Some((self.config.width, self.config.height))
        {
            return;
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("panea-retained-frame"),
            size: wgpu::Extent3d {
                width: self.config.width,
                height: self.config.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        self.retained_frame.view =
            Some(texture.create_view(&wgpu::TextureViewDescriptor::default()));
        self.retained_frame.texture = Some(texture);
        self.retained_frame.size = Some((self.config.width, self.config.height));
        self.retained_frame.initialized = false;
    }

    fn take_device_loss_signal(&self) -> Option<DeviceLossSignal> {
        self.device_loss_signal
            .lock()
            .ok()
            .and_then(|mut signal| signal.take())
    }

    fn poll_gpu_timing(&mut self) {
        self.gpu_timing.poll(&self.device);
    }

    fn upload_atlas(&mut self, rasterizer: &TerminalRasterizer, batches: &PreparedRenderBatches) {
        let atlas_size = rasterizer.atlas_dimensions();
        if self.glyph_atlas_size != Some(atlas_size) {
            let mask_texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("panea-glyph-mask-atlas"),
                size: wgpu::Extent3d {
                    width: atlas_size.0,
                    height: atlas_size.1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let color_texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("panea-glyph-color-atlas"),
                size: wgpu::Extent3d {
                    width: atlas_size.0,
                    height: atlas_size.1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let mask_view = mask_texture.create_view(&wgpu::TextureViewDescriptor::default());
            let color_view = color_texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("panea-glyph-bind-group"),
                layout: &self.glyph_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&mask_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&color_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.glyph_sampler),
                    },
                ],
            });
            let logo_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("panea-logo-bind-group"),
                layout: &self.glyph_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&mask_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&color_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.logo_sampler),
                    },
                ],
            });
            self.glyph_mask_atlas_texture = Some(mask_texture);
            self.glyph_color_atlas_texture = Some(color_texture);
            self.glyph_atlas_size = Some(atlas_size);
            self.glyph_bind_group = Some(bind_group);
            self.logo_bind_group = Some(logo_bind_group);
        }

        let (Some(mask_texture), Some(color_texture)) = (
            self.glyph_mask_atlas_texture.as_ref(),
            self.glyph_color_atlas_texture.as_ref(),
        ) else {
            return;
        };

        for upload in &batches.atlas_uploads {
            if upload.entry.width == 0 || upload.entry.height == 0 {
                continue;
            }
            if upload.entry.x < GLYPH_ATLAS_PADDING || upload.entry.y < GLYPH_ATLAS_PADDING {
                continue;
            }

            let Some(padded) = padded_atlas_upload(upload) else {
                continue;
            };
            let padded_width = upload.entry.width + GLYPH_ATLAS_PADDING * 2;
            let texture = match upload.format {
                GlyphBitmapFormat::Alpha => mask_texture,
                GlyphBitmapFormat::Rgba => color_texture,
            };

            self.queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: upload.entry.x - GLYPH_ATLAS_PADDING,
                        y: upload.entry.y - GLYPH_ATLAS_PADDING,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                &padded.pixels,
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(padded.bytes_per_row),
                    rows_per_image: Some(padded.rows_per_image),
                },
                wgpu::Extent3d {
                    width: padded_width,
                    height: padded.rows_per_image,
                    depth_or_array_layers: 1,
                },
            );
        }
    }

    fn upload_cursor_image(&mut self, asset: &CursorImageAsset) {
        if self.cursor_image_asset_id == Some(asset.id) {
            return;
        }
        let Ok(layer_count) = u32::try_from(asset.frames.len()) else {
            return;
        };
        if asset.width == 0 || asset.height == 0 || layer_count == 0 {
            return;
        }
        self.ensure_cursor_image_resources();
        let Some(resources) = self.cursor_image_resources.as_ref() else {
            return;
        };
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("panea-cursor-image-array"),
            size: wgpu::Extent3d {
                width: asset.width,
                height: asset.height,
                depth_or_array_layers: layer_count,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let unpadded_row_bytes = asset.width.saturating_mul(4);
        let padded_row_bytes = unpadded_row_bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        for (layer, frame) in asset.frames.iter().enumerate() {
            let expected = usize::try_from(unpadded_row_bytes.saturating_mul(asset.height))
                .unwrap_or(usize::MAX);
            if frame.pixels.len() != expected {
                return;
            }
            let mut upload = vec![
                0;
                usize::try_from(padded_row_bytes.saturating_mul(asset.height))
                    .unwrap_or(0)
            ];
            for row in 0..asset.height {
                let source_start =
                    usize::try_from(row.saturating_mul(unpadded_row_bytes)).unwrap_or(usize::MAX);
                let target_start =
                    usize::try_from(row.saturating_mul(padded_row_bytes)).unwrap_or(usize::MAX);
                let row_len = usize::try_from(unpadded_row_bytes).unwrap_or(0);
                let (Some(source), Some(target)) = (
                    frame
                        .pixels
                        .get(source_start..source_start.saturating_add(row_len)),
                    upload.get_mut(target_start..target_start.saturating_add(row_len)),
                ) else {
                    return;
                };
                target.copy_from_slice(source);
            }
            self.queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: u32::try_from(layer).unwrap_or(u32::MAX),
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                &upload,
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row_bytes),
                    rows_per_image: Some(asset.height),
                },
                wgpu::Extent3d {
                    width: asset.width,
                    height: asset.height,
                    depth_or_array_layers: 1,
                },
            );
        }
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("panea-cursor-image-array-view"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            base_array_layer: 0,
            array_layer_count: Some(layer_count),
            ..wgpu::TextureViewDescriptor::default()
        });
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("panea-cursor-image-bind-group"),
            layout: &resources.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&resources.sampler),
                },
            ],
        });
        self.cursor_image_texture = Some(texture);
        self.cursor_image_asset_id = Some(asset.id);
        self.cursor_image_bind_group = Some(bind_group);
    }

    fn ensure_cursor_image_resources(&mut self) {
        if self.cursor_image_resources.is_some() {
            return;
        }

        let shader = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("panea-cursor-image-shader"),
                source: wgpu::ShaderSource::Wgsl(CURSOR_IMAGE_SHADER.into()),
            });
        let bind_group_layout =
            self.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("panea-cursor-image-bind-group-layout"),
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
        let pipeline_layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("panea-cursor-image-pipeline-layout"),
                bind_group_layouts: &[&bind_group_layout],
                push_constant_ranges: &[],
            });
        let premultiplied_alpha = self.alpha_mode == wgpu::CompositeAlphaMode::PreMultiplied;
        let pipeline = create_composited_batch_pipeline(
            &self.device,
            &pipeline_layout,
            &shader,
            self.config.format,
            "panea-cursor-image-pipeline",
            if self.config.format.is_srgb() && premultiplied_alpha {
                "fs_cursor_image_srgb_target_premultiplied"
            } else if self.config.format.is_srgb() {
                "fs_cursor_image_srgb_target"
            } else if premultiplied_alpha {
                "fs_cursor_image_unorm_target_premultiplied"
            } else {
                "fs_cursor_image_unorm_target"
            },
            self.alpha_mode,
        );
        let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("panea-cursor-image-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..wgpu::SamplerDescriptor::default()
        });
        self.cursor_image_resources = Some(CursorImageGpuResources {
            pipeline,
            bind_group_layout,
            sampler,
        });
    }

    fn present_batches(
        &mut self,
        batches: &PreparedRenderBatches,
        retained_damage_enabled: bool,
        load_retained_frame: bool,
    ) -> Result<PresentOutcome, RendererError> {
        match self.configure_surface_from_window(false) {
            SurfaceSizeAction::Skip => return Ok(PresentOutcome::Skipped),
            SurfaceSizeAction::Reconfigure(_, _) => {
                return Ok(PresentOutcome::SurfaceReconfigured(
                    RenderRecoveryReason::SurfaceOutdated,
                ));
            }
            SurfaceSizeAction::Keep => {}
        }
        if retained_damage_enabled {
            self.ensure_retained_frame();
        } else {
            self.retained_frame.invalidate();
        }
        let output = match self.surface.get_current_texture() {
            Ok(output) => output,
            Err(wgpu::SurfaceError::Lost) => {
                if self.configure_surface_from_window(true) == SurfaceSizeAction::Skip {
                    return Ok(PresentOutcome::Skipped);
                }
                return Ok(PresentOutcome::SurfaceReconfigured(
                    RenderRecoveryReason::SurfaceLost,
                ));
            }
            Err(wgpu::SurfaceError::Outdated) => {
                if self.configure_surface_from_window(true) == SurfaceSizeAction::Skip {
                    return Ok(PresentOutcome::Skipped);
                }
                return Ok(PresentOutcome::SurfaceReconfigured(
                    RenderRecoveryReason::SurfaceOutdated,
                ));
            }
            Err(wgpu::SurfaceError::Timeout) => return Ok(PresentOutcome::Timeout),
            Err(wgpu::SurfaceError::OutOfMemory) => {
                return Err(RendererError::DeviceLost {
                    reason: RenderRecoveryReason::OutOfMemory,
                    message: "surface reported out-of-memory; GPU resources must be recreated"
                        .to_owned(),
                });
            }
        };
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let upload_context = GpuUploadContext {
            device: &self.device,
            queue: &self.queue,
            width: self.config.width,
            height: self.config.height,
        };
        let load_previous = load_retained_frame
            && self.retained_frame.texture.is_some()
            && self.retained_frame.initialized;
        let damage_clear = prepare_frame_clear_batch(
            load_previous,
            &batches.damage_regions,
            self.config.width,
            self.config.height,
            surface_background_color(self.transparent, self.background),
        );
        self.batches.damage_clear.upload(
            &upload_context,
            "damage-clear",
            &damage_clear.vertices,
            &damage_clear.indices,
        );
        self.batches.background.upload(
            &upload_context,
            "background",
            &batches.background.vertices,
            &batches.background.indices,
        );
        self.batches.decorations.upload(
            &upload_context,
            "decorations",
            &batches.decorations.vertices,
            &batches.decorations.indices,
        );
        self.batches.cursor_effects.upload(
            &upload_context,
            "cursor-effects",
            &batches.cursor_effects.vertices,
            &batches.cursor_effects.indices,
        );
        self.batches.cursor_trail.upload(
            &upload_context,
            "cursor-trail",
            &batches.cursor_trail.vertices,
            &batches.cursor_trail.indices,
        );
        self.batches.window_chrome.upload(
            &upload_context,
            "window-chrome",
            &batches.window_chrome.vertices,
            &batches.window_chrome.indices,
        );
        self.batches.selections.upload(
            &upload_context,
            "selections",
            &batches.selections.vertices,
            &batches.selections.indices,
        );
        self.batches.cursor.upload(
            &upload_context,
            "cursor",
            &batches.cursor.vertices,
            &batches.cursor.indices,
        );
        self.batches.cursor_image.upload(
            &upload_context,
            "cursor-image",
            &batches.cursor_image.vertices,
            &batches.cursor_image.indices,
        );
        self.batches.glyphs.upload(
            &upload_context,
            "glyphs",
            &batches.glyphs.vertices,
            &batches.glyphs.indices,
        );
        self.batches.logo_glyphs.upload(
            &upload_context,
            "logo-glyphs",
            &batches.logo_glyphs.vertices,
            &batches.logo_glyphs.indices,
        );
        self.batches.overlay_glyphs.upload(
            &upload_context,
            "overlay-glyphs",
            &batches.overlay_glyphs.vertices,
            &batches.overlay_glyphs.indices,
        );
        let target_view = self.retained_frame.view.as_ref().unwrap_or(&view);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("panea-batch-encoder"),
            });
        let timestamp_written = self.gpu_timing.can_write_this_frame();
        let timestamp_writes = self.gpu_timing.render_pass_writes();
        encode_retained_frame(
            &mut encoder,
            target_view,
            load_previous,
            surface_clear_color_for_alpha_mode(
                surface_background_color(self.transparent, self.background),
                self.config.format,
                self.alpha_mode,
            ),
            GpuFrameDraw {
                clear_pipeline: &self.clear_pipeline,
                quad_pipeline: &self.quad_pipeline,
                glyph_pipeline: &self.glyph_pipeline,
                glyph_bind_group: self.glyph_bind_group.as_ref(),
                logo_bind_group: self.logo_bind_group.as_ref(),
                batches: &self.batches,
                damage_regions: &batches.damage_regions,
                target_width: self.config.width,
                target_height: self.config.height,
            },
            timestamp_writes,
        );

        if let Some(retained_frame) = self.retained_frame.texture.as_ref() {
            encoder.copy_texture_to_texture(
                wgpu::ImageCopyTexture {
                    texture: retained_frame,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::ImageCopyTexture {
                    texture: &output.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: self.config.width,
                    height: self.config.height,
                    depth_or_array_layers: 1,
                },
            );
            self.retained_frame.initialized = true;
        }

        encode_cursor_overlay(
            &mut encoder,
            &view,
            GpuCursorOverlayDraw {
                quad_pipeline: &self.quad_pipeline,
                cursor_image_pipeline: self
                    .cursor_image_resources
                    .as_ref()
                    .map(|resources| &resources.pipeline),
                cursor_image_bind_group: self.cursor_image_bind_group.as_ref(),
                batches: &self.batches,
                cursor_image_active: self.cursor_image_asset_id
                    == batches.cursor_image_asset.as_ref().map(|asset| asset.id),
            },
        );

        if timestamp_written {
            self.gpu_timing.resolve_after_pass(&mut encoder);
        }
        self.queue.submit(Some(encoder.finish()));
        if timestamp_written {
            self.gpu_timing.start_readback();
        }
        output.present();
        Ok(PresentOutcome::Submitted)
    }

    fn present_cursor_overlay(
        &mut self,
        batches: &PreparedCursorOverlay,
    ) -> Result<PresentOutcome, RendererError> {
        match self.configure_surface_from_window(false) {
            SurfaceSizeAction::Skip => return Ok(PresentOutcome::Skipped),
            SurfaceSizeAction::Reconfigure(_, _) => {
                return Ok(PresentOutcome::SurfaceReconfigured(
                    RenderRecoveryReason::SurfaceOutdated,
                ));
            }
            SurfaceSizeAction::Keep => {}
        }
        let Some(retained_frame) = self.retained_frame.texture.as_ref() else {
            return Ok(PresentOutcome::SurfaceReconfigured(
                RenderRecoveryReason::SurfaceOutdated,
            ));
        };
        let output = match self.surface.get_current_texture() {
            Ok(output) => output,
            Err(wgpu::SurfaceError::Lost) => {
                if self.configure_surface_from_window(true) == SurfaceSizeAction::Skip {
                    return Ok(PresentOutcome::Skipped);
                }
                return Ok(PresentOutcome::SurfaceReconfigured(
                    RenderRecoveryReason::SurfaceLost,
                ));
            }
            Err(wgpu::SurfaceError::Outdated) => {
                if self.configure_surface_from_window(true) == SurfaceSizeAction::Skip {
                    return Ok(PresentOutcome::Skipped);
                }
                return Ok(PresentOutcome::SurfaceReconfigured(
                    RenderRecoveryReason::SurfaceOutdated,
                ));
            }
            Err(wgpu::SurfaceError::Timeout) => return Ok(PresentOutcome::Timeout),
            Err(wgpu::SurfaceError::OutOfMemory) => {
                return Err(RendererError::DeviceLost {
                    reason: RenderRecoveryReason::OutOfMemory,
                    message: "surface reported out-of-memory; GPU resources must be recreated"
                        .to_owned(),
                });
            }
        };
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let upload_context = GpuUploadContext {
            device: &self.device,
            queue: &self.queue,
            width: self.config.width,
            height: self.config.height,
        };
        self.batches.cursor_effects.upload(
            &upload_context,
            "cursor-effects-fast",
            &batches.effects.vertices,
            &batches.effects.indices,
        );
        self.batches.cursor_trail.upload(
            &upload_context,
            "cursor-trail-fast",
            &batches.cursor_trail.vertices,
            &batches.cursor_trail.indices,
        );
        self.batches.cursor_image.instance_count = 0;

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("panea-cursor-overlay-encoder"),
            });
        encoder.copy_texture_to_texture(
            wgpu::ImageCopyTexture {
                texture: retained_frame,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyTexture {
                texture: &output.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: self.config.width,
                height: self.config.height,
                depth_or_array_layers: 1,
            },
        );
        encode_cursor_overlay(
            &mut encoder,
            &view,
            GpuCursorOverlayDraw {
                quad_pipeline: &self.quad_pipeline,
                cursor_image_pipeline: None,
                cursor_image_bind_group: None,
                batches: &self.batches,
                cursor_image_active: false,
            },
        );
        self.queue.submit(Some(encoder.finish()));
        output.present();
        Ok(PresentOutcome::Submitted)
    }

    fn present_background(&mut self) -> Result<PresentOutcome, RendererError> {
        match self.configure_surface_from_window(false) {
            SurfaceSizeAction::Skip => return Ok(PresentOutcome::Skipped),
            SurfaceSizeAction::Reconfigure(_, _) => {
                return Ok(PresentOutcome::SurfaceReconfigured(
                    RenderRecoveryReason::SurfaceOutdated,
                ));
            }
            SurfaceSizeAction::Keep => {}
        }
        let output = match self.surface.get_current_texture() {
            Ok(output) => output,
            Err(wgpu::SurfaceError::Lost) => {
                if self.configure_surface_from_window(true) == SurfaceSizeAction::Skip {
                    return Ok(PresentOutcome::Skipped);
                }
                return Ok(PresentOutcome::SurfaceReconfigured(
                    RenderRecoveryReason::SurfaceLost,
                ));
            }
            Err(wgpu::SurfaceError::Outdated) => {
                if self.configure_surface_from_window(true) == SurfaceSizeAction::Skip {
                    return Ok(PresentOutcome::Skipped);
                }
                return Ok(PresentOutcome::SurfaceReconfigured(
                    RenderRecoveryReason::SurfaceOutdated,
                ));
            }
            Err(wgpu::SurfaceError::Timeout) => return Ok(PresentOutcome::Timeout),
            Err(wgpu::SurfaceError::OutOfMemory) => {
                return Err(RendererError::DeviceLost {
                    reason: RenderRecoveryReason::OutOfMemory,
                    message: "surface reported out-of-memory while presenting startup background"
                        .to_owned(),
                });
            }
        };
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("panea-startup-background-encoder"),
            });
        let background = surface_background_color(self.transparent, self.background);
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("panea-startup-background-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(surface_clear_color_for_alpha_mode(
                            background,
                            self.config.format,
                            self.alpha_mode,
                        )),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });
        }
        self.queue.submit(Some(encoder.finish()));
        output.present();
        Ok(PresentOutcome::Submitted)
    }
}

fn surface_background_color(transparent: bool, mut background: RenderColor) -> RenderColor {
    if !transparent {
        background.alpha = u8::MAX;
    }
    background
}

fn surface_clear_color(color: RenderColor, format: wgpu::TextureFormat) -> wgpu::Color {
    let convert = |channel: u8| {
        let encoded = f64::from(channel) / 255.0;
        if format.is_srgb() {
            if encoded <= 0.04045 {
                encoded / 12.92
            } else {
                ((encoded + 0.055) / 1.055).powf(2.4)
            }
        } else {
            encoded
        }
    };
    wgpu::Color {
        r: convert(color.red),
        g: convert(color.green),
        b: convert(color.blue),
        a: f64::from(color.alpha) / 255.0,
    }
}

fn surface_clear_color_for_alpha_mode(
    color: RenderColor,
    format: wgpu::TextureFormat,
    alpha_mode: wgpu::CompositeAlphaMode,
) -> wgpu::Color {
    let mut clear = surface_clear_color(color, format);
    if alpha_mode == wgpu::CompositeAlphaMode::PreMultiplied {
        clear.r *= clear.a;
        clear.g *= clear.a;
        clear.b *= clear.a;
    }
    clear
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PresentOutcome {
    Submitted,
    SurfaceReconfigured(RenderRecoveryReason),
    Timeout,
    Skipped,
}

fn present_outcome_requires_full_redraw(outcome: PresentOutcome) -> bool {
    !matches!(outcome, PresentOutcome::Submitted)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SurfaceSizeAction {
    Skip,
    Keep,
    Reconfigure(u32, u32),
}

fn surface_size_action(configured: (u32, u32), current: (u32, u32)) -> SurfaceSizeAction {
    if current.0 == 0 || current.1 == 0 {
        SurfaceSizeAction::Skip
    } else if configured == current {
        SurfaceSizeAction::Keep
    } else {
        SurfaceSizeAction::Reconfigure(current.0, current.1)
    }
}

fn map_device_lost_reason(reason: wgpu::DeviceLostReason) -> Option<RenderRecoveryReason> {
    match reason {
        wgpu::DeviceLostReason::Unknown | wgpu::DeviceLostReason::DeviceInvalid => {
            Some(RenderRecoveryReason::DeviceLost)
        }
        wgpu::DeviceLostReason::Destroyed
        | wgpu::DeviceLostReason::Dropped
        | wgpu::DeviceLostReason::ReplacedCallback => None,
    }
}

fn device_loss_signal_from_uncaptured(error: &wgpu::Error) -> DeviceLossSignal {
    DeviceLossSignal {
        reason: match error {
            wgpu::Error::OutOfMemory { .. } => RenderRecoveryReason::OutOfMemory,
            wgpu::Error::Validation { .. } | wgpu::Error::Internal { .. } => {
                RenderRecoveryReason::DeviceLost
            }
        },
        message: format!("uncaptured wgpu error: {error}"),
    }
}
