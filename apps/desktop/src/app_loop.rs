// Desktop event loop, frame scheduling, window state, and live runtime policy.

fn run(gui_smoke: Option<GuiSmokeOptions>) -> Result<(), Box<dyn Error>> {
    let startup_probe_started = gui_smoke.as_ref().map(|_| Instant::now());
    if gui_smoke.is_some() {
        eprintln!("gui-smoke milestone=config-load-start");
    }
    let loaded_config = load_desktop_config()?;
    if let Some(gui_smoke) = gui_smoke.as_ref() {
        let elapsed = startup_probe_started.map_or(Duration::ZERO, |started| started.elapsed());
        if let Ok(mut report) = gui_smoke.report.lock() {
            report.config_loaded = Some(elapsed);
        }
        eprintln!(
            "gui-smoke milestone=config-loaded elapsed_ms={}",
            elapsed.as_millis()
        );
    }
    log_config_diagnostics(&loaded_config.diagnostics);
    let mut config = loaded_config.config;
    let mut config_revision = 1_u64;
    if let Some(backend) = gui_smoke
        .as_ref()
        .and_then(|options| options.renderer_backend_override)
    {
        config.renderer.backend = backend;
    }
    let mut configured_performance = config.performance.clone();
    let mut power_monitor =
        DesktopPowerMonitor::with_enabled(config.performance.disable_expensive_effects_on_battery);
    let startup_power_state = power_monitor.power_state();
    if let Some(gui_smoke) = gui_smoke.as_ref()
        && let Ok(mut report) = gui_smoke.report.lock()
    {
        report.power_source = Some(power_source_label(startup_power_state.state.source));
        report.charge_percent = startup_power_state.state.charge_percent;
    }
    apply_power_policy(
        &mut config.performance,
        &configured_performance,
        startup_power_state.state,
    );
    log_power_policy(&config.performance, &startup_power_state);
    let cursor_asset_base_dir = loaded_config.asset_base_dir;
    let watcher_to_spawn = loaded_config.watcher;
    let _ssh_session_profiles: Vec<SshConnectionProfile> = config
        .ssh_profiles
        .iter()
        .map(ssh_connection_profile)
        .collect();
    let settings = window_settings(&config);
    let event_loop = create_event_loop(settings.linux_backend)?;
    let transport_waker = TransportWakeHandle::new({
        let event_loop_proxy = event_loop.create_proxy();
        move || {
            let _ = event_loop_proxy.send_event(());
        }
    });
    // Watching runs on its own thread so a config edit reaches the event loop
    // even while the window is idle, and never reads the filesystem on the
    // render thread.
    let mut config_watcher = watcher_to_spawn
        .map(|watcher| DesktopConfigWatchThread::spawn(watcher, transport_waker.clone()));
    let desktop_window = DesktopWindow::create(&event_loop, &settings)?;
    if let Some(gui_smoke) = gui_smoke.as_ref() {
        let elapsed = startup_probe_started.map_or(Duration::ZERO, |started| started.elapsed());
        if let Ok(mut report) = gui_smoke.report.lock() {
            report.window_created = Some(elapsed);
        }
        eprintln!(
            "gui-smoke milestone=window-created elapsed_ms={}",
            elapsed.as_millis()
        );
    }
    if let Some(fallback) = desktop_window.diagnostics().window_mode.fallback.as_ref() {
        eprintln!(
            "platform fallback [{}]: requested={} effective={} reason={}",
            fallback.feature, fallback.requested, fallback.effective, fallback.reason
        );
    }
    if let Some(fallback) = desktop_window.diagnostics().decoration.fallback.as_ref() {
        eprintln!(
            "platform fallback [{}]: requested={} effective={} reason={}",
            fallback.feature, fallback.requested, fallback.effective, fallback.reason
        );
    }
    if let Some(fallback) = desktop_window
        .diagnostics()
        .linux
        .as_ref()
        .and_then(|diagnostic| diagnostic.fallback.as_ref())
    {
        eprintln!(
            "platform fallback [{}]: requested={} effective={} reason={}",
            fallback.feature, fallback.requested, fallback.effective, fallback.reason
        );
    }
    let window = desktop_window.window();
    let capabilities = platform_capabilities(&event_loop, &window);
    let _diagnostics =
        DesktopDiagnosticsPlaceholder::new(desktop_window.diagnostics().clone(), capabilities);
    let mut input_translator = InputTranslator::new();
    let mut clipboard = ClipboardBridge::new();
    let mut notification_provider = DesktopNotificationProvider::new(config.notifications.enabled);
    let mut url_opener = DesktopUrlOpener::new();
    let mut current_window_mode = desktop_window.diagnostics().window_mode.effective;
    let decoration_mode = map_decoration_mode(config.window.decoration_strategy);
    let mut clipboard_config = config.clipboard.clone();
    let mut paste_config = config.paste.clone();
    let mut osc52_policy = osc52_policy(&clipboard_config);

    let mut dpi_scale_factor = window.scale_factor();
    // Start font discovery from the files that satisfied the last launch. A full
    // system scan parses every installed font, which is seconds on a cold file
    // cache; the catalog falls back to that scan by itself the first time a query
    // misses, so a stale or absent cache costs correctness nothing.
    let font_cache = font_cache_path();
    let font_signature = font_directory_signature();
    let mut fonts = FontSystem::with_font_files(
        font_config(&config.font),
        dpi_scale_factor,
        &cached_font_files(&font_cache, &font_signature),
    );
    // Fallback faces load off the UI thread, so the frame that first prints a
    // CJK or emoji character draws without them. This wake brings us back to
    // redraw it once the real face is resident.
    fonts.set_font_load_waker(FontLoadWaker::new({
        let event_loop_proxy = event_loop.create_proxy();
        move || {
            let _ = event_loop_proxy.send_event(());
        }
    }));
    let mut metrics = fonts.cell_metrics()?;
    // Cell metrics force the primary face, so by here we know what was used.
    store_font_files(&font_cache, &font_signature, &fonts.resolved_font_files());
    if let Some(gui_smoke) = gui_smoke.as_ref() {
        let elapsed = startup_probe_started.map_or(Duration::ZERO, |started| started.elapsed());
        if let Ok(mut report) = gui_smoke.report.lock() {
            report.fonts_ready = Some(elapsed);
        }
        eprintln!(
            "gui-smoke milestone=fonts-ready elapsed_ms={}",
            elapsed.as_millis()
        );
    }
    // Start the transport as soon as cell metrics are available. PTY startup
    // and initial shell output can then overlap GPU adapter/device creation.
    let mut surface_size = window.inner_size();
    let mut mux_runtime = MuxRuntime::new(
        &config,
        metrics,
        surface_size.width,
        surface_size.height,
        transport_waker.clone(),
    );
    if let Some(gui_smoke) = gui_smoke.as_ref() {
        let elapsed = startup_probe_started.map_or(Duration::ZERO, |started| started.elapsed());
        if let Ok(mut report) = gui_smoke.report.lock() {
            report.session_created = Some(elapsed);
        }
        eprintln!(
            "gui-smoke milestone=session-created elapsed_ms={}",
            elapsed.as_millis()
        );
    }
    let mut renderer = pollster::block_on(GpuTerminalRenderer::new(
        Arc::clone(&window),
        renderer_options(&config),
    ))?;
    if let Some(gui_smoke) = gui_smoke.as_ref() {
        let elapsed = startup_probe_started.map_or(Duration::ZERO, |started| started.elapsed());
        if let Ok(mut report) = gui_smoke.report.lock() {
            report.renderer_initialized = Some(elapsed);
            report.renderer = renderer.startup_diagnostics().cloned();
        }
        eprintln!(
            "gui-smoke milestone=renderer-initialized elapsed_ms={}",
            elapsed.as_millis()
        );
    }
    let startup_background_presented = match renderer.present_startup_background() {
        Ok(()) => true,
        Err(error) => {
            eprintln!(
                "renderer startup background fallback: {error}; revealing the window for normal first-frame rendering"
            );
            false
        }
    };
    if config.renderer.damage_tracking {
        let retained_status = renderer.retained_damage_status();
        if retained_status != RetainedDamageStatus::Enabled {
            eprintln!(
                "renderer fallback: retained damage presentation is {retained_status}; using event-driven full-frame GPU batches"
            );
        }
    }
    if let Some(gui_smoke) = gui_smoke.as_ref() {
        let elapsed = startup_probe_started.map_or(Duration::ZERO, |started| started.elapsed());
        if let Ok(mut report) = gui_smoke.report.lock() {
            if startup_background_presented {
                report.startup_background_presented = Some(elapsed);
            }
            report.renderer_created = Some(elapsed);
            report.renderer = renderer.startup_diagnostics().cloned();
        }
        eprintln!(
            "gui-smoke milestone=renderer-created elapsed_ms={}",
            elapsed.as_millis()
        );
    }
    if config.window.opacity < 1.0 && !renderer.transparency_active() {
        eprintln!(
            "window opacity fallback: GPU/window backend exposes only opaque composition; rendering remains fully opaque"
        );
    }
    let mut fullscreen_chrome = FullscreenChromeController::new(fullscreen_chrome_settings(
        &config,
        surface_size.width,
        dpi_scale_factor,
        renderer.retained_damage_status() == RetainedDamageStatus::Enabled,
    ));
    let _ = fullscreen_chrome.set_active(fullscreen_chrome_mode_active(current_window_mode));
    let mut fullscreen_chrome_instrumentation = FullscreenChromeInstrumentation::new(
        config.window.fullscreen_titlebar.enabled
            && fullscreen_chrome_mode_active(current_window_mode),
    );
    let mut scheduler = FrameScheduler::new();
    let mut damage_tracker = DamageTracker::new();
    let mut scene_cache = SceneCache::default();
    let mut performance_overlay_ui = PerformanceOverlayUiState::new(&config.diagnostics);
    let mut performance_overlay = PerformanceOverlay::new(performance_overlay_ui.enabled, "wgpu");
    update_performance_overlay_context(
        &mut performance_overlay,
        &config,
        startup_power_state.state,
    );
    let mut performance_budget = performance_budget_from_config(&config);
    let mut cursor_animator = CursorAnimationRuntime::new();
    let mut animation_frame_pacer = AnimationFramePacer::new();
    let mut cursor_blink = CursorBlinkRuntime::new();
    let mut window_focused = true;
    let mut pointer_visible = true;
    let mut pending_terminal_resize = PendingTerminalResize::default();
    let mut cursor_image_cache = AnimatedCursorImageCache::new();
    let mut cursor_image_runtime = AnimatedCursorImageRuntime::new();
    let mut cursor_image_status_reported: Option<String> = None;
    let mut cursor_vector_cache = CursorVectorCache::new();
    let mut cursor_vector_runtime = CursorVectorRuntime::new();
    let mut cursor_vector_status_reported: Option<String> = None;
    request_cursor_image_if_enabled(
        &mut cursor_image_cache,
        &config,
        cursor_asset_base_dir.as_deref(),
    );
    request_cursor_vector_if_enabled(
        &mut cursor_vector_cache,
        &config,
        cursor_asset_base_dir.as_deref(),
    );

    input_translator.arm_initial_focus_handoff();
    window.set_visible(true);
    scheduler.terminal_content_changed();
    let gui_smoke_deadline = gui_smoke
        .as_ref()
        .map(|smoke| Instant::now() + smoke.timeout);
    let gui_smoke_mode = gui_smoke.as_ref().map(|smoke| smoke.mode);
    let gui_smoke_hold = gui_smoke
        .as_ref()
        .map_or(Duration::ZERO, |smoke| smoke.hold_after_success);
    let gui_smoke_report = gui_smoke.as_ref().map(|smoke| Arc::clone(&smoke.report));
    let gui_smoke_completed = gui_smoke.map(|smoke| smoke.completed);
    let gui_smoke_result = gui_smoke_completed.clone();
    let mut gui_smoke_command_sent = false;
    let mut gui_smoke_input_prompt_observed_at = None;
    let mut gui_smoke_startup_prompt_observed_at = None;
    let mut gui_smoke_startup_validated = false;
    let mut gui_smoke_success_presented = false;
    let mut gui_smoke_hold_until = None;
    let mut ime_cursor_area = ImeCursorAreaTracker::default();
    let mut consumed_keys = HashSet::new();

    // Winit 0.30 keeps the closure API as a migration bridge; moving this large state machine to
    // ApplicationHandler is a separate architectural change from the macOS compatibility update.
    #[allow(deprecated)]
    event_loop.run(move |event, target| {
        target.set_control_flow(ControlFlow::Wait);

        match event {
            Event::WindowEvent { event, window_id } if window_id == window.id() => match event {
                WindowEvent::RedrawRequested => {
                    let scene_preparation_started =
                        gui_smoke_report.as_ref().map(|_| Instant::now());
                    let frame_reason = match scheduler.next_frame() {
                        FrameDecision::FrameNeeded(reason) => reason,
                        FrameDecision::NoFrameNeeded => FrameRequestReason::Explicit,
                    };
                    // Collect fallback faces that finished loading in the
                    // background before anything is shaped, so this frame draws
                    // the real glyphs rather than repeating the tofu.
                    if fonts.poll_loaded_fonts() {
                        damage_tracker.request_full_redraw();
                    }
                    let refreshed_metrics = fonts.cell_metrics().ok();
                    if let Some(current_metrics) = refreshed_metrics {
                        metrics = current_metrics;
                    }
                    let metrics = refreshed_metrics;
                    let observed_size = window.inner_size();
                    if surface_size_is_renderable(observed_size) && observed_size != surface_size {
                        surface_size = observed_size;
                        renderer.resize(surface_size.width, surface_size.height);
                        pending_terminal_resize.queue(surface_size);
                        damage_tracker.request_full_redraw();
                    }
                    let other_animation_active = cursor_image_runtime.next_frame_after().is_some()
                        || fullscreen_chrome.next_deadline().is_some();
                    let reuse_cursor_scene = should_reuse_scene_for_cursor_animation(
                        frame_reason,
                        scene_cache.has_scene(),
                        cursor_animator.needs_frame(),
                        other_animation_active,
                        performance_overlay.is_enabled(),
                    );
                    if reuse_cursor_scene {
                        let scene = scene_cache.scene_mut();
                        if let Some(metrics) = metrics {
                            cursor_animator.refresh_retained_scene(
                                scene,
                                metrics,
                                cursor_animation_settings(&config),
                            );
                        }
                    } else {
                        scene_cache.prepare(
                            &mux_runtime,
                            metrics,
                            &config,
                            config_revision,
                            Some(&mut cursor_animator),
                            Some(&mut cursor_image_runtime),
                            Some(&mut cursor_vector_runtime),
                            CursorPresentation {
                                blink_visible: cursor_blink.visible(),
                                window_focused,
                            },
                        );
                        let scene = scene_cache.scene_mut();
                        append_fullscreen_chrome_visual(
                            scene,
                            &fullscreen_chrome,
                            &config.window.title,
                            config.window.fullscreen_titlebar.show_window_controls,
                        );
                        if let Some(metrics) = metrics {
                            append_performance_overlay(
                                scene,
                                &performance_overlay,
                                &performance_overlay_ui,
                                performance_budget,
                                metrics,
                            );
                        }
                    }
                    let scene_update = if reuse_cursor_scene {
                        SceneCacheUpdate::default()
                    } else {
                        scene_cache.last_update()
                    };
                    let scene = scene_cache.scene_mut();
                    if let Some(metrics) = metrics {
                        if let Some(cursor) = scene.cursor {
                            let x = scene.content_offset.x.max(0) as f64
                                + f64::from(cursor.position.col) * f64::from(metrics.cell_width);
                            let y = scene.content_offset.y.max(0) as f64
                                + cursor.position.row.max(0) as f64
                                    * f64::from(metrics.cell_height);
                            let area = ImeCursorArea::new(
                                x,
                                y,
                                f64::from(metrics.cell_width),
                                f64::from(metrics.cell_height),
                            );
                            if ime_cursor_area.update(area) {
                                window.set_ime_cursor_area(
                                    winit::dpi::PhysicalPosition::new(area.x, area.y),
                                    winit::dpi::PhysicalSize::new(area.width, area.height),
                                );
                            }
                        }
                        scene.damage_regions = if reuse_cursor_scene {
                            damage_tracker.update_animations_only(scene, metrics)
                        } else {
                            damage_tracker.update(scene, metrics)
                        };
                    }
                    let first_scene_preparation =
                        scene_preparation_started.map(|started| started.elapsed());
                    let idle_wakeups = scheduler.take_idle_wakeups();
                    let render_submission_started =
                        gui_smoke_report.as_ref().map(|_| Instant::now());
                    let render_result = catch_unwind(AssertUnwindSafe(|| {
                        if reuse_cursor_scene
                            && let Some(metrics) = metrics
                            && renderer.render_cursor_overlay(
                                &CursorOverlayFrame::from_scene(scene),
                                metrics,
                            )?
                        {
                            return Ok(());
                        }
                        renderer.render_scene(scene, &mut fonts)
                    }));
                    let first_render_submission =
                        render_submission_started.map(|started| started.elapsed());
                    match render_result {
                        Ok(Ok(())) => {
                            let mut instrumentation = renderer.last_instrumentation();
                            instrumentation.idle_wakeups = idle_wakeups;
                            instrumentation.scene_layout_cache_hits = scene_update.layout_hits;
                            instrumentation.scene_layout_builds = scene_update.layout_builds;
                            instrumentation.scene_rows_rebuilt = scene_update.rows_rebuilt;
                            instrumentation.scene_rows_reused = scene_update.rows_reused;
                            fullscreen_chrome_instrumentation.record_presented_frame(
                                &scene.damage_regions,
                                instrumentation,
                            );
                            if performance_overlay.is_enabled() {
                                mux_runtime.populate_performance_sample(&mut instrumentation);
                            }
                            let had_performance_sample = performance_overlay.latest().is_some();
                            performance_overlay.record(instrumentation);
                            if performance_overlay.is_enabled() && !had_performance_sample {
                                scheduler.terminal_content_changed();
                                window.request_redraw();
                            }
                            if matches!(config.diagnostics.log_level, LogLevel::Trace)
                                && let Some(text) =
                                    performance_overlay.render_text(performance_budget)
                            {
                                eprintln!("performance {text}");
                            }
                            if let Some(completed) = &gui_smoke_completed {
                                let milestone_reached = match gui_smoke_mode {
                                    Some(GuiSmokeMode::Startup) => gui_smoke_startup_validated,
                                    Some(GuiSmokeMode::TerminalIo) => {
                                        gui_smoke_command_sent
                                            && mux_runtime
                                                .active_visible_text()
                                                .matches(GUI_SMOKE_MARKER)
                                                .count()
                                                >= 2
                                    }
                                    Some(GuiSmokeMode::InputEcho) => {
                                        gui_smoke_command_sent
                                            && mux_runtime
                                                .active_visible_text()
                                                .contains(GUI_INPUT_ECHO_MARKER)
                                    }
                                    Some(GuiSmokeMode::FirstFrame) | None => true,
                                };
                                if milestone_reached && !gui_smoke_success_presented {
                                    eprintln!("gui-smoke milestone=frame-presented");
                                    gui_smoke_success_presented = true;
                                    if let Some(report) = &gui_smoke_report
                                        && let Ok(mut report) = report.lock()
                                    {
                                        if report.first_scene_preparation.is_none() {
                                            report.first_scene_preparation = first_scene_preparation;
                                        }
                                        if report.first_render_submission.is_none() {
                                            report.first_render_submission = first_render_submission;
                                        }
                                        let presented = startup_probe_started
                                            .map(|started| started.elapsed());
                                        if matches!(
                                            gui_smoke_mode,
                                            Some(
                                                GuiSmokeMode::InputEcho
                                                    | GuiSmokeMode::TerminalIo
                                            )
                                        ) && report.input_observed.is_none()
                                        {
                                            report.input_observed = presented;
                                        }
                                        report.success_frame_presented = presented;
                                    }
                                    if gui_smoke_hold.is_zero() {
                                        completed.store(true, Ordering::Release);
                                        mux_runtime.shutdown_all();
                                        target.exit();
                                    } else {
                                        gui_smoke_hold_until = Some(Instant::now() + gui_smoke_hold);
                                    }
                                }
                            }
                        }
                        Ok(Err(error)) => match error {
                            RendererError::DeviceLost { reason, message } => {
                                eprintln!("render device lost ({reason:?}): {message}");
                                match pollster::block_on(renderer.recover_from_device_loss(reason))
                                {
                                    Ok(event) => {
                                        eprintln!("render recovery: {}", event.message);
                                        let chrome_update = sync_fullscreen_chrome(
                                            &mut fullscreen_chrome,
                                            &mut fullscreen_chrome_instrumentation,
                                            &config,
                                            current_window_mode,
                                            surface_size.width,
                                            dpi_scale_factor,
                                            renderer.retained_damage_status()
                                                == RetainedDamageStatus::Enabled,
                                        );
                                        request_fullscreen_chrome_frame(
                                            chrome_update.redraw,
                                            &mut fullscreen_chrome_instrumentation,
                                            &mut scheduler,
                                        );
                                        damage_tracker.request_full_redraw();
                                        scheduler.terminal_content_changed();
                                        window.request_redraw();
                                    }
                                    Err(recovery_error) => {
                                        eprintln!("render recovery failed: {recovery_error}");
                                    }
                                }
                            }
                            error => {
                                eprintln!("render error: {error}");
                            }
                        },
                        Err(panic) => {
                            eprintln!("render panic boundary: {}", panic_payload(panic));
                            scheduler.terminal_content_changed();
                        }
                    }
                }
                _ => {
                    let platform_events = match catch_unwind(AssertUnwindSafe(|| {
                        input_translator.translate_window_event(&event)
                    })) {
                        Ok(events) => events,
                        Err(panic) => {
                            eprintln!("platform event panic boundary: {}", panic_payload(panic));
                            Vec::new()
                        }
                    };
                    if platform_events
                        .iter()
                        .any(|event| matches!(event, InputEvent::Key(_) | InputEvent::Ime(_)))
                    {
                        // Apply synchronous terminal query responses before
                        // keyboard/IME input can overtake them. Pointer and
                        // unrelated window events never poll transport output.
                        let poll = mux_runtime.poll_outputs(
                            &mut clipboard,
                            &mut notification_provider,
                            MuxPollContext {
                                osc52_policy: &osc52_policy,
                                clipboard_config: &clipboard_config,
                                notification_config: &config.notifications,
                                window_focused,
                                metrics,
                                config: &config,
                            },
                        );
                        if poll.exit_application {
                            mux_runtime.shutdown_all();
                            target.exit();
                            return;
                        }
                        if poll.content_changed {
                            record_gui_smoke_input_observed(
                                gui_smoke_mode,
                                gui_smoke_command_sent,
                                &mux_runtime,
                                gui_smoke_report.as_ref(),
                                startup_probe_started,
                            );
                            scheduler.terminal_content_changed();
                            window.request_redraw();
                        }
                    }
                    for platform_event in platform_events {
                        match platform_event {
                            InputEvent::CloseRequested => {
                                mux_runtime.shutdown_all();
                                target.exit();
                            }
                            InputEvent::Resized { width, height } => {
                                let resized = winit::dpi::PhysicalSize::new(width, height);
                                if !surface_size_is_renderable(resized) {
                                    continue;
                                }
                                surface_size = resized;
                                if let Err(panic) = catch_unwind(AssertUnwindSafe(|| {
                                    renderer.resize(width, height)
                                })) {
                                    eprintln!(
                                        "renderer resize panic boundary: {}",
                                        panic_payload(panic)
                                    );
                                }
                                pending_terminal_resize.queue(resized);
                                let chrome_update = sync_fullscreen_chrome(
                                    &mut fullscreen_chrome,
                                    &mut fullscreen_chrome_instrumentation,
                                    &config,
                                    current_window_mode,
                                    surface_size.width,
                                    dpi_scale_factor,
                                    renderer.retained_damage_status()
                                        == RetainedDamageStatus::Enabled,
                                );
                                request_fullscreen_chrome_frame(
                                    chrome_update.redraw,
                                    &mut fullscreen_chrome_instrumentation,
                                    &mut scheduler,
                                );
                                scheduler.window_resized();
                                window.request_redraw();
                            }
                            InputEvent::ScaleFactorChanged { scale_factor } => {
                                let observed_size = window.inner_size();
                                if surface_size_is_renderable(observed_size) {
                                    surface_size = observed_size;
                                }
                                renderer.resize(surface_size.width, surface_size.height);
                                dpi_scale_factor = scale_factor;
                                if fonts.set_scale_factor(scale_factor) {
                                    renderer.request_full_redraw();
                                }
                                if let Ok(current_metrics) = fonts.cell_metrics() {
                                    metrics = current_metrics;
                                }
                                pending_terminal_resize.queue(surface_size);
                                let chrome_update = sync_fullscreen_chrome(
                                    &mut fullscreen_chrome,
                                    &mut fullscreen_chrome_instrumentation,
                                    &config,
                                    current_window_mode,
                                    surface_size.width,
                                    dpi_scale_factor,
                                    renderer.retained_damage_status()
                                        == RetainedDamageStatus::Enabled,
                                );
                                request_fullscreen_chrome_frame(
                                    chrome_update.redraw,
                                    &mut fullscreen_chrome_instrumentation,
                                    &mut scheduler,
                                );
                                if matches!(config.diagnostics.log_level, LogLevel::Debug | LogLevel::Trace) {
                                    eprintln!("DPI scale changed to {scale_factor:.3}");
                                }
                                scheduler.window_resized();
                                window.request_redraw();
                            }
                            InputEvent::Key(key) => {
                                if key.state != KeyState::Pressed {
                                    if take_consumed_key_release(&mut consumed_keys, &key) {
                                        continue;
                                    }
                                    if let Some(bytes) = mux_runtime.input_bytes(&key) {
                                        mux_runtime.write_active(&bytes);
                                    }
                                    continue;
                                }
                                if config.mouse.hide_cursor_when_typing && pointer_visible {
                                    window.set_cursor_visible(false);
                                    pointer_visible = false;
                                }

                                if let Some(changed) = mux_runtime.handle_modal_key(
                                    &key,
                                    &mut clipboard,
                                    &osc52_policy,
                                    &clipboard_config,
                                ) {
                                    remember_consumed_key(&mut consumed_keys, &key);
                                    if changed {
                                        scheduler.terminal_content_changed();
                                        window.request_redraw();
                                    }
                                    continue;
                                }

                                if let Some(action) = keybinding_action(&key, &config) {
                                    remember_consumed_key(&mut consumed_keys, &key);
                                    match action.as_str() {
                                        "copy" => {
                                            if clipboard_config.enabled
                                                && let Some(text) =
                                                    mux_runtime.active_selected_text()
                                            {
                                                copy_text_with_diagnostics(
                                                    &mut clipboard,
                                                    &text,
                                                    &clipboard_config,
                                                    "selection copy",
                                                );
                                            }
                                        }
                                        "paste" => {
                                            if clipboard_config.enabled
                                                && let Ok(text) = clipboard.paste_text()
                                            {
                                                cursor_animator.record_typing();
                                                if cursor_blink.record_activity() {
                                                    scheduler.cursor_blink_changed();
                                                }
                                                mux_runtime.paste_into_active(
                                                    &text,
                                                    &clipboard_config,
                                                    &paste_config,
                                                );
                                            }
                                        }
                                        "scroll_page_up" => {
                                            if mux_runtime.scroll_active_page(true) {
                                                scheduler.terminal_content_changed();
                                                window.request_redraw();
                                            }
                                        }
                                        "scroll_page_down" => {
                                            if mux_runtime.scroll_active_page(false) {
                                                scheduler.terminal_content_changed();
                                                window.request_redraw();
                                            }
                                        }
                                        "scroll_to_top" => {
                                            if mux_runtime.scroll_active_to_top() {
                                                scheduler.terminal_content_changed();
                                                window.request_redraw();
                                            }
                                        }
                                        "scroll_to_bottom" => {
                                            if mux_runtime.scroll_active_to_bottom() {
                                                scheduler.terminal_content_changed();
                                                window.request_redraw();
                                            }
                                        }
                                        "search_scrollback" => {
                                            mux_runtime.start_search();
                                            scheduler.terminal_content_changed();
                                            window.request_redraw();
                                        }
                                        "keyboard_select" => {
                                            mux_runtime.start_keyboard_selection(
                                                SelectionKind::Normal,
                                            );
                                            scheduler.terminal_content_changed();
                                            window.request_redraw();
                                        }
                                        "keyboard_select_rectangular" => {
                                            mux_runtime.start_keyboard_selection(
                                                SelectionKind::Rectangular,
                                            );
                                            scheduler.terminal_content_changed();
                                            window.request_redraw();
                                        }
                                        "jump_to_previous_command" => {
                                            if config.command_blocks.jump_actions_enabled {
                                                let _ = mux_runtime.run_semantic_action(
                                                    SemanticAction::JumpToPreviousCommand,
                                                );
                                                scheduler.terminal_content_changed();
                                                window.request_redraw();
                                            }
                                        }
                                        "jump_to_next_command" => {
                                            if config.command_blocks.jump_actions_enabled {
                                                let _ = mux_runtime.run_semantic_action(
                                                    SemanticAction::JumpToNextCommand,
                                                );
                                                scheduler.terminal_content_changed();
                                                window.request_redraw();
                                            }
                                        }
                                        "select_current_command_output" => {
                                            if config.command_blocks.copy_actions_enabled {
                                                let _ = mux_runtime.run_semantic_action(
                                                    SemanticAction::SelectCurrentCommandOutput,
                                                );
                                                scheduler.terminal_content_changed();
                                                window.request_redraw();
                                            }
                                        }
                                        "copy_current_command_output" => {
                                            if config.command_blocks.copy_actions_enabled
                                                && let SemanticActionResult::Text(text) = mux_runtime
                                                .run_semantic_action(
                                                    SemanticAction::CopyCurrentCommandOutput,
                                                )
                                            {
                                                copy_text_with_diagnostics(
                                                    &mut clipboard,
                                                    &text,
                                                    &clipboard_config,
                                                    "semantic command output copy",
                                                );
                                            }
                                        }
                                        "copy_command_and_output" => {
                                            if config.command_blocks.copy_actions_enabled
                                                && let SemanticActionResult::Text(text) = mux_runtime
                                                .run_semantic_action(
                                                    SemanticAction::CopyCommandAndOutput,
                                                )
                                            {
                                                copy_text_with_diagnostics(
                                                    &mut clipboard,
                                                    &text,
                                                    &clipboard_config,
                                                    "semantic command and output copy",
                                                );
                                            }
                                        }
                                        "toggle_current_command_output" => {
                                            if mux_runtime.toggle_current_command_output(&config) {
                                                scheduler.terminal_content_changed();
                                                window.request_redraw();
                                            }
                                        }
                                        "reconnect_session" => {
                                            if mux_runtime.reconnect_active(&config, metrics) {
                                                scheduler.terminal_content_changed();
                                                window.request_redraw();
                                            }
                                        }
                                        "toggle_performance_overlay" => {
                                            performance_overlay_ui.toggle();
                                            performance_overlay.set_enabled(
                                                performance_overlay_ui.enabled,
                                            );
                                            let power_state = power_monitor.power_state();
                                            update_performance_overlay_context(
                                                &mut performance_overlay,
                                                &config,
                                                power_state.state,
                                            );
                                            scheduler.terminal_content_changed();
                                            window.request_redraw();
                                        }
                                        "toggle_fullscreen" => {
                                            current_window_mode = if matches!(
                                                current_window_mode,
                                                WindowMode::Windowed
                                            ) {
                                                WindowMode::BorderlessFullscreen
                                            } else {
                                                WindowMode::Windowed
                                            };
                                            current_window_mode = apply_window_mode_logged(
                                                &window,
                                                current_window_mode,
                                                decoration_mode,
                                            );
                                            let chrome_update = sync_fullscreen_chrome(
                                                &mut fullscreen_chrome,
                                                &mut fullscreen_chrome_instrumentation,
                                                &config,
                                                current_window_mode,
                                                surface_size.width,
                                                dpi_scale_factor,
                                                renderer.retained_damage_status()
                                                    == RetainedDamageStatus::Enabled,
                                            );
                                            request_fullscreen_chrome_frame(
                                                chrome_update.redraw,
                                                &mut fullscreen_chrome_instrumentation,
                                                &mut scheduler,
                                            );
                                            scheduler.window_resized();
                                            window.request_redraw();
                                        }
                                        "restore_window_decorations" => {
                                            current_window_mode = WindowMode::Windowed;
                                            current_window_mode = apply_window_mode_logged(
                                                &window,
                                                current_window_mode,
                                                decoration_mode,
                                            );
                                            let chrome_update = sync_fullscreen_chrome(
                                                &mut fullscreen_chrome,
                                                &mut fullscreen_chrome_instrumentation,
                                                &config,
                                                current_window_mode,
                                                surface_size.width,
                                                dpi_scale_factor,
                                                renderer.retained_damage_status()
                                                    == RetainedDamageStatus::Enabled,
                                            );
                                            request_fullscreen_chrome_frame(
                                                chrome_update.redraw,
                                                &mut fullscreen_chrome_instrumentation,
                                                &mut scheduler,
                                            );
                                            scheduler.window_resized();
                                            window.request_redraw();
                                        }
                                        "toggle_frameless" => {
                                            current_window_mode = if matches!(
                                                current_window_mode,
                                                WindowMode::FramelessWindowed
                                            ) {
                                                WindowMode::Windowed
                                            } else {
                                                WindowMode::FramelessWindowed
                                            };
                                            current_window_mode = apply_window_mode_logged(
                                                &window,
                                                current_window_mode,
                                                decoration_mode,
                                            );
                                            let chrome_update = sync_fullscreen_chrome(
                                                &mut fullscreen_chrome,
                                                &mut fullscreen_chrome_instrumentation,
                                                &config,
                                                current_window_mode,
                                                surface_size.width,
                                                dpi_scale_factor,
                                                renderer.retained_damage_status()
                                                    == RetainedDamageStatus::Enabled,
                                            );
                                            request_fullscreen_chrome_frame(
                                                chrome_update.redraw,
                                                &mut fullscreen_chrome_instrumentation,
                                                &mut scheduler,
                                            );
                                            scheduler.window_resized();
                                            window.request_redraw();
                                        }
                                        "close_window" => {
                                            mux_runtime.shutdown_all();
                                            target.exit();
                                        }
                                        "open_command_palette_later" => {
                                            eprintln!(
                                                "command palette action is reserved for a later phase"
                                            );
                                        }
                                        action if action.starts_with("send_bytes:") => {
                                            match parse_send_bytes_action(action) {
                                                Ok(Some(bytes)) => {
                                                    cursor_animator.record_typing();
                                                    if cursor_blink.record_activity() {
                                                        scheduler.cursor_blink_changed();
                                                    }
                                                    mux_runtime.write_active(&bytes);
                                                }
                                                Err(error) => {
                                                    eprintln!(
                                                        "invalid keybinding action '{action}': {error}"
                                                    );
                                                }
                                                Ok(None) => {}
                                            }
                                        }
                                        _ => {
                                            if mux_runtime.handle_profile_mux_action(
                                                &action,
                                                &config,
                                                metrics,
                                                surface_size.width,
                                                surface_size.height,
                                            ) {
                                                scheduler.terminal_content_changed();
                                                window.request_redraw();
                                            } else if let Some(action) = MuxAction::named(&action) {
                                                if mux_runtime.handle_mux_action(
                                                    action,
                                                    &config,
                                                    metrics,
                                                    surface_size.width,
                                                    surface_size.height,
                                                ) {
                                                    scheduler.terminal_content_changed();
                                                    window.request_redraw();
                                                }
                                            } else {
                                                eprintln!("unhandled keybinding action: {action}");
                                            }
                                        }
                                    }
                                } else if let Some(bytes) = mux_runtime.input_bytes(&key) {
                                    cursor_animator.record_typing();
                                    if cursor_blink.record_activity() {
                                        scheduler.cursor_blink_changed();
                                    }
                                    mux_runtime.write_active(&bytes);
                                }
                            }
                            InputEvent::Mouse(mouse) => {
                                if !pointer_visible {
                                    window.set_cursor_visible(true);
                                    pointer_visible = true;
                                }
                                let chrome_route = route_fullscreen_chrome_mouse(
                                    &mut fullscreen_chrome,
                                    mouse,
                                    Instant::now(),
                                );
                                request_fullscreen_chrome_frame(
                                    chrome_route.redraw,
                                    &mut fullscreen_chrome_instrumentation,
                                    &mut scheduler,
                                );
                                if chrome_route.redraw {
                                    window.request_redraw();
                                }
                                if let Some(action) = chrome_route.action {
                                    if matches!(action, WindowChromeAction::LeaveFullscreen) {
                                        current_window_mode = apply_window_mode_logged(
                                            &window,
                                            WindowMode::Windowed,
                                            decoration_mode,
                                        );
                                        let update = fullscreen_chrome.set_active(false);
                                        fullscreen_chrome_instrumentation.set_active(false);
                                        if update.redraw {
                                            scheduler.animation_changed();
                                            window.request_redraw();
                                        }
                                        continue;
                                    }

                                    let diagnostic = apply_window_chrome_action(&window, action);
                                    if let Some(fallback) = diagnostic.fallback.as_ref() {
                                        eprintln!(
                                            "platform fallback [{}]: requested={} effective={} reason={}",
                                            fallback.feature,
                                            fallback.requested,
                                            fallback.effective,
                                            fallback.reason
                                        );
                                    }
                                    if diagnostic.applied {
                                        match action {
                                            WindowChromeAction::Close => {
                                                mux_runtime.shutdown_all();
                                                target.exit();
                                            }
                                            WindowChromeAction::BeginDrag
                                            | WindowChromeAction::Minimize
                                            | WindowChromeAction::LeaveFullscreen => {}
                                        }
                                    }
                                }
                                if chrome_route.consumed {
                                    continue;
                                }
                                if let Ok(metrics) = fonts.cell_metrics() {
                                    if handle_performance_overlay_mouse(
                                        mouse,
                                        &performance_overlay,
                                        &mut performance_overlay_ui,
                                        performance_budget,
                                        metrics,
                                        mux_runtime.surface_cols,
                                        mux_runtime.surface_rows,
                                        &config,
                                    ) {
                                        performance_overlay
                                            .set_enabled(performance_overlay_ui.enabled);
                                        scheduler.terminal_content_changed();
                                        window.request_redraw();
                                        continue;
                                    }
                                    let outcome = mux_runtime.handle_mouse(
                                        mouse,
                                        metrics,
                                        &config,
                                        &clipboard_config,
                                        &paste_config,
                                        &mut clipboard,
                                    );
                                    if let Some(url) = outcome.open_url
                                        && let Err(diagnostic) = url_opener.open_url(&url)
                                    {
                                        eprintln!("URL action failed: {diagnostic:?}");
                                    }
                                    if outcome.changed {
                                        scheduler.terminal_content_changed();
                                        window.request_redraw();
                                    }
                                }
                            }
                            InputEvent::Ime(platform_core::ImeEvent::Commit { text }) => {
                                let _ = mux_runtime.update_active_ime_preedit(String::new(), None);
                                if mux_runtime.append_modal_text(&text)
                                    || mux_runtime.append_search_text(&text)
                                {
                                    scheduler.terminal_content_changed();
                                    window.request_redraw();
                                } else {
                                    cursor_animator.record_typing();
                                    if cursor_blink.record_activity() {
                                        scheduler.cursor_blink_changed();
                                    }
                                    mux_runtime.write_active(text.as_bytes());
                                }
                            }
                            InputEvent::Ime(platform_core::ImeEvent::Preedit { text, cursor }) => {
                                if mux_runtime.update_active_ime_preedit(text, cursor) {
                                    scheduler.terminal_content_changed();
                                    window.request_redraw();
                                }
                            }
                            InputEvent::Ime(platform_core::ImeEvent::Enabled) => {}
                            InputEvent::Ime(platform_core::ImeEvent::Disabled) => {
                                if mux_runtime.update_active_ime_preedit(String::new(), None) {
                                    scheduler.terminal_content_changed();
                                    window.request_redraw();
                                }
                            }
                            InputEvent::Focused(focused) => {
                                if !focused {
                                    consumed_keys.clear();
                                }
                                window_focused = focused;
                                let chrome_update =
                                    fullscreen_chrome.focus_changed(focused, Instant::now());
                                request_fullscreen_chrome_frame(
                                    chrome_update.redraw,
                                    &mut fullscreen_chrome_instrumentation,
                                    &mut scheduler,
                                );
                                if cursor_blink.record_activity() {
                                    scheduler.cursor_blink_changed();
                                }
                                mux_runtime.send_focus_event(focused);
                                scheduler.cursor_blink_changed();
                                window.request_redraw();
                            }
                            InputEvent::WindowAction(action) => match action {
                                WindowAction::ToggleFullscreen => {
                                    current_window_mode =
                                        if matches!(current_window_mode, WindowMode::Windowed) {
                                            WindowMode::BorderlessFullscreen
                                        } else {
                                            WindowMode::Windowed
                                        };
                                    current_window_mode = apply_window_mode_logged(
                                        &window,
                                        current_window_mode,
                                        decoration_mode,
                                    );
                                    let chrome_update = sync_fullscreen_chrome(
                                        &mut fullscreen_chrome,
                                        &mut fullscreen_chrome_instrumentation,
                                        &config,
                                        current_window_mode,
                                        surface_size.width,
                                        dpi_scale_factor,
                                        renderer.retained_damage_status()
                                            == RetainedDamageStatus::Enabled,
                                    );
                                    request_fullscreen_chrome_frame(
                                        chrome_update.redraw,
                                        &mut fullscreen_chrome_instrumentation,
                                        &mut scheduler,
                                    );
                                    scheduler.window_resized();
                                    window.request_redraw();
                                }
                                WindowAction::RestoreWindowDecorations => {
                                    current_window_mode = WindowMode::Windowed;
                                    current_window_mode = apply_window_mode_logged(
                                        &window,
                                        current_window_mode,
                                        decoration_mode,
                                    );
                                    let chrome_update = sync_fullscreen_chrome(
                                        &mut fullscreen_chrome,
                                        &mut fullscreen_chrome_instrumentation,
                                        &config,
                                        current_window_mode,
                                        surface_size.width,
                                        dpi_scale_factor,
                                        renderer.retained_damage_status()
                                            == RetainedDamageStatus::Enabled,
                                    );
                                    request_fullscreen_chrome_frame(
                                        chrome_update.redraw,
                                        &mut fullscreen_chrome_instrumentation,
                                        &mut scheduler,
                                    );
                                    scheduler.window_resized();
                                    window.request_redraw();
                                }
                                WindowAction::ToggleFrameless => {
                                    current_window_mode = if matches!(
                                        current_window_mode,
                                        WindowMode::FramelessWindowed
                                    ) {
                                        WindowMode::Windowed
                                    } else {
                                        WindowMode::FramelessWindowed
                                    };
                                    current_window_mode = apply_window_mode_logged(
                                        &window,
                                        current_window_mode,
                                        decoration_mode,
                                    );
                                    let chrome_update = sync_fullscreen_chrome(
                                        &mut fullscreen_chrome,
                                        &mut fullscreen_chrome_instrumentation,
                                        &config,
                                        current_window_mode,
                                        surface_size.width,
                                        dpi_scale_factor,
                                        renderer.retained_damage_status()
                                            == RetainedDamageStatus::Enabled,
                                    );
                                    request_fullscreen_chrome_frame(
                                        chrome_update.redraw,
                                        &mut fullscreen_chrome_instrumentation,
                                        &mut scheduler,
                                    );
                                    scheduler.window_resized();
                                    window.request_redraw();
                                }
                                WindowAction::CloseWindow => {
                                    mux_runtime.shutdown_all();
                                    target.exit();
                                }
                                WindowAction::OpenCommandPaletteLater => {
                                    eprintln!(
                                        "command palette action is reserved for a later phase"
                                    );
                                }
                            },
                        }
                    }
                }
            },
            Event::UserEvent(()) => {
                transport_waker.clear_pending();
                // A dropped remote session retries on its own schedule; run any
                // retry that has come due before draining output.
                if mux_runtime.drive_automatic_reconnects(&config, metrics) {
                    window.request_redraw();
                }
                // Local PTY readers wake the event loop when bytes become
                // available. Process that wake immediately instead of waiting
                // for AboutToWait, where queued keyboard events could otherwise
                // be handled first.
                let poll = mux_runtime.poll_outputs(
                    &mut clipboard,
                    &mut notification_provider,
                    MuxPollContext {
                        osc52_policy: &osc52_policy,
                        clipboard_config: &clipboard_config,
                        notification_config: &config.notifications,
                        window_focused,
                        metrics,
                        config: &config,
                    },
                );
                if poll.exit_application {
                    mux_runtime.shutdown_all();
                    target.exit();
                    return;
                }
                if poll.content_changed {
                    record_gui_smoke_input_observed(
                        gui_smoke_mode,
                        gui_smoke_command_sent,
                        &mux_runtime,
                        gui_smoke_report.as_ref(),
                        startup_probe_started,
                    );
                    scheduler.terminal_content_changed();
                    window.request_redraw();
                }
            }
            Event::AboutToWait => {
                let now = Instant::now();
                let chrome_update = fullscreen_chrome.tick(now);
                request_fullscreen_chrome_frame(
                    chrome_update.redraw,
                    &mut fullscreen_chrome_instrumentation,
                    &mut scheduler,
                );
                if chrome_update.redraw {
                    window.request_redraw();
                }
                if let Some(size) = pending_terminal_resize.take_due(now) {
                    if let Ok(current_metrics) = fonts.cell_metrics() {
                        metrics = current_metrics;
                    }
                    mux_runtime.resize_all(size.width, size.height, metrics, &config);
                    damage_tracker.request_full_redraw();
                    scheduler.window_resized();
                    window.request_redraw();
                }
                if gui_smoke_hold_until.is_some_and(|deadline| Instant::now() >= deadline) {
                    if let Some(completed) = &gui_smoke_completed {
                        completed.store(true, Ordering::Release);
                    }
                    mux_runtime.shutdown_all();
                    target.exit();
                    return;
                }
                if !gui_smoke_success_presented
                    && gui_smoke_deadline.is_some_and(|deadline| Instant::now() >= deadline)
                {
                    eprintln!("gui-smoke milestone=timeout");
                    if matches!(
                        gui_smoke_mode,
                        Some(
                            GuiSmokeMode::Startup
                                | GuiSmokeMode::InputEcho
                                | GuiSmokeMode::TerminalIo
                        )
                    ) {
                        let preview = mux_runtime.active_visible_text();
                        eprintln!(
                            "gui-smoke terminal-preview={:?} command-sent={gui_smoke_command_sent}",
                            preview.chars().take(1024).collect::<String>()
                        );
                    }
                    mux_runtime.shutdown_all();
                    target.exit();
                    return;
                }
                if let Some(config_watcher) = config_watcher.as_mut() {
                    match config_watcher.poll() {
                        DesktopConfigWatchEvent::Unchanged => {}
                        DesktopConfigWatchEvent::Pending { path } => {
                            if matches!(
                                config.diagnostics.log_level,
                                LogLevel::Debug | LogLevel::Trace
                            ) {
                                eprintln!(
                                    "config reload pending{}",
                                    path.as_ref()
                                        .map(|path| format!(" for {}", path.display()))
                                        .unwrap_or_default()
                                );
                            }
                        }
                        DesktopConfigWatchEvent::Reloaded {
                            config: loaded,
                            diagnostics,
                        } => {
                            let loaded = *loaded;
                            let next_configured_performance = loaded.performance.clone();
                            log_config_diagnostics(&diagnostics);
                            let plan = config.reload_plan_from(&loaded);
                            log_reload_plan(&plan);
                            match apply_live_config_reload(
                                &mut config,
                                loaded,
                                &plan,
                                &mut fonts,
                                &mut clipboard_config,
                                &mut paste_config,
                                &mut osc52_policy,
                                &mut notification_provider,
                                &mut performance_overlay,
                                &mut performance_overlay_ui,
                                &mut performance_budget,
                                &mut renderer,
                                &window,
                                dpi_scale_factor,
                            ) {
                                Ok(reloaded) => {
                                    configured_performance = next_configured_performance;
                                    power_monitor.set_enabled(
                                        configured_performance
                                            .disable_expensive_effects_on_battery,
                                    );
                                    let power_state = power_monitor.power_state();
                                    apply_power_policy(
                                        &mut config.performance,
                                        &configured_performance,
                                        power_state.state,
                                    );
                                    renderer.set_glyph_cache_capacity(
                                        config.performance.glyph_cache_entries,
                                    );
                                    performance_budget = performance_budget_from_config(&config);
                                    update_performance_overlay_context(
                                        &mut performance_overlay,
                                        &config,
                                        power_state.state,
                                    );
                                    request_cursor_image_if_enabled(
                                        &mut cursor_image_cache,
                                        &config,
                                        cursor_asset_base_dir.as_deref(),
                                    );
                                    cursor_image_status_reported = None;
                                    request_cursor_vector_if_enabled(
                                        &mut cursor_vector_cache,
                                        &config,
                                        cursor_asset_base_dir.as_deref(),
                                    );
                                    cursor_vector_status_reported = None;
                                    if reloaded {
                                        config_revision = config_revision.wrapping_add(1).max(1);
                                        mux_runtime.update_terminal_dynamic_colors(&config);
                                        if let Ok(current_metrics) = fonts.cell_metrics() {
                                            metrics = current_metrics;
                                        }
                                        if reload_requires_terminal_resize(&plan)
                                        {
                                            mux_runtime.resize_all(
                                                surface_size.width,
                                                surface_size.height,
                                                metrics,
                                                &config,
                                            );
                                        }
                                        let chrome_update = sync_fullscreen_chrome(
                                            &mut fullscreen_chrome,
                                            &mut fullscreen_chrome_instrumentation,
                                            &config,
                                            current_window_mode,
                                            surface_size.width,
                                            dpi_scale_factor,
                                            renderer.retained_damage_status()
                                                == RetainedDamageStatus::Enabled,
                                        );
                                        request_fullscreen_chrome_frame(
                                            chrome_update.redraw,
                                            &mut fullscreen_chrome_instrumentation,
                                            &mut scheduler,
                                        );
                                        scheduler.terminal_content_changed();
                                        window.request_redraw();
                                    }
                                }
                                Err(message) => {
                                    eprintln!(
                                        "config reload rejected: {message}; keeping previous valid config"
                                    );
                                }
                            }
                        }
                        DesktopConfigWatchEvent::Failed { path, error } => {
                            eprintln!(
                                "config reload failed{}: {error}; keeping previous valid config",
                                path.as_ref()
                                    .map(|path| format!(" for {}", path.display()))
                                    .unwrap_or_default()
                            );
                        }
                    }
                }

                if power_monitor.refresh_if_due() {
                    let power_state = power_monitor.power_state();
                    apply_power_policy(
                        &mut config.performance,
                        &configured_performance,
                        power_state.state,
                    );
                    renderer.set_glyph_cache_capacity(config.performance.glyph_cache_entries);
                    performance_budget = performance_budget_from_config(&config);
                    request_cursor_image_if_enabled(
                        &mut cursor_image_cache,
                        &config,
                        cursor_asset_base_dir.as_deref(),
                    );
                    cursor_image_status_reported = None;
                    request_cursor_vector_if_enabled(
                        &mut cursor_vector_cache,
                        &config,
                        cursor_asset_base_dir.as_deref(),
                    );
                    cursor_vector_status_reported = None;
                    update_performance_overlay_context(
                        &mut performance_overlay,
                        &config,
                        power_state.state,
                    );
                    let chrome_update = sync_fullscreen_chrome(
                        &mut fullscreen_chrome,
                        &mut fullscreen_chrome_instrumentation,
                        &config,
                        current_window_mode,
                        surface_size.width,
                        dpi_scale_factor,
                        renderer.retained_damage_status() == RetainedDamageStatus::Enabled,
                    );
                    request_fullscreen_chrome_frame(
                        chrome_update.redraw,
                        &mut fullscreen_chrome_instrumentation,
                        &mut scheduler,
                    );
                    log_power_policy(&config.performance, &power_state);
                    scheduler.animation_changed();
                    window.request_redraw();
                }

                if matches!(
                    gui_smoke_mode,
                    Some(GuiSmokeMode::InputEcho | GuiSmokeMode::TerminalIo)
                )
                    && !gui_smoke_command_sent
                    && shell_prompt_visible(&mux_runtime.active_visible_text())
                {
                    if let Some(report) = &gui_smoke_report
                        && let Ok(mut report) = report.lock()
                        && report.prompt_observed.is_none()
                    {
                        report.prompt_observed =
                            startup_probe_started.map(|started| started.elapsed());
                    }
                    if gui_smoke_input_settled(
                        &mut gui_smoke_input_prompt_observed_at,
                        Instant::now(),
                    ) {
                        match gui_smoke_mode {
                            Some(GuiSmokeMode::InputEcho) => {
                                mux_runtime.write_active(GUI_INPUT_ECHO_MARKER.as_bytes());
                            }
                            Some(GuiSmokeMode::TerminalIo) => {
                                mux_runtime.write_active(
                                    format!("echo {GUI_SMOKE_MARKER}\r").as_bytes(),
                                );
                            }
                            _ => unreachable!("input smoke mode was checked"),
                        }
                        gui_smoke_command_sent = true;
                        if let Some(report) = &gui_smoke_report
                            && let Ok(mut report) = report.lock()
                        {
                            report.input_sent =
                                startup_probe_started.map(|started| started.elapsed());
                        }
                        eprintln!("gui-smoke milestone=settled-prompt-input-sent");
                    }
                }

                if gui_smoke_mode == Some(GuiSmokeMode::Startup)
                    && !gui_smoke_startup_validated
                {
                    let visible = mux_runtime.active_visible_text();
                    if shell_prompt_visible(&visible) {
                        if let Some(report) = &gui_smoke_report
                            && let Ok(mut report) = report.lock()
                            && report.prompt_observed.is_none()
                        {
                            report.prompt_observed =
                                startup_probe_started.map(|started| started.elapsed());
                        }
                        let observed_at = gui_smoke_startup_prompt_observed_at
                            .get_or_insert_with(Instant::now);
                        let settled_at = *observed_at + Duration::from_millis(500);
                        if Instant::now() >= settled_at {
                            let prompt_count = shell_prompt_line_count(&visible);
                            eprintln!(
                                "gui-smoke startup-prompt-count={prompt_count} terminal-preview={:?}",
                                visible.chars().take(1024).collect::<String>()
                            );
                            if prompt_count != 1 {
                                eprintln!(
                                    "gui-smoke startup failed: expected exactly one prompt without user input"
                                );
                                mux_runtime.shutdown_all();
                                target.exit();
                                return;
                            }
                            gui_smoke_startup_validated = true;
                            scheduler.terminal_content_changed();
                            window.request_redraw();
                        } else {
                            // The shared wake-deadline calculation below includes
                            // this settle deadline without overwriting animation,
                            // transport, hold, or overall smoke deadlines.
                        }
                    }
                }

                match cursor_image_cache.poll() {
                    AnimatedCursorImageStatus::Ready(image) => {
                        if cursor_image_runtime.set_image(&image) {
                            scheduler.animation_changed();
                            window.request_redraw();
                        }
                        let key = format!("ready:{}", image.path.display());
                        if cursor_image_status_reported.as_deref() != Some(&key) {
                            for warning in image.warnings {
                                eprintln!("cursor image warning: {warning}");
                            }
                            cursor_image_status_reported = Some(key);
                        }
                    }
                    AnimatedCursorImageStatus::Failed { path, message } => {
                        if cursor_image_runtime.clear() {
                            scheduler.animation_changed();
                            window.request_redraw();
                        }
                        let key = format!("failed:{}:{message}", path.display());
                        if cursor_image_status_reported.as_deref() != Some(&key) {
                            eprintln!("cursor image {} failed: {message}", path.display());
                            cursor_image_status_reported = Some(key);
                        }
                    }
                    AnimatedCursorImageStatus::Disabled => {
                        if cursor_image_runtime.clear() {
                            scheduler.animation_changed();
                            window.request_redraw();
                        }
                    }
                    AnimatedCursorImageStatus::Loading { .. } => {}
                }

                match cursor_vector_cache.poll() {
                    CursorVectorStatus::Ready(vector) => {
                        if cursor_vector_runtime.set_vector(&vector) {
                            scheduler.terminal_content_changed();
                            window.request_redraw();
                        }
                    }
                    CursorVectorStatus::Failed { path, message } => {
                        if cursor_vector_runtime.clear() {
                            scheduler.terminal_content_changed();
                            window.request_redraw();
                        }
                        let key = format!("{}:{message}", path.display());
                        if cursor_vector_status_reported.as_deref() != Some(&key) {
                            eprintln!("cursor vector {} failed: {message}", path.display());
                            cursor_vector_status_reported = Some(key);
                        }
                    }
                    CursorVectorStatus::Disabled => {
                        if cursor_vector_runtime.clear() {
                            scheduler.terminal_content_changed();
                            window.request_redraw();
                        }
                    }
                    CursorVectorStatus::Loading { .. } => {}
                }

                let cursor_settings = cursor_animation_settings(&config);
                let blink_enabled = window_focused
                    && config.cursor.blink
                    && mux_runtime.active_cursor_blinks();
                if cursor_blink.update(
                    blink_enabled,
                    Duration::from_millis(u64::from(config.cursor.blink_interval_ms)),
                ) {
                    scheduler.cursor_blink_changed();
                }
                let animation_delay = cursor_animator.next_frame_after(cursor_settings);
                let cursor_image_delay = cursor_image_runtime.next_frame_after();
                let visual_animation_delay = [animation_delay, cursor_image_delay]
                    .into_iter()
                    .flatten()
                    .min();
                let blink_delay = cursor_blink.next_frame_after();
                let power_delay = power_monitor.next_refresh_after();
                // Connection establishment has no socket-readiness callback.
                // Connected SSH and local PTYs wake this event loop directly.
                let transport_poll_delay = mux_runtime
                    .requires_periodic_transport_poll()
                    .then_some(Duration::from_millis(50));
                let next_delay = [blink_delay, power_delay, transport_poll_delay]
                .into_iter()
                .flatten()
                .min();
                let mut next_wake = next_delay.map(|delay| now + delay);
                pace_animation_wake(
                    now,
                    visual_animation_delay,
                    &mut animation_frame_pacer,
                    &mut scheduler,
                    &mut next_wake,
                );
                if gui_smoke_mode == Some(GuiSmokeMode::Startup)
                    && !gui_smoke_startup_validated
                    && let Some(observed_at) = gui_smoke_startup_prompt_observed_at
                {
                    retain_earliest_deadline(
                        &mut next_wake,
                        observed_at + Duration::from_millis(500),
                    );
                }
                if matches!(
                    gui_smoke_mode,
                    Some(GuiSmokeMode::InputEcho | GuiSmokeMode::TerminalIo)
                ) && !gui_smoke_command_sent
                    && let Some(observed_at) = gui_smoke_input_prompt_observed_at
                {
                    retain_earliest_deadline(
                        &mut next_wake,
                        observed_at + GUI_INPUT_SETTLE_DELAY,
                    );
                }
                if let Some(deadline) = gui_smoke_deadline {
                    retain_earliest_deadline(&mut next_wake, deadline);
                }
                if let Some(deadline) = gui_smoke_hold_until {
                    retain_earliest_deadline(&mut next_wake, deadline);
                }
                if let Some(deadline) = pending_terminal_resize.deadline() {
                    retain_earliest_deadline(&mut next_wake, deadline);
                }
                if let Some(deadline) = mux_runtime.next_synchronized_output_deadline() {
                    retain_earliest_deadline(&mut next_wake, deadline);
                }
                if let Some(deadline) = fullscreen_chrome.next_deadline() {
                    retain_earliest_deadline(&mut next_wake, deadline);
                }
                if let Some(deadline) = next_wake {
                    target.set_control_flow(ControlFlow::WaitUntil(deadline));
                }

                if scheduler.has_pending_frame() {
                    window.request_redraw();
                }
            }
            _ => {}
        }
    })?;

    if let Err(error) = wait_for_mux_state_saves() {
        eprintln!("mux state save worker did not finish before exit: {error}");
    }

    if gui_smoke_result.is_some() {
        eprintln!("gui-smoke milestone=event-loop-exited");
    }

    Ok(())
}

