use super::*;
use semantics::{SemanticEventKind, SemanticTimeline};

fn unique_mux_state_test_path(name: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock after Unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("panea-{name}-{}-{unique}.json", std::process::id()))
}

#[test]
fn config_changes_wake_the_event_loop_without_ui_thread_polling() {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock after Unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("panea-config-watch-{unique}.toml"));
    fs::write(&path, "[font]\nsize = 13.0\n").expect("write initial config");

    let watcher = DesktopConfigWatcher::Toml(
        config_toml::ConfigWatcher::new(config_toml::ConfigLoadOptions {
            explicit_path: Some(path.clone()),
            ..config_toml::ConfigLoadOptions::default()
        })
        .with_content_check_interval(Duration::ZERO),
    );
    let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = Arc::clone(&wakes);
    let mut watch = DesktopConfigWatchThread::spawn(
        watcher,
        TransportWakeHandle::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        }),
    );

    // Nothing has changed, so the UI thread sees nothing and is not woken.
    assert!(matches!(watch.poll(), DesktopConfigWatchEvent::Unchanged));

    fs::write(&path, "[font]\nsize = 17.0\n").expect("rewrite config");

    // The edit must arrive on its own, with no further polling from this thread
    // — that is what a config change does while the window sits idle.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut observed = None;
    while Instant::now() < deadline {
        match watch.poll() {
            DesktopConfigWatchEvent::Unchanged => thread::sleep(Duration::from_millis(20)),
            event => {
                observed = Some(event);
                break;
            }
        }
    }

    let _ = fs::remove_file(&path);
    let observed = observed.expect("config watcher must report the edit");
    assert!(
        matches!(
            observed,
            DesktopConfigWatchEvent::Pending { .. } | DesktopConfigWatchEvent::Reloaded { .. }
        ),
        "unexpected watch event: {}",
        match observed {
            DesktopConfigWatchEvent::Failed { error, .. } => error,
            _ => "non-failure".to_owned(),
        }
    );
    assert!(
        wakes.load(Ordering::Relaxed) > 0,
        "the event loop must be woken so an idle window still reloads"
    );
}

#[test]
fn runtime_shell_hook_is_written_once_per_content_hash() {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock after Unix epoch")
        .as_nanos();
    let profile = format!("hook-cache-{unique}");
    let path =
        write_runtime_shell_hook(&profile, "bashrc", "first").expect("write initial runtime hook");
    fs::write(&path, "sentinel").expect("replace hook outside the writer");

    let same =
        write_runtime_shell_hook(&profile, "bashrc", "first").expect("reuse cached runtime hook");
    assert_eq!(same, path);
    assert_eq!(
        fs::read_to_string(&path).expect("read sentinel"),
        "sentinel"
    );

    write_runtime_shell_hook(&profile, "bashrc", "second").expect("rewrite changed runtime hook");
    assert!(
        fs::read_to_string(&path)
            .expect("read changed hook")
            .contains("second")
    );
    fs::remove_file(&path).expect("remove runtime hook test file");
    fs::remove_dir(path.parent().expect("hook directory"))
        .expect("remove runtime hook test directory");
}

#[test]
fn mux_state_write_atomically_replaces_an_existing_snapshot() {
    let path = unique_mux_state_test_path("atomic-mux-state");
    fs::write(&path, "stale snapshot").expect("seed mux state");
    let mut model = MuxModel::new(SessionSpec::local("default"));
    let tab_id = model.active_tab().id;
    model.rename_tab(tab_id, "latest").expect("rename tab");
    let snapshot = model.restore_snapshot();

    write_mux_state_atomically(&path, &snapshot).expect("write mux state atomically");

    let restored: RestoreSnapshot =
        serde_json::from_str(&fs::read_to_string(&path).expect("read mux state"))
            .expect("parse mux state");
    assert_eq!(restored, snapshot);
    assert!(!mux_state_temp_path(&path).exists());
    fs::remove_file(path).expect("remove mux state test file");
}

#[test]
fn mux_state_saves_are_debounced_to_the_latest_snapshot() {
    let path = unique_mux_state_test_path("debounced-mux-state");
    let mut model = MuxModel::new(SessionSpec::local("default"));
    schedule_mux_state_save(path.clone(), model.restore_snapshot());
    let tab_id = model.active_tab().id;
    model.rename_tab(tab_id, "latest").expect("rename tab");
    let latest = model.restore_snapshot();
    schedule_mux_state_save(path.clone(), latest.clone());

    thread::sleep(MUX_STATE_SAVE_DEBOUNCE / 4);
    assert!(!path.exists(), "debounced save ran before the quiet period");

    let deadline = Instant::now() + Duration::from_secs(2);
    let restored = loop {
        if let Ok(json) = fs::read_to_string(&path)
            && let Ok(snapshot) = serde_json::from_str::<RestoreSnapshot>(&json)
        {
            break snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "debounced mux state was not saved"
        );
        thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(restored, latest);
    fs::remove_file(path).expect("remove mux state test file");
}

#[test]
fn mux_state_saver_keeps_independent_paths_when_requests_overlap() {
    let first_path = unique_mux_state_test_path("first-mux-window");
    let second_path = unique_mux_state_test_path("second-mux-window");
    let first = MuxModel::new(SessionSpec::local("first")).restore_snapshot();
    let second = MuxModel::new(SessionSpec::local("second")).restore_snapshot();

    schedule_mux_state_save(first_path.clone(), first.clone());
    schedule_mux_state_save(second_path.clone(), second.clone());

    let deadline = Instant::now() + Duration::from_secs(2);
    while (!first_path.exists() || !second_path.exists()) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        serde_json::from_str::<RestoreSnapshot>(
            &fs::read_to_string(&first_path).expect("read first mux state")
        )
        .expect("parse first mux state"),
        first
    );
    assert_eq!(
        serde_json::from_str::<RestoreSnapshot>(
            &fs::read_to_string(&second_path).expect("read second mux state")
        )
        .expect("parse second mux state"),
        second
    );
    fs::remove_file(first_path).expect("remove first mux state test file");
    fs::remove_file(second_path).expect("remove second mux state test file");
}

#[test]
fn successful_mux_layout_action_schedules_a_state_save() {
    let path = unique_mux_state_test_path("mux-action-save");
    let mut config = AppConfig::default();
    config.mux.restore_sessions = true;
    let mut runtime = MuxRuntime {
        model: MuxModel::new(SessionSpec::local("default")),
        panes: HashMap::new(),
        surface_cols: 80,
        surface_rows: 24,
        performance: RuntimePerformanceCounters::new(),
        restore_sessions: true,
        state_path: path.clone(),
        drag: None,
        output_waker: test_transport_waker(),
    };

    assert!(runtime.handle_mux_action(
        MuxAction::RenameTab {
            name: "persisted".to_owned(),
        },
        &config,
        test_metrics(),
        800,
        480,
    ));

    let deadline = Instant::now() + Duration::from_secs(2);
    while !path.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(path.exists(), "layout action did not schedule a state save");
    fs::remove_file(path).expect("remove mux action state file");
}

#[test]
fn mux_state_flush_persists_the_latest_snapshot_before_returning() {
    let path = unique_mux_state_test_path("flushed-mux-state");
    let mut model = MuxModel::new(SessionSpec::local("default"));
    schedule_mux_state_save(path.clone(), model.restore_snapshot());
    let tab_id = model.active_tab().id;
    model.rename_tab(tab_id, "latest").expect("rename tab");
    let latest = model.restore_snapshot();

    flush_mux_state_save(path.clone(), latest.clone()).expect("flush mux state");

    let restored = serde_json::from_str::<RestoreSnapshot>(
        &fs::read_to_string(&path).expect("read flushed mux state"),
    )
    .expect("parse flushed mux state");
    assert_eq!(restored, latest);
    thread::sleep(MUX_STATE_SAVE_DEBOUNCE + Duration::from_millis(25));
    let restored_after_debounce = serde_json::from_str::<RestoreSnapshot>(
        &fs::read_to_string(&path).expect("read mux state after debounce"),
    )
    .expect("parse mux state after debounce");
    assert_eq!(restored_after_debounce, latest);
    fs::remove_file(path).expect("remove flushed mux state test file");
}

#[test]
fn mux_state_async_flush_completes_before_worker_barrier() {
    let path = unique_mux_state_test_path("async-flushed-mux-state");
    let mut model = MuxModel::new(SessionSpec::local("default"));
    let tab_id = model.active_tab().id;
    model.rename_tab(tab_id, "latest").expect("rename tab");
    let latest = model.restore_snapshot();

    enqueue_mux_state_flush(path.clone(), latest.clone());
    wait_for_mux_state_saves().expect("wait for mux state save worker");

    let restored = serde_json::from_str::<RestoreSnapshot>(
        &fs::read_to_string(&path).expect("read asynchronously flushed mux state"),
    )
    .expect("parse asynchronously flushed mux state");
    assert_eq!(restored, latest);
    fs::remove_file(path).expect("remove asynchronously flushed mux state test file");
}

struct BlockingInputTransport {
    started: mpsc::Sender<Vec<u8>>,
    release: mpsc::Receiver<()>,
}

struct BurstOutputTransport {
    bytes: Option<Vec<u8>>,
}

struct CleanExitTransport {
    emitted: bool,
}

impl TerminalTransport for BurstOutputTransport {
    fn write_input(&mut self, bytes: &[u8]) -> TransportResult<usize> {
        Ok(bytes.len())
    }

    fn resize(&mut self, _size: TransportSize) -> TransportResult<()> {
        Ok(())
    }

    fn poll_output(&mut self) -> TransportResult<TransportOutput> {
        Ok(TransportOutput::bytes(
            self.bytes.take().unwrap_or_default(),
        ))
    }

    fn shutdown(&mut self) -> TransportResult<()> {
        Ok(())
    }

    fn session_metadata(&self) -> transport_core::SessionMetadata {
        transport_core::SessionMetadata {
            id: "burst-output-test".to_owned(),
            kind: transport_core::TransportKind::LocalPty,
            title: None,
            shell: None,
            current_working_directory: None,
            remote_host: None,
        }
    }

    fn state(&self) -> TransportState {
        TransportState::Running
    }
}

impl TerminalTransport for CleanExitTransport {
    fn write_input(&mut self, bytes: &[u8]) -> TransportResult<usize> {
        Ok(bytes.len())
    }

    fn resize(&mut self, _size: TransportSize) -> TransportResult<()> {
        Ok(())
    }

    fn poll_output(&mut self) -> TransportResult<TransportOutput> {
        if self.emitted {
            return Ok(TransportOutput::bytes(Vec::new()));
        }
        self.emitted = true;
        Ok(TransportOutput {
            bytes: Vec::new(),
            closed: true,
            lifecycle: vec![
                transport_core::TransportLifecycleEvent::Exited { exit_code: Some(0) },
                transport_core::TransportLifecycleEvent::Closed,
            ],
        })
    }

    fn shutdown(&mut self) -> TransportResult<()> {
        Ok(())
    }

    fn session_metadata(&self) -> transport_core::SessionMetadata {
        transport_core::SessionMetadata {
            id: "clean-exit-test".to_owned(),
            kind: transport_core::TransportKind::LocalPty,
            title: None,
            shell: None,
            current_working_directory: None,
            remote_host: None,
        }
    }

    fn state(&self) -> TransportState {
        if self.emitted {
            TransportState::Closed { exit_code: Some(0) }
        } else {
            TransportState::Running
        }
    }
}

impl TerminalTransport for BlockingInputTransport {
    fn write_input(&mut self, bytes: &[u8]) -> TransportResult<usize> {
        let _ = self.started.send(bytes.to_vec());
        let _ = self.release.recv_timeout(Duration::from_secs(1));
        Ok(bytes.len())
    }

    fn resize(&mut self, _size: TransportSize) -> TransportResult<()> {
        Ok(())
    }

    fn poll_output(&mut self) -> TransportResult<TransportOutput> {
        Ok(TransportOutput::bytes(Vec::new()))
    }

    fn shutdown(&mut self) -> TransportResult<()> {
        Ok(())
    }

    fn session_metadata(&self) -> transport_core::SessionMetadata {
        transport_core::SessionMetadata {
            id: "blocking-input-test".to_owned(),
            kind: transport_core::TransportKind::LocalPty,
            title: None,
            shell: None,
            current_working_directory: None,
            remote_host: None,
        }
    }

    fn state(&self) -> TransportState {
        TransportState::Running
    }
}

#[test]
fn desktop_pane_input_never_waits_for_the_transport_backend() {
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let mut transport = PaneTransportLoop::new(
        BlockingInputTransport {
            started: started_tx,
            release: release_rx,
        },
        test_transport_waker(),
    );

    let started_at = Instant::now();
    transport
        .send(TransportCommand::write_input(b"x".as_slice()))
        .expect("queue input");
    assert!(
        started_at.elapsed() < Duration::from_millis(20),
        "window-thread input must only enqueue bytes"
    );
    assert_eq!(
        started_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("transport worker receives input"),
        b"x"
    );
    let _ = release_tx.send(());
    transport.shutdown().expect("shutdown worker");
}

#[test]
fn desktop_transport_output_is_bounded_per_gui_tick() {
    let expected = vec![b'x'; MAX_OUTPUT_BYTES_PER_GUI_TICK * 2 + 17];
    let mut transport = PaneTransportLoop::new(
        BurstOutputTransport {
            bytes: Some(expected.clone()),
        },
        test_transport_waker(),
    );
    let deadline = Instant::now() + Duration::from_millis(250);
    let mut received = Vec::new();

    while received.len() < expected.len() && Instant::now() < deadline {
        let output = transport.poll_output().expect("poll burst output");
        assert!(
            output.bytes.len() <= MAX_OUTPUT_BYTES_PER_GUI_TICK,
            "one GUI tick received {} bytes",
            output.bytes.len()
        );
        received.extend_from_slice(&output.bytes);
        if output.bytes.is_empty() {
            thread::sleep(Duration::from_millis(1));
        }
    }

    assert_eq!(received, expected);
    transport.shutdown().expect("shutdown output worker");
}