fn retain_earliest_deadline(current: &mut Option<Instant>, candidate: Instant) {
    if current.is_none_or(|deadline| candidate < deadline) {
        *current = Some(candidate);
    }
}

fn pace_animation_wake(
    now: Instant,
    next_frame_after: Option<Duration>,
    pacer: &mut AnimationFramePacer,
    scheduler: &mut FrameScheduler,
    next_wake: &mut Option<Instant>,
) -> bool {
    match pacer.poll(now, next_frame_after) {
        AnimationFramePacerDecision::Idle => false,
        AnimationFramePacerDecision::WaitUntil(deadline) => {
            retain_earliest_deadline(next_wake, deadline);
            false
        }
        AnimationFramePacerDecision::FrameDue => {
            scheduler.animation_changed();
            true
        }
    }
}

const fn should_reuse_scene_for_cursor_animation(
    reason: FrameRequestReason,
    cached_scene_available: bool,
    cursor_animation_active: bool,
    other_animation_active: bool,
    performance_overlay_active: bool,
) -> bool {
    matches!(reason, FrameRequestReason::Animation)
        && cached_scene_available
        && cursor_animation_active
        && !other_animation_active
        && !performance_overlay_active
}

fn reload_requires_terminal_resize(plan: &ReloadPlan) -> bool {
    plan.live.iter().any(|section| {
        matches!(
            section,
            ReloadableSection::Font | ReloadableSection::Mux | ReloadableSection::WindowPadding
        )
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FullscreenChromeRoute {
    consumed: bool,
    redraw: bool,
    action: Option<WindowChromeAction>,
}

#[derive(Debug, Default)]
struct FullscreenChromeInstrumentation {
    metrics: Option<diagnostics::FullscreenChromePerformanceMetrics>,
    frame_pending: bool,
}

impl FullscreenChromeInstrumentation {
    fn new(active: bool) -> Self {
        Self {
            metrics: active.then(diagnostics::FullscreenChromePerformanceMetrics::default),
            frame_pending: false,
        }
    }

    fn set_active(&mut self, active: bool) {
        if active {
            if self.metrics.is_none() {
                self.metrics = Some(diagnostics::FullscreenChromePerformanceMetrics::default());
            }
        } else {
            self.metrics = None;
            self.frame_pending = false;
        }
    }

    fn mark_frame(&mut self) {
        if self.metrics.is_some() {
            self.frame_pending = true;
        }
    }

    fn record_presented_frame(
        &mut self,
        damage_regions: &[RenderRect],
        instrumentation: RenderInstrumentation,
    ) {
        if !std::mem::take(&mut self.frame_pending) {
            return;
        }
        if let Some(metrics) = self.metrics.as_mut() {
            metrics.record_frame(damage_regions, instrumentation);
        }
    }

    #[cfg(test)]
    const fn metrics(&self) -> Option<&diagnostics::FullscreenChromePerformanceMetrics> {
        self.metrics.as_ref()
    }
}

fn request_fullscreen_chrome_frame(
    redraw: bool,
    instrumentation: &mut FullscreenChromeInstrumentation,
    scheduler: &mut FrameScheduler,
) {
    if redraw {
        instrumentation.mark_frame();
        scheduler.animation_changed();
    }
}

impl FullscreenChromeRoute {
    #[cfg(test)]
    const fn terminal() -> Self {
        Self {
            consumed: false,
            redraw: false,
            action: None,
        }
    }
}

fn route_fullscreen_chrome_mouse(
    chrome: &mut FullscreenChromeController,
    mouse: MouseEvent,
    now: Instant,
) -> FullscreenChromeRoute {
    let point = ChromePoint {
        x: mouse.x,
        y: mouse.y,
    };
    let update = match mouse.kind {
        MouseEventKind::Pressed(MouseButton::Left) => chrome.pointer_button(ChromePointerButton {
            point,
            pressed: true,
            now,
        }),
        MouseEventKind::Released(MouseButton::Left) => chrome.pointer_button(ChromePointerButton {
            point,
            pressed: false,
            now,
        }),
        MouseEventKind::Moved
        | MouseEventKind::Wheel(_)
        | MouseEventKind::Pressed(_)
        | MouseEventKind::Released(_) => chrome.pointer_moved(point, now),
    };
    FullscreenChromeRoute {
        consumed: update.consumed,
        redraw: update.redraw,
        action: update.intent.map(window_chrome_action_for_intent),
    }
}

const fn window_chrome_action_for_intent(intent: ChromeIntent) -> WindowChromeAction {
    match intent {
        ChromeIntent::Minimize => WindowChromeAction::Minimize,
        ChromeIntent::LeaveFullscreen => WindowChromeAction::LeaveFullscreen,
        ChromeIntent::Close => WindowChromeAction::Close,
    }
}

const fn fullscreen_chrome_mode_active(mode: WindowMode) -> bool {
    matches!(
        mode,
        WindowMode::BorderlessFullscreen | WindowMode::FramelessFullscreen
    )
}

fn fullscreen_chrome_settings(
    config: &AppConfig,
    surface_width: u32,
    dpi_scale_factor: f64,
    retained_damage_available: bool,
) -> ChromeSettings {
    let configured = &config.window.fullscreen_titlebar;
    let chrome_height = logical_to_physical_pixels(configured.height, dpi_scale_factor);
    let reveal_height =
        logical_to_physical_pixels(configured.reveal_height, dpi_scale_factor).min(chrome_height);
    let animation_budget_available = config.performance.max_active_animations > 0
        && u64::from(surface_width) * u64::from(chrome_height)
            <= u64::from(config.performance.max_animated_region_pixels);
    let motion = match configured.effective_animation(
        false,
        retained_damage_available,
        animation_budget_available,
    ) {
        FullscreenChromeAnimation::Instant => ChromeMotion::Instant,
        FullscreenChromeAnimation::Smooth => ChromeMotion::Smooth,
    };
    let fps = config
        .performance
        .frame_rate_limit
        .map_or(config.performance.max_animation_fps, |limit| {
            limit.min(config.performance.max_animation_fps)
        })
        .max(1);

    ChromeSettings {
        enabled: configured.enabled,
        surface_width,
        chrome_height,
        reveal_height,
        control_width: if configured.show_window_controls {
            chrome_height
        } else {
            0
        },
        motion,
        transition_duration: Duration::from_millis(u64::from(configured.animation_duration_ms)),
        hide_delay: Duration::from_millis(u64::from(configured.hide_delay_ms)),
        frame_interval: Duration::from_nanos(1_000_000_000u64 / u64::from(fps)),
    }
}

fn logical_to_physical_pixels(logical: u16, scale_factor: f64) -> u32 {
    (f64::from(logical) * scale_factor)
        .round()
        .clamp(1.0, f64::from(u32::MAX)) as u32
}

fn sync_fullscreen_chrome(
    chrome: &mut FullscreenChromeController,
    instrumentation: &mut FullscreenChromeInstrumentation,
    config: &AppConfig,
    mode: WindowMode,
    surface_width: u32,
    dpi_scale_factor: f64,
    retained_damage_available: bool,
) -> ChromeUpdate {
    let settings_update = chrome.update_settings(fullscreen_chrome_settings(
        config,
        surface_width,
        dpi_scale_factor,
        retained_damage_available,
    ));
    let active_update = chrome.set_active(fullscreen_chrome_mode_active(mode));
    instrumentation.set_active(
        config.window.fullscreen_titlebar.enabled && fullscreen_chrome_mode_active(mode),
    );
    ChromeUpdate {
        consumed: settings_update.consumed || active_update.consumed,
        redraw: settings_update.redraw || active_update.redraw,
        intent: active_update.intent.or(settings_update.intent),
    }
}

fn append_fullscreen_chrome_visual(
    scene: &mut RenderScene,
    chrome: &FullscreenChromeController,
    title: &str,
    show_window_controls: bool,
) {
    let Some(presentation) = chrome.presentation() else {
        scene.window_chrome = None;
        return;
    };
    let settings = chrome.settings();
    let visible_height = (u64::from(settings.chrome_height) * u64::from(presentation.progress))
        .div_ceil(u64::from(u16::MAX))
        .min(u64::from(settings.chrome_height)) as u32;
    let y = i64::from(visible_height) - i64::from(settings.chrome_height);
    let y = y.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
    let controls = if show_window_controls {
        [
            (WindowChromeControlKind::Minimize, 3u32),
            (WindowChromeControlKind::LeaveFullscreen, 2u32),
            (WindowChromeControlKind::Close, 1u32),
        ]
        .into_iter()
        .map(|(kind, slot)| {
            let control = match kind {
                WindowChromeControlKind::Minimize => fullscreen_chrome::ChromeControl::Minimize,
                WindowChromeControlKind::LeaveFullscreen => {
                    fullscreen_chrome::ChromeControl::LeaveFullscreen
                }
                WindowChromeControlKind::Close => fullscreen_chrome::ChromeControl::Close,
            };
            WindowChromeControlVisual {
                kind,
                bounds: RenderRect {
                    x: settings
                        .surface_width
                        .saturating_sub(settings.control_width.saturating_mul(slot))
                        .min(i32::MAX as u32) as i32,
                    y,
                    width: settings.control_width,
                    height: settings.chrome_height,
                },
                hovered: presentation.hovered_control == Some(control),
                pressed: presentation.pressed_control == Some(control),
            }
        })
        .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    scene.window_chrome = Some(WindowChromeVisual {
        bounds: RenderRect {
            x: 0,
            y,
            width: settings.surface_width,
            height: settings.chrome_height,
        },
        opacity: presentation.progress,
        title: title.to_owned(),
        show_logo: true,
        controls,
    });
}

fn log_config_diagnostics(diagnostics: &[config_core::ConfigDiagnostic]) {
    for diagnostic in diagnostics {
        let level = match diagnostic.severity {
            ConfigDiagnosticSeverity::Error => "error",
            ConfigDiagnosticSeverity::Warning => "warning",
        };
        eprintln!(
            "config {level} at {}: {}",
            diagnostic.path, diagnostic.message
        );
    }
}

fn log_reload_plan(plan: &ReloadPlan) {
    if !plan.live.is_empty() {
        eprintln!("config reload live sections: {:?}", plan.live);
    }
    for change in &plan.restart_required {
        eprintln!(
            "config reload restart required for {}: {}",
            change.path, change.reason
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_live_config_reload(
    current: &mut AppConfig,
    next: AppConfig,
    plan: &ReloadPlan,
    fonts: &mut FontSystem,
    clipboard_config: &mut ClipboardConfig,
    paste_config: &mut PasteConfig,
    runtime_osc52_policy: &mut Osc52ClipboardPolicy,
    notification_provider: &mut DesktopNotificationProvider,
    performance_overlay: &mut PerformanceOverlay,
    performance_overlay_ui: &mut PerformanceOverlayUiState,
    runtime_performance_budget: &mut PerformanceBudget,
    renderer: &mut GpuTerminalRenderer,
    window: &winit::window::Window,
    dpi_scale_factor: f64,
) -> Result<bool, String> {
    if plan.live.is_empty() {
        return Ok(false);
    }

    if plan.live.contains(&ReloadableSection::Font) {
        let mut reloaded_fonts = fonts.reconfigured(font_config(&next.font), dpi_scale_factor);
        reloaded_fonts
            .cell_metrics()
            .map_err(|error| format!("font reload failed: {error}"))?;
        *fonts = reloaded_fonts;
    }

    for section in &plan.live {
        match section {
            ReloadableSection::Colors => {
                current.colors = next.colors.clone();
                renderer.set_background(window_background(current));
            }
            ReloadableSection::Cursor => current.cursor = next.cursor.clone(),
            ReloadableSection::Diagnostics => {
                current.diagnostics = next.diagnostics.clone();
                performance_overlay_ui.apply_config(&current.diagnostics);
                performance_overlay.set_enabled(performance_overlay_ui.enabled);
            }
            ReloadableSection::Font => current.font = next.font.clone(),
            ReloadableSection::Input => {
                current.mouse = next.mouse.clone();
                current.clipboard = next.clipboard.clone();
                current.paste = next.paste.clone();
                *clipboard_config = current.clipboard.clone();
                *paste_config = current.paste.clone();
                *runtime_osc52_policy = osc52_policy(clipboard_config);
            }
            ReloadableSection::Keybindings => current.keyboard = next.keyboard.clone(),
            ReloadableSection::Mux => current.mux = next.mux.clone(),
            ReloadableSection::Notifications => {
                current.notifications = next.notifications.clone();
                notification_provider.set_enabled(current.notifications.enabled);
            }
            ReloadableSection::Performance => {
                renderer.set_glyph_cache_capacity(next.performance.glyph_cache_entries);
                current.performance = next.performance.clone();
                *runtime_performance_budget = performance_budget_from_config(current);
            }
            ReloadableSection::VisualSemantics => {
                current.visual_theme = next.visual_theme.clone();
                current.command_blocks = next.command_blocks.clone();
                current.prompt_decorations = next.prompt_decorations.clone();
                current.shell_integration = next.shell_integration.clone();
            }
            ReloadableSection::WindowChrome => {
                current.window.fullscreen_titlebar = next.window.fullscreen_titlebar.clone();
                renderer.request_full_redraw();
            }
            ReloadableSection::WindowPadding => {
                current.window.padding_x = next.window.padding_x;
                current.window.padding_y = next.window.padding_y;
                current.window.margin_x = next.window.margin_x;
                current.window.margin_y = next.window.margin_y;
                renderer.request_full_redraw();
            }
            ReloadableSection::WindowTitle => {
                current.window.title = next.window.title.clone();
                window.set_title(&current.window.title);
            }
        }
    }

    eprintln!("config reload applied live sections without restarting sessions");
    Ok(true)
}

fn font_config(config: &config_core::FontConfig) -> RuntimeFontConfig {
    RuntimeFontConfig {
        family: config.family.clone(),
        fallback_families: config.fallback_families.clone(),
        size: config.size as f32,
        line_height: config.line_height as f32,
        ligatures: config.ligatures,
    }
}

fn apply_power_policy(
    effective: &mut PerformanceConfig,
    configured: &PerformanceConfig,
    power: PowerState,
) {
    *effective = configured.clone();
    if !power.is_on_battery() || !configured.disable_expensive_effects_on_battery {
        return;
    }

    let mut battery = PerformanceConfig::default();
    battery.apply_profile(PerformanceProfile::BatterySaver);
    effective.frame_rate_limit = Some(
        effective
            .frame_rate_limit
            .unwrap_or(u16::MAX)
            .min(battery.frame_rate_limit.unwrap_or(30)),
    );
    effective.glyph_cache_entries = effective
        .glyph_cache_entries
        .min(battery.glyph_cache_entries);
    effective.max_animation_fps = effective.max_animation_fps.min(battery.max_animation_fps);
    effective.max_active_animations = effective
        .max_active_animations
        .min(battery.max_active_animations);
    effective.max_animated_region_pixels = effective
        .max_animated_region_pixels
        .min(battery.max_animated_region_pixels);
}

fn update_performance_overlay_context(
    overlay: &mut PerformanceOverlay,
    config: &AppConfig,
    power: PowerState,
) {
    if !overlay.is_enabled() {
        return;
    }
    overlay.set_runtime_context(
        performance_profile_label(config.performance.profile),
        power_source_label(power.source),
    );
}

const fn performance_profile_label(profile: PerformanceProfile) -> &'static str {
    match profile {
        PerformanceProfile::MaximumPerformance => "maximum_performance",
        PerformanceProfile::Balanced => "balanced",
        PerformanceProfile::Visual => "visual",
        PerformanceProfile::BatterySaver => "battery_saver",
    }
}

fn log_power_policy(config: &PerformanceConfig, diagnostic: &platform_core::PowerStateDiagnostic) {
    if let Some(message) = diagnostic.message.as_deref() {
        eprintln!("power diagnostics: {message}");
    }
    if diagnostic.state.is_on_battery() && config.disable_expensive_effects_on_battery {
        eprintln!(
            "performance power policy: battery caps active (charge={:?}%, fps={:?}, animations={}, pixels={})",
            diagnostic.state.charge_percent,
            config.frame_rate_limit,
            config.max_active_animations,
            config.max_animated_region_pixels
        );
    }
}

const fn power_source_label(source: PowerSource) -> &'static str {
    match source {
        PowerSource::Ac => "ac",
        PowerSource::Battery => "battery",
        PowerSource::Unknown => "unknown",
    }
}

fn renderer_options(config: &AppConfig) -> RendererOptions {
    RendererOptions {
        backend: match config.renderer.backend {
            config_core::RendererBackendPreference::Auto => GpuBackendPreference::Auto,
            config_core::RendererBackendPreference::Vulkan => GpuBackendPreference::Vulkan,
            config_core::RendererBackendPreference::Metal => GpuBackendPreference::Metal,
            config_core::RendererBackendPreference::Dx12 => GpuBackendPreference::Dx12,
            config_core::RendererBackendPreference::Gl => GpuBackendPreference::Gl,
        },
        present_mode: match config.renderer.present_mode {
            PresentModePreference::Immediate => PresentMode::Immediate,
            PresentModePreference::Fifo => PresentMode::Fifo,
            PresentModePreference::Mailbox => PresentMode::Mailbox,
            PresentModePreference::Auto if config.renderer.vsync => PresentMode::Auto,
            PresentModePreference::Auto => PresentMode::Immediate,
        },
        damage_tracking: config.renderer.damage_tracking,
        gpu_timestamps: config.renderer.gpu_timestamps,
        text_gamma_adjustment: config.renderer.text_gamma_adjustment,
        transparent: config.window.opacity < 1.0,
        glyph_cache_entries: config.performance.glyph_cache_entries,
        background: window_background(config),
    }
}

fn window_background(config: &AppConfig) -> RenderColor {
    let mut background = render_color(config.colors.background);
    background.alpha = ((f64::from(background.alpha) * config.window.opacity)
        .round()
        .clamp(0.0, 255.0)) as u8;
    background
}

fn cursor_animation_settings(config: &AppConfig) -> CursorAnimationSettings {
    let fps = config
        .performance
        .frame_rate_limit
        .map_or(config.performance.max_animation_fps, |limit| {
            limit.min(config.performance.max_animation_fps)
        });
    let mut settings = CursorAnimationSettings {
        enabled: config.cursor.animations_enabled,
        tilt: false,
        smooth_movement: config.cursor.smooth_movement,
        typing_pulse: config.cursor.typing_pulse,
        typing_stretch: config.cursor.typing_stretch,
        trail: config.cursor.trail,
        trail_delay: Duration::from_millis(u64::from(config.cursor.trail_delay_ms)),
        trail_start_threshold_cells: config.cursor.trail_start_threshold_cells,
        trail_decay_fast: Duration::from_millis(u64::from(config.cursor.trail_decay_fast_ms)),
        trail_decay_slow: Duration::from_millis(u64::from(config.cursor.trail_decay_slow_ms)),
        blink_easing: config.cursor.blink_easing,
        short_lived_glow: config.cursor.short_lived_glow,
        shadow: config.cursor.shadow,
        fps,
        max_active_animations: config.performance.max_active_animations,
        max_animated_region_pixels: config.performance.max_animated_region_pixels,
    };
    match config.cursor.animation {
        Some(config_core::CursorAnimationProfile::Static) => settings.enabled = false,
        Some(config_core::CursorAnimationProfile::Panea) => {
            return CursorAnimationSettings::panea(
                fps,
                config.performance.max_active_animations,
                config.performance.max_animated_region_pixels,
            );
        }
        Some(config_core::CursorAnimationProfile::Custom) => settings.enabled = true,
        None => {}
    }
    settings
}

fn request_cursor_image_if_enabled(
    cache: &mut AnimatedCursorImageCache,
    config: &AppConfig,
    config_base_dir: Option<&Path>,
) {
    if !config.cursor.image.enabled || config.performance.max_active_animations == 0 {
        cache.disable();
        return;
    }

    let path = resolve_cursor_image_path(&config.cursor.image.path, config_base_dir);
    cache.request(AnimatedCursorImageRequest {
        path,
        fps: config
            .cursor
            .image
            .fps
            .min(config.performance.max_animation_fps)
            .max(1),
        max_size_kb: config.performance.max_cursor_asset_size_kb,
        warn_if_expensive: config.cursor.image.warn_if_expensive,
    });
}

fn request_cursor_vector_if_enabled(
    cache: &mut CursorVectorCache,
    config: &AppConfig,
    config_base_dir: Option<&Path>,
) {
    if !config.cursor.vector.enabled {
        cache.disable();
        return;
    }
    cache.request(CursorVectorRequest {
        path: resolve_cursor_image_path(&config.cursor.vector.path, config_base_dir),
        max_size_kb: config.performance.max_cursor_asset_size_kb,
    });
}

fn resolve_cursor_image_path(configured: &str, config_base_dir: Option<&Path>) -> PathBuf {
    let configured_path = expand_home_path(Path::new(configured));
    if configured_path.is_relative() {
        config_base_dir.map_or(configured_path.clone(), |base| base.join(&configured_path))
    } else {
        configured_path
    }
}

fn expand_home_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    let Some(relative) = text.strip_prefix("~/").or_else(|| text.strip_prefix("~\\")) else {
        return path.to_path_buf();
    };
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map_or_else(
            || path.to_path_buf(),
            |home| PathBuf::from(home).join(relative),
        )
}

fn performance_budget_from_config(config: &AppConfig) -> PerformanceBudget {
    PerformanceBudget {
        max_frame_time: Duration::from_millis(u64::from(config.performance.max_frame_time_ms)),
        ..PerformanceBudget::default()
    }
}

fn panic_payload(panic: Box<dyn Any + Send>) -> String {
    if let Some(message) = panic.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = panic.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_owned()
    }
}

fn window_settings(config: &AppConfig) -> WindowSettings {
    WindowSettings {
        title: config.window.title.clone(),
        initial_width: config.window.initial_width,
        initial_height: config.window.initial_height,
        visible_on_create: false,
        mode: map_window_mode(config.window.mode),
        linux_backend: map_linux_backend(config.window.linux_backend),
        decoration_mode: map_decoration_mode(config.window.decoration_strategy),
        opacity: config.window.opacity,
        icon: panea_window_icon(),
    }
}

fn panea_window_icon() -> Option<platform_winit::WindowIcon> {
    static ICON: std::sync::OnceLock<Option<platform_winit::WindowIcon>> =
        std::sync::OnceLock::new();
    ICON.get_or_init(|| {
        let bitmap = panea_brand_bitmap()?;
        match platform_winit::WindowIcon::from_rgba(
            bitmap.pixels.as_ref().to_vec(),
            bitmap.width,
            bitmap.height,
        ) {
            Ok(icon) => Some(icon),
            Err(error) => {
                eprintln!("window icon fallback: invalid Panea icon: {error}");
                None
            }
        }
    })
    .clone()
}

#[derive(Debug)]
struct PaneaBrandBitmap {
    pixels: Arc<[u8]>,
    width: u32,
    height: u32,
}

fn panea_brand_bitmap() -> Option<&'static PaneaBrandBitmap> {
    const PANEA_ICON_PNG: &[u8] =
        include_bytes!("../../../crates/assets/branding/generated/panea-icon-128.png");
    static BITMAP: std::sync::OnceLock<Option<PaneaBrandBitmap>> = std::sync::OnceLock::new();
    BITMAP
        .get_or_init(|| match image::load_from_memory(PANEA_ICON_PNG) {
            Ok(decoded) => {
                let decoded = decoded.into_rgba8();
                let (width, height) = decoded.dimensions();
                Some(PaneaBrandBitmap {
                    pixels: Arc::from(decoded.into_raw()),
                    width,
                    height,
                })
            }
            Err(error) => {
                eprintln!("window icon fallback: failed to decode Panea icon: {error}");
                None
            }
        })
        .as_ref()
}

fn apply_window_mode_logged(
    window: &winit::window::Window,
    requested: WindowMode,
    decoration: DecorationMode,
) -> WindowMode {
    let diagnostic = apply_window_mode_with_decoration(window, requested, decoration);
    log_window_mode_diagnostic(&diagnostic);
    diagnostic.effective
}

fn log_window_mode_diagnostic(diagnostic: &platform_core::WindowModeDiagnostic) {
    if let Some(fallback) = diagnostic.fallback.as_ref() {
        eprintln!(
            "platform fallback [{}]: requested={} effective={} reason={}",
            fallback.feature, fallback.requested, fallback.effective, fallback.reason
        );
    }
}

fn map_window_mode(mode: WindowModeConfig) -> WindowMode {
    match mode {
        WindowModeConfig::Windowed => WindowMode::Windowed,
        WindowModeConfig::Maximized => WindowMode::Maximized,
        WindowModeConfig::Fullscreen => WindowMode::Fullscreen,
        WindowModeConfig::BorderlessFullscreen => WindowMode::BorderlessFullscreen,
        WindowModeConfig::FramelessWindowed => WindowMode::FramelessWindowed,
        WindowModeConfig::FramelessFullscreen => WindowMode::FramelessFullscreen,
    }
}

fn map_linux_backend(backend: LinuxBackendConfig) -> LinuxWindowBackend {
    match backend {
        LinuxBackendConfig::Auto => LinuxWindowBackend::Auto,
        LinuxBackendConfig::X11 => LinuxWindowBackend::X11,
        LinuxBackendConfig::Wayland => LinuxWindowBackend::Wayland,
    }
}

fn map_decoration_mode(mode: DecorationStrategyConfig) -> DecorationMode {
    match mode {
        DecorationStrategyConfig::Auto => DecorationMode::Auto,
        DecorationStrategyConfig::Native => DecorationMode::Native,
        DecorationStrategyConfig::ClientSide => DecorationMode::ClientSide,
        DecorationStrategyConfig::Custom => DecorationMode::Custom,
        DecorationStrategyConfig::None => DecorationMode::None,
        DecorationStrategyConfig::FallbackDecorated => DecorationMode::FallbackDecorated,
    }
}

struct DesktopDiagnosticsPlaceholder {
    _window: platform_winit::DesktopWindowDiagnostics,
    _capabilities: platform_core::PlatformCapabilities,
}

impl DesktopDiagnosticsPlaceholder {
    fn new(
        window: platform_winit::DesktopWindowDiagnostics,
        capabilities: platform_core::PlatformCapabilities,
    ) -> Self {
        Self {
            _window: window,
            _capabilities: capabilities,
        }
    }
}