#[test]
fn pane_runtime_reports_a_clean_local_process_exit() {
    let mut pane = test_pane(80, 24);
    pane.connection_state = PaneConnectionState::Connected;
    pane.transport = Some(PaneTransport::Local(PaneTransportLoop::new(
        CleanExitTransport { emitted: false },
        test_transport_waker(),
    )));
    let deadline = Instant::now() + Duration::from_millis(250);
    let mut clipboard = ClipboardBridge::new();
    let policy = Osc52ClipboardPolicy::default();

    loop {
        let stats = pane.poll_output(&mut clipboard, &policy, &ClipboardConfig::default());
        if stats.closed {
            assert!(stats.clean_exit);
            assert_eq!(pane.exit_code, Some(0));
            break;
        }
        assert!(Instant::now() < deadline, "clean exit was not delivered");
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn desktop_startup_uses_configured_background_before_window_reveal() {
    let mut config = AppConfig::default();
    config.colors.background = config_core::RgbaColor {
        red: 17,
        green: 34,
        blue: 51,
        alpha: 255,
    };

    let settings = window_settings(&config);
    assert!(!settings.visible_on_create);
    assert!(settings.icon.is_some());
    assert_eq!(
        renderer_options(&config).background,
        RenderColor::rgb(17, 34, 51)
    );
}

#[test]
fn renderer_backend_preference_reaches_the_gpu_layer() {
    let mut config = AppConfig::default();
    let cases = [
        (
            config_core::RendererBackendPreference::Auto,
            render_wgpu::GpuBackendPreference::Auto,
        ),
        (
            config_core::RendererBackendPreference::Vulkan,
            render_wgpu::GpuBackendPreference::Vulkan,
        ),
        (
            config_core::RendererBackendPreference::Metal,
            render_wgpu::GpuBackendPreference::Metal,
        ),
        (
            config_core::RendererBackendPreference::Dx12,
            render_wgpu::GpuBackendPreference::Dx12,
        ),
        (
            config_core::RendererBackendPreference::Gl,
            render_wgpu::GpuBackendPreference::Gl,
        ),
    ];

    for (configured, expected) in cases {
        config.renderer.backend = configured;
        assert_eq!(renderer_options(&config).backend, expected);
    }
}

#[test]
fn gui_smoke_backend_override_parser_is_portable() {
    assert_eq!(
        parse_gui_smoke_backend("auto"),
        Some(config_core::RendererBackendPreference::Auto)
    );
    assert_eq!(
        parse_gui_smoke_backend("vulkan"),
        Some(config_core::RendererBackendPreference::Vulkan)
    );
    assert_eq!(
        parse_gui_smoke_backend("metal"),
        Some(config_core::RendererBackendPreference::Metal)
    );
    assert_eq!(
        parse_gui_smoke_backend("dx12"),
        Some(config_core::RendererBackendPreference::Dx12)
    );
    assert_eq!(
        parse_gui_smoke_backend("gl"),
        Some(config_core::RendererBackendPreference::Gl)
    );
    assert_eq!(parse_gui_smoke_backend("direct3d"), None);
}

#[test]
fn gui_smoke_json_exposes_renderer_startup_phases() {
    let report = GuiSmokeReport {
        power_source: Some("battery"),
        charge_percent: Some(64),
        config_loaded: Some(Duration::from_millis(5)),
        window_created: Some(Duration::from_millis(10)),
        fonts_ready: Some(Duration::from_millis(15)),
        session_created: Some(Duration::from_millis(20)),
        renderer_initialized: Some(Duration::from_millis(25)),
        startup_background_presented: Some(Duration::from_millis(29)),
        renderer_created: Some(Duration::from_millis(30)),
        first_scene_preparation: Some(Duration::from_micros(700)),
        first_render_submission: Some(Duration::from_micros(900)),
        prompt_observed: None,
        input_sent: None,
        input_observed: None,
        success_frame_presented: Some(Duration::from_millis(40)),
        renderer: Some(render_wgpu::RendererStartupDiagnostics {
            requested_backend: GpuBackendPreference::Dx12,
            effective_backend: "Dx12".to_owned(),
            adapter: "test-adapter".to_owned(),
            attempted_backends: vec![GpuBackendPreference::Dx12],
            fallback_errors: Vec::new(),
            timings: render_wgpu::RendererStartupTimings {
                instance_and_surface: Duration::from_micros(100),
                adapter_request: Duration::from_micros(200),
                device_request: Duration::from_micros(300),
                surface_configuration: Duration::from_micros(400),
                pipeline_creation: Duration::from_micros(500),
                total: Duration::from_micros(1_500),
            },
        }),
    };

    let json = gui_smoke_json(
        true,
        GuiSmokeMode::FirstFrame,
        Duration::from_millis(40),
        &report,
    );

    assert_eq!(json["renderer"]["requested_backend"], "dx12");
    assert_eq!(json["power_source"], "battery");
    assert_eq!(json["charge_percent"], 64);
    assert_eq!(json["renderer"]["effective_backend"], "Dx12");
    assert_eq!(json["renderer"]["pipeline_creation_us"], 500);
    assert_eq!(json["config_loaded_us"], 5_000);
    assert_eq!(json["fonts_ready_us"], 15_000);
    assert_eq!(json["renderer_initialized_us"], 25_000);
    assert_eq!(json["startup_background_present_us"], 4_000);
    assert_eq!(json["first_scene_preparation_us"], 700);
    assert_eq!(json["first_render_submission_us"], 900);
    assert_eq!(json["renderer_created_us"], 30_000);
}

#[test]
fn gui_smoke_json_reports_end_to_end_input_echo_latency() {
    let report = GuiSmokeReport {
        input_sent: Some(Duration::from_micros(120_000)),
        input_observed: Some(Duration::from_micros(121_250)),
        success_frame_presented: Some(Duration::from_micros(123_500)),
        ..GuiSmokeReport::default()
    };

    let json = gui_smoke_json(
        true,
        GuiSmokeMode::InputEcho,
        Duration::from_millis(124),
        &report,
    );

    assert_eq!(json["milestone"], "prompt_input_echo_presented");
    assert_eq!(json["input_to_output_us"], 1_250);
    assert_eq!(json["output_to_present_us"], 2_250);
    assert_eq!(json["input_to_present_us"], 3_500);
}

#[test]
fn gui_input_smoke_excludes_prompt_startup_from_latency_sample() {
    let observed = Instant::now();
    let mut observed_at = None;

    assert!(!gui_smoke_input_settled(&mut observed_at, observed));
    assert!(!gui_smoke_input_settled(
        &mut observed_at,
        observed + GUI_INPUT_SETTLE_DELAY - Duration::from_millis(1)
    ));
    assert!(gui_smoke_input_settled(
        &mut observed_at,
        observed + GUI_INPUT_SETTLE_DELAY
    ));
}

#[test]
fn transparent_window_applies_opacity_to_renderer_surface_background() {
    let mut config = AppConfig::default();
    config.window.opacity = 0.92;
    config.colors.background = config_core::RgbaColor {
        red: 30,
        green: 30,
        blue: 46,
        alpha: 255,
    };

    assert_eq!(
        renderer_options(&config).background,
        RenderColor {
            red: 30,
            green: 30,
            blue: 46,
            alpha: 235,
        }
    );
}

#[derive(Debug, Default)]
struct RecordingNotificationProvider {
    requests: Vec<NotificationRequest>,
}

impl NotificationProvider for RecordingNotificationProvider {
    fn notify(
        &mut self,
        request: NotificationRequest,
    ) -> Result<(), platform_core::NotificationDiagnostic> {
        self.requests.push(request);
        Ok(())
    }

    fn diagnostic(&self) -> platform_core::NotificationDiagnostic {
        platform_core::NotificationDiagnostic {
            backend: platform_core::NotificationBackend::Unsupported,
            availability: platform_core::NotificationAvailability::Available,
            message: "test provider".to_owned(),
        }
    }
}

#[derive(Debug, Default)]
struct RecordingInputSink {
    writes: Vec<Vec<u8>>,
}

impl TerminalInputSink for RecordingInputSink {
    fn write_terminal_bytes(&mut self, bytes: &[u8]) -> TransportResult<()> {
        self.writes.push(bytes.to_vec());
        Ok(())
    }
}

#[test]
fn terminal_protocol_responses_are_written_before_user_input() {
    let mut terminal = TerminalEmulator::new(CoreTerminalSize::new(80, 24));
    terminal
        .apply_bytes(b"\x1b[4;8H\x1b[6n")
        .expect("apply cursor-position query");
    let mut sink = RecordingInputSink::default();

    write_terminal_input(&mut terminal, &mut sink, b"typed");

    assert_eq!(sink.writes, vec![b"\x1b[4;8R".to_vec(), b"typed".to_vec()]);
    assert!(terminal.state().pending_output().is_empty());
}

#[test]
fn shell_smoke_parser_reassembles_split_terminal_queries() {
    let mut terminal = TerminalEmulator::new(CoreTerminalSize::new(80, 24));

    assert_eq!(
        terminal_responses_for_shell_smoke(&mut terminal, b"\x1b[").unwrap(),
        Vec::<u8>::new()
    );
    assert_eq!(
        terminal_responses_for_shell_smoke(&mut terminal, b"6n").unwrap(),
        b"\x1b[1;1R"
    );
}

#[test]
fn battery_policy_is_bounded_and_reversible() {
    let mut configured = PerformanceConfig::default();
    configured.apply_profile(PerformanceProfile::Visual);
    let mut effective = configured.clone();
    apply_power_policy(
        &mut effective,
        &configured,
        PowerState {
            source: PowerSource::Battery,
            battery_count: 1,
            charge_percent: Some(50),
        },
    );

    assert!(effective.max_animation_fps <= 30);
    assert!(effective.max_active_animations <= 2);
    assert!(effective.glyph_cache_entries <= 4096);

    apply_power_policy(
        &mut effective,
        &configured,
        PowerState {
            source: PowerSource::Ac,
            battery_count: 1,
            charge_percent: Some(51),
        },
    );
    assert_eq!(effective, configured);
}

#[test]
fn desktop_cursor_trail_is_immediate_and_fast_for_typing() {
    let mut config = AppConfig::default();
    config.cursor.animations_enabled = true;
    config.cursor.trail = true;
    config.cursor.trail_delay_ms = 0;
    config.cursor.trail_start_threshold_cells = 0;
    config.cursor.trail_decay_fast_ms = 80;
    config.cursor.trail_decay_slow_ms = 320;

    let settings = cursor_animation_settings(&config);

    assert_eq!(settings.trail_delay, Duration::ZERO);
    assert_eq!(settings.trail_start_threshold_cells, 0);
    assert_eq!(settings.trail_decay_fast, Duration::from_millis(80));
    assert_eq!(settings.trail_decay_slow, Duration::from_millis(320));
}

#[test]
fn panea_cursor_animation_profile_compiles_to_a_fast_tilt_overlay() {
    let mut config = AppConfig::default();
    config.cursor.animation = Some(config_core::CursorAnimationProfile::Panea);

    let settings = cursor_animation_settings(&config);

    assert!(settings.enabled);
    assert!(settings.tilt);
    assert!(!settings.trail);
    assert!(!settings.smooth_movement);
    assert!(!settings.typing_pulse);
    assert!(!settings.typing_stretch);
}

#[test]
fn cursor_animation_profiles_preserve_static_and_legacy_behavior() {
    let mut config = AppConfig::default();
    assert!(!cursor_animation_settings(&config).any_effect_enabled());

    config.cursor.animations_enabled = true;
    config.cursor.trail = true;
    let custom = cursor_animation_settings(&config);
    assert!(custom.any_effect_enabled());
    assert!(!custom.tilt);

    config.cursor.animation = Some(config_core::CursorAnimationProfile::Static);
    assert!(!cursor_animation_settings(&config).any_effect_enabled());

    config.cursor.animation = Some(config_core::CursorAnimationProfile::Custom);
    config.cursor.animations_enabled = false;
    assert!(cursor_animation_settings(&config).any_effect_enabled());
}

#[test]
fn desktop_animation_wake_does_not_request_redraw_before_deadline() {
    let started = Instant::now();
    let mut pacer = render_wgpu::AnimationFramePacer::new();
    let mut scheduler = FrameScheduler::new();
    let mut next_wake = None;

    assert!(!pace_animation_wake(
        started,
        Some(Duration::from_millis(8)),
        &mut pacer,
        &mut scheduler,
        &mut next_wake,
    ));
    assert_eq!(
        scheduler.next_frame(),
        FrameDecision::NoFrameNeeded,
        "a future animation deadline must not spin the render loop"
    );

    assert!(pace_animation_wake(
        started + Duration::from_millis(8),
        Some(Duration::from_millis(8)),
        &mut pacer,
        &mut scheduler,
        &mut next_wake,
    ));
    assert!(matches!(
        scheduler.next_frame(),
        FrameDecision::FrameNeeded(_)
    ));
}

#[test]
fn desktop_reuses_scene_only_for_an_isolated_cursor_animation_frame() {
    assert!(should_reuse_scene_for_cursor_animation(
        render_core::FrameRequestReason::Animation,
        true,
        true,
        false,
        false,
    ));
    assert!(!should_reuse_scene_for_cursor_animation(
        render_core::FrameRequestReason::TerminalContentChanged,
        true,
        true,
        false,
        false,
    ));
    assert!(!should_reuse_scene_for_cursor_animation(
        render_core::FrameRequestReason::Animation,
        true,
        true,
        true,
        false,
    ));
    assert!(!should_reuse_scene_for_cursor_animation(
        render_core::FrameRequestReason::Animation,
        true,
        true,
        false,
        true,
    ));
}

#[test]
fn disabled_battery_adaptation_preserves_configured_profile() {
    let configured = PerformanceConfig {
        disable_expensive_effects_on_battery: false,
        ..PerformanceConfig::default()
    };
    let mut effective = configured.clone();
    apply_power_policy(
        &mut effective,
        &configured,
        PowerState {
            source: PowerSource::Battery,
            battery_count: 1,
            charge_percent: None,
        },
    );
    assert_eq!(effective, configured);
}

fn mouse_event(kind: MouseEventKind) -> MouseEvent {
    MouseEvent {
        kind,
        x: 0.0,
        y: 0.0,
        modifiers: KeyModifiers::default(),
    }
}

fn key_event(logical_key: &str, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent {
        physical_key: None,
        logical_key: logical_key.to_owned(),
        logical_key_without_modifiers: logical_key.to_owned(),
        text: None,
        state: KeyState::Pressed,
        modifiers,
        repeat: false,
    }
}

fn text_key_event(logical_key: &str, text: &str, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent {
        physical_key: None,
        logical_key: logical_key.to_owned(),
        logical_key_without_modifiers: logical_key.to_owned(),
        text: Some(text.to_owned()),
        state: KeyState::Pressed,
        modifiers,
        repeat: false,
    }
}

fn test_transport_waker() -> TransportWakeHandle {
    TransportWakeHandle::new(|| {})
}

#[test]
fn terminal_input_ignores_modifier_and_unknown_named_keys() {
    for key in [
        "Alt",
        "AltGraph",
        "Control",
        "Shift",
        "Super",
        "CapsLock",
        "NumLock",
        "PrintScreen",
        "Pause",
        "Unidentified",
    ] {
        assert_eq!(terminal_key(&key_event(key, KeyModifiers::default())), None);
    }
}

#[test]
fn terminal_input_uses_text_only_for_printable_character_events() {
    assert_eq!(
        terminal_key(&text_key_event("a", "a", KeyModifiers::default())),
        Some(TerminalKey::Character("a".to_owned()))
    );
    assert_eq!(
        terminal_key(&text_key_event("Dead(Acute)", "", KeyModifiers::default())),
        None
    );
}

#[test]
fn alt_character_input_uses_the_key_without_modifiers() {
    let modifiers = KeyModifiers {
        alt: true,
        ..KeyModifiers::default()
    };
    let mut event = text_key_event("å", "å", modifiers);
    event.logical_key_without_modifiers = "a".to_owned();

    assert_eq!(terminal_character_text(&event).as_deref(), Some("a"));
}

#[test]
fn recovery_chords_obey_the_user_keybinding_table() {
    let modifiers = KeyModifiers {
        ctrl: true,
        shift: true,
        ..KeyModifiers::default()
    };
    let event = key_event("f", modifiers);
    let mut config = AppConfig::default();
    assert_eq!(
        keybinding_action(&event, &config).as_deref(),
        Some("toggle_fullscreen")
    );

    config.keyboard.keybindings.clear();
    assert_eq!(keybinding_action(&event, &config), None);
}

#[test]
fn fractional_wheel_deltas_accumulate_by_native_unit() {
    let metrics = test_metrics();
    let mut remainder = 0.0;

    for _ in 0..3 {
        assert_eq!(
            accumulated_scroll_lines(
                platform_core::MouseScrollDelta::Lines { x: 0.0, y: 0.1 },
                metrics,
                &mut remainder,
            ),
            0
        );
    }
    assert_eq!(
        accumulated_scroll_lines(
            platform_core::MouseScrollDelta::Lines { x: 0.0, y: 0.1 },
            metrics,
            &mut remainder,
        ),
        1
    );

    remainder = 0.0;
    assert_eq!(
        accumulated_scroll_lines(
            platform_core::MouseScrollDelta::Pixels { x: 0.0, y: 8.0 },
            metrics,
            &mut remainder,
        ),
        0
    );
    assert_eq!(
        accumulated_scroll_lines(
            platform_core::MouseScrollDelta::Pixels { x: 0.0, y: 8.0 },
            metrics,
            &mut remainder,
        ),
        1
    );

    remainder = 0.0;
    assert_eq!(
        accumulated_scroll_lines(
            platform_core::MouseScrollDelta::Pixels { x: 0.0, y: 11.0 },
            metrics,
            &mut remainder,
        ),
        0
    );
    assert_eq!(
        accumulated_scroll_lines(
            platform_core::MouseScrollDelta::Pixels { x: 0.0, y: 5.0 },
            metrics,
            &mut remainder,
        ),
        1
    );
}

#[test]
fn ime_cursor_area_updates_only_when_cursor_or_metrics_change() {
    let mut tracker = ImeCursorAreaTracker::default();
    let first = ImeCursorArea::new(8.0, 16.0, 8.0, 16.0);
    assert!(tracker.update(first));
    assert!(!tracker.update(first));
    assert!(tracker.update(ImeCursorArea::new(16.0, 16.0, 8.0, 16.0)));
    assert!(tracker.update(ImeCursorArea::new(16.0, 16.0, 9.0, 18.0)));
}

#[test]
fn terminal_input_encodes_ctrl_from_logical_key_not_control_text() {
    let modifiers = KeyModifiers {
        ctrl: true,
        ..KeyModifiers::default()
    };
    let event = text_key_event("a", "\u{1}", modifiers);
    assert_eq!(
        terminal_key(&event),
        Some(TerminalKey::Character("a".to_owned()))
    );
    assert_eq!(
        encode_terminal_key(
            &terminal_key(&event).expect("Ctrl+A terminal key"),
            terminal_modifiers(event.modifiers),
            &BTreeSet::new(),
        ),
        Some(vec![0x01])
    );
}

#[test]
fn terminal_key_keeps_printable_identity_for_protocol_release_events() {
    let mut event = key_event("a", KeyModifiers::default());
    event.state = KeyState::Released;

    assert_eq!(
        terminal_key(&event),
        Some(TerminalKey::Character("a".to_owned()))
    );
}

#[test]
fn locally_consumed_key_press_quarantines_its_release() {
    let mut consumed = HashSet::new();
    let mut press = key_event("Enter", KeyModifiers::default());
    press.physical_key = Some("Code(Enter)".to_owned());
    remember_consumed_key(&mut consumed, &press);

    let mut release = press;
    release.state = KeyState::Released;
    assert!(take_consumed_key_release(&mut consumed, &release));
    assert!(consumed.is_empty());
    assert!(!take_consumed_key_release(&mut consumed, &release));
}

#[test]
fn gui_terminal_io_smoke_recognizes_common_cross_platform_prompts() {
    assert!(shell_prompt_visible(
        "Windows PowerShell\nPS C:\\Users\\panea>"
    ));
    assert_eq!(
        shell_prompt_line_count("Windows PowerShell\nPS C:\\Users\\panea>\nPS C:\\Users\\panea>"),
        2
    );
    assert!(shell_prompt_visible(
        "Windows PowerShell\nPS C:\\Users\\panea>\n\n\n\n\n"
    ));
    assert!(shell_prompt_visible("panea@host:~$"));
    assert!(shell_prompt_visible("root@host:/#"));
    assert!(shell_prompt_visible("host%"));
    assert!(shell_prompt_visible(
        "\u{e0b0}~ \u{e0b0}\n\u{276f}                         \u{e0b2} 8ms \u{e0b2} shres \u{e0b2} pwsh \u{e0b4}"
    ));
    assert!(!shell_prompt_visible(
        "Copyright (C) Microsoft Corporation."
    ));
}

#[test]
fn a_dropped_remote_session_arms_a_backoff_and_recovers() {
    let mut pane = test_pane(40, 6);
    pane.remote_session = true;

    // A lost connection arms a retry and tells the user, once.
    let first = pane
        .arm_automatic_reconnect("SSH session disconnected")
        .expect("a transient drop must arm a retry");
    assert!(pane.reconnect_at.is_some());
    assert!(first >= Duration::from_secs(1));
    assert!(!pane.automatic_reconnect_is_due(Instant::now()));
    assert!(pane.automatic_reconnect_is_due(Instant::now() + first + Duration::from_millis(10)));

    // Backoff widens as attempts are spent.
    pane.reconnect_attempts = 2;
    let later = pane
        .arm_automatic_reconnect("SSH session disconnected")
        .expect("still within the budget");
    assert!(later > first, "backoff must widen: {later:?} vs {first:?}");

    // Exhausting the budget stops the retries.
    pane.reconnect_attempts = pane.reconnect_policy.max_attempts;
    assert!(
        pane.arm_automatic_reconnect("SSH session disconnected")
            .is_none(),
        "an exhausted budget must not keep retrying"
    );
    assert!(pane.reconnect_at.is_none());
}

#[test]
fn a_local_shell_exit_is_never_retried() {
    let mut pane = test_pane(20, 4);
    pane.remote_session = false;

    // A local shell exited because it was asked to; reopening it would be wrong.
    assert!(pane.arm_automatic_reconnect("session exited").is_none());
    assert!(pane.reconnect_at.is_none());
}

#[test]
fn a_failure_needing_a_human_is_not_retried_behind_their_back() {
    let mut pane = test_pane(40, 6);
    pane.remote_session = true;

    assert!(
        pane.arm_automatic_reconnect(transport_ssh::AUTHENTICATION_REJECTED)
            .is_none(),
        "a rejected credential must not be retried"
    );
    assert!(pane.reconnect_at.is_none());
    // The pane says so rather than failing silently.
    let text = pane
        .terminal
        .state()
        .grid()
        .lines
        .iter()
        .map(term_core::Line::raw_text)
        .collect::<Vec<_>>()
        .join("");
    assert!(
        text.contains("will not reconnect automatically"),
        "the pane must explain why it stopped: {text:?}"
    );
}

/// Feeds bytes the way `poll_output` does, including the semantic sync.
fn feed_pane(pane: &mut PaneRuntime, bytes: &[u8]) {
    let cursor = pane.terminal.state().cursor_buffer_position();
    let parsed = pane
        .semantic_parser
        .parse(bytes, BufferPosition::new(cursor.row, cursor.col));
    let mut applied = 0usize;
    for parsed in parsed {
        let source_end = parsed.source_end.min(bytes.len()).max(applied);
        let _ = pane.terminal.apply_bytes(&bytes[applied..source_end]);
        applied = source_end;
        PaneRuntime::sync_semantic_rows(
            &pane.terminal,
            &mut pane.semantic_timeline,
            &mut pane.semantic_rows_dropped,
            &mut pane.alternate_screen_semantics,
        );
        let cursor = pane.terminal.state().cursor_buffer_position();
        pane.semantic_timeline.apply_event(
            parsed
                .event
                .at_position(BufferPosition::new(cursor.row, cursor.col)),
        );
    }
    let _ = pane.terminal.apply_bytes(&bytes[applied..]);
    PaneRuntime::sync_semantic_rows(
        &pane.terminal,
        &mut pane.semantic_timeline,
        &mut pane.semantic_rows_dropped,
        &mut pane.alternate_screen_semantics,
    );
}

#[test]
fn semantic_rows_are_absolute_buffer_rows() {
    let mut pane = test_pane(20, 3);
    pane.parse_semantic_events = true;
    // Push content into scrollback first, so a screen-relative row and an
    // absolute row cannot coincide.
    for index in 0..8 {
        feed_pane(
            &mut pane,
            format!(
                "line{index}
"
            )
            .as_bytes(),
        );
    }
    let scrollback = pane.terminal.scrollback_lines().len() as i64;
    assert!(scrollback > 0, "the test needs scrollback to exist");

    feed_pane(&mut pane, b"]133;A$ ");

    let region = pane
        .semantic_timeline
        .regions()
        .find(|region| region.kind == SemanticRegionKind::Prompt)
        .expect("prompt region");
    // Overlays subtract the viewport origin from these rows, so they must be
    // absolute. A screen-relative row would be below the scrollback length.
    assert!(
        region.start.row >= scrollback,
        "prompt row {} must be absolute, scrollback is {scrollback}",
        region.start.row
    );
}

#[test]
fn semantic_rows_follow_their_text_when_scrollback_is_evicted() {
    let mut pane = test_pane(20, 3);
    pane.parse_semantic_events = true;
    pane.terminal.state_mut().set_scrollback_limit(4);

    feed_pane(
        &mut pane,
        b"]133;A$ cmd
",
    );
    let recorded = pane
        .semantic_timeline
        .regions()
        .find(|region| region.kind == SemanticRegionKind::Prompt)
        .expect("prompt region")
        .start
        .row;

    // Push past the cap so lines are evicted from the top.
    for index in 0..12 {
        feed_pane(
            &mut pane,
            format!(
                "filler{index}
"
            )
            .as_bytes(),
        );
    }
    let dropped = pane.terminal.state().scrollback_dropped();
    assert!(dropped > 0, "the cap must have evicted lines");

    match pane
        .semantic_timeline
        .regions()
        .find(|region| region.kind == SemanticRegionKind::Prompt)
    {
        Some(region) => assert_eq!(
            region.start.row,
            recorded - dropped as i64,
            "a surviving region must move up by the number of evicted lines"
        ),
        // Pruned because its text left the buffer: also correct.
        None => assert!(recorded - (dropped as i64) < 0),
    }
}

#[test]
fn a_restarted_scrollback_counter_does_not_corrupt_semantic_rows() {
    let mut pane = test_pane(20, 3);
    pane.parse_semantic_events = true;
    pane.terminal.state_mut().set_scrollback_limit(2);
    for index in 0..8 {
        feed_pane(
            &mut pane,
            format!(
                "line{index}
"
            )
            .as_bytes(),
        );
    }
    assert!(pane.semantic_rows_dropped > 0);

    // A full reset rebuilds the terminal, so the eviction counter restarts.
    let _ = pane.terminal.apply_bytes(b"c");
    PaneRuntime::sync_semantic_rows(
        &pane.terminal,
        &mut pane.semantic_timeline,
        &mut pane.semantic_rows_dropped,
        &mut pane.alternate_screen_semantics,
    );

    assert_eq!(
        pane.semantic_rows_dropped,
        pane.terminal.state().scrollback_dropped(),
        "the baseline must follow the counter back down rather than underflow"
    );
}

#[test]
fn alternate_screen_markers_are_not_rebased_and_do_not_outlive_the_screen() {
    let mut pane = test_pane(20, 3);
    pane.parse_semantic_events = true;
    pane.terminal.state_mut().set_scrollback_limit(2);

    // A prompt on the primary screen, then push past the cap so eviction runs.
    feed_pane(&mut pane, b"]133;A$ ");
    for index in 0..6 {
        feed_pane(
            &mut pane,
            format!(
                "out{index}
"
            )
            .as_bytes(),
        );
    }
    let primary_regions = pane.semantic_timeline.region_count();

    // Enter the alternate screen, as a multiplexer or editor does, and record a
    // marker there. Its rows belong to that buffer.
    feed_pane(&mut pane, b"[?1049h]133;Aalt");
    assert!(
        pane.alternate_screen_semantics.is_some(),
        "the alternate-screen boundary must be recorded"
    );
    let during_alt = pane.semantic_timeline.region_count();
    assert!(during_alt > primary_regions);

    // Output while on the alternate screen must not rebase anything: the
    // primary scrollback cannot grow, so nothing has moved.
    let dropped_before = pane.semantic_rows_dropped;
    for index in 0..6 {
        feed_pane(
            &mut pane,
            format!(
                "altout{index}
"
            )
            .as_bytes(),
        );
    }
    assert_eq!(
        pane.semantic_rows_dropped, dropped_before,
        "alternate-screen output must not drive a primary rebase"
    );

    // Leaving the alternate screen discards the markers that described it.
    feed_pane(&mut pane, b"[?1049l");
    assert!(pane.alternate_screen_semantics.is_none());
    assert!(
        pane.semantic_timeline.region_count() <= primary_regions,
        "alternate-screen regions must not outlive the screen they described"
    );
}

#[test]
fn an_unchanged_search_does_not_rescan_the_buffer() {
    let mut pane = test_pane(20, 4);
    for index in 0..6 {
        let _ = pane.terminal.apply_bytes(
            format!(
                "needle{index}
"
            )
            .as_bytes(),
        );
    }
    pane.search.query = "needle".to_owned();
    pane.refresh_search();
    let first = pane.search.matches.clone();
    assert!(!first.is_empty(), "the query must match something");

    // Clear the hit list behind the cache's back. A refresh that honours the
    // cache leaves it cleared; one that rescans would repopulate it. This is the
    // only way to observe the scan being skipped rather than merely repeated.
    pane.search.matches.clear();
    pane.refresh_search();
    assert!(
        pane.search.matches.is_empty(),
        "an unchanged query over unchanged content must not rescan the buffer"
    );

    // New output changes the buffer, so the search must run again.
    let _ = pane.terminal.apply_bytes(
        b"needle-extra
",
    );
    pane.refresh_search();
    assert!(
        pane.search.matches.len() > first.len(),
        "new content must invalidate the cached hits and find the new match"
    );
}

/// Covers the constructor a pane uses for its history depth.
///
/// The pass-through in `PaneRuntime::new` itself is not reachable from a test:
/// that constructor spawns a transport, and nothing else calls it. This pins the
/// half that can be asserted — that a configured depth is honoured rather than
/// silently replaced by term-core's default — and that the default is sane.
#[test]
fn a_configured_scrollback_depth_is_honoured_by_the_terminal_constructor() {
    for lines in [64usize, 4096] {
        let mut config = AppConfig::default();
        config.scrollback.lines = lines;
        let terminal = TerminalEmulator::with_scrollback_limit(
            CoreTerminalSize::new(80, 24),
            config.scrollback.lines,
        );
        assert_eq!(terminal.state().scrollback_limit(), lines);
    }

    assert_ne!(
        AppConfig::default().scrollback.lines,
        0,
        "a zero default would disable history entirely"
    );
}

#[test]
fn a_panicking_semantic_parser_does_not_take_the_pane_with_it() {
    let mut parser = SemanticEscapeParser::new();
    let position = BufferPosition::new(0, 0);

    // The payload shape that used to panic while percent-decoding OSC 7. The
    // decode is fixed, but this path runs its own scanner over untrusted bytes
    // and must stay behind a boundary regardless.
    let bytes = b"\x1b]7;file://host/%\xe2\x82\xac\x07";
    let _ = parse_semantic_markers(&mut parser, bytes, position);

    // The parser is still usable afterwards.
    let events = parse_semantic_markers(&mut parser, b"\x1b]133;A\x07", position);
    assert_eq!(events.len(), 1, "a real marker must still be recognised");
}

fn alt_key_event(logical: &str, state: KeyState, alt_held: bool) -> KeyEvent {
    KeyEvent {
        physical_key: Some(format!("Key{logical}")),
        logical_key: logical.to_owned(),
        logical_key_without_modifiers: logical.to_owned(),
        text: None,
        state,
        modifiers: KeyModifiers {
            alt: alt_held,
            ..KeyModifiers::default()
        },
        repeat: false,
    }
}

#[test]
fn pressing_a_modifier_alone_produces_no_terminal_input() {
    // Pressing Alt typed the literal word "Alt" into the terminal, and again on
    // release while Alt was still held, giving "AltAlt".
    for name in ["Alt", "AltGraph", "Control", "Shift", "Super", "Meta"] {
        for state in [KeyState::Pressed, KeyState::Released] {
            let event = alt_key_event(name, state, true);
            assert!(
                terminal_key(&event).is_none(),
                "{name} ({state:?}) must produce no terminal key, got {:?}",
                terminal_key(&event)
            );
        }
    }
}

#[test]
fn a_named_key_never_becomes_its_own_name_as_text() {
    // Any named key stringifies to its name; none of them are text.
    for name in ["F5", "CapsLock", "NumLock", "PrintScreen", "Pause"] {
        let event = alt_key_event(name, KeyState::Pressed, true);
        let key = terminal_key(&event);
        assert!(
            !matches!(&key, Some(TerminalKey::Character(text)) if text == name),
            "{name} must not be sent as the text {name:?}, got {key:?}"
        );
    }
}

#[test]
fn alt_still_sends_the_unmodified_character() {
    // The behaviour the guard must preserve: Option+a is ESC a, not ESC å.
    let mut event = alt_key_event("a", KeyState::Pressed, true);
    event.logical_key_without_modifiers = "a".to_owned();
    event.text = Some("\u{e5}".to_owned());
    assert!(
        matches!(terminal_key(&event), Some(TerminalKey::Character(text)) if text == "a"),
        "Alt+a must send the unmodified key, got {:?}",
        terminal_key(&event)
    );
}

fn escaped(bytes: &[u8]) -> String {
    let mut out = String::new();
    for &byte in bytes {
        match byte {
            0x1b => out.push_str("\\e"),
            0x07 => out.push_str("\\a"),
            b'\r' => out.push_str("\\r"),
            b'\n' => out.push_str("\\n"),
            0x20..=0x7e => out.push(byte as char),
            _ => out.push_str(&format!("\\x{byte:02x}")),
        }
    }
    out
}

#[test]
#[ignore = "launches the real wmux binary"]
fn probe_wmux_startup_and_keystrokes() {
    // Diagnostic for the Windows-native multiplexer that exposed the
    // win32-input-mode / kitty precedence bug. Skips where it cannot run.
    let config_path = r"C:\Users\shres\panea\personal-config\wmux\config.wmux";
    if std::process::Command::new("wmux.exe")
        .arg("--version")
        .output()
        .is_err()
        || !std::path::Path::new(config_path).exists()
    {
        eprintln!("wmux probe skipped: wmux.exe or its personal config is not available");
        return;
    }
    // Match the user's environment exactly: their powershell.ps1 exports both.
    // A distinct session so the probe never collides with a live server.
    let mut profile = LocalShellProfile::custom("wmux-probe", "wmux.exe").with_args([
        "new-session",
        "-s",
        "panea-probe",
    ]);
    profile = profile
        .with_env(
            "WMUX_CONFIG",
            r"C:\Users\shres\panea\personal-config\wmux\config.wmux",
        )
        .with_env(
            "WMUX_SHELL",
            r#"powershell.exe -NoLogo -ExecutionPolicy Bypass -NoExit -File "C:\Users\shres\panea\personal-config\powershell.ps1""#,
        );
    let size = TransportSize::new(80, 24, 800, 480);
    let mut transport = LocalPtyTransport::spawn(profile, size).expect("spawn wmux");
    let mut terminal = TerminalEmulator::new(CoreTerminalSize::new(80, 24));

    let mut raw_in = Vec::new();
    let mut replies = Vec::new();
    let pump = |transport: &mut LocalPtyTransport,
                terminal: &mut TerminalEmulator,
                raw_in: &mut Vec<u8>,
                replies: &mut Vec<u8>,
                for_ms: u64|
     -> usize {
        let deadline = Instant::now() + Duration::from_millis(for_ms);
        let mut got = 0usize;
        while Instant::now() < deadline {
            let output = transport.poll_output().expect("poll");
            if !output.bytes.is_empty() {
                got += output.bytes.len();
                raw_in.extend_from_slice(&output.bytes);
                let _ = terminal.apply_bytes(&output.bytes);
                let response = terminal.state_mut().take_pending_output();
                if !response.is_empty() {
                    replies.extend_from_slice(&response);
                    transport.write_input(&response).expect("write reply");
                }
            } else {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        got
    };

    let startup = pump(
        &mut transport,
        &mut terminal,
        &mut raw_in,
        &mut replies,
        8000,
    );

    let modes = format!("{:?}", terminal.modes_ref());
    let kitty = terminal.state().kitty_keyboard_flags();

    let state_after_startup = format!("{:?}", transport.state());

    // Send one keystroke, "x", in three encodings and see which wmux echoes.
    // Only bytes that arrive AFTER each write are attributed to it.
    let send_and_capture = |transport: &mut LocalPtyTransport,
                            terminal: &mut TerminalEmulator,
                            raw_in: &mut Vec<u8>,
                            replies: &mut Vec<u8>,
                            bytes: &[u8]|
     -> (Result<(), String>, String) {
        let before = raw_in.len();
        let written = transport
            .write_input(bytes)
            .map(|_| ())
            .map_err(|e| e.to_string());
        pump(transport, terminal, raw_in, replies, 700);
        (written, escaped(&raw_in[before..]))
    };
    let plain = send_and_capture(
        &mut transport,
        &mut terminal,
        &mut raw_in,
        &mut replies,
        b"a",
    );
    // CSI-u for "b" (98).
    let csi_u = send_and_capture(
        &mut transport,
        &mut terminal,
        &mut raw_in,
        &mut replies,
        b"\x1b[98u",
    );
    // win32-input-mode record for "c": Vk=0x43 (67), Sc=46, Uc=99, KeyDown then KeyUp.
    let win32 = send_and_capture(
        &mut transport,
        &mut terminal,
        &mut raw_in,
        &mut replies,
        b"\x1b[67;46;99;1;0;1_\x1b[67;46;99;0;0;1_",
    );
    // Punctuation: plain VT bytes versus win32 records. '&' is Shift+7 on a US
    // layout: Vk=0x37 (55), Sc=8, Uc=38, KeyDown, Cs=SHIFT_PRESSED (16), Rc=1.
    let punct_plain = send_and_capture(
        &mut transport,
        &mut terminal,
        &mut raw_in,
        &mut replies,
        b"&!",
    );
    let punct_win32 = send_and_capture(
        &mut transport,
        &mut terminal,
        &mut raw_in,
        &mut replies,
        b"\x1b[55;8;38;1;16;1_\x1b[55;8;38;0;16;1_",
    );
    // Unshifted symbol as a record: '-' is Vk=0xBD (189), Sc=12, Uc=45, Cs=0.
    let minus_plain = send_and_capture(
        &mut transport,
        &mut terminal,
        &mut raw_in,
        &mut replies,
        b"-",
    );
    let minus_win32 = send_and_capture(
        &mut transport,
        &mut terminal,
        &mut raw_in,
        &mut replies,
        b"\x1b[189;12;45;1;0;1_\x1b[189;12;45;0;0;1_",
    );
    // '&' the way Windows Terminal really sends it: Shift down (Vk=16, Sc=42),
    // then '7' with Cs=16 producing '&', then both releases.
    let amp_wt = send_and_capture(
        &mut transport,
        &mut terminal,
        &mut raw_in,
        &mut replies,
        b"\x1b[16;42;0;1;16;1_\x1b[55;8;38;1;16;1_\x1b[55;8;38;0;16;1_\x1b[16;42;0;0;0;1_",
    );
    // Map the failing class precisely: which plain characters reach the pane?
    let mut landed = String::new();
    let mut dropped = String::new();
    for ch in "7 1 @ # $ % ^ * ( ) _ + , . / ; ' [ ] = ` ~ { } : < > ? |".split(' ') {
        let out = send_and_capture(
            &mut transport,
            &mut terminal,
            &mut raw_in,
            &mut replies,
            ch.as_bytes(),
        );
        if out.1.contains(ch) {
            landed.push_str(ch);
        } else {
            dropped.push_str(ch);
        }
    }
    eprintln!(
        "=== CLASS landed={landed:?}
=== CLASS dropped={dropped:?}"
    );
    // Which record form does wmux honour for shifted input?
    // 1. shifted LETTER 'D': Vk=68 Sc=32 Uc=68 Cs=16
    let upper_rec = send_and_capture(
        &mut transport,
        &mut terminal,
        &mut raw_in,
        &mut replies,
        b"\x1b[68;32;68;1;16;1_\x1b[68;32;68;0;16;1_",
    );
    // 2. shifted letter as a plain byte
    let upper_plain = send_and_capture(
        &mut transport,
        &mut terminal,
        &mut raw_in,
        &mut replies,
        b"E",
    );
    // 3. '&' with Vk of '7' but Cs=0, Uc=38: does wmux key off Uc?
    let amp_uc = send_and_capture(
        &mut transport,
        &mut terminal,
        &mut raw_in,
        &mut replies,
        b"\x1b[55;8;38;1;0;1_\x1b[55;8;38;0;0;1_",
    );
    // 4. pure-unicode record, Vk=0 Sc=0 (how Windows Terminal sends IME/AltGr text)
    let amp_vk0 = send_and_capture(
        &mut transport,
        &mut terminal,
        &mut raw_in,
        &mut replies,
        b"\x1b[0;0;38;1;0;1_\x1b[0;0;38;0;0;1_",
    );
    // 5. '!' via Vk '1' (49), Sc 2, Uc 33, Cs=0
    let bang_uc = send_and_capture(
        &mut transport,
        &mut terminal,
        &mut raw_in,
        &mut replies,
        b"\x1b[49;2;33;1;0;1_\x1b[49;2;33;0;0;1_",
    );
    eprintln!(
        "=== UPPER D record Cs=16     -> echoed={:?}\n=== UPPER E plain byte      -> echoed={:?}\n=== AMP   Vk7 Cs=0 Uc=&     -> echoed={:?}\n=== AMP   Vk0 Sc0 Uc=&      -> echoed={:?}\n=== BANG  Vk1 Cs=0 Uc=!     -> echoed={:?}",
        upper_rec.1, upper_plain.1, amp_uc.1, amp_vk0.1, bang_uc.1
    );
    eprintln!(
        "=== PUNCT plain &!        -> echoed={:?}\n=== PUNCT win32 & (Cs only) -> echoed={:?}\n=== MINUS plain -         -> echoed={:?}\n=== MINUS win32 -         -> echoed={:?}\n=== AMP   WT sequence     -> echoed={:?}",
        punct_plain.1, punct_win32.1, minus_plain.1, minus_win32.1, amp_wt.1
    );
    // End-to-end: encode "d" exactly as the application does, from the live
    // modes and kitty flags. This is the path the user's keystrokes take.
    let app_encoded = encode_terminal_key_with_protocol(
        &TerminalKey::Character("d".to_owned()),
        TerminalKeyModifiers::default(),
        terminal.modes_ref(),
        terminal.state().kitty_keyboard_flags(),
        TerminalKeyEventType::Press,
    )
    .expect("encode d");
    let app_path = send_and_capture(
        &mut transport,
        &mut terminal,
        &mut raw_in,
        &mut replies,
        &app_encoded,
    );
    let app_encoded_text = escaped(&app_encoded);
    // Let the pane settle, then read what the screen actually shows.
    pump(
        &mut transport,
        &mut terminal,
        &mut raw_in,
        &mut replies,
        1500,
    );
    let screen = terminal
        .state()
        .grid()
        .lines
        .iter()
        .map(term_core::Line::raw_text)
        .filter(|line| !line.trim().is_empty())
        .map(|line| format!("=== SCREEN| {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let final_state = format!("{:?}", transport.state());

    eprintln!(
        "\n=== startup_bytes={startup} state_after_startup={state_after_startup} final_state={final_state}\n\
         === kitty_flags={kitty} modes={modes}\n\
         === replies_sent={}\n\
         === PLAIN   a           -> write={:?} echoed={:?}\n\
         === CSI-u   ESC[98u b   -> write={:?} echoed={:?}\n\
         === WIN32   c record    -> write={:?} echoed={:?}\n\
         === APP     d as {}  -> write={:?} echoed={:?}\n\
         === SCREEN (letters that landed appear here):\n{screen}\n",
        escaped(&replies),
        plain.0,
        plain.1,
        csi_u.0,
        csi_u.1,
        win32.0,
        win32.1,
        app_encoded_text,
        app_path.0,
        app_path.1,
    );

    // The regression itself: a keystroke encoded the way the application does
    // must reach the multiplexer's pane. Before win32-input-mode took precedence
    // over the kitty flags it went out as `CSI 100u`, which wmux drops.
    assert!(
        app_path.1.contains('d'),
        "the app-encoded keystroke ({app_encoded_text}) must reach the pane; echoed {:?}",
        app_path.1
    );
    assert!(
        !app_encoded_text.contains("100u"),
        "with win32-input-mode on, plain text must not be sent as CSI u"
    );
}

fn win32_key(
    physical: &str,
    logical: &str,
    unmodified: &str,
    text: Option<&str>,
    state: KeyState,
    modifiers: KeyModifiers,
) -> KeyEvent {
    KeyEvent {
        physical_key: Some(format!("Code({physical})")),
        logical_key: logical.to_owned(),
        logical_key_without_modifiers: unmodified.to_owned(),
        text: text.map(str::to_owned),
        state,
        modifiers,
        repeat: false,
    }
}

#[test]
fn win32_input_records_cover_text_symbols_controls_navigation_and_releases() {
    let none = KeyModifiers::default();
    let shift = KeyModifiers {
        shift: true,
        ..none
    };
    let ctrl = KeyModifiers { ctrl: true, ..none };
    let ctrl_shift = KeyModifiers {
        ctrl: true,
        shift: true,
        ..none
    };
    let alt = KeyModifiers { alt: true, ..none };
    let altgr = KeyModifiers {
        alt: true,
        alt_graph: true,
        ..none
    };
    let down = KeyState::Pressed;
    let up = KeyState::Released;

    let cases: &[(&str, KeyEvent, &[u8])] = &[
        (
            "letter",
            win32_key("KeyA", "a", "a", Some("a"), down, none),
            b"\x1b[65;30;97;1;0;1_",
        ),
        (
            "letter release",
            win32_key("KeyA", "a", "a", Some("a"), up, none),
            b"\x1b[65;30;97;0;0;1_",
        ),
        (
            "digit",
            win32_key("Digit7", "7", "7", Some("7"), down, none),
            b"\x1b[55;8;55;1;0;1_",
        ),
        // Every shifted digit-row symbol: the class legacy encodings cannot carry with shift state.
        (
            "&",
            win32_key("Digit7", "&", "7", Some("&"), down, shift),
            b"\x1b[55;8;38;1;16;1_",
        ),
        (
            "!",
            win32_key("Digit1", "!", "1", Some("!"), down, shift),
            b"\x1b[49;2;33;1;16;1_",
        ),
        (
            "@",
            win32_key("Digit2", "@", "2", Some("@"), down, shift),
            b"\x1b[50;3;64;1;16;1_",
        ),
        (
            "(",
            win32_key("Digit9", "(", "9", Some("("), down, shift),
            b"\x1b[57;10;40;1;16;1_",
        ),
        (
            ")",
            win32_key("Digit0", ")", "0", Some(")"), down, shift),
            b"\x1b[48;11;41;1;16;1_",
        ),
        // OEM symbols, shifted and not.
        (
            "-",
            win32_key("Minus", "-", "-", Some("-"), down, none),
            b"\x1b[189;12;45;1;0;1_",
        ),
        (
            "_",
            win32_key("Minus", "_", "-", Some("_"), down, shift),
            b"\x1b[189;12;95;1;16;1_",
        ),
        (
            "{",
            win32_key("BracketLeft", "{", "[", Some("{"), down, shift),
            b"\x1b[219;26;123;1;16;1_",
        ),
        (
            "|",
            win32_key("Backslash", "|", "\\", Some("|"), down, shift),
            b"\x1b[220;43;124;1;16;1_",
        ),
        // Control combinations carry the control character; the state tells them apart.
        (
            "Ctrl+B",
            win32_key("KeyB", "b", "b", None, down, ctrl),
            b"\x1b[66;48;2;1;8;1_",
        ),
        (
            "Ctrl+Shift+B",
            win32_key("KeyB", "B", "b", None, down, ctrl_shift),
            b"\x1b[66;48;2;1;24;1_",
        ),
        // Enter variants are distinguishable, unlike legacy CR for all three.
        (
            "Enter",
            win32_key("Enter", "Enter", "Enter", None, down, none),
            b"\x1b[13;28;13;1;0;1_",
        ),
        (
            "Shift+Enter",
            win32_key("Enter", "Enter", "Enter", None, down, shift),
            b"\x1b[13;28;13;1;16;1_",
        ),
        (
            "Ctrl+Enter",
            win32_key("Enter", "Enter", "Enter", None, down, ctrl),
            b"\x1b[13;28;10;1;8;1_",
        ),
        (
            "Tab",
            win32_key("Tab", "Tab", "Tab", None, down, none),
            b"\x1b[9;15;9;1;0;1_",
        ),
        (
            "Backspace",
            win32_key("Backspace", "Backspace", "Backspace", None, down, none),
            b"\x1b[8;14;8;1;0;1_",
        ),
        (
            "Escape",
            win32_key("Escape", "Escape", "Escape", None, down, none),
            b"\x1b[27;1;27;1;0;1_",
        ),
        // Navigation keys have no character and are enhanced keys.
        (
            "Up",
            win32_key("ArrowUp", "ArrowUp", "ArrowUp", None, down, none),
            b"\x1b[38;72;0;1;256;1_",
        ),
        (
            "Delete",
            win32_key("Delete", "Delete", "Delete", None, down, none),
            b"\x1b[46;83;0;1;256;1_",
        ),
        (
            "F5",
            win32_key("F5", "F5", "F5", None, down, none),
            b"\x1b[116;63;0;1;0;1_",
        ),
        // A modifier on its own is a real record here, not silence.
        (
            "Shift down",
            win32_key("ShiftLeft", "Shift", "Shift", None, down, shift),
            b"\x1b[16;42;0;1;16;1_",
        ),
        (
            "Alt+x",
            win32_key("KeyX", "x", "x", Some("x"), down, alt),
            b"\x1b[88;45;120;1;2;1_",
        ),
        // AltGr is right-Alt plus left-Ctrl on Windows.
        (
            "AltGr @",
            win32_key("KeyQ", "@", "q", Some("@"), down, altgr),
            b"\x1b[81;16;64;1;9;1_",
        ),
        // Text with no physical mapping travels as a pure character record.
        (
            "IME e-acute",
            win32_key(
                "Unidentified(0)",
                "\u{e9}",
                "\u{e9}",
                Some("\u{e9}"),
                down,
                none,
            ),
            b"\x1b[0;0;233;1;0;1_",
        ),
    ];
    for (label, event, expected) in cases {
        let encoded = win32_input_records(event).unwrap_or_else(|| panic!("{label}: no record"));
        assert_eq!(
            encoded,
            *expected,
            "{label}: got {:?}",
            String::from_utf8_lossy(&encoded)
        );
    }

    // A non-BMP character needs two UTF-16 code units, hence two records.
    let emoji = win32_input_records(&win32_key(
        "Unidentified(0)",
        "\u{1f600}",
        "\u{1f600}",
        Some("\u{1f600}"),
        down,
        none,
    ))
    .expect("emoji records");
    assert_eq!(emoji, b"\x1b[0;0;55357;1;0;1_\x1b[0;0;56832;1;0;1_");
}

#[test]
fn win32_input_mode_switches_the_whole_key_path_and_switches_back() {
    let mut terminal = TerminalEmulator::new(CoreTerminalSize::new(20, 4));
    let ctrl_b = win32_key(
        "KeyB",
        "b",
        "b",
        None,
        KeyState::Pressed,
        KeyModifiers {
            ctrl: true,
            ..KeyModifiers::default()
        },
    );
    let shift_only = win32_key(
        "ShiftLeft",
        "Shift",
        "Shift",
        None,
        KeyState::Pressed,
        KeyModifiers {
            shift: true,
            ..KeyModifiers::default()
        },
    );

    // Mode off: legacy bytes, and a bare modifier sends nothing.
    assert_eq!(
        encode_key_for_terminal(&terminal, &ctrl_b).as_deref(),
        Some(b"\x02".as_slice())
    );
    assert_eq!(encode_key_for_terminal(&terminal, &shift_only), None);

    // The application enables win32-input-mode: records for everything.
    terminal.apply_bytes(b"\x1b[?9001h").unwrap();
    assert_eq!(
        encode_key_for_terminal(&terminal, &ctrl_b).as_deref(),
        Some(b"\x1b[66;48;2;1;8;1_".as_slice())
    );
    assert_eq!(
        encode_key_for_terminal(&terminal, &shift_only).as_deref(),
        Some(b"\x1b[16;42;0;1;16;1_".as_slice())
    );

    // And back off again when the application resets it.
    terminal.apply_bytes(b"\x1b[?9001l").unwrap();
    assert_eq!(
        encode_key_for_terminal(&terminal, &ctrl_b).as_deref(),
        Some(b"\x02".as_slice())
    );
}

fn test_pane(cols: u16, rows: u16) -> PaneRuntime {
    PaneRuntime {
        semantic_rows_dropped: 0,
        alternate_screen_semantics: None,
        reconnect_policy: SshReconnectPolicy::default(),
        reconnect_attempts: 0,
        reconnect_at: None,
        terminal: TerminalEmulator::new(CoreTerminalSize::new(cols, rows)),
        semantic_parser: SemanticEscapeParser::new(),
        semantic_timeline: SemanticTimelineStore::new(),
        heuristic_detector: None,
        parse_semantic_events: false,
        remote_session: false,
        session_spec: SessionSpec::local("default"),
        last_size: TerminalGridSize::new(cols, rows),
        connection_state: PaneConnectionState::Disconnected("test".to_owned()),
        exit_code: None,
        disconnect_notified: false,
        ssh_prompt: None,
        osc52_prompt: None,
        ime_preedit: String::new(),
        ime_preedit_cursor: None,
        ime_preedit_cells: 0,
        transport: None,
        mouse_protocol: MouseProtocolState::default(),
        wheel_remainder: 0.0,
        selection_anchor: None,
        selection_kind: SelectionKind::Normal,
        keyboard_selection: None,
        search: PaneSearch::default(),
        command_output_collapsed: HashMap::new(),
        command_overlay_revision: 1,
        output_waker: test_transport_waker(),
        synchronized_output_since: None,
    }
}

#[test]
fn pane_resize_remaps_semantics_and_refreshes_search_matches() {
    let mut pane = test_pane(8, 2);
    pane.terminal
        .apply_bytes(b"zero\r\nabcdefgh\r\ntail")
        .expect("terminal fixture");
    let original = pane.terminal.state().search("cdef", true).remove(0);
    semantics::SemanticTimeline::prompt_started(
        &mut pane.semantic_timeline,
        BufferPosition::new(original.start.row, original.start.col),
        SemanticMetadata::default(),
    );
    semantics::SemanticTimeline::prompt_ended(
        &mut pane.semantic_timeline,
        BufferPosition::new(original.end.row, original.end.col),
    );
    pane.search.query = "cdef".to_owned();
    refresh_search_state(&mut pane.search, &mut pane.terminal);

    pane.resize(TerminalGridSize::new(4, 2), test_metrics());

    let search_match = pane.search.matches.first().copied().expect("search match");
    assert_eq!(
        pane.terminal.state().text_for_selection(search_match),
        "cdef"
    );
    let prompt = pane
        .semantic_timeline
        .regions()
        .find(|region| region.kind == SemanticRegionKind::Prompt)
        .expect("prompt region");
    let prompt_selection = Selection::normal(
        GridPosition::new(prompt.start.row, prompt.start.col),
        GridPosition::new(prompt.end.unwrap().row, prompt.end.unwrap().col),
    );
    assert_eq!(
        pane.terminal.state().text_for_selection(prompt_selection),
        "cdef"
    );
}

#[test]
fn synchronized_output_coalesces_frames_and_releases_after_150_ms() {
    let mut pane = test_pane(80, 24);
    pane.terminal
        .apply_bytes(b"\x1b[?2026hframe one")
        .expect("enable synchronized output");
    let started = Instant::now();

    assert!(!PaneRuntime::update_synchronized_output(
        &mut pane.terminal,
        &mut pane.synchronized_output_since,
        started,
        true,
    ));
    assert_eq!(
        pane.synchronized_output_deadline(),
        Some(started + SYNCHRONIZED_OUTPUT_TIMEOUT)
    );
    assert!(!PaneRuntime::update_synchronized_output(
        &mut pane.terminal,
        &mut pane.synchronized_output_since,
        started + Duration::from_millis(149),
        true,
    ));
    assert!(PaneRuntime::update_synchronized_output(
        &mut pane.terminal,
        &mut pane.synchronized_output_since,
        started + Duration::from_millis(150),
        false,
    ));
    assert!(
        !pane
            .terminal
            .modes_ref()
            .contains(&TerminalMode::SynchronizedOutput)
    );
}

#[test]
fn remote_osc52_prompt_never_displays_clipboard_contents() {
    let mut pane = test_pane(80, 24);
    pane.remote_session = true;
    pane.session_spec = SessionSpec::ssh("prod");
    let prompt = Osc52PromptState {
        request: SecurityOsc52Request {
            target: Osc52ClipboardTarget::Clipboard,
            payload_base64: "c2VjcmV0LWNsaXBib2FyZA==".to_owned(),
            remote: true,
        },
        reason: "explicit confirmation required".to_owned(),
        bytes: 16,
    };

    let lines = osc52_prompt_lines(&pane, &prompt).join("\n");

    assert!(lines.contains("prod"));
    assert!(lines.contains("16 bytes"));
    assert!(!lines.contains("secret-clipboard"));
    assert!(!lines.contains(&prompt.request.payload_base64));
}

#[test]
fn ime_preedit_cursor_range_is_kept_with_the_active_pane() {
    let mut pane = test_pane(80, 24);

    assert!(pane.update_ime_preedit("かな".to_owned(), Some((0, 3))));
    assert_eq!(pane.ime_preedit, "かな");
    assert_eq!(pane.ime_preedit_cursor, Some((0, 3)));
    assert_eq!(pane.ime_preedit_cells, 4);
}

#[test]
fn session_notifications_are_background_only_by_default() {
    let mut pane = test_pane(80, 24);
    pane.remote_session = true;
    pane.session_spec = SessionSpec::ssh("prod");
    let config = NotificationConfig::default();
    let poll = PanePollStats {
        closed: true,
        ..PanePollStats::default()
    };
    let mut provider = RecordingNotificationProvider::default();

    notify_for_pane_transition(&mut provider, &config, true, &pane, poll);
    assert!(provider.requests.is_empty());

    notify_for_pane_transition(&mut provider, &config, false, &pane, poll);
    assert_eq!(provider.requests.len(), 1);
    assert!(provider.requests[0].title.contains("SSH"));
    assert!(provider.requests[0].body.contains("prod"));
}

#[test]
fn pane_metadata_refresh_requires_content_or_lifecycle_change() {
    assert!(!pane_poll_needs_metadata_refresh(PanePollStats::default()));
    assert!(pane_poll_needs_metadata_refresh(PanePollStats {
        content_changed: true,
        ..PanePollStats::default()
    }));
    assert!(pane_poll_needs_metadata_refresh(PanePollStats {
        closed: true,
        ..PanePollStats::default()
    }));
    assert!(pane_poll_needs_metadata_refresh(PanePollStats {
        error: true,
        ..PanePollStats::default()
    }));
}

fn text_key(text: &str) -> KeyEvent {
    KeyEvent {
        physical_key: None,
        logical_key: text.to_owned(),
        logical_key_without_modifiers: text.to_owned(),
        text: Some(text.to_owned()),
        state: KeyState::Pressed,
        modifiers: KeyModifiers::default(),
        repeat: false,
    }
}

#[test]
fn unknown_ssh_host_prompt_requires_explicit_trust_action() {
    let key = security::HostKey::from_raw("host.example", 22, "ssh-ed25519", b"key");
    let request = HostKeyTrustRequest::unknown(key, "explicit decision required");
    let (response, decision) = mpsc::sync_channel(1);
    let mut prompt = SshPromptState::HostTrust {
        request,
        response: Some(response),
    };

    assert!(!prompt.handle_key(&text_key("x")));
    assert!(matches!(decision.try_recv(), Err(TryRecvError::Empty)));
    assert!(prompt.handle_key(&text_key("s")));
    assert_eq!(decision.recv().unwrap(), HostKeyTrustAction::TrustAndStore);
}

#[test]
fn ssh_secret_prompt_masks_and_returns_persistence_intent() {
    let request = SecretRequest::SshPassword {
        profile: "prod".to_owned(),
        host: "host.example".to_owned(),
        username: "alice".to_owned(),
    };
    let (response, result) = mpsc::sync_channel(1);
    let mut prompt = SshPromptState::Secret {
        request,
        keychain: KeychainProviderCapability {
            platform: security::SecurityPlatform::Windows,
            backend: security::KeychainBackend::WindowsCredentialManager,
            available: true,
            persistent: true,
            secure_storage: true,
            message: "available".to_owned(),
        },
        response: Some(response),
        input: String::new(),
        save_to_keychain: false,
    };

    assert!(!prompt.handle_key(&text_key("secret")));
    let rendered = ssh_prompt_lines(&prompt).join("\n");
    assert!(rendered.contains("******"));
    assert!(!rendered.contains("Secret: secret"));
    assert!(!prompt.handle_key(&key_event("Tab", KeyModifiers::default())));
    assert!(prompt.handle_key(&key_event("Enter", KeyModifiers::default())));
    let response = result.recv().unwrap().expect("secret response");
    assert!(response.save_to_keychain);
    assert_eq!(response.secret.expose(), "secret");
}

fn smoke_size() -> TransportSize {
    TransportSize::new(80, 24, 640, 384)
}

#[test]
fn paste_protection_normalizes_and_strips_controls() {
    let clipboard = ClipboardConfig::default();
    let paste = PasteConfig::default();

    let bytes = paste_bytes("a\r\nb\u{7}c", &clipboard, &paste, false);

    assert_eq!(String::from_utf8(bytes).unwrap(), "a\nbc");
}

#[test]
fn bracketed_paste_wraps_only_when_terminal_mode_is_enabled() {
    let clipboard = ClipboardConfig::default();
    let paste = PasteConfig::default();

    let bytes = paste_bytes("panea", &clipboard, &paste, true);

    assert_eq!(bytes, b"\x1b[200~panea\x1b[201~");
}

#[test]
fn middle_click_paste_is_suppressed_when_mouse_reporting_is_active() {
    let mouse = mouse_event(MouseEventKind::Pressed(MouseButton::Middle));
    let mut modes = BTreeSet::new();

    assert!(should_middle_click_paste(
        &mouse,
        &modes,
        &ClipboardConfig::default()
    ));

    modes.insert(TerminalMode::MouseReporting);
    assert!(!should_middle_click_paste(
        &mouse,
        &modes,
        &ClipboardConfig::default()
    ));
}

#[test]
fn focus_reports_are_emitted_only_when_requested() {
    let mut modes = BTreeSet::new();
    assert_eq!(focus_report_bytes(true, &modes), None);

    modes.insert(TerminalMode::FocusEvents);
    assert_eq!(focus_report_bytes(true, &modes), Some(b"\x1b[I".as_slice()));
    assert_eq!(
        focus_report_bytes(false, &modes),
        Some(b"\x1b[O".as_slice())
    );
}

#[test]
fn osc52_policy_mapping_keeps_remote_denied_by_default() {
    let policy = osc52_policy(&ClipboardConfig::default());
    let request = SecurityOsc52Request {
        target: Osc52ClipboardTarget::Clipboard,
        payload_base64: "cGFuZWE=".to_owned(),
        remote: true,
    };

    let decision = evaluate_osc52_clipboard_write(&request, &policy);

    assert!(
        matches!(decision, Osc52ClipboardDecision::Deny { reason } if reason.contains("remote"))
    );
}

#[test]
fn configured_keybindings_drive_mux_actions() {
    let config = AppConfig::default();
    let event = key_event(
        "T",
        KeyModifiers {
            ctrl: true,
            shift: true,
            ..KeyModifiers::default()
        },
    );

    assert_eq!(
        keybinding_action(&event, &config).as_deref(),
        Some("new_tab")
    );
    assert_eq!(
        canonical_key_spec("Shift+Ctrl+T"),
        canonical_key_event(&event)
    );

    let backspace = key_event(
        "Backspace",
        KeyModifiers {
            ctrl: true,
            ..KeyModifiers::default()
        },
    );
    assert_eq!(
        keybinding_action(&backspace, &config).as_deref(),
        Some("send_bytes:17")
    );
}

#[test]
fn send_bytes_keybinding_decodes_hex_payload() {
    assert_eq!(
        parse_send_bytes_action("send_bytes:17"),
        Ok(Some(vec![0x17]))
    );
    assert_eq!(
        parse_send_bytes_action("send_bytes:1b5b41"),
        Ok(Some(b"\x1b[A".to_vec()))
    );
    assert_eq!(parse_send_bytes_action("new_tab"), Ok(None));
}

#[test]
fn send_bytes_keybinding_rejects_malformed_payload() {
    assert!(parse_send_bytes_action("send_bytes:").is_err());
    assert!(parse_send_bytes_action("send_bytes:1").is_err());
    assert!(parse_send_bytes_action("send_bytes:zz").is_err());
}

#[test]
fn desktop_key_mapping_preserves_terminal_protocol_keys() {
    let mut event = key_event("ArrowUp", KeyModifiers::default());
    assert_eq!(terminal_key(&event), Some(TerminalKey::Up));

    event.logical_key = "F12".to_owned();
    assert_eq!(terminal_key(&event), Some(TerminalKey::Function(12)));

    event.physical_key = Some("Code(NumpadEnter)".to_owned());
    event.logical_key = "Enter".to_owned();
    assert_eq!(
        terminal_key(&event),
        Some(TerminalKey::Keypad(KeypadKey::Enter))
    );
}

#[test]
fn selection_visual_projects_only_visible_selected_cells() {
    let mut terminal = TerminalEmulator::new(CoreTerminalSize::new(4, 2));
    terminal
        .apply_bytes(b"abc\r\ndef\r\nghi")
        .expect("terminal input");
    terminal.state_mut().scroll_viewport(1);
    terminal.state_mut().set_selection(Selection::normal(
        GridPosition::new(0, 2),
        GridPosition::new(1, 1),
    ));
    let viewport = terminal.visible_grid().viewport;

    let visual =
        selection_visual(&terminal, viewport, &AppConfig::default()).expect("selection visual");

    assert_eq!(
        visual.cells,
        vec![
            CellPosition { row: 0, col: 2 },
            CellPosition { row: 0, col: 3 },
            CellPosition { row: 1, col: 0 },
            CellPosition { row: 1, col: 1 },
        ]
    );
}

#[test]
fn plain_mouse_click_does_not_leave_a_single_cell_selection() {
    let mut pane = test_pane(20, 4);
    pane.terminal.apply_bytes(b"panea").expect("terminal text");
    let metrics = test_metrics();
    let mut click = mouse_event(MouseEventKind::Pressed(MouseButton::Left));
    click.x = f64::from(metrics.cell_width) * 2.5;
    click.y = f64::from(metrics.cell_height) * 0.5;

    pane.handle_selection_or_scrollback(click, metrics);
    click.kind = MouseEventKind::Released(MouseButton::Left);
    pane.handle_selection_or_scrollback(click, metrics);

    assert!(pane.terminal.selection_state().is_none());
}

#[test]
fn mouse_drag_still_selects_across_terminal_cells() {
    let mut pane = test_pane(20, 4);
    pane.terminal.apply_bytes(b"panea").expect("terminal text");
    let metrics = test_metrics();
    let mut mouse = mouse_event(MouseEventKind::Pressed(MouseButton::Left));
    mouse.x = f64::from(metrics.cell_width) * 0.5;
    mouse.y = f64::from(metrics.cell_height) * 0.5;
    pane.handle_selection_or_scrollback(mouse, metrics);

    mouse.kind = MouseEventKind::Moved;
    mouse.x = f64::from(metrics.cell_width) * 2.5;
    pane.handle_selection_or_scrollback(mouse, metrics);
    mouse.kind = MouseEventKind::Released(MouseButton::Left);
    pane.handle_selection_or_scrollback(mouse, metrics);

    assert_eq!(
        pane.terminal.state().selected_text().as_deref(),
        Some("pan")
    );
}

#[test]
fn sgr_mouse_reports_press_drag_release_and_modifiers() {
    let mut protocol = MouseProtocolState::default();
    let mut modes = BTreeSet::from([TerminalMode::MouseCellMotion, TerminalMode::SgrMouse]);
    let metrics = test_metrics();
    let mut press = mouse_event(MouseEventKind::Pressed(MouseButton::Left));
    press.x = 15.0;
    press.y = 25.0;
    press.modifiers.shift = true;
    assert_eq!(
        protocol.report_bytes(press, metrics, &modes),
        Some(b"\x1b[<4;2;2M".to_vec())
    );

    let mut drag = press;
    drag.kind = MouseEventKind::Moved;
    assert_eq!(
        protocol.report_bytes(drag, metrics, &modes),
        Some(b"\x1b[<36;2;2M".to_vec())
    );

    let mut release = press;
    release.kind = MouseEventKind::Released(MouseButton::Left);
    assert_eq!(
        protocol.report_bytes(release, metrics, &modes),
        Some(b"\x1b[<4;2;2m".to_vec())
    );

    modes.clear();
    assert_eq!(protocol.report_bytes(press, metrics, &modes), None);
}

#[test]
fn utf8_and_urxvt_mouse_modes_use_their_distinct_wire_formats() {
    let mut protocol = MouseProtocolState::default();
    let metrics = test_metrics();
    let mut press = mouse_event(MouseEventKind::Pressed(MouseButton::Left));
    press.x = 2_555.0;
    press.y = 25.0;

    let utf8_modes = BTreeSet::from([TerminalMode::MouseReporting, TerminalMode::Utf8Mouse]);
    let utf8 = protocol
        .report_bytes(press, metrics, &utf8_modes)
        .expect("UTF-8 mouse report");
    assert!(utf8.starts_with(b"\x1b[M "));
    assert!(utf8.len() > 6, "large coordinates must use UTF-8 encoding");

    let urxvt_modes = BTreeSet::from([TerminalMode::MouseReporting, TerminalMode::UrxvtMouse]);
    assert_eq!(
        protocol.report_bytes(press, metrics, &urxvt_modes),
        Some(b"\x1b[32;320;2M".to_vec())
    );
}

#[test]
fn keyboard_selection_mode_extends_normal_and_rectangular_ranges() {
    let mut pane = test_pane(10, 2);
    pane.terminal.apply_bytes(b"abc").unwrap();
    pane.start_keyboard_selection(SelectionKind::Normal);

    assert!(pane.handle_keyboard_selection_key(&key_event("ArrowLeft", KeyModifiers::default())));
    assert_eq!(pane.terminal.state().selected_text().as_deref(), Some("c"));
    assert_eq!(
        pane.terminal
            .state()
            .selection_state()
            .map(|value| value.kind),
        Some(SelectionKind::Normal)
    );

    pane.start_keyboard_selection(SelectionKind::Rectangular);
    pane.handle_keyboard_selection_key(&key_event("ArrowLeft", KeyModifiers::default()));
    assert_eq!(
        pane.terminal
            .state()
            .selection_state()
            .map(|value| value.kind),
        Some(SelectionKind::Rectangular)
    );
    pane.handle_keyboard_selection_key(&key_event("Escape", KeyModifiers::default()));
    assert!(pane.terminal.state().selection_state().is_none());
}

#[test]
fn interactive_search_updates_navigates_and_closes_without_pty_input() {
    let mut pane = test_pane(20, 3);
    pane.terminal
        .apply_bytes(b"needle one\r\nneedle two")
        .unwrap();
    pane.search.start();

    assert!(pane.append_search_text("needle"));
    assert_eq!(pane.search.matches.len(), 2);
    assert_eq!(pane.search.active_match, 0);

    pane.handle_search_key(&key_event("Enter", KeyModifiers::default()));
    assert_eq!(pane.search.active_match, 1);
    pane.handle_search_key(&key_event("ArrowUp", KeyModifiers::default()));
    assert_eq!(pane.search.active_match, 0);
    pane.handle_search_key(&key_event("Escape", KeyModifiers::default()));
    assert!(!pane.search.input_active);
    assert!(pane.search.matches.is_empty());
}

#[test]
fn search_overlay_and_url_hit_testing_use_visible_terminal_content() {
    let mut pane = test_pane(40, 2);
    pane.terminal
        .apply_bytes("\u{754c} https://example.com now".as_bytes())
        .unwrap();
    pane.search.start();
    pane.append_search_text("example");
    let viewport = pane.terminal.visible_grid().viewport;
    assert_eq!(pane.search.rows.len(), 1);
    assert_eq!(pane.search.rows.values().map(Vec::len).sum::<usize>(), 1);

    let overlays = search_overlays(
        &pane.search,
        viewport,
        test_metrics(),
        &AppConfig::default(),
    );
    assert!(
        overlays
            .iter()
            .any(|overlay| overlay.kind == OverlayKind::SearchHighlight)
    );

    let mut mouse = mouse_event(MouseEventKind::Released(MouseButton::Left));
    mouse.x = 8.0 * 10.0;
    mouse.y = 2.0;
    assert_eq!(
        pane.url_at_mouse(mouse, test_metrics()).as_deref(),
        Some("https://example.com")
    );
    assert_eq!(visible_url_hints(&pane.terminal, 0)[0].start.col, 3);
}

#[test]
fn mux_layout_reserves_tab_bar_only_when_configured_and_needed() {
    let mut config = AppConfig::default();
    let mut model = MuxModel::new(SessionSpec::local("default"));
    model
        .new_tab("2", SessionSpec::local("default"))
        .expect("new tab");
    let active_tab = model.active_tab().id;
    let runtime = MuxRuntime {
        model,
        panes: HashMap::new(),
        surface_cols: 100,
        surface_rows: 30,
        performance: RuntimePerformanceCounters::new(),
        restore_sessions: false,
        state_path: std::env::temp_dir().join("panea-test-mux-state.json"),
        drag: None,
        output_waker: test_transport_waker(),
    };

    let layout = runtime.active_layouts(&config);
    assert_eq!(layout[0].rect.y, 1.0);
    assert_eq!(layout[0].terminal_size.rows, 29);
    let mut mouse = mouse_event(MouseEventKind::Pressed(MouseButton::Left));
    mouse.x = f64::from(horizontal_content_inset(&config)) + 8.0 * 7.0;
    mouse.y = f64::from(vertical_content_inset(&config)) + 4.0;
    assert_eq!(
        runtime.tab_at_mouse(mouse, test_metrics(), &config),
        Some(active_tab)
    );

    config.mux.show_tab_bar = false;
    let layout = runtime.active_layouts(&config);
    assert_eq!(layout[0].rect.y, 0.0);
    assert_eq!(layout[0].terminal_size.rows, 30);
}

#[test]
fn mux_scene_retains_a_disjoint_content_clip_for_each_pane() {
    let mut config = AppConfig::default();
    config.mux.show_tab_bar = false;
    let mut model = MuxModel::new(SessionSpec::local("default"));
    let first = model.active_tab().active_pane;
    let second = model
        .split_active_pane(SplitAxis::Horizontal, SessionSpec::local("default"))
        .expect("split pane");
    let mut first_pane = test_pane(40, 24);
    first_pane.terminal.apply_bytes(b"left").expect("left text");
    let mut second_pane = test_pane(40, 24);
    second_pane
        .terminal
        .apply_bytes(b"right")
        .expect("right text");
    let runtime = MuxRuntime {
        model,
        panes: HashMap::from([(first, first_pane), (second, second_pane)]),
        surface_cols: 80,
        surface_rows: 24,
        performance: RuntimePerformanceCounters::new(),
        restore_sessions: false,
        state_path: std::env::temp_dir().join("panea-test-mux-state.json"),
        drag: None,
        output_waker: test_transport_waker(),
    };

    let scene = scene_from_mux(
        None,
        &runtime,
        Some(test_metrics()),
        &config,
        None,
        None,
        None,
        CursorPresentation {
            blink_visible: true,
            window_focused: true,
        },
    );

    assert_eq!(scene.content_clips.len(), 2);
    let left = scene.content_clips[0];
    let right = scene.content_clips[1];
    assert!(left.bounds.x + left.bounds.width as i32 <= right.bounds.x);
    assert_eq!(left.cells.end, right.cells.start);
    assert_eq!(right.cells.end, scene.grid.cells.len());
    assert_eq!(
        scene.decorations.len(),
        1,
        "a two-pane split should render one foreground separator"
    );
    assert!(
        scene
            .semantic_overlays
            .iter()
            .all(|overlay| overlay.kind != OverlayKind::Decoration),
        "pane separators must not be terminal underlays that text can erase"
    );
    let separator = &scene.decorations[0];
    assert!(
        separator.bounds.x > 0,
        "separator must not outline the left edge"
    );
    assert_eq!(separator.bounds.y, 0);
    assert_eq!(separator.bounds.height, left.bounds.height);

    let cell_capacity = scene.grid.cells.capacity();
    let clip_capacity = scene.content_clips.capacity();
    let reused = scene_from_mux(
        Some(scene),
        &runtime,
        Some(test_metrics()),
        &config,
        None,
        None,
        None,
        CursorPresentation {
            blink_visible: true,
            window_focused: true,
        },
    );
    assert_eq!(reused.grid.cells.capacity(), cell_capacity);
    assert_eq!(reused.content_clips.capacity(), clip_capacity);
}

#[test]
fn scene_cache_reuses_layout_and_unchanged_pane_rows() {
    let mut config = AppConfig::default();
    config.mux.show_tab_bar = false;
    let mut model = MuxModel::new(SessionSpec::local("default"));
    let first = model.active_tab().active_pane;
    let second = model
        .split_active_pane(SplitAxis::Horizontal, SessionSpec::local("default"))
        .expect("split pane");
    let mut first_pane = test_pane(40, 24);
    first_pane.terminal.apply_bytes(b"left").expect("left text");
    let mut second_pane = test_pane(40, 24);
    second_pane
        .terminal
        .apply_bytes(b"right")
        .expect("right text");
    let mut runtime = MuxRuntime {
        model,
        panes: HashMap::from([(first, first_pane), (second, second_pane)]),
        surface_cols: 80,
        surface_rows: 24,
        performance: RuntimePerformanceCounters::new(),
        restore_sessions: false,
        state_path: std::env::temp_dir().join("panea-test-scene-cache.json"),
        drag: None,
        output_waker: test_transport_waker(),
    };
    let presentation = CursorPresentation {
        blink_visible: true,
        window_focused: true,
    };
    let mut cache = SceneCache::default();

    cache.prepare(
        &runtime,
        Some(test_metrics()),
        &config,
        1,
        None,
        None,
        None,
        presentation,
    );
    let first_stats = cache.last_update();
    assert!(first_stats.full_rebuild);
    assert_eq!(first_stats.layout_builds, 1);

    cache.prepare(
        &runtime,
        Some(test_metrics()),
        &config,
        1,
        None,
        None,
        None,
        presentation,
    );
    let retained = cache.last_update();
    assert!(!retained.full_rebuild);
    assert_eq!(retained.layout_hits, 1);
    assert_eq!(retained.rows_rebuilt, 0);
    assert_eq!(retained.rows_reused, 48);

    runtime
        .panes
        .get_mut(&first)
        .expect("first pane")
        .terminal
        .apply_bytes(b"!")
        .expect("changed row");
    cache.prepare(
        &runtime,
        Some(test_metrics()),
        &config,
        1,
        None,
        None,
        None,
        presentation,
    );
    let changed = cache.last_update();
    assert!(!changed.full_rebuild);
    assert_eq!(changed.layout_hits, 1);
    assert_eq!(changed.rows_rebuilt, 1);
    assert_eq!(changed.rows_reused, 47);

    runtime
        .panes
        .get_mut(&first)
        .expect("first pane")
        .terminal
        .state_mut()
        .apply_action(TerminalAction::MoveCursor {
            direction: term_core::CursorDirection::Back,
            count: 1,
        })
        .expect("cursor-only mutation");
    cache.prepare(
        &runtime,
        Some(test_metrics()),
        &config,
        1,
        None,
        None,
        None,
        presentation,
    );
    let cursor_only = cache.last_update();
    assert_eq!(cursor_only.rows_rebuilt, 0);
    assert_eq!(cursor_only.rows_reused, 48);

    runtime
        .panes
        .get_mut(&first)
        .expect("first pane")
        .terminal
        .state_mut()
        .set_selection(Selection {
            start: GridPosition::new(0, 0),
            end: GridPosition::new(0, 1),
            kind: SelectionKind::Normal,
        });
    cache.prepare(
        &runtime,
        Some(test_metrics()),
        &config,
        1,
        None,
        None,
        None,
        presentation,
    );
    let selection = cache.last_update();
    assert_eq!(selection.rows_rebuilt, 24);
    assert_eq!(selection.rows_reused, 24);

    cache.prepare(
        &runtime,
        Some(test_metrics()),
        &config,
        2,
        None,
        None,
        None,
        presentation,
    );
    let config_change = cache.last_update();
    assert!(config_change.full_rebuild);
    assert_eq!(config_change.layout_hits, 1);

    runtime.surface_cols = 81;
    cache.prepare(
        &runtime,
        Some(test_metrics()),
        &config,
        2,
        None,
        None,
        None,
        presentation,
    );
    let resize = cache.last_update();
    assert!(resize.full_rebuild);
    assert_eq!(resize.layout_builds, 1);
}

#[test]
fn scene_cache_rebuilds_changed_tab_chrome_without_recomputing_layout() {
    let config = AppConfig::default();
    let mut model = MuxModel::new(SessionSpec::local("default"));
    let pane_id = model.active_tab().active_pane;
    let second_tab = model
        .new_tab("2", SessionSpec::local("default"))
        .expect("new tab");
    let second_pane_id = model.active_tab().active_pane;
    model
        .switch_tab(model.active_workspace().active_window().tabs[0].id)
        .expect("switch to first tab");
    let mut runtime = MuxRuntime {
        model,
        panes: HashMap::from([
            (pane_id, test_pane(80, 23)),
            (second_pane_id, test_pane(80, 23)),
        ]),
        surface_cols: 80,
        surface_rows: 24,
        performance: RuntimePerformanceCounters::new(),
        restore_sessions: false,
        state_path: std::env::temp_dir().join("panea-test-scene-cache-title.json"),
        drag: None,
        output_waker: test_transport_waker(),
    };
    let presentation = CursorPresentation {
        blink_visible: true,
        window_focused: true,
    };
    let mut cache = SceneCache::default();
    cache.prepare(
        &runtime,
        Some(test_metrics()),
        &config,
        1,
        None,
        None,
        None,
        presentation,
    );

    runtime
        .model
        .update_pane_title(pane_id, "updated title")
        .expect("update pane title");
    cache.prepare(
        &runtime,
        Some(test_metrics()),
        &config,
        1,
        None,
        None,
        None,
        presentation,
    );

    let update = cache.last_update();
    assert!(update.full_rebuild);
    assert_eq!(update.layout_hits, 1);
    assert_eq!(update.layout_builds, 0);
    assert_ne!(runtime.model.active_tab().id, second_tab);
}

#[test]
fn odd_width_mux_scene_aligns_each_clip_with_its_first_cell() {
    let mut config = AppConfig::default();
    config.mux.show_tab_bar = false;
    let mut model = MuxModel::new(SessionSpec::local("default"));
    let first = model.active_tab().active_pane;
    let second = model
        .split_active_pane(SplitAxis::Horizontal, SessionSpec::local("default"))
        .expect("split pane");
    let runtime = MuxRuntime {
        model,
        panes: HashMap::from([(first, test_pane(41, 24)), (second, test_pane(40, 24))]),
        surface_cols: 81,
        surface_rows: 24,
        performance: RuntimePerformanceCounters::new(),
        restore_sessions: false,
        state_path: std::env::temp_dir().join("panea-test-mux-state.json"),
        drag: None,
        output_waker: test_transport_waker(),
    };

    let metrics = test_metrics();
    let scene = scene_from_mux(
        None,
        &runtime,
        Some(metrics),
        &config,
        None,
        None,
        None,
        CursorPresentation {
            blink_visible: true,
            window_focused: true,
        },
    );

    assert_eq!(scene.content_clips.len(), 2);
    for clip in &scene.content_clips {
        let first_cell = &scene.grid.cells[clip.cells.start];
        let first_cell_x = (f32::from(first_cell.position.col) * metrics.cell_width).floor() as i32;
        let first_cell_y = (first_cell.position.row as f32 * metrics.cell_height).floor() as i32;
        assert_eq!(clip.bounds.x, first_cell_x);
        assert_eq!(clip.bounds.y, first_cell_y);
    }
}

#[test]
fn mux_runtime_removes_a_cleanly_exited_split_pane() {
    let config = AppConfig::default();
    let mut model = MuxModel::new(SessionSpec::local("default"));
    let first = model.active_tab().active_pane;
    let exited = model
        .split_active_pane(SplitAxis::Horizontal, SessionSpec::local("default"))
        .expect("split pane");
    let mut runtime = MuxRuntime {
        model,
        panes: HashMap::from([(first, test_pane(40, 24)), (exited, test_pane(40, 24))]),
        surface_cols: 80,
        surface_rows: 24,
        performance: RuntimePerformanceCounters::new(),
        restore_sessions: false,
        state_path: std::env::temp_dir().join("panea-test-mux-state.json"),
        drag: None,
        output_waker: test_transport_waker(),
    };

    assert!(!runtime.close_cleanly_exited_panes(&[exited], test_metrics(), &config));
    assert!(!runtime.panes.contains_key(&exited));
    assert_eq!(runtime.model.active_tab().active_pane, first);
}

#[test]
fn mux_runtime_requests_window_exit_for_the_final_clean_session() {
    let config = AppConfig::default();
    let model = MuxModel::new(SessionSpec::local("default"));
    let exited = model.active_tab().active_pane;
    let mut runtime = MuxRuntime {
        model,
        panes: HashMap::from([(exited, test_pane(80, 24))]),
        surface_cols: 80,
        surface_rows: 24,
        performance: RuntimePerformanceCounters::new(),
        restore_sessions: false,
        state_path: std::env::temp_dir().join("panea-test-mux-state.json"),
        drag: None,
        output_waker: test_transport_waker(),
    };

    assert!(runtime.close_cleanly_exited_panes(&[exited], test_metrics(), &config));
    assert!(runtime.panes.contains_key(&exited));
}

#[test]
fn abnormal_local_exit_renders_an_actionable_session_overlay() {
    let model = MuxModel::new(SessionSpec::local("default"));
    let pane_id = model.active_tab().active_pane;
    let mut pane = test_pane(80, 24);
    pane.exit_code = Some(7);
    pane.connection_state = PaneConnectionState::Disconnected("session exited".to_owned());
    let runtime = MuxRuntime {
        model,
        panes: HashMap::from([(pane_id, pane)]),
        surface_cols: 80,
        surface_rows: 24,
        performance: RuntimePerformanceCounters::new(),
        restore_sessions: false,
        state_path: std::env::temp_dir().join("panea-test-mux-state.json"),
        drag: None,
        output_waker: test_transport_waker(),
    };
    let mut scene = RenderScene {
        grid: RenderGrid {
            columns: 80,
            rows: 24,
            cells: Vec::new(),
        },
        ..RenderScene::default()
    };

    append_session_product_overlay(&mut scene, &runtime, test_metrics());

    let label = scene.semantic_overlays[0]
        .label
        .as_deref()
        .expect("session overlay label");
    assert!(label.contains("code 7"));
    assert!(label.contains("Ctrl+Alt+R"));
}

#[test]
fn tab_drag_reorders_without_replacing_session_models() {
    let config = AppConfig::default();
    let mut model = MuxModel::new(SessionSpec::local("default"));
    let first = model.active_tab().id;
    let second = model
        .new_tab("2", SessionSpec::local("default"))
        .expect("new tab");
    let mut runtime = MuxRuntime {
        model,
        panes: HashMap::new(),
        surface_cols: 100,
        surface_rows: 30,
        performance: RuntimePerformanceCounters::new(),
        restore_sessions: false,
        state_path: std::env::temp_dir().join("panea-test-mux-state.json"),
        drag: None,
        output_waker: test_transport_waker(),
    };
    let metrics = test_metrics();
    let mut clipboard = ClipboardBridge::new();
    let mut press = mouse_event(MouseEventKind::Pressed(MouseButton::Left));
    press.x = f64::from(horizontal_content_inset(&config)) + 4.0;
    press.y = f64::from(vertical_content_inset(&config)) + 4.0;
    assert!(
        runtime
            .handle_mouse(
                press,
                metrics,
                &config,
                &config.clipboard,
                &config.paste,
                &mut clipboard,
            )
            .changed
    );
    assert_eq!(
        runtime.drag,
        Some(MuxDragState::Tab {
            source: first,
            target: first
        })
    );

    let first_width = formatted_tab_width(
        &config,
        &runtime.model.active_workspace().name,
        0,
        &runtime.model.active_workspace().active_window().tabs[0],
    );
    let mut moved = press;
    moved.kind = MouseEventKind::Moved;
    moved.x = f64::from(horizontal_content_inset(&config))
        + (first_width as f64 + 1.0) * f64::from(metrics.cell_width);
    assert!(
        runtime
            .handle_mouse(
                moved,
                metrics,
                &config,
                &config.clipboard,
                &config.paste,
                &mut clipboard,
            )
            .changed
    );
    assert_eq!(
        runtime.drag,
        Some(MuxDragState::Tab {
            source: first,
            target: second
        })
    );

    moved.kind = MouseEventKind::Released(MouseButton::Left);
    runtime.handle_mouse(
        moved,
        metrics,
        &config,
        &config.clipboard,
        &config.paste,
        &mut clipboard,
    );
    assert_eq!(
        runtime
            .model
            .active_workspace()
            .active_window()
            .tabs
            .iter()
            .map(|tab| tab.id)
            .collect::<Vec<_>>(),
        vec![second, first]
    );
}

#[test]
fn pane_drag_target_is_visual_only() {
    let config = AppConfig::default();
    let mut model = MuxModel::new(SessionSpec::local("default"));
    let source = model.active_tab().active_pane;
    let target = model
        .split_active_pane(SplitAxis::Vertical, SessionSpec::local("default"))
        .expect("split");
    let runtime = MuxRuntime {
        model,
        panes: HashMap::new(),
        surface_cols: 80,
        surface_rows: 24,
        performance: RuntimePerformanceCounters::new(),
        restore_sessions: false,
        state_path: std::env::temp_dir().join("panea-test-mux-state.json"),
        drag: Some(MuxDragState::Pane { source, target }),
        output_waker: test_transport_waker(),
    };
    let mut scene = RenderScene::default();
    let layouts = runtime.active_layouts(&config);
    append_mux_drag_overlay(&mut scene, &runtime, &layouts, test_metrics(), &config);
    assert_eq!(scene.semantic_overlays.len(), 1);
    assert_eq!(scene.semantic_overlays[0].kind, OverlayKind::DragTarget);
    assert!(scene.grid.cells.is_empty());
}

#[test]
fn startup_mux_snapshot_preserves_nested_local_and_ssh_transports() {
    let mut config = AppConfig::default();
    config.mux.startup_workspaces = vec![config_core::MuxWorkspaceConfig {
        name: "work".to_owned(),
        tabs: vec![config_core::MuxTabConfig {
            name: "mixed".to_owned(),
            layout: MuxLayoutConfig::Split {
                axis: MuxSplitAxisConfig::Vertical,
                ratio: 0.7,
                first: Box::new(MuxLayoutConfig::Pane {
                    profile: "default".to_owned(),
                    transport: MuxTransportConfig::Local,
                    working_directory: Some("local".to_owned()),
                }),
                second: Box::new(MuxLayoutConfig::Pane {
                    profile: "prod".to_owned(),
                    transport: MuxTransportConfig::Ssh,
                    working_directory: Some("remote".to_owned()),
                }),
            },
        }],
    }];

    let snapshot = startup_mux_snapshot(&config).expect("startup snapshot");
    let tab = &snapshot.workspaces[0].windows[0].tabs[0];
    assert_eq!(tab.panes.len(), 2);
    assert_eq!(
        tab.panes[0].transport,
        if cfg!(windows) {
            SessionTransportKind::WindowsPseudoconsole
        } else {
            SessionTransportKind::LocalPty
        }
    );
    assert_eq!(tab.panes[1].transport, SessionTransportKind::Ssh);
    assert_eq!(
        MuxModel::from_restore_snapshot(&snapshot, SessionSpec::local("default"))
            .expect("restore")
            .active_tab()
            .layout(LogicalRect::unit())
            .len(),
        2
    );
}

#[test]
fn shell_integration_full_mode_injects_supported_runtime_hook() {
    let mut config = AppConfig {
        default_shell_profile: Some("bash".to_owned()),
        shell_profiles: vec![ShellProfile {
            name: "bash".to_owned(),
            kind: ShellProfileKind::Custom,
            program: "bash".to_owned(),
            ..ShellProfile::default()
        }],
        ..AppConfig::default()
    };
    config.shell_integration.activation = ShellIntegrationActivationConfig::Full;

    let (profile, activation) = initial_local_shell_profile(&config, None);

    assert_eq!(
        activation.action,
        ShellIntegrationActivationAction::InjectRuntimeScript
    );
    assert_eq!(
        profile
            .env
            .get("PANEA_SHELL_INTEGRATION")
            .map(String::as_str),
        Some("full")
    );
    if cfg!(windows) {
        assert!(
            profile
                .startup_command
                .as_deref()
                .unwrap_or_default()
                .contains("777")
                || profile.args.iter().any(|arg| arg.contains("panea"))
        );
    } else {
        assert!(profile.args.iter().any(|arg| arg.contains("panea")));
    }
}

#[test]
fn shell_integration_off_mode_does_not_inject_or_parse() {
    let mut config = AppConfig {
        default_shell_profile: Some("bash".to_owned()),
        shell_profiles: vec![ShellProfile {
            name: "bash".to_owned(),
            kind: ShellProfileKind::Custom,
            program: "bash".to_owned(),
            ..ShellProfile::default()
        }],
        ..AppConfig::default()
    };
    config.shell_integration.activation = ShellIntegrationActivationConfig::Disabled;

    let (profile, activation) = initial_local_shell_profile(&config, None);

    assert_eq!(
        semantic_mode_for_activation(&activation),
        IntegrationMode::Disabled
    );
    assert!(!activation.parses_escape_sequences());
    assert!(profile.args.is_empty());
    assert_eq!(
        profile
            .env
            .get("PANEA_SHELL_INTEGRATION")
            .map(String::as_str),
        Some("0")
    );
}

#[test]
fn explicit_shell_args_prevent_runtime_hook_injection() {
    let mut config = AppConfig {
        default_shell_profile: Some("bash".to_owned()),
        shell_profiles: vec![ShellProfile {
            name: "bash".to_owned(),
            kind: ShellProfileKind::Custom,
            program: "bash".to_owned(),
            args: vec!["--login".to_owned()],
            ..ShellProfile::default()
        }],
        ..AppConfig::default()
    };
    config.shell_integration.activation = ShellIntegrationActivationConfig::Full;

    let (profile, activation) = initial_local_shell_profile(&config, None);

    assert_eq!(
        activation.action,
        ShellIntegrationActivationAction::InjectRuntimeScript
    );
    assert_eq!(profile.args, ["--login"]);
    assert!(profile.startup_command.is_none());
}

fn test_metrics() -> CellMetrics {
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
fn font_diagnostics_label_unmatched_style_faces_explicitly() {
    let label = format_font_diagnostic(
        "bold-face",
        "Panea Test Font",
        false,
        &FontSource::File(PathBuf::from("PaneaTest-Regular.ttf")),
    );

    assert_eq!(
        label,
        "bold-face:Panea Test Font=file:PaneaTest-Regular.ttf (style fallback)"
    );
}

fn test_semantic_viewport(origin_row: i64) -> SemanticOverlayViewport {
    SemanticOverlayViewport {
        origin_row,
        rows: 10,
        cols: 80,
        metrics: test_metrics(),
    }
}

fn command_timeline() -> SemanticTimelineStore {
    let mut timeline = SemanticTimelineStore::new();
    timeline.apply_event(semantics::SemanticEvent::ShellMetadataChanged {
        position: BufferPosition::new(0, 0),
        metadata: semantics::ShellMetadata {
            shell: Some("pwsh".to_owned()),
            current_working_directory: Some("C:\\Users\\shres\\panea".to_owned()),
            ..semantics::ShellMetadata::default()
        },
    });
    timeline.input_started(BufferPosition::new(1, 0));
    timeline.input_ended(BufferPosition::new(1, 10));
    timeline.output_started(BufferPosition::new(2, 0));
    timeline.command_finished(
        BufferPosition::new(4, 0),
        CommandStatus::Code(0),
        Duration::from_millis(42),
    );
    timeline
}

#[test]
fn semantic_navigation_selection_and_copy_use_raw_pane_text() {
    let mut pane = test_pane(40, 4);
    pane.terminal
        .apply_bytes(b"echo panea\r\npanea-output")
        .expect("terminal output");
    pane.semantic_timeline
        .input_started(BufferPosition::new(0, 0));
    pane.semantic_timeline
        .input_ended(BufferPosition::new(0, 10));
    pane.semantic_timeline
        .output_started(BufferPosition::new(1, 0));
    pane.semantic_timeline.command_finished(
        BufferPosition::new(1, 12),
        CommandStatus::Code(0),
        Duration::from_millis(5),
    );

    assert_eq!(
        pane.run_semantic_action(SemanticAction::CopyCurrentCommandOutput),
        SemanticActionResult::Text("panea-output".to_owned())
    );
    assert!(matches!(
        pane.run_semantic_action(SemanticAction::SelectCurrentCommandOutput),
        SemanticActionResult::Selection(_)
    ));
    assert_eq!(
        pane.terminal.state().selected_text().as_deref(),
        Some("panea-output")
    );
}

#[test]
fn command_block_overlays_include_groups_and_metadata_badges() {
    let timeline = command_timeline();
    let mut config = AppConfig::default();
    config.command_blocks.enabled = true;
    config.command_blocks.style = CommandBlockStyle::Card;
    config.visual_theme.grouping_style = InputOutputGroupingStyle::InputOutputSplit;

    let overlays = semantic_visual_overlays(
        &timeline,
        &HashMap::new(),
        false,
        test_semantic_viewport(0),
        &config,
    );

    assert!(
        overlays
            .iter()
            .any(|overlay| overlay.kind == OverlayKind::CommandBlock)
    );
    assert!(
        overlays
            .iter()
            .any(|overlay| overlay.kind == OverlayKind::InputOutputGroup)
    );
    let labels = overlays
        .iter()
        .filter_map(|overlay| overlay.label.as_deref())
        .collect::<Vec<_>>();
    assert!(labels.contains(&"ok"));
    assert!(labels.contains(&"42ms"));
    assert!(labels.iter().any(|label| label.starts_with("cwd ")));
    assert!(labels.contains(&"pwsh"));
    assert!(
        overlays
            .iter()
            .filter(|overlay| overlay.kind == OverlayKind::Badge)
            .all(|overlay| overlay.z_index > 20)
    );
}

#[test]
fn semantic_visuals_are_suppressed_in_alternate_screen_by_default() {
    let timeline = command_timeline();
    let mut config = AppConfig::default();
    config.command_blocks.enabled = true;
    config.prompt_decorations.enabled = true;

    let overlays = semantic_visual_overlays(
        &timeline,
        &HashMap::new(),
        true,
        test_semantic_viewport(0),
        &config,
    );
    assert!(overlays.is_empty());

    config.command_blocks.allow_in_alternate_screen = true;
    let overlays = semantic_visual_overlays(
        &timeline,
        &HashMap::new(),
        true,
        test_semantic_viewport(0),
        &config,
    );
    assert!(
        overlays
            .iter()
            .any(|overlay| overlay.kind == OverlayKind::CommandBlock)
    );
    assert!(
        !overlays
            .iter()
            .any(|overlay| overlay.kind == OverlayKind::PromptDecoration)
    );
}

#[test]
fn disabled_semantic_visuals_have_no_overlay_projection_cost() {
    let timeline = command_timeline();
    let config = AppConfig::default();

    let overlays = semantic_visual_overlays(
        &timeline,
        &HashMap::new(),
        false,
        test_semantic_viewport(0),
        &config,
    );

    assert!(overlays.is_empty());
}

#[test]
fn prompt_overlay_uses_real_metadata_elevation_and_previous_status() {
    let mut timeline = command_timeline();
    timeline.prompt_started(
        BufferPosition::new(5, 0),
        SemanticMetadata {
            shell: semantics::ShellMetadata {
                shell: Some("pwsh".to_owned()),
                current_working_directory: Some("C:\\work\\panea".to_owned()),
                ..semantics::ShellMetadata::default()
            },
            attributes: vec![("elevated".to_owned(), "true".to_owned())],
            ..SemanticMetadata::default()
        },
    );
    timeline.prompt_ended(BufferPosition::new(5, 8));
    let mut config = AppConfig::default();
    config.prompt_decorations.enabled = true;
    config.prompt_decorations.style = PromptDecorationStyle::RoundedBox;
    config.prompt_decorations.show_shell_badge = true;
    config.prompt_decorations.show_current_directory = true;
    config.prompt_decorations.show_admin_badge = true;
    config.prompt_decorations.show_previous_status_accent = true;

    let overlays = semantic_visual_overlays(
        &timeline,
        &HashMap::new(),
        false,
        test_semantic_viewport(0),
        &config,
    );
    let prompt = overlays
        .iter()
        .find(|overlay| overlay.kind == OverlayKind::PromptDecoration)
        .expect("prompt overlay");

    let label = prompt.label.as_deref().expect("prompt metadata label");
    assert!(label.contains("pwsh"));
    assert!(label.contains("panea"));
    assert!(label.contains("admin"));
    assert!(label.contains("ok"));
    assert_eq!(
        prompt.border_color,
        Some(render_color(config.visual_theme.success_color))
    );
}

#[test]
fn semantic_overlays_follow_absolute_scrollback_positions() {
    let mut timeline = SemanticTimelineStore::new();
    timeline.input_started(BufferPosition::new(101, 0));
    timeline.input_ended(BufferPosition::new(101, 4));
    timeline.output_started(BufferPosition::new(102, 0));
    timeline.command_finished(
        BufferPosition::new(104, 0),
        CommandStatus::Code(0),
        Duration::from_millis(1),
    );
    let mut config = AppConfig::default();
    config.command_blocks.enabled = true;
    config.command_blocks.style = CommandBlockStyle::Card;

    let overlays = semantic_visual_overlays(
        &timeline,
        &HashMap::new(),
        false,
        test_semantic_viewport(100),
        &config,
    );
    let block = overlays
        .iter()
        .find(|overlay| overlay.kind == OverlayKind::CommandBlock)
        .expect("visible command block");
    assert!(block.bounds.y < (test_metrics().cell_height * 5.0) as i32);
}

#[test]
fn collapsed_output_is_foreground_mask_and_preserves_raw_copy_text() {
    let mut pane = test_pane(40, 8);
    pane.terminal
        .apply_bytes(b"echo panea\r\none\r\ntwo\r\nthree\r\nfour")
        .expect("terminal output");
    pane.semantic_timeline
        .input_started(BufferPosition::new(0, 0));
    pane.semantic_timeline
        .input_ended(BufferPosition::new(0, 10));
    pane.semantic_timeline
        .output_started(BufferPosition::new(1, 0));
    pane.semantic_timeline.command_finished(
        BufferPosition::new(5, 0),
        CommandStatus::Code(0),
        Duration::from_millis(5),
    );
    let block_id = pane.semantic_timeline.command_blocks()[0].region_id;
    pane.command_output_collapsed.insert(block_id, true);
    let mut config = AppConfig::default();
    config.command_blocks.enabled = true;
    config.command_blocks.style = CommandBlockStyle::Card;
    config.command_blocks.collapsed_preview_lines = 1;

    let raw_before = pane.run_semantic_action(SemanticAction::CopyCurrentCommandOutput);
    let scene = scene_from_terminal(
        &pane.terminal,
        &pane.semantic_timeline,
        &pane.search,
        &pane.command_output_collapsed,
        Some(test_metrics()),
        &config,
        CursorPresentation {
            window_focused: true,
            blink_visible: true,
        },
    );
    let raw_after = pane.run_semantic_action(SemanticAction::CopyCurrentCommandOutput);

    assert_eq!(raw_before, raw_after);
    assert!(matches!(raw_after, SemanticActionResult::Text(text) if text.contains("four")));
    assert!(
        scene
            .semantic_overlays
            .iter()
            .any(|overlay| overlay.kind == OverlayKind::ContentMask
                && overlay.color.alpha == u8::MAX)
    );
}

#[test]
fn terminal_scene_appends_directly_with_pane_offsets() {
    let mut pane = test_pane(4, 2);
    pane.terminal.apply_bytes(b"x").expect("terminal output");
    let mut target = RenderScene::default();
    target.grid.cells.push(RenderCell {
        position: CellPosition { row: 0, col: 0 },
        text: "sentinel".into(),
        foreground: RenderColor::rgb(1, 2, 3),
        background: RenderColor::rgb(4, 5, 6),
        style: RenderCellStyle::default(),
    });

    append_terminal_scene(
        &mut target,
        &pane.terminal,
        &pane.semantic_timeline,
        &pane.search,
        &pane.command_output_collapsed,
        None,
        &AppConfig::default(),
        CursorPresentation {
            window_focused: true,
            blink_visible: true,
        },
        2,
        3,
        true,
    );

    assert_eq!(target.grid.cells[0].text, "sentinel");
    assert_eq!(
        target.grid.cells[1].position,
        CellPosition { row: 2, col: 3 }
    );
    assert_eq!(target.grid.cells[1].text, "x");
    assert_eq!(
        target.cursor.unwrap().position,
        CellPosition { row: 2, col: 4 }
    );
}

#[test]
fn traditional_command_style_projects_no_command_visuals() {
    let timeline = command_timeline();
    let mut config = AppConfig::default();
    config.command_blocks.enabled = true;
    config.command_blocks.style = CommandBlockStyle::Traditional;

    let overlays = semantic_visual_overlays(
        &timeline,
        &HashMap::new(),
        false,
        test_semantic_viewport(0),
        &config,
    );

    assert!(overlays.is_empty());
}

#[test]
fn disabled_performance_overlay_projects_no_scene_work() {
    let mut scene = RenderScene::default();
    scene.grid.columns = 80;
    scene.grid.rows = 24;
    let mut overlay = PerformanceOverlay::new(false, "test");
    overlay.record(RenderInstrumentation {
        frame_time: Duration::from_millis(16),
        ..RenderInstrumentation::default()
    });

    append_performance_overlay(
        &mut scene,
        &overlay,
        &PerformanceOverlayUiState {
            enabled: false,
            position: PerformanceOverlayPosition::TopRight,
            detail: PerformanceOverlayDetail::Compact,
            menu_open: false,
            persist: false,
            loaded_from_state: false,
            state_path: std::env::temp_dir().join("panea-test-ui-state.json"),
        },
        PerformanceBudget::default(),
        test_metrics(),
    );

    assert!(scene.semantic_overlays.is_empty());
}

#[test]
fn enabled_performance_overlay_is_visual_only() {
    let mut scene = RenderScene::default();
    scene.grid.columns = 80;
    scene.grid.rows = 24;
    let mut overlay = PerformanceOverlay::new(true, "test");
    overlay.record(RenderInstrumentation {
        frame_time: Duration::from_millis(16),
        cpu_prepare_time: Duration::from_millis(4),
        draw_call_count: 3,
        ..RenderInstrumentation::default()
    });

    append_performance_overlay(
        &mut scene,
        &overlay,
        &PerformanceOverlayUiState {
            enabled: true,
            position: PerformanceOverlayPosition::TopRight,
            detail: PerformanceOverlayDetail::Compact,
            menu_open: false,
            persist: false,
            loaded_from_state: false,
            state_path: std::env::temp_dir().join("panea-test-ui-state.json"),
        },
        PerformanceBudget::default(),
        test_metrics(),
    );

    assert!(
        scene
            .semantic_overlays
            .iter()
            .all(|overlay| overlay.kind == OverlayKind::PerformanceOverlay)
    );
    assert!(scene.grid.cells.is_empty());
}

#[test]
fn performance_overlay_click_menu_changes_runtime_preferences() {
    let config = AppConfig::default();
    let metrics = test_metrics();
    let mut overlay = PerformanceOverlay::new(true, "test");
    overlay.record(RenderInstrumentation {
        frame_time: Duration::from_millis(16),
        ..RenderInstrumentation::default()
    });
    let mut ui = PerformanceOverlayUiState {
        enabled: true,
        position: PerformanceOverlayPosition::TopLeft,
        detail: PerformanceOverlayDetail::Compact,
        menu_open: false,
        persist: false,
        loaded_from_state: false,
        state_path: std::env::temp_dir().join("panea-test-ui-state.json"),
    };
    let mut click = mouse_event(MouseEventKind::Pressed(MouseButton::Left));
    click.x = f64::from(horizontal_content_inset(&config)) + 12.0;
    click.y = f64::from(vertical_content_inset(&config)) + 12.0;
    assert!(handle_performance_overlay_mouse(
        click,
        &overlay,
        &mut ui,
        PerformanceBudget::default(),
        metrics,
        80,
        24,
        &config,
    ));
    assert!(ui.menu_open);

    let lines = performance_overlay_lines(&overlay, &ui, PerformanceBudget::default())
        .expect("overlay lines")
        .0;
    let layout = performance_overlay_layout(&lines, 80, 24, metrics, ui.position);
    let detail_row = layout.rows[2];
    click.x = f64::from(horizontal_content_inset(&config)) + f64::from(detail_row.x) + 2.0;
    click.y = f64::from(vertical_content_inset(&config)) + f64::from(detail_row.y) + 2.0;
    assert!(handle_performance_overlay_mouse(
        click,
        &overlay,
        &mut ui,
        PerformanceBudget::default(),
        metrics,
        80,
        24,
        &config,
    ));
    assert_eq!(ui.detail, PerformanceOverlayDetail::Detailed);
}

#[test]
fn static_cursor_resolution_honors_modes_terminal_requests_and_focus() {
    let mut config = AppConfig::default();
    config.cursor.shape = config_core::CursorShape::HollowBlock;
    config
        .cursor
        .mode_specific_styles
        .insert("insert".to_owned(), config_core::CursorShape::Beam);
    let mut modes = BTreeSet::new();

    assert_eq!(
        resolved_cursor_shape(&config, CursorShape::Block, &modes, true),
        RenderCursorShape::HollowBlock
    );
    assert_eq!(
        resolved_cursor_shape(&config, CursorShape::Underline, &modes, true),
        RenderCursorShape::Underline
    );
    modes.insert(TerminalMode::Insert);
    assert_eq!(
        resolved_cursor_shape(&config, CursorShape::Block, &modes, true),
        RenderCursorShape::Beam
    );
    assert_eq!(
        resolved_cursor_shape(&config, CursorShape::Block, &modes, false),
        RenderCursorShape::HollowBlock
    );
}

#[test]
fn cursor_presentation_does_not_restyle_or_decorate_terminal_text() {
    let mut pane = test_pane(80, 4);
    pane.terminal
        .apply_bytes(b"abc\x1b[2D https://example.com")
        .expect("terminal output");
    let mut config = AppConfig::default();
    config.colors.cursor_text = Some(config_core::RgbaColor {
        red: 1,
        green: 2,
        blue: 3,
        alpha: 255,
    });

    let scene = scene_from_terminal(
        &pane.terminal,
        &pane.semantic_timeline,
        &pane.search,
        &pane.command_output_collapsed,
        Some(test_metrics()),
        &config,
        CursorPresentation {
            window_focused: true,
            blink_visible: true,
        },
    );

    assert_eq!(
        scene.grid.cells[1].foreground,
        render_color(config.colors.foreground),
        "cursor presentation must not split or reshape the terminal text run"
    );
    assert_eq!(
        scene.cursor.and_then(|cursor| cursor.text_color),
        config.colors.cursor_text.map(render_color)
    );
    assert!(
        scene.semantic_overlays.is_empty(),
        "detected URLs must not alter application-owned UI by default"
    );
}

#[test]
fn relative_cursor_assets_resolve_from_the_config_directory() {
    let base = Path::new("portable-config");
    assert_eq!(
        resolve_cursor_image_path("assets/cursor.gif", Some(base)),
        base.join("assets/cursor.gif")
    );
    let absolute = std::env::temp_dir().join("panea-cursor.png");
    assert_eq!(
        resolve_cursor_image_path(&absolute.to_string_lossy(), Some(base)),
        absolute
    );
}

#[test]
fn mouse_bindings_are_modifier_order_independent() {
    let config = config_core::MouseConfig {
        bindings: vec![config_core::MouseBinding::new(
            "Shift+Ctrl+LeftRelease",
            "copy",
        )],
        ..config_core::MouseConfig::default()
    };
    let event = MouseEvent {
        kind: MouseEventKind::Released(MouseButton::Left),
        x: 0.0,
        y: 0.0,
        modifiers: KeyModifiers {
            ctrl: true,
            shift: true,
            ..KeyModifiers::default()
        },
    };

    assert_eq!(
        mousebinding_action(&event, &config).as_deref(),
        Some("copy")
    );
}

#[test]
fn indexed_color_mapping_covers_ansi_cube_and_grayscale() {
    let config = AppConfig::default();
    assert_eq!(
        ansi_color(1, &config),
        render_color(config.colors.palette[1])
    );
    assert_eq!(ansi_color(16, &config), RenderColor::rgb(0, 0, 0));
    assert_eq!(ansi_color(196, &config), RenderColor::rgb(255, 0, 0));
    assert_eq!(ansi_color(255, &config), RenderColor::rgb(238, 238, 238));
}

#[test]
fn window_padding_and_margin_reduce_the_terminal_extent() {
    let mut config = AppConfig::default();
    config.window.padding_x = 8;
    config.window.margin_x = 4;
    assert_eq!(horizontal_content_inset(&config), 12);
    assert_eq!(content_extent(100, horizontal_content_inset(&config)), 76);
}

#[test]
#[ignore = "spawns a real PowerShell process"]
fn real_powershell_emits_semantic_shell_events() {
    run_real_shell_semantic_smoke(
        ShellKind::PowerShell,
        LocalShellProfile::powershell().with_args([
            "-NoLogo".to_owned(),
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            shell_integration::verification_sequence(
                ShellKind::PowerShell,
                "panea-shell-integration-smoke",
            )
            .expect("PowerShell verification sequence"),
        ]),
    );
}

#[test]
#[ignore = "spawns an interactive PowerShell process with runtime integration"]
fn real_powershell_runtime_activation_emits_complete_command_cycle() {
    let mut config = AppConfig {
        default_shell_profile: Some("powershell".to_owned()),
        shell_profiles: vec![ShellProfile {
            name: "powershell".to_owned(),
            kind: ShellProfileKind::PowerShell,
            program: "powershell.exe".to_owned(),
            ..ShellProfile::default()
        }],
        ..AppConfig::default()
    };
    config.shell_integration.activation = ShellIntegrationActivationConfig::Full;
    let (profile, activation) = initial_local_shell_profile(&config, None);
    assert_eq!(
        activation.action,
        ShellIntegrationActivationAction::InjectRuntimeScript
    );
    run_interactive_shell_activation_smoke(profile);
}

#[test]
#[ignore = "spawns an interactive PowerShell process and waits for startup to settle"]
fn real_default_powershell_startup_events_keep_one_prompt() {
    let config = AppConfig {
        default_shell_profile: Some("powershell".to_owned()),
        shell_profiles: vec![ShellProfile {
            name: "powershell".to_owned(),
            kind: ShellProfileKind::PowerShell,
            program: "powershell.exe".to_owned(),
            ..ShellProfile::default()
        }],
        ..AppConfig::default()
    };
    let (profile, _) = initial_local_shell_profile(&config, None);
    let initial_size = TransportSize::new(120, 36, 960, 576);
    let resized = TransportSize::new(147, 42, 1176, 672);
    let mut transport = LocalPtyTransport::spawn(profile, initial_size).expect("spawn shell");
    let mut terminal = TerminalEmulator::new(CoreTerminalSize::new(120, 36));
    let mut raw = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut resized_after_prompt = false;
    let mut focus_reported_after_prompt = false;

    while Instant::now() < deadline {
        let output = transport.poll_output().expect("poll shell output");
        if !output.bytes.is_empty() {
            terminal
                .apply_bytes(&output.bytes)
                .expect("apply startup output");
            flush_terminal_responses(&mut terminal, &mut transport);
            raw.extend_from_slice(&output.bytes);
        }
        if !resized_after_prompt && raw.windows(3).any(|window| window == b"PS ") {
            terminal
                .resize(CoreTerminalSize::new(resized.cols, resized.rows))
                .expect("resize terminal grid");
            transport.resize(resized).expect("resize PTY");
            resized_after_prompt = true;
        }
        if resized_after_prompt && !focus_reported_after_prompt {
            transport
                .write_input(b"\x1b[I")
                .expect("send initial focus report");
            focus_reported_after_prompt = true;
        }
        thread::sleep(Duration::from_millis(10));
    }

    let visible = terminal
        .visible_grid()
        .cells
        .chunks(usize::from(resized.cols))
        .map(|cells| {
            let mut line = term_core::Line::default();
            line.cells = cells.to_vec();
            line
        })
        .map(|line| line.raw_text())
        .collect::<Vec<_>>()
        .join("\n");
    let prompt_count = visible
        .lines()
        .filter(|line| line.trim_start().starts_with("PS ") || line.trim() == "PS>")
        .count();
    let _ = transport.shutdown();
    assert_eq!(
        prompt_count,
        1,
        "PowerShell startup events produced {prompt_count} prompts; resized_after_prompt={resized_after_prompt}; focus_reported_after_prompt={focus_reported_after_prompt}; visible={visible:?}; raw={:?}",
        String::from_utf8_lossy(&raw)
    );
}

#[test]
#[ignore = "spawns an interactive PowerShell process and verifies grid/cursor coherence"]
fn real_powershell_input_echo_keeps_grid_and_cursor_coherent() {
    let profile = LocalShellProfile::powershell();
    // Match the normal desktop launch more closely: Panea starts at a
    // modest window size, then may receive multiple grow/resize events as
    // the native window is presented or maximized.
    let size = TransportSize::new(86, 26, 944, 548);
    let mut transport = LocalPtyTransport::spawn(profile, size).expect("spawn shell");
    let mut terminal = TerminalEmulator::new(CoreTerminalSize::new(size.cols, size.rows));
    let startup_deadline = Instant::now() + Duration::from_secs(2);
    let mut protocol_trace = Vec::new();

    while Instant::now() < startup_deadline {
        let output = transport.poll_output().expect("poll shell output");
        if !output.bytes.is_empty() {
            terminal
                .apply_bytes(&output.bytes)
                .expect("apply startup output");
            let responses = terminal.state_mut().take_pending_output();
            protocol_trace.push(format!(
                "startup output={:?} cursor={:?} response={:?}",
                String::from_utf8_lossy(&output.bytes),
                terminal.cursor_state().position,
                String::from_utf8_lossy(&responses)
            ));
            if !responses.is_empty() {
                transport
                    .write_input(&responses)
                    .expect("write startup terminal response");
            }
        }
        if terminal_visible_lines(&terminal)
            .iter()
            .any(|line| line.trim_start().starts_with("PS "))
        {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }

    let resized = TransportSize::new(171, 42, 1868, 868);
    terminal
        .resize(CoreTerminalSize::new(resized.cols, resized.rows))
        .expect("resize terminal grid before input");
    transport.resize(resized).expect("resize PTY before input");
    transport
        .resize(resized)
        .expect("repeat native resize event before input");
    const MARKER: &str = "panea-grid-cursor-check";
    const INPUT: &str = "Write-Output panea-grid-cursor-check";
    let mut typed = String::new();
    for character in INPUT.chars() {
        let mut encoded = [0u8; 4];
        let bytes = character.encode_utf8(&mut encoded).as_bytes();
        protocol_trace.push(format!(
            "input={character:?} cursor={:?}",
            terminal.cursor_state().position
        ));
        write_terminal_input(&mut terminal, &mut transport, bytes);
        typed.push(character);
        let visible_prefix = typed.trim_end();

        let echo_deadline = Instant::now() + Duration::from_millis(250);
        while Instant::now() < echo_deadline {
            let output = transport.poll_output().expect("poll input echo");
            if !output.bytes.is_empty() {
                terminal
                    .apply_bytes(&output.bytes)
                    .expect("apply input echo");
                let responses = terminal.state_mut().take_pending_output();
                protocol_trace.push(format!(
                    "echo output={:?} cursor={:?} response={:?}",
                    String::from_utf8_lossy(&output.bytes),
                    terminal.cursor_state().position,
                    String::from_utf8_lossy(&responses)
                ));
                if !responses.is_empty() {
                    transport
                        .write_input(&responses)
                        .expect("write input terminal response");
                }
            }
            if terminal_visible_lines(&terminal)
                .iter()
                .any(|line| line.trim_start().starts_with("PS ") && line.contains(visible_prefix))
            {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }

        let lines = terminal_visible_lines(&terminal);
        let input_row = lines
                .iter()
                .position(|line| {
                    line.trim_start().starts_with("PS ") && line.contains(visible_prefix)
                })
                .unwrap_or_else(|| {
                    panic!(
                        "PowerShell did not echo typed prefix {typed:?}; cursor={:?}; visible={lines:?}; trace:\n{protocol_trace}",
                        terminal.cursor_state().position,
                        protocol_trace = protocol_trace.join("\n")
                    )
                });
        assert!(
            lines[input_row].trim_start().starts_with("PS "),
            "typed prefix moved away from its prompt; prefix={typed:?}; cursor={:?}; visible={lines:?}",
            terminal.cursor_state().position
        );
    }

    let lines = terminal_visible_lines(&terminal);
    let prompt_count = lines
        .iter()
        .filter(|line| line.trim_start().starts_with("PS "))
        .count();
    assert_eq!(
        prompt_count,
        1,
        "startup/resize produced duplicate prompts before submission; cursor={:?}; visible={lines:?}",
        terminal.cursor_state().position
    );
    let input_row = lines
        .iter()
        .position(|line| line.contains(INPUT))
        .expect("typed input must be visible");
    let input_line = &lines[input_row];
    assert!(
        input_line.trim_start().starts_with("PS "),
        "input echo moved away from its prompt; cursor={:?}; visible={lines:?}",
        terminal.cursor_state().position
    );
    let input_end_col = input_line.find(INPUT).expect("input column") + INPUT.len();
    assert_eq!(
        terminal.cursor_state().position,
        GridPosition::new(input_row as i64, input_end_col as u16),
        "cursor does not follow the visible input; visible={lines:?}"
    );

    transport.write_input(b"\r\n").expect("submit input");
    let command_deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < command_deadline {
        let output = transport.poll_output().expect("poll command output");
        if !output.bytes.is_empty() {
            terminal
                .apply_bytes(&output.bytes)
                .expect("apply command output");
            flush_terminal_responses(&mut terminal, &mut transport);
        }
        let lines = terminal_visible_lines(&terminal);
        if lines
            .iter()
            .enumerate()
            .any(|(row, line)| row > input_row && line.trim() == MARKER)
            && lines
                .iter()
                .enumerate()
                .any(|(row, line)| row > input_row && line.trim_start().starts_with("PS "))
        {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }

    let lines = terminal_visible_lines(&terminal);
    let output_row = lines
        .iter()
        .enumerate()
        .find_map(|(row, line)| (row > input_row && line.trim() == MARKER).then_some(row))
        .expect("command output must be below submitted input");
    let next_prompt_row = lines
        .iter()
        .enumerate()
        .find_map(|(row, line)| {
            (row > output_row && line.trim_start().starts_with("PS ")).then_some(row)
        })
        .expect("next prompt must be below command output");
    assert_eq!(
        terminal.cursor_state().position.row,
        next_prompt_row as i64,
        "cursor row must match the next visible prompt; visible={lines:?}"
    );
    let _ = transport.shutdown();
}

fn terminal_visible_lines(terminal: &TerminalEmulator) -> Vec<String> {
    let visible = terminal.visible_grid();
    visible
        .cells
        .chunks(usize::from(visible.viewport.size.cols.max(1)))
        .map(|cells| {
            let mut line = term_core::Line::default();
            line.cells = cells.to_vec();
            line
        })
        .map(|line| line.raw_text())
        .collect()
}

fn run_interactive_shell_activation_smoke(profile: LocalShellProfile) {
    let marker = b"panea-runtime-integration-smoke";
    let size = smoke_size();
    let mut transport = LocalPtyTransport::spawn(profile, size).expect("spawn shell");
    let mut query_terminal = TerminalEmulator::new(CoreTerminalSize::new(size.cols, size.rows));
    let mut parser = SemanticEscapeParser::new();
    let mut events = Vec::new();
    let mut bytes = Vec::new();
    let mut command_sent = false;
    let deadline = Instant::now() + Duration::from_secs(5);

    while Instant::now() < deadline {
        let output = transport.poll_output().expect("poll shell output");
        if !output.bytes.is_empty() {
            query_terminal
                .apply_bytes(&output.bytes)
                .expect("apply shell output for terminal queries");
            flush_terminal_responses(&mut query_terminal, &mut transport);
            for event in parser.parse(&output.bytes, BufferPosition::new(0, 0)) {
                events.push(event.event.kind());
            }
            bytes.extend_from_slice(&output.bytes);
        }
        if !command_sent && events.contains(&SemanticEventKind::InputStarted) {
            transport
                .write_input(b"Write-Output panea-runtime-integration-smoke\r\n")
                .expect("write command");
            command_sent = true;
        }
        let observed_marker = bytes.windows(marker.len()).any(|window| window == marker);
        if observed_marker
            && [
                SemanticEventKind::PromptStarted,
                SemanticEventKind::PromptEnded,
                SemanticEventKind::InputStarted,
                SemanticEventKind::InputEnded,
                SemanticEventKind::OutputStarted,
                SemanticEventKind::OutputEnded,
                SemanticEventKind::CommandFinished,
                SemanticEventKind::CurrentWorkingDirectoryChanged,
                SemanticEventKind::ShellMetadataChanged,
            ]
            .iter()
            .all(|kind| events.contains(kind))
        {
            let _ = transport.shutdown();
            return;
        }
        if output.closed {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }

    let diagnostics = transport.diagnostics();
    let _ = transport.shutdown();
    panic!(
        "interactive shell integration did not complete; command_sent={command_sent}, bytes={}, events={events:?}, diagnostics={diagnostics:?}",
        bytes.len()
    );
}

#[test]
#[ignore = "spawns a real bash process"]
fn real_bash_emits_semantic_shell_events() {
    run_real_shell_semantic_smoke(
        ShellKind::Bash,
        LocalShellProfile::custom("bash", "bash").with_args([
            "-lc".to_owned(),
            shell_integration::verification_sequence(
                ShellKind::Bash,
                "panea-shell-integration-smoke",
            )
            .expect("bash verification sequence"),
        ]),
    );
}

#[test]
#[ignore = "spawns a real zsh process"]
fn real_zsh_emits_semantic_shell_events() {
    run_real_shell_semantic_smoke(
        ShellKind::Zsh,
        LocalShellProfile::custom("zsh", "zsh").with_args([
            "-lc".to_owned(),
            shell_integration::verification_sequence(
                ShellKind::Zsh,
                "panea-shell-integration-smoke",
            )
            .expect("zsh verification sequence"),
        ]),
    );
}

#[test]
#[ignore = "spawns a real fish process"]
fn real_fish_emits_semantic_shell_events() {
    run_real_shell_semantic_smoke(
        ShellKind::Fish,
        LocalShellProfile::custom("fish", "fish").with_args([
            "-c".to_owned(),
            shell_integration::verification_sequence(
                ShellKind::Fish,
                "panea-shell-integration-smoke",
            )
            .expect("fish verification sequence"),
        ]),
    );
}

fn run_real_shell_semantic_smoke(shell: ShellKind, profile: LocalShellProfile) {
    let marker = b"panea-shell-integration-smoke";
    let size = smoke_size();
    let mut transport = match LocalPtyTransport::spawn(profile.clone(), size) {
        Ok(transport) => transport,
        Err(error) => {
            eprintln!(
                "skipping real {shell:?} semantic smoke because spawn failed for {}: {error}",
                profile.program
            );
            return;
        }
    };
    let mut query_terminal = TerminalEmulator::new(CoreTerminalSize::new(size.cols, size.rows));
    let mut parser = SemanticEscapeParser::new();
    let mut bytes = Vec::new();
    let mut events = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);

    while std::time::Instant::now() < deadline {
        let output = transport.poll_output().expect("poll shell output");
        if !output.bytes.is_empty() {
            query_terminal
                .apply_bytes(&output.bytes)
                .expect("apply shell output for terminal queries");
            flush_terminal_responses(&mut query_terminal, &mut transport);
            events.extend(
                parser
                    .parse(&output.bytes, BufferPosition::new(0, 0))
                    .into_iter()
                    .map(|parsed| parsed.event.kind()),
            );
            bytes.extend(output.bytes);
        }

        if bytes.windows(marker.len()).any(|window| window == marker)
            && events.contains(&SemanticEventKind::CommandFinished)
        {
            let _ = transport.shutdown();
            assert!(events.contains(&SemanticEventKind::ShellMetadataChanged));
            assert!(events.contains(&SemanticEventKind::OutputStarted));
            return;
        }

        if output.closed {
            break;
        }

        std::thread::sleep(Duration::from_millis(10));
    }

    let diagnostics = transport.diagnostics();
    let _ = transport.shutdown();
    panic!(
        "real {shell:?} semantic smoke did not observe expected events; bytes={}, events={events:?}, diagnostics={diagnostics:?}",
        bytes.len()
    );
}

#[test]
fn fullscreen_chrome_hover_never_changes_native_window_mode() {
    let desktop_source = include_str!("../../src/main.rs");
    let platform_source = include_str!("../../../../crates/platform-winit/src/lib.rs");
    for forbidden in [
        ["reveal_native_fullscreen", "_titlebar"].concat(),
        ["hide_native_fullscreen", "_titlebar"].concat(),
        ["NativeFullscreen", "TitlebarState"].concat(),
    ] {
        assert!(
            !desktop_source.contains(&forbidden) && !platform_source.contains(&forbidden),
            "fullscreen hover must not reconstruct native window state through {forbidden}"
        );
    }
}

#[test]
fn fullscreen_chrome_routes_top_edge_before_terminal_mouse_input() {
    let now = Instant::now();
    let mut chrome = test_fullscreen_chrome(true, ChromeMotion::Instant);
    let event = MouseEvent {
        kind: MouseEventKind::Moved,
        x: 24.0,
        y: 1.0,
        modifiers: KeyModifiers::default(),
    };

    let route = route_fullscreen_chrome_mouse(&mut chrome, event, now);

    assert!(route.consumed);
    assert!(route.redraw);
    assert_eq!(route.action, None);
}

#[test]
fn fullscreen_chrome_disabled_routes_mouse_to_terminal() {
    let now = Instant::now();
    let mut chrome = test_fullscreen_chrome(false, ChromeMotion::Instant);
    let event = MouseEvent {
        kind: MouseEventKind::Moved,
        x: 24.0,
        y: 1.0,
        modifiers: KeyModifiers::default(),
    };

    assert_eq!(
        route_fullscreen_chrome_mouse(&mut chrome, event, now),
        FullscreenChromeRoute::terminal()
    );
}

#[test]
fn fullscreen_chrome_control_release_dispatches_exactly_one_action() {
    let now = Instant::now();
    let mut chrome = test_fullscreen_chrome(true, ChromeMotion::Instant);
    route_fullscreen_chrome_mouse(
        &mut chrome,
        MouseEvent {
            kind: MouseEventKind::Moved,
            x: 24.0,
            y: 1.0,
            modifiers: KeyModifiers::default(),
        },
        now,
    );
    let press = MouseEvent {
        kind: MouseEventKind::Pressed(MouseButton::Left),
        x: 980.0,
        y: 20.0,
        modifiers: KeyModifiers::default(),
    };
    let release = MouseEvent {
        kind: MouseEventKind::Released(MouseButton::Left),
        ..press
    };

    let pressed = route_fullscreen_chrome_mouse(&mut chrome, press, now);
    let released =
        route_fullscreen_chrome_mouse(&mut chrome, release, now + Duration::from_millis(1));
    let repeated_release =
        route_fullscreen_chrome_mouse(&mut chrome, release, now + Duration::from_millis(2));

    assert!(pressed.consumed);
    assert_eq!(pressed.action, None);
    assert_eq!(
        released.action,
        Some(platform_core::WindowChromeAction::Close)
    );
    assert_eq!(repeated_release.action, None);
}

#[test]
fn fullscreen_chrome_visual_does_not_change_terminal_geometry() {
    let now = Instant::now();
    let mut chrome = test_fullscreen_chrome(true, ChromeMotion::Instant);
    route_fullscreen_chrome_mouse(
        &mut chrome,
        MouseEvent {
            kind: MouseEventKind::Moved,
            x: 24.0,
            y: 1.0,
            modifiers: KeyModifiers::default(),
        },
        now,
    );
    let mut scene = RenderScene {
        grid: RenderGrid {
            columns: 120,
            rows: 40,
            cells: Vec::new(),
        },
        content_offset: RenderOffset { x: 8, y: 12 },
        ..RenderScene::default()
    };
    let grid = scene.grid.clone();
    let offset = scene.content_offset;

    append_fullscreen_chrome_visual(&mut scene, &chrome, "Panea", true);

    assert_eq!(scene.grid, grid);
    assert_eq!(scene.content_offset, offset);
    assert!(scene.window_chrome.is_some());
}

#[test]
fn fullscreen_chrome_only_reload_does_not_resize_terminal_sessions() {
    let chrome_only = ReloadPlan {
        live: vec![ReloadableSection::WindowChrome],
        restart_required: Vec::new(),
    };
    let font_reload = ReloadPlan {
        live: vec![ReloadableSection::Font],
        restart_required: Vec::new(),
    };

    assert!(!reload_requires_terminal_resize(&chrome_only));
    assert!(reload_requires_terminal_resize(&font_reload));
}

#[test]
fn fullscreen_chrome_instrumentation_allocates_metrics_only_when_active() {
    let mut instrumentation = FullscreenChromeInstrumentation::new(false);
    instrumentation.mark_frame();
    instrumentation.record_presented_frame(
        &[RenderRect {
            x: 0,
            y: 0,
            width: 1_920,
            height: 36,
        }],
        RenderInstrumentation {
            draw_call_count: 2,
            ..RenderInstrumentation::default()
        },
    );
    assert!(instrumentation.metrics().is_none());

    instrumentation.set_active(true);
    instrumentation.mark_frame();
    instrumentation.record_presented_frame(
        &[RenderRect {
            x: 0,
            y: 0,
            width: 1_920,
            height: 36,
        }],
        RenderInstrumentation {
            draw_call_count: 2,
            ..RenderInstrumentation::default()
        },
    );

    assert_eq!(
        instrumentation
            .metrics()
            .expect("active chrome metrics")
            .animation_frames,
        1
    );
}

fn test_fullscreen_chrome(
    enabled: bool,
    motion: fullscreen_chrome::ChromeMotion,
) -> fullscreen_chrome::FullscreenChromeController {
    fullscreen_chrome::FullscreenChromeController::new(fullscreen_chrome::ChromeSettings {
        enabled,
        surface_width: 1_000,
        chrome_height: 48,
        reveal_height: 3,
        control_width: 48,
        motion,
        transition_duration: Duration::from_millis(120),
        hide_delay: Duration::from_millis(120),
        frame_interval: Duration::from_millis(16),
    })
}

/// End-to-end regression through a real nested ConPTY.
///
/// The unit tables above pin the encoding; this pins the contract with a live
/// Windows-native multiplexer: it enables win32-input-mode, and keystrokes
/// encoded by the production dispatcher must reach its pane. Skips where the
/// binary is unavailable, so CI on other machines stays green.
#[test]
#[ignore = "launches the real wmux binary"]
fn win32_input_records_from_the_production_encoder_reach_a_live_multiplexer_pane() {
    let config_path = r"C:\Users\shres\panea\personal-config\wmux\config.wmux";
    if std::process::Command::new("wmux.exe")
        .arg("--version")
        .output()
        .is_err()
        || !std::path::Path::new(config_path).exists()
    {
        eprintln!("win32 integration skipped: wmux.exe or its personal config is not available");
        return;
    }

    let profile = LocalShellProfile::custom("wmux-win32", "wmux.exe")
        .with_args(["new-session", "-s", "panea-win32"])
        .with_env("WMUX_CONFIG", config_path)
        .with_env(
            "WMUX_SHELL",
            r#"powershell.exe -NoLogo -ExecutionPolicy Bypass -NoExit -File "C:\Users\shres\panea\personal-config\powershell.ps1""#,
        );
    let mut transport = LocalPtyTransport::spawn(profile, TransportSize::new(80, 24, 800, 480))
        .expect("spawn wmux");
    let mut terminal = TerminalEmulator::new(CoreTerminalSize::new(80, 24));
    let mut raw_in = Vec::new();

    let pump = |transport: &mut LocalPtyTransport,
                terminal: &mut TerminalEmulator,
                raw_in: &mut Vec<u8>,
                for_ms: u64| {
        let deadline = Instant::now() + Duration::from_millis(for_ms);
        while Instant::now() < deadline {
            let output = transport.poll_output().expect("poll");
            if output.bytes.is_empty() {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            raw_in.extend_from_slice(&output.bytes);
            let _ = terminal.apply_bytes(&output.bytes);
            let reply = terminal.state_mut().take_pending_output();
            if !reply.is_empty() {
                transport.write_input(&reply).expect("write reply");
            }
        }
    };

    pump(&mut transport, &mut terminal, &mut raw_in, 8000);

    // The premise of the whole feature: this multiplexer asks for records.
    assert!(
        terminal.modes_ref().contains(&TerminalMode::Win32InputMode),
        "wmux is expected to enable win32-input-mode; modes were {:?}",
        terminal.modes_ref()
    );

    let none = KeyModifiers::default();
    let shift = KeyModifiers {
        shift: true,
        ..none
    };
    // Keys the multiplexer accepts: a shifted letter, an unshifted OEM symbol,
    // and Enter. The shifted digit row is deliberately absent — wmux drops those
    // client-side even for byte-identical Windows Terminal records, so asserting
    // on them would be testing someone else's bug.
    /// physical key, logical key, unmodified key, text, modifiers, echoed char.
    type LiveKeyCase<'a> = (
        &'a str,
        &'a str,
        &'a str,
        Option<&'a str>,
        KeyModifiers,
        char,
    );
    let cases: &[LiveKeyCase<'_>] = &[
        ("KeyD", "D", "d", Some("D"), shift, 'D'),
        ("Minus", "-", "-", Some("-"), none, '-'),
        ("KeyQ", "q", "q", Some("q"), none, 'q'),
    ];

    for (physical, logical, bare, text, modifiers, expected) in cases {
        let press = win32_key(
            physical,
            logical,
            bare,
            *text,
            KeyState::Pressed,
            *modifiers,
        );
        let release = win32_key(
            physical,
            logical,
            bare,
            *text,
            KeyState::Released,
            *modifiers,
        );
        let mut bytes = encode_key_for_terminal(&terminal, &press)
            .unwrap_or_else(|| panic!("{physical}: no encoding"));
        bytes.extend_from_slice(
            &encode_key_for_terminal(&terminal, &release)
                .unwrap_or_else(|| panic!("{physical}: no release encoding")),
        );
        assert!(
            bytes.starts_with(b"\x1b[") && bytes.ends_with(b"_"),
            "{physical}: expected win32 records, got {:?}",
            escaped(&bytes)
        );

        let before = raw_in.len();
        transport.write_input(&bytes).expect("write key");
        pump(&mut transport, &mut terminal, &mut raw_in, 900);
        let echoed = escaped(&raw_in[before..]);
        assert!(
            echoed.contains(*expected),
            "{physical} encoded as {} must reach the pane; echoed {echoed:?}",
            escaped(&bytes)
        );
    }

    // Enter has to be a record too, and distinguishable from Ctrl+Enter.
    let enter = win32_key("Enter", "Enter", "Enter", None, KeyState::Pressed, none);
    let ctrl_enter = win32_key(
        "Enter",
        "Enter",
        "Enter",
        None,
        KeyState::Pressed,
        KeyModifiers { ctrl: true, ..none },
    );
    let enter_bytes = encode_key_for_terminal(&terminal, &enter).expect("encode Enter");
    let ctrl_enter_bytes =
        encode_key_for_terminal(&terminal, &ctrl_enter).expect("encode Ctrl+Enter");
    assert_ne!(
        enter_bytes, ctrl_enter_bytes,
        "Enter and Ctrl+Enter must differ under win32-input-mode"
    );
    let before = raw_in.len();
    transport.write_input(&enter_bytes).expect("write Enter");
    pump(&mut transport, &mut terminal, &mut raw_in, 1200);
    assert!(
        !raw_in[before..].is_empty(),
        "Enter as a record must produce a response from the pane"
    );

    // Leave the user's server as it was found.
    let _ = std::process::Command::new("wmux.exe")
        .args(["kill-session", "-t", "panea-win32"])
        .output();
}
