// Workspace, tab, pane-tree orchestration, persistence, and mux polling.

fn mux_state_path() -> PathBuf {
    if cfg!(target_os = "windows") {
        return std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("Panea")
            .join("mux-state.json");
    }
    if cfg!(target_os = "macos") {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("Library")
            .join("Application Support")
            .join("Panea")
            .join("mux-state.json");
    }
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("state"))
        })
        .unwrap_or_else(std::env::temp_dir)
        .join("panea")
        .join("mux-state.json")
}

const MUX_STATE_SAVE_DEBOUNCE: Duration = Duration::from_millis(250);

#[derive(Debug)]
enum MuxStateSaveRequest {
    Debounced {
        path: PathBuf,
        snapshot: RestoreSnapshot,
    },
    Flush {
        path: PathBuf,
        snapshot: RestoreSnapshot,
        completion: Option<SyncSender<Result<(), String>>>,
    },
    Barrier {
        completion: SyncSender<()>,
    },
}

fn mux_state_temp_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("mux-state.json");
    path.with_file_name(format!("{file_name}.tmp"))
}

fn write_mux_state_atomically(path: &Path, snapshot: &RestoreSnapshot) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let serialized = serde_json::to_vec_pretty(snapshot).map_err(|error| error.to_string())?;
    let temporary_path = mux_state_temp_path(path);
    fs::write(&temporary_path, serialized).map_err(|error| error.to_string())?;
    fs::rename(&temporary_path, path).map_err(|error| error.to_string())
}

fn mux_state_save_sender() -> &'static mpsc::Sender<MuxStateSaveRequest> {
    static SENDER: OnceLock<mpsc::Sender<MuxStateSaveRequest>> = OnceLock::new();
    SENDER.get_or_init(|| {
        let (sender, receiver) = mpsc::channel();
        thread::Builder::new()
            .name("panea-mux-state-save".to_owned())
            .spawn(move || mux_state_save_worker(&receiver))
            .expect("spawn mux state save worker");
        sender
    })
}

fn schedule_mux_state_save(path: PathBuf, snapshot: RestoreSnapshot) {
    if mux_state_save_sender()
        .send(MuxStateSaveRequest::Debounced { path, snapshot })
        .is_err()
    {
        eprintln!("mux state save worker is unavailable");
    }
}

#[cfg(test)]
fn flush_mux_state_save(path: PathBuf, snapshot: RestoreSnapshot) -> Result<(), String> {
    let (completion, completed) = mpsc::sync_channel(1);
    mux_state_save_sender()
        .send(MuxStateSaveRequest::Flush {
            path,
            snapshot,
            completion: Some(completion),
        })
        .map_err(|_| "mux state save worker is unavailable".to_owned())?;
    completed
        .recv_timeout(Duration::from_secs(2))
        .map_err(|_| "timed out waiting for mux state save".to_owned())?
}

fn enqueue_mux_state_flush(path: PathBuf, snapshot: RestoreSnapshot) {
    if mux_state_save_sender()
        .send(MuxStateSaveRequest::Flush {
            path,
            snapshot,
            completion: None,
        })
        .is_err()
    {
        eprintln!("mux state save worker is unavailable");
    }
}

fn wait_for_mux_state_saves() -> Result<(), String> {
    let (completion, completed) = mpsc::sync_channel(1);
    mux_state_save_sender()
        .send(MuxStateSaveRequest::Barrier { completion })
        .map_err(|_| "mux state save worker is unavailable".to_owned())?;
    completed
        .recv_timeout(Duration::from_secs(2))
        .map_err(|_| "timed out waiting for mux state save worker".to_owned())
}

fn mux_state_save_worker(receiver: &Receiver<MuxStateSaveRequest>) {
    let mut pending = HashMap::<PathBuf, (RestoreSnapshot, Instant)>::new();
    loop {
        if pending.is_empty() {
            let Ok(request) = receiver.recv() else {
                return;
            };
            handle_mux_state_save_request(request, &mut pending);
        }

        let now = Instant::now();
        let wait = pending
            .values()
            .map(|(_, deadline)| deadline.saturating_duration_since(now))
            .min()
            .unwrap_or(MUX_STATE_SAVE_DEBOUNCE);
        match receiver.recv_timeout(wait) {
            Ok(request) => {
                handle_mux_state_save_request(request, &mut pending);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let now = Instant::now();
                let ready = pending
                    .iter()
                    .filter(|(_, (_, deadline))| *deadline <= now)
                    .map(|(path, _)| path.clone())
                    .collect::<Vec<_>>();
                for path in ready {
                    if let Some((snapshot, _)) = pending.remove(&path) {
                        save_debounced_mux_state(&path, &snapshot);
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                for (path, (snapshot, _)) in pending {
                    save_debounced_mux_state(&path, &snapshot);
                }
                return;
            }
        }
    }
}

fn handle_mux_state_save_request(
    request: MuxStateSaveRequest,
    pending: &mut HashMap<PathBuf, (RestoreSnapshot, Instant)>,
) {
    match request {
        MuxStateSaveRequest::Debounced { path, snapshot } => {
            pending.insert(path, (snapshot, Instant::now() + MUX_STATE_SAVE_DEBOUNCE));
        }
        MuxStateSaveRequest::Flush {
            path,
            snapshot,
            completion,
        } => {
            pending.remove(&path);
            let result = write_mux_state_atomically(&path, &snapshot);
            if let Some(completion) = completion {
                let _ = completion.send(result);
            } else if let Err(error) = result {
                eprintln!(
                    "mux state could not be flushed to {}: {error}",
                    path.display()
                );
            }
        }
        MuxStateSaveRequest::Barrier { completion } => {
            let _ = completion.send(());
        }
    }
}

fn save_debounced_mux_state(path: &Path, snapshot: &RestoreSnapshot) {
    if let Err(error) = write_mux_state_atomically(path, snapshot) {
        eprintln!(
            "mux state could not be saved to {}: {error}",
            path.display()
        );
    }
}

fn desktop_ui_state_path() -> PathBuf {
    mux_state_path()
        .parent()
        .map_or_else(std::env::temp_dir, Path::to_path_buf)
        .join("ui-state.json")
}

#[derive(Debug, Clone)]
struct PerformanceOverlayUiState {
    enabled: bool,
    position: PerformanceOverlayPosition,
    detail: PerformanceOverlayDetail,
    menu_open: bool,
    persist: bool,
    loaded_from_state: bool,
    state_path: PathBuf,
}

impl PerformanceOverlayUiState {
    fn new(config: &config_core::DiagnosticsConfig) -> Self {
        let state_path = desktop_ui_state_path();
        let mut state = Self {
            enabled: config.performance_overlay,
            position: config.performance_overlay_position,
            detail: config.performance_overlay_detail,
            menu_open: false,
            persist: config.persist_performance_overlay,
            loaded_from_state: false,
            state_path,
        };
        if state.persist {
            state.load();
        }
        state
    }

    fn load(&mut self) {
        let Ok(contents) = fs::read_to_string(&self.state_path) else {
            return;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents) else {
            eprintln!(
                "performance overlay preference ignored: {} is invalid JSON",
                self.state_path.display()
            );
            return;
        };
        if let Some(enabled) = value.get("enabled").and_then(serde_json::Value::as_bool) {
            self.enabled = enabled;
        }
        if let Some(position) = value.get("position").and_then(serde_json::Value::as_str) {
            self.position = match position {
                "top_left" => PerformanceOverlayPosition::TopLeft,
                "bottom_left" => PerformanceOverlayPosition::BottomLeft,
                "bottom_right" => PerformanceOverlayPosition::BottomRight,
                _ => PerformanceOverlayPosition::TopRight,
            };
        }
        if let Some(detail) = value.get("detail").and_then(serde_json::Value::as_str) {
            self.detail = if detail == "detailed" {
                PerformanceOverlayDetail::Detailed
            } else {
                PerformanceOverlayDetail::Compact
            };
        }
        self.loaded_from_state = true;
    }

    fn apply_config(&mut self, config: &config_core::DiagnosticsConfig) {
        self.persist = config.persist_performance_overlay;
        if !self.persist || !self.loaded_from_state {
            self.enabled = config.performance_overlay;
            self.position = config.performance_overlay_position;
            self.detail = config.performance_overlay_detail;
        }
        if !self.enabled {
            self.menu_open = false;
        }
    }

    fn toggle(&mut self) {
        self.enabled = !self.enabled;
        self.menu_open = false;
        self.persist();
    }

    fn cycle_detail(&mut self) {
        self.detail = match self.detail {
            PerformanceOverlayDetail::Compact => PerformanceOverlayDetail::Detailed,
            PerformanceOverlayDetail::Detailed => PerformanceOverlayDetail::Compact,
        };
        self.persist();
    }

    fn cycle_position(&mut self) {
        self.position = match self.position {
            PerformanceOverlayPosition::TopLeft => PerformanceOverlayPosition::TopRight,
            PerformanceOverlayPosition::TopRight => PerformanceOverlayPosition::BottomRight,
            PerformanceOverlayPosition::BottomRight => PerformanceOverlayPosition::BottomLeft,
            PerformanceOverlayPosition::BottomLeft => PerformanceOverlayPosition::TopLeft,
        };
        self.persist();
    }

    fn hide(&mut self) {
        self.enabled = false;
        self.menu_open = false;
        self.persist();
    }

    fn persist(&mut self) {
        if !self.persist {
            return;
        }
        let Some(parent) = self.state_path.parent() else {
            return;
        };
        if let Err(error) = fs::create_dir_all(parent) {
            eprintln!("performance overlay preference directory failed: {error}");
            return;
        }
        let value = serde_json::json!({
            "enabled": self.enabled,
            "position": performance_overlay_position_name(self.position),
            "detail": performance_overlay_detail_name(self.detail),
        });
        if let Err(error) = serde_json::to_string_pretty(&value)
            .map_err(|error| error.to_string())
            .and_then(|json| fs::write(&self.state_path, json).map_err(|error| error.to_string()))
        {
            eprintln!("performance overlay preference save failed: {error}");
            return;
        }
        self.loaded_from_state = true;
    }

    fn diagnostic(&self) -> String {
        format!(
            "enabled={} position={} detail={} persistence={} source={}",
            self.enabled,
            performance_overlay_position_name(self.position),
            performance_overlay_detail_name(self.detail),
            self.persist,
            if self.loaded_from_state {
                "runtime preference"
            } else {
                "config"
            }
        )
    }
}

const fn performance_overlay_position_name(position: PerformanceOverlayPosition) -> &'static str {
    match position {
        PerformanceOverlayPosition::TopLeft => "top_left",
        PerformanceOverlayPosition::TopRight => "top_right",
        PerformanceOverlayPosition::BottomLeft => "bottom_left",
        PerformanceOverlayPosition::BottomRight => "bottom_right",
    }
}

const fn performance_overlay_detail_name(detail: PerformanceOverlayDetail) -> &'static str {
    match detail {
        PerformanceOverlayDetail::Compact => "compact",
        PerformanceOverlayDetail::Detailed => "detailed",
    }
}

fn initial_mux_model(config: &AppConfig, state_path: &PathBuf) -> MuxModel {
    let fallback = session_spec_for_config(config);
    if config.mux.restore_sessions {
        match fs::read_to_string(state_path) {
            Ok(contents) => match serde_json::from_str::<RestoreSnapshot>(&contents)
                .map_err(|error| error.to_string())
                .and_then(|snapshot| {
                    MuxModel::from_restore_snapshot(&snapshot, fallback.clone())
                        .map_err(|error| error.to_string())
                }) {
                Ok(model) => return model,
                Err(error) => eprintln!(
                    "mux restore fallback: {} could not be restored: {error}",
                    state_path.display()
                ),
            },
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => eprintln!(
                "mux restore fallback: {} could not be read: {error}",
                state_path.display()
            ),
            Err(_) => {}
        }
    }

    if let Some(snapshot) = startup_mux_snapshot(config) {
        match MuxModel::from_restore_snapshot(&snapshot, fallback.clone()) {
            Ok(model) => return model,
            Err(error) => eprintln!("startup mux layout rejected: {error}"),
        }
    }

    let mut model = MuxModel::new(fallback);
    model.active_workspace_mut().name = config.mux.default_workspace.clone();
    model
}

fn startup_mux_snapshot(config: &AppConfig) -> Option<RestoreSnapshot> {
    if config.mux.startup_workspaces.is_empty() {
        return None;
    }
    let mut next_pane_id = 1u64;
    let workspaces = config
        .mux
        .startup_workspaces
        .iter()
        .map(|workspace| WorkspaceRestore {
            name: workspace.name.clone(),
            windows: vec![WindowRestore {
                active_tab_name: workspace.tabs.first().map(|tab| tab.name.clone()),
                tabs: workspace
                    .tabs
                    .iter()
                    .map(|tab| {
                        let mut panes = Vec::new();
                        let layout =
                            startup_layout_tree(&tab.layout, &mut next_pane_id, &mut panes);
                        TabRestore {
                            name: tab.name.clone(),
                            active_pane: layout
                                .first_pane()
                                .expect("validated startup layout contains a pane"),
                            layout,
                            panes,
                        }
                    })
                    .collect(),
            }],
        })
        .collect();
    Some(RestoreSnapshot { workspaces })
}

fn startup_layout_tree(
    layout: &MuxLayoutConfig,
    next_pane_id: &mut u64,
    panes: &mut Vec<PaneRestore>,
) -> SplitTree {
    match layout {
        MuxLayoutConfig::Pane {
            profile,
            transport,
            working_directory,
        } => {
            let pane_id = PaneId(*next_pane_id);
            *next_pane_id = next_pane_id.saturating_add(1);
            panes.push(PaneRestore {
                pane_id,
                session_profile: profile.clone(),
                transport: match transport {
                    MuxTransportConfig::Local => {
                        if cfg!(windows) {
                            SessionTransportKind::WindowsPseudoconsole
                        } else {
                            SessionTransportKind::LocalPty
                        }
                    }
                    MuxTransportConfig::Ssh => SessionTransportKind::Ssh,
                },
                working_directory: working_directory.clone(),
            });
            SplitTree::Pane(pane_id)
        }
        MuxLayoutConfig::Split {
            axis,
            ratio,
            first,
            second,
        } => SplitTree::Split {
            axis: match axis {
                MuxSplitAxisConfig::Horizontal => SplitAxis::Horizontal,
                MuxSplitAxisConfig::Vertical => SplitAxis::Vertical,
            },
            children: vec![
                startup_layout_tree(first, next_pane_id, panes),
                startup_layout_tree(second, next_pane_id, panes),
            ],
            ratios: vec![*ratio, 1.0 - *ratio],
        },
    }
}

fn pane_session_specs(model: &MuxModel) -> Vec<(PaneId, SessionSpec)> {
    let mut specs = Vec::new();
    for workspace in model.workspaces.values() {
        for window in &workspace.windows {
            for tab in &window.tabs {
                for pane in tab.panes.values() {
                    if let Some(session) = tab.sessions.get(&pane.session_id) {
                        specs.push((pane.id, session.spec.clone()));
                    }
                }
            }
        }
    }
    specs
}

struct MuxRuntime {
    model: MuxModel,
    panes: HashMap<PaneId, PaneRuntime>,
    surface_cols: u16,
    surface_rows: u16,
    performance: RuntimePerformanceCounters,
    restore_sessions: bool,
    state_path: PathBuf,
    drag: Option<MuxDragState>,
    output_waker: TransportWakeHandle,
}

#[derive(Debug, Clone, Copy, Default)]
struct MuxPollOutcome {
    content_changed: bool,
    exit_application: bool,
}

#[derive(Clone, Copy)]
struct MuxPollContext<'a> {
    osc52_policy: &'a Osc52ClipboardPolicy,
    clipboard_config: &'a ClipboardConfig,
    notification_config: &'a NotificationConfig,
    window_focused: bool,
    metrics: CellMetrics,
    config: &'a AppConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MuxDragState {
    Tab { source: TabId, target: TabId },
    Pane { source: PaneId, target: PaneId },
}

impl MuxRuntime {
    fn new(
        config: &AppConfig,
        metrics: CellMetrics,
        width: u32,
        height: u32,
        output_waker: TransportWakeHandle,
    ) -> Self {
        let state_path = mux_state_path();
        let mut model = initial_mux_model(config, &state_path);

        let surface_cols = cols_for_width(
            content_extent(width, horizontal_content_inset(config)),
            metrics,
        )
        .max(1);
        let surface_rows = rows_for_height(
            content_extent(height, vertical_content_inset(config)),
            metrics,
        )
        .max(1);
        let initial_size = TerminalGridSize::new(
            surface_cols,
            surface_rows
                .saturating_sub(tab_bar_rows(&model, config))
                .max(1),
        );
        let mut panes = HashMap::new();
        for (pane_id, spec) in pane_session_specs(&model) {
            let pane = PaneRuntime::new(config, &spec, initial_size, metrics, output_waker.clone());
            let status = session_status_for_pane(&pane);
            panes.insert(pane_id, pane);
            mark_session_status(&mut model, pane_id, status);
        }

        let mut runtime = Self {
            model,
            panes,
            surface_cols,
            surface_rows,
            performance: RuntimePerformanceCounters::new(),
            restore_sessions: config.mux.restore_sessions,
            state_path,
            drag: None,
            output_waker,
        };
        runtime.resize_all(width, height, metrics, config);
        runtime
    }

    fn resize_all(&mut self, width: u32, height: u32, metrics: CellMetrics, config: &AppConfig) {
        self.surface_cols = cols_for_width(
            content_extent(width, horizontal_content_inset(config)),
            metrics,
        )
        .max(1);
        self.surface_rows = rows_for_height(
            content_extent(height, vertical_content_inset(config)),
            metrics,
        )
        .max(1);
        self.resize_active_tab(metrics, config);
    }

    fn update_terminal_dynamic_colors(&mut self, config: &AppConfig) {
        let foreground = config_rgb(config.colors.foreground);
        let background = config_rgb(config.colors.background);
        for pane in self.panes.values_mut() {
            pane.terminal
                .state_mut()
                .set_dynamic_colors(foreground, background);
        }
    }

    fn resize_active_tab(&mut self, metrics: CellMetrics, config: &AppConfig) {
        let layouts = self.active_layouts(config);
        for layout in layouts {
            if let Some(pane) = self.panes.get_mut(&layout.pane_id) {
                pane.resize(layout.terminal_size, metrics);
            }
            if let Some(model_pane) = self.model.active_tab_mut().panes.get_mut(&layout.pane_id) {
                model_pane.last_size = Some(layout.terminal_size);
            }
        }
    }

    fn handle_mux_action(
        &mut self,
        action: MuxAction,
        config: &AppConfig,
        metrics: CellMetrics,
        width: u32,
        height: u32,
    ) -> bool {
        if !config.mux.enabled {
            eprintln!("mux action ignored because mux.enabled is false");
            return false;
        }

        let result = match action {
            MuxAction::NewTab => {
                self.new_tab_with_spec(
                    config,
                    session_spec_for_config(config),
                    metrics,
                    width,
                    height,
                );
                Ok(())
            }
            MuxAction::NewWorkspace => {
                let number = self.model.workspaces.len() + 1;
                let workspace_id = self.model.new_workspace(
                    format!("workspace {number}"),
                    session_spec_for_config(config),
                );
                let pane_id = self.model.active_tab().active_pane;
                self.insert_runtime_for_pane(pane_id, config, metrics);
                self.resize_all(width, height, metrics, config);
                debug_assert_eq!(self.model.active_workspace, workspace_id);
                Ok(())
            }
            MuxAction::CloseWorkspace => self.close_active_workspace(metrics, config),
            MuxAction::NextWorkspace => self.switch_relative_workspace(1, metrics, config),
            MuxAction::PreviousWorkspace => self.switch_relative_workspace(-1, metrics, config),
            MuxAction::CloseTab => self.close_active_tab(metrics, config),
            MuxAction::NextTab => self.switch_relative_tab(1, metrics, config),
            MuxAction::PreviousTab => self.switch_relative_tab(-1, metrics, config),
            MuxAction::RenameTab { name } => {
                let tab_id = self.model.active_tab().id;
                let name = if name.trim().is_empty() {
                    format!("tab {}", tab_id.0)
                } else {
                    name
                };
                self.model.rename_tab(tab_id, name)
            }
            MuxAction::MoveTab { target_index } => self
                .model
                .move_tab(self.model.active_tab().id, target_index),
            MuxAction::SplitHorizontal => {
                self.split_active(SplitAxis::Horizontal, config, metrics, width, height);
                Ok(())
            }
            MuxAction::SplitVertical => {
                self.split_active(SplitAxis::Vertical, config, metrics, width, height);
                Ok(())
            }
            MuxAction::ClosePane => self.close_active_pane(metrics, config),
            MuxAction::FocusDirection(direction) => {
                self.model.focus_direction(direction).map(|_| {
                    self.sync_active_tab_title();
                    self.resize_active_tab(metrics, config);
                })
            }
            MuxAction::ResizePane(direction) => self
                .model
                .resize_active_pane(direction, config.mux.pane_resize_step as f32)
                .map(|_| self.resize_active_tab(metrics, config)),
            MuxAction::ZoomPane => {
                self.model.toggle_zoom_active_pane();
                self.resize_active_tab(metrics, config);
                Ok(())
            }
            MuxAction::MovePane(direction) | MuxAction::SwapPaneDirection(direction) => self
                .model
                .move_active_pane(direction)
                .map(|_| self.resize_active_tab(metrics, config)),
            MuxAction::SwapPane { other } => self
                .model
                .swap_panes(self.model.active_tab().active_pane, other)
                .map(|_| self.resize_active_tab(metrics, config)),
        };

        if let Err(error) = result {
            eprintln!("mux action failed: {error}");
            return false;
        }
        self.schedule_state_save();
        true
    }

    fn new_tab_with_spec(
        &mut self,
        config: &AppConfig,
        spec: SessionSpec,
        metrics: CellMetrics,
        width: u32,
        height: u32,
    ) {
        let tab_number = self.model.active_workspace().active_window().tabs.len() + 1;
        match self.model.new_tab(tab_number.to_string(), spec) {
            Ok(_) => {
                let pane_id = self.model.active_tab().active_pane;
                self.insert_runtime_for_pane(pane_id, config, metrics);
                self.resize_all(width, height, metrics, config);
            }
            Err(error) => eprintln!("mux new tab failed: {error}"),
        }
    }

    fn split_active(
        &mut self,
        axis: SplitAxis,
        config: &AppConfig,
        metrics: CellMetrics,
        width: u32,
        height: u32,
    ) {
        self.split_active_with_spec(
            axis,
            session_spec_for_config(config),
            config,
            metrics,
            width,
            height,
        );
    }

    fn split_active_with_spec(
        &mut self,
        axis: SplitAxis,
        spec: SessionSpec,
        config: &AppConfig,
        metrics: CellMetrics,
        width: u32,
        height: u32,
    ) {
        match self.model.split_active_pane(axis, spec) {
            Ok(pane_id) => {
                self.insert_runtime_for_pane(pane_id, config, metrics);
                self.resize_all(width, height, metrics, config);
            }
            Err(error) => eprintln!("mux split failed: {error}"),
        }
    }

    fn insert_runtime_for_pane(
        &mut self,
        pane_id: PaneId,
        config: &AppConfig,
        metrics: CellMetrics,
    ) {
        let size = self
            .active_layouts(config)
            .into_iter()
            .find(|layout| layout.pane_id == pane_id)
            .map(|layout| layout.terminal_size)
            .unwrap_or_else(|| TerminalGridSize::new(self.surface_cols, self.surface_rows));
        let spec = match self.model.session_for_pane(pane_id) {
            Ok(session) => session.spec.clone(),
            Err(error) => {
                eprintln!("mux pane runtime could not find session: {error}");
                return;
            }
        };
        let pane = PaneRuntime::new(config, &spec, size, metrics, self.output_waker.clone());
        mark_session_status(&mut self.model, pane_id, session_status_for_pane(&pane));
        self.panes.insert(pane_id, pane);
    }

    fn close_active_tab(&mut self, metrics: CellMetrics, config: &AppConfig) -> mux::MuxResult<()> {
        let tab_id = self.model.active_tab().id;
        let pane_ids = self
            .model
            .active_tab()
            .panes
            .keys()
            .copied()
            .collect::<Vec<_>>();
        self.model.close_tab(tab_id)?;
        for pane_id in pane_ids {
            if let Some(mut pane) = self.panes.remove(&pane_id) {
                pane.shutdown();
            }
        }
        self.resize_active_tab(metrics, config);
        Ok(())
    }

    fn switch_relative_tab(
        &mut self,
        delta: isize,
        metrics: CellMetrics,
        config: &AppConfig,
    ) -> mux::MuxResult<()> {
        let window = self.model.active_workspace().active_window();
        let active_index = window
            .tabs
            .iter()
            .position(|tab| tab.id == window.active_tab)
            .ok_or(mux::MuxError::TabNotFound(window.active_tab))?;
        let next_index = (active_index as isize + delta).rem_euclid(window.tabs.len() as isize);
        let next_tab = window.tabs[next_index as usize].id;
        self.model.switch_tab(next_tab)?;
        self.resize_active_tab(metrics, config);
        Ok(())
    }

    fn switch_relative_workspace(
        &mut self,
        delta: isize,
        metrics: CellMetrics,
        config: &AppConfig,
    ) -> mux::MuxResult<()> {
        let workspace_ids = self.model.workspaces.keys().copied().collect::<Vec<_>>();
        let active_index = workspace_ids
            .iter()
            .position(|id| *id == self.model.active_workspace)
            .ok_or(mux::MuxError::WorkspaceNotFound(
                self.model.active_workspace,
            ))?;
        let next_index =
            (active_index as isize + delta).rem_euclid(workspace_ids.len() as isize) as usize;
        self.model.switch_workspace(workspace_ids[next_index])?;
        self.resize_active_tab(metrics, config);
        Ok(())
    }

    fn close_active_workspace(
        &mut self,
        metrics: CellMetrics,
        config: &AppConfig,
    ) -> mux::MuxResult<()> {
        let workspace_id = self.model.active_workspace;
        let pane_ids = self
            .model
            .active_workspace()
            .windows
            .iter()
            .flat_map(|window| &window.tabs)
            .flat_map(|tab| tab.panes.keys().copied())
            .collect::<Vec<_>>();
        self.model.close_workspace(workspace_id)?;
        for pane_id in pane_ids {
            if let Some(mut pane) = self.panes.remove(&pane_id) {
                pane.shutdown();
            }
        }
        self.resize_active_tab(metrics, config);
        Ok(())
    }

    fn handle_profile_mux_action(
        &mut self,
        action: &str,
        config: &AppConfig,
        metrics: CellMetrics,
        width: u32,
        height: u32,
    ) -> bool {
        let Some((command, profile)) = action.split_once(':') else {
            return false;
        };
        if profile.trim().is_empty() {
            eprintln!("mux profile action requires a non-empty profile name");
            return true;
        }
        match command {
            "new_workspace" => {
                let pane_id = {
                    self.model
                        .new_workspace(profile, session_spec_for_config(config));
                    self.model.active_tab().active_pane
                };
                self.insert_runtime_for_pane(pane_id, config, metrics);
                self.resize_all(width, height, metrics, config);
                self.schedule_state_save();
                return true;
            }
            "rename_workspace" => {
                if self
                    .model
                    .rename_workspace(self.model.active_workspace, profile)
                    .is_ok()
                {
                    self.schedule_state_save();
                }
                return true;
            }
            "switch_workspace" => {
                if let Some(workspace_id) = self
                    .model
                    .workspaces
                    .values()
                    .find(|workspace| workspace.name == profile)
                    .map(|workspace| workspace.id)
                {
                    if self.model.switch_workspace(workspace_id).is_ok() {
                        self.resize_active_tab(metrics, config);
                        self.schedule_state_save();
                    }
                } else {
                    eprintln!("mux workspace '{profile}' does not exist");
                }
                return true;
            }
            "rename_tab" => {
                if self
                    .model
                    .rename_tab(self.model.active_tab().id, profile)
                    .is_ok()
                {
                    self.schedule_state_save();
                }
                return true;
            }
            _ => {}
        }
        let spec = match command {
            "new_local_tab" | "split_local_horizontal" | "split_local_vertical" => {
                local_session_spec(profile)
            }
            "new_ssh_tab" | "split_ssh_horizontal" | "split_ssh_vertical" => {
                SessionSpec::ssh(profile)
            }
            _ => return false,
        };
        match command {
            "new_local_tab" | "new_ssh_tab" => {
                self.new_tab_with_spec(config, spec, metrics, width, height)
            }
            "split_local_horizontal" | "split_ssh_horizontal" => self.split_active_with_spec(
                SplitAxis::Horizontal,
                spec,
                config,
                metrics,
                width,
                height,
            ),
            "split_local_vertical" | "split_ssh_vertical" => self.split_active_with_spec(
                SplitAxis::Vertical,
                spec,
                config,
                metrics,
                width,
                height,
            ),
            _ => unreachable!("profile mux command was validated above"),
        }
        self.schedule_state_save();
        true
    }

    fn close_active_pane(
        &mut self,
        metrics: CellMetrics,
        config: &AppConfig,
    ) -> mux::MuxResult<()> {
        let pane_id = self.model.active_tab().active_pane;
        self.model.close_pane(pane_id)?;
        if let Some(mut pane) = self.panes.remove(&pane_id) {
            pane.shutdown();
        }
        self.resize_active_tab(metrics, config);
        Ok(())
    }

    fn write_active(&mut self, bytes: &[u8]) {
        if let Some(pane) = self.active_pane_mut() {
            pane.write_input(bytes);
        }
    }

    fn input_bytes(&self, event: &KeyEvent) -> Option<Vec<u8>> {
        encode_key_for_terminal(&self.active_pane()?.terminal, event)
    }

    fn start_search(&mut self) {
        if let Some(pane) = self.active_pane_mut() {
            pane.search.start();
        }
    }

    fn append_search_text(&mut self, text: &str) -> bool {
        self.active_pane_mut()
            .is_some_and(|pane| pane.append_search_text(text))
    }

    fn append_modal_text(&mut self, text: &str) -> bool {
        self.active_pane_mut()
            .is_some_and(|pane| pane.append_modal_text(text))
    }

    fn update_active_ime_preedit(&mut self, text: String, cursor: Option<(usize, usize)>) -> bool {
        self.active_pane_mut()
            .is_some_and(|pane| pane.update_ime_preedit(text, cursor))
    }

    fn reconnect_active(&mut self, config: &AppConfig, metrics: CellMetrics) -> bool {
        let pane_id = self.model.active_tab().active_pane;
        let reconnected = self
            .panes
            .get_mut(&pane_id)
            .is_some_and(|pane| pane.reconnect(config, metrics));
        if let Some(pane) = self.panes.get(&pane_id) {
            mark_session_status(&mut self.model, pane_id, session_status_for_pane(pane));
        }
        reconnected
    }

    fn start_keyboard_selection(&mut self, kind: SelectionKind) {
        if let Some(pane) = self.active_pane_mut() {
            pane.start_keyboard_selection(kind);
        }
    }

    fn run_semantic_action(&mut self, action: SemanticAction) -> SemanticActionResult {
        self.active_pane_mut()
            .map_or(SemanticActionResult::Noop, |pane| {
                pane.run_semantic_action(action)
            })
    }

    fn toggle_current_command_output(&mut self, config: &AppConfig) -> bool {
        self.active_pane_mut()
            .is_some_and(|pane| pane.toggle_current_command_output(config))
    }

    fn handle_modal_key(
        &mut self,
        event: &KeyEvent,
        clipboard: &mut ClipboardBridge,
        policy: &Osc52ClipboardPolicy,
        clipboard_config: &ClipboardConfig,
    ) -> Option<bool> {
        self.active_pane_mut()?
            .handle_modal_key(event, clipboard, policy, clipboard_config)
    }

    fn scroll_active_page(&mut self, toward_older: bool) -> bool {
        let Some(pane) = self.active_pane_mut() else {
            return false;
        };
        let rows = i64::from(pane.terminal.state().viewport().size.rows).max(1);
        pane.terminal
            .state_mut()
            .scroll_viewport(if toward_older { rows } else { -rows })
    }

    fn scroll_active_to_top(&mut self) -> bool {
        let Some(pane) = self.active_pane_mut() else {
            return false;
        };
        let lines = i64::try_from(pane.terminal.scrollback_line_count()).unwrap_or(i64::MAX);
        pane.terminal.state_mut().scroll_viewport(lines)
    }

    fn scroll_active_to_bottom(&mut self) -> bool {
        self.active_pane_mut()
            .is_some_and(|pane| pane.terminal.state_mut().scroll_to_bottom())
    }

    fn paste_into_active(
        &mut self,
        text: &str,
        clipboard: &ClipboardConfig,
        paste_config: &PasteConfig,
    ) {
        if let Some(pane) = self.active_pane_mut() {
            let bytes = paste_bytes(
                text,
                clipboard,
                paste_config,
                pane.terminal
                    .modes_ref()
                    .contains(&TerminalMode::BracketedPaste),
            );
            pane.write_input(&bytes);
        }
    }

    fn send_focus_event(&mut self, focused: bool) {
        if let Some(pane) = self.active_pane_mut()
            && let Some(bytes) = focus_report_bytes(focused, pane.terminal.modes_ref())
        {
            pane.write_input(bytes);
        }
    }

    fn handle_mouse(
        &mut self,
        mouse: MouseEvent,
        metrics: CellMetrics,
        config: &AppConfig,
        clipboard_config: &ClipboardConfig,
        paste_config: &PasteConfig,
        clipboard: &mut ClipboardBridge,
    ) -> MouseHandling {
        if let Some(outcome) = self.update_mux_drag(mouse, metrics, config) {
            return outcome;
        }
        if let Some(tab_id) = self.tab_at_mouse(mouse, metrics, config) {
            if matches!(mouse.kind, MouseEventKind::Pressed(MouseButton::Left)) {
                if self.model.switch_tab(tab_id).is_ok() {
                    if config.mux.drag_tabs {
                        self.drag = Some(MuxDragState::Tab {
                            source: tab_id,
                            target: tab_id,
                        });
                    }
                    self.resize_active_tab(metrics, config);
                    self.schedule_state_save();
                    return MouseHandling {
                        changed: true,
                        open_url: None,
                    };
                }
            } else if matches!(mouse.kind, MouseEventKind::Pressed(MouseButton::Middle))
                && self.model.active_workspace().active_window().tabs.len() > 1
            {
                let pane_ids = self
                    .model
                    .active_workspace()
                    .active_window()
                    .tabs
                    .iter()
                    .find(|tab| tab.id == tab_id)
                    .map(|tab| tab.panes.keys().copied().collect::<Vec<_>>())
                    .unwrap_or_default();
                if self.model.close_tab(tab_id).is_ok() {
                    for pane_id in pane_ids {
                        if let Some(mut pane) = self.panes.remove(&pane_id) {
                            pane.shutdown();
                        }
                    }
                    self.resize_active_tab(metrics, config);
                    self.schedule_state_save();
                    return MouseHandling {
                        changed: true,
                        open_url: None,
                    };
                }
            }
            return MouseHandling::default();
        }
        let Some((pane_id, local_mouse)) = self.local_mouse_event(mouse, metrics, config) else {
            return MouseHandling::default();
        };
        if config.mux.drag_panes
            && local_mouse.modifiers.ctrl
            && local_mouse.modifiers.shift
            && matches!(local_mouse.kind, MouseEventKind::Pressed(MouseButton::Left))
        {
            let _ = self.model.focus_pane(pane_id);
            self.drag = Some(MuxDragState::Pane {
                source: pane_id,
                target: pane_id,
            });
            return MouseHandling {
                changed: true,
                open_url: None,
            };
        }
        if matches!(mouse.kind, MouseEventKind::Pressed(_)) {
            let _ = self.model.focus_pane(pane_id);
            self.sync_active_tab_title();
        }
        let Some(pane) = self.panes.get_mut(&pane_id) else {
            return MouseHandling::default();
        };
        let binding_action = mousebinding_action(&local_mouse, &config.mouse);
        if binding_action.as_deref() == Some("open_url") {
            return MouseHandling {
                changed: false,
                open_url: matches!(local_mouse.kind, MouseEventKind::Released(_))
                    .then(|| pane.url_at_mouse(local_mouse, metrics))
                    .flatten(),
            };
        }
        let modes = pane.terminal.modes_ref();
        if !local_mouse.modifiers.shift
            && let Some(bytes) = pane
                .mouse_protocol
                .report_bytes(local_mouse, metrics, modes)
        {
            pane.write_input(&bytes);
            return MouseHandling::default();
        }

        match binding_action.as_deref() {
            Some("ignore") => return MouseHandling::default(),
            Some("paste") if matches!(local_mouse.kind, MouseEventKind::Pressed(_)) => {
                if let Ok(text) = clipboard.paste_text() {
                    let bytes = paste_bytes(&text, clipboard_config, paste_config, false);
                    pane.write_input(&bytes);
                }
                return MouseHandling::default();
            }
            Some("paste_primary") if matches!(local_mouse.kind, MouseEventKind::Pressed(_)) => {
                if let Ok(text) = paste_for_middle_click(clipboard, clipboard_config) {
                    let bytes = paste_bytes(&text, clipboard_config, paste_config, false);
                    pane.write_input(&bytes);
                }
                return MouseHandling::default();
            }
            Some("copy") if matches!(local_mouse.kind, MouseEventKind::Released(_)) => {
                if let Some(text) = pane.terminal.state().selected_text() {
                    copy_text_with_diagnostics(clipboard, &text, clipboard_config, "mouse copy");
                }
                return MouseHandling::default();
            }
            _ => {}
        }

        let mut local_mouse = local_mouse;
        if binding_action.as_deref() == Some("select_rectangular") {
            local_mouse.modifiers.alt = true;
        } else if binding_action.as_deref() == Some("select") {
            local_mouse.modifiers.alt = false;
        } else if should_middle_click_paste(&local_mouse, modes, clipboard_config)
            && let Ok(text) = paste_for_middle_click(clipboard, clipboard_config)
        {
            let bytes = paste_bytes(&text, clipboard_config, paste_config, false);
            pane.write_input(&bytes);
            return MouseHandling::default();
        }

        let selection_completed = matches!(
            local_mouse.kind,
            MouseEventKind::Released(MouseButton::Left)
        );
        let changed = pane.handle_selection_or_scrollback(local_mouse, metrics);
        if changed
            && selection_completed
            && let Some(text) = pane.terminal.state().selected_text()
        {
            if cfg!(target_os = "linux")
                && clipboard_config.prefer_primary_selection_on_linux
                && let Err(diagnostic) = clipboard.copy_primary_text(&text)
            {
                eprintln!("Linux primary selection copy failed: {diagnostic:?}");
            }
            if clipboard_config.enabled
                && (clipboard_config.copy_on_select || config.mouse.copy_on_select)
            {
                copy_text_with_diagnostics(clipboard, &text, clipboard_config, "copy-on-select");
            }
        }
        MouseHandling {
            changed,
            open_url: None,
        }
    }

    fn update_mux_drag(
        &mut self,
        mouse: MouseEvent,
        metrics: CellMetrics,
        config: &AppConfig,
    ) -> Option<MouseHandling> {
        let drag = self.drag?;
        match mouse.kind {
            MouseEventKind::Moved => {
                let next = match drag {
                    MuxDragState::Tab { source, target: _ } => self
                        .tab_at_mouse(mouse, metrics, config)
                        .map_or(drag, |target| MuxDragState::Tab { source, target }),
                    MuxDragState::Pane { source, target: _ } => self
                        .local_mouse_event(mouse, metrics, config)
                        .map_or(drag, |(target, _)| MuxDragState::Pane { source, target }),
                };
                let changed = next != drag;
                self.drag = Some(next);
                Some(MouseHandling {
                    changed,
                    open_url: None,
                })
            }
            MouseEventKind::Released(MouseButton::Left) => {
                self.drag = None;
                let changed = match drag {
                    MuxDragState::Tab { source, target } if source != target => {
                        let target_index = self
                            .model
                            .active_workspace()
                            .active_window()
                            .tabs
                            .iter()
                            .position(|tab| tab.id == target);
                        target_index.is_some_and(|target_index| {
                            self.model.move_tab(source, target_index).is_ok()
                        })
                    }
                    MuxDragState::Pane { source, target } if source != target => self
                        .model
                        .swap_panes(source, target)
                        .map(|_| self.resize_active_tab(metrics, config))
                        .is_ok(),
                    MuxDragState::Tab { .. } | MuxDragState::Pane { .. } => true,
                };
                if changed {
                    self.schedule_state_save();
                }
                Some(MouseHandling {
                    changed,
                    open_url: None,
                })
            }
            MouseEventKind::Released(_) => {
                self.drag = None;
                Some(MouseHandling {
                    changed: true,
                    open_url: None,
                })
            }
            MouseEventKind::Pressed(_) | MouseEventKind::Wheel(_) => Some(MouseHandling::default()),
        }
    }

    fn local_mouse_event(
        &self,
        mouse: MouseEvent,
        metrics: CellMetrics,
        config: &AppConfig,
    ) -> Option<(PaneId, MouseEvent)> {
        let inset_x = f64::from(horizontal_content_inset(config));
        let inset_y = f64::from(vertical_content_inset(config));
        if mouse.x < inset_x || mouse.y < inset_y {
            return None;
        }
        let content_x = mouse.x - inset_x;
        let content_y = mouse.y - inset_y;
        let x_cells = (content_x as f32 / metrics.cell_width).floor();
        let y_cells = (content_y as f32 / metrics.cell_height).floor();
        self.active_layouts(config)
            .into_iter()
            .find(|layout| {
                x_cells >= layout.rect.x
                    && x_cells < layout.rect.x + layout.rect.width
                    && y_cells >= layout.rect.y
                    && y_cells < layout.rect.y + layout.rect.height
            })
            .map(|layout| {
                let local = MouseEvent {
                    x: (content_x as f32 - layout.rect.x * metrics.cell_width).max(0.0) as f64,
                    y: (content_y as f32 - layout.rect.y * metrics.cell_height).max(0.0) as f64,
                    ..mouse
                };
                (layout.pane_id, local)
            })
    }

    fn tab_at_mouse(
        &self,
        mouse: MouseEvent,
        metrics: CellMetrics,
        config: &AppConfig,
    ) -> Option<TabId> {
        if tab_bar_rows(&self.model, config) == 0 {
            return None;
        }
        let inset_x = f64::from(horizontal_content_inset(config));
        let inset_y = f64::from(vertical_content_inset(config));
        if mouse.x < inset_x
            || mouse.y < inset_y
            || mouse.y >= inset_y + f64::from(metrics.cell_height)
        {
            return None;
        }
        let mouse_col = ((mouse.x - inset_x) / f64::from(metrics.cell_width)).floor() as usize;
        let workspace = self.model.active_workspace();
        let window = workspace.active_window();
        let mut start = 0usize;
        for (index, tab) in window.tabs.iter().enumerate() {
            let width = formatted_tab_width(config, &workspace.name, index, tab);
            let end = start.saturating_add(width);
            if (start..end).contains(&mouse_col) {
                return Some(tab.id);
            }
            start = end;
        }
        None
    }

    fn poll_outputs(
        &mut self,
        clipboard: &mut ClipboardBridge,
        notification_provider: &mut dyn NotificationProvider,
        context: MuxPollContext<'_>,
    ) -> MuxPollOutcome {
        let mut content_changed = false;
        let mut clean_exits = Vec::new();
        let mut status_updates = Vec::new();
        let mut metadata_updates = Vec::new();
        for (pane_id, pane) in &mut self.panes {
            let poll = pane.poll_output(clipboard, context.osc52_policy, context.clipboard_config);
            self.performance.record_pty_bytes(poll.pty_bytes);
            self.performance.record_parser_bytes(poll.parser_bytes);
            if poll.content_changed {
                content_changed = true;
            }
            if poll.clean_exit {
                clean_exits.push(*pane_id);
            }
            notify_for_pane_transition(
                notification_provider,
                context.notification_config,
                context.window_focused,
                pane,
                poll,
            );
            if pane_poll_needs_metadata_refresh(poll) {
                status_updates.push((*pane_id, session_status_for_pane(pane)));
                metadata_updates.push((
                    *pane_id,
                    pane.terminal.state().title().map(ToOwned::to_owned),
                    pane.semantic_timeline
                        .metadata()
                        .remote
                        .as_ref()
                        .and_then(|remote| remote.remote_current_working_directory.clone())
                        .or_else(|| {
                            pane.semantic_timeline
                                .metadata()
                                .shell
                                .current_working_directory
                                .clone()
                        }),
                ));
            }
        }
        for (pane_id, status) in status_updates {
            mark_session_status(&mut self.model, pane_id, status);
        }
        for (pane_id, title, directory) in metadata_updates {
            if let Some(title) = title {
                let _ = self.model.update_pane_title(pane_id, title);
            }
            if let Ok(session) = self.model.session_for_pane_mut(pane_id) {
                session.current_working_directory = directory;
            }
        }
        let exit_application =
            self.close_cleanly_exited_panes(&clean_exits, context.metrics, context.config);
        MuxPollOutcome {
            content_changed: content_changed || !clean_exits.is_empty(),
            exit_application,
        }
    }

    fn close_cleanly_exited_panes(
        &mut self,
        pane_ids: &[PaneId],
        metrics: CellMetrics,
        config: &AppConfig,
    ) -> bool {
        let mut layout_changed = false;
        for pane_id in pane_ids.iter().copied() {
            match self.model.close_exited_pane(pane_id) {
                Ok(PaneExitDisposition::ExitApplication) => {
                    if let Some(pane) = self.panes.get_mut(&pane_id) {
                        pane.transport.take();
                    }
                    return true;
                }
                Ok(
                    PaneExitDisposition::PaneClosed
                    | PaneExitDisposition::TabClosed
                    | PaneExitDisposition::WindowClosed
                    | PaneExitDisposition::WorkspaceClosed,
                ) => {
                    self.panes.remove(&pane_id);
                    if self.drag.is_some_and(|drag| match drag {
                        MuxDragState::Pane { source, target } => {
                            source == pane_id || target == pane_id
                        }
                        MuxDragState::Tab { .. } => false,
                    }) {
                        self.drag = None;
                    }
                    layout_changed = true;
                }
                Err(error) => eprintln!("mux clean-exit handling failed: {error}"),
            }
        }
        if layout_changed {
            self.resize_active_tab(metrics, config);
            self.schedule_state_save();
        }
        false
    }

    fn requires_periodic_transport_poll(&self) -> bool {
        self.panes.values().any(|pane| {
            // A pane waiting on an automatic reconnect needs the loop to keep
            // ticking, or its retry timer would never come due on an idle window.
            pane.reconnect_at.is_some()
                || pane
                    .transport
                    .as_ref()
                    .is_some_and(PaneTransport::requires_periodic_poll)
        })
    }

    /// Runs any automatic reconnect whose backoff has elapsed.
    fn drive_automatic_reconnects(&mut self, config: &AppConfig, metrics: CellMetrics) -> bool {
        let due = self
            .panes
            .iter()
            .filter(|(_, pane)| pane.automatic_reconnect_is_due(Instant::now()))
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        let mut reconnected = false;
        for id in due {
            if let Some(pane) = self.panes.get_mut(&id) {
                reconnected |= pane.run_automatic_reconnect(config, metrics);
            }
        }
        reconnected
    }

    fn active_visible_text(&self) -> String {
        self.active_pane()
            .map(|pane| {
                let visible = pane.terminal.visible_grid();
                visible
                    .cells
                    .chunks(usize::from(visible.viewport.size.cols.max(1)))
                    .map(|cells| {
                        let mut line = term_core::Line::default();
                        line.cells = cells.to_vec();
                        line.raw_text()
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    }

    fn populate_performance_sample(&mut self, sample: &mut RenderInstrumentation) {
        let throughput = self.performance.sample_throughput();
        sample.pty_read_bytes_per_second = throughput.pty_read_bytes_per_second;
        sample.parser_bytes_per_second = throughput.parser_bytes_per_second;
        sample.scrollback_memory_bytes = self.scrollback_memory_bytes();
        sample.memory_usage_bytes = Some(
            sample
                .scrollback_memory_bytes
                .saturating_add(sample.glyphs.atlas_capacity_bytes),
        );
    }

    fn scrollback_memory_bytes(&self) -> u64 {
        self.panes
            .values()
            .map(PaneRuntime::scrollback_memory_bytes)
            .sum()
    }

    fn next_synchronized_output_deadline(&self) -> Option<Instant> {
        self.panes
            .values()
            .filter_map(PaneRuntime::synchronized_output_deadline)
            .min()
    }

    fn active_selected_text(&self) -> Option<String> {
        self.active_pane()
            .and_then(|pane| pane.terminal.state().selected_text())
    }

    fn active_cursor_blinks(&self) -> bool {
        self.active_pane()
            .is_some_and(|pane| pane.terminal.cursor_state().blinking)
    }

    fn active_pane(&self) -> Option<&PaneRuntime> {
        self.panes.get(&self.model.active_tab().active_pane)
    }

    fn active_pane_mut(&mut self) -> Option<&mut PaneRuntime> {
        let pane_id = self.model.active_tab().active_pane;
        self.panes.get_mut(&pane_id)
    }

    fn sync_active_tab_title(&mut self) {
        let pane_id = self.model.active_tab().active_pane;
        if let Some(title) = self
            .panes
            .get(&pane_id)
            .and_then(|pane| pane.terminal.state().title())
            .map(ToOwned::to_owned)
        {
            let _ = self.model.update_pane_title(pane_id, title);
        }
    }

    fn active_layouts(&self, config: &AppConfig) -> Vec<PaneLayout> {
        let tab_bar_rows = tab_bar_rows(&self.model, config);
        self.model.active_tab().layout(LogicalRect::new(
            0.0,
            f32::from(tab_bar_rows),
            f32::from(self.surface_cols),
            f32::from(self.surface_rows.saturating_sub(tab_bar_rows).max(1)),
        ))
    }

    fn shutdown_all(&mut self) {
        self.enqueue_state_flush();
        for pane in self.panes.values_mut() {
            pane.shutdown();
        }
    }

    fn schedule_state_save(&mut self) {
        if !self.restore_sessions {
            return;
        }
        self.sync_session_metadata();
        schedule_mux_state_save(self.state_path.clone(), self.model.restore_snapshot());
    }

    fn enqueue_state_flush(&mut self) {
        if !self.restore_sessions {
            return;
        }
        self.sync_session_metadata();
        enqueue_mux_state_flush(self.state_path.clone(), self.model.restore_snapshot());
    }

    fn sync_session_metadata(&mut self) {
        for (pane_id, pane) in &self.panes {
            if let Ok(session) = self.model.session_for_pane_mut(*pane_id) {
                let metadata = pane.semantic_timeline.metadata();
                session.current_working_directory = metadata
                    .remote
                    .as_ref()
                    .and_then(|remote| remote.remote_current_working_directory.clone())
                    .or_else(|| metadata.shell.current_working_directory.clone());
            }
        }
    }
}

fn shell_prompt_visible(text: &str) -> bool {
    shell_prompt_line_count(text) > 0
}

fn gui_smoke_input_settled(observed_at: &mut Option<Instant>, now: Instant) -> bool {
    let observed_at = *observed_at.get_or_insert(now);
    now.saturating_duration_since(observed_at) >= GUI_INPUT_SETTLE_DELAY
}

fn record_gui_smoke_input_observed(
    mode: Option<GuiSmokeMode>,
    input_sent: bool,
    runtime: &MuxRuntime,
    report: Option<&Arc<Mutex<GuiSmokeReport>>>,
    started: Option<Instant>,
) {
    if !input_sent
        || !matches!(
            mode,
            Some(GuiSmokeMode::InputEcho | GuiSmokeMode::TerminalIo)
        )
    {
        return;
    }

    let visible = runtime.active_visible_text();
    let observed = match mode {
        Some(GuiSmokeMode::InputEcho) => visible.contains(GUI_INPUT_ECHO_MARKER),
        Some(GuiSmokeMode::TerminalIo) => visible.matches(GUI_SMOKE_MARKER).count() >= 2,
        _ => false,
    };
    if observed
        && let Some(report) = report
        && let Ok(mut report) = report.lock()
        && report.input_observed.is_none()
    {
        report.input_observed = started.map(|started| started.elapsed());
    }
}

fn shell_prompt_line_count(text: &str) -> usize {
    text.lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .filter(|line| {
            let line = line.trim_end();
            (line.starts_with("PS ") && line.ends_with('>'))
                || line.ends_with('$')
                || line.ends_with('#')
                || line.ends_with('%')
                || line.trim_start().starts_with('\u{276f}')
                || (cfg!(windows) && line.ends_with('>'))
        })
        .count()
}

fn session_status_for_pane(pane: &PaneRuntime) -> SessionStatus {
    match &pane.connection_state {
        PaneConnectionState::Connecting => SessionStatus::Pending,
        PaneConnectionState::Connected => SessionStatus::Running,
        PaneConnectionState::Disconnected(message) if pane.remote_session => {
            SessionStatus::Failed {
                message: message.clone(),
            }
        }
        PaneConnectionState::Disconnected(_) => SessionStatus::Exited {
            exit_code: pane.exit_code,
        },
    }
}

fn notify_for_pane_transition(
    provider: &mut dyn NotificationProvider,
    config: &NotificationConfig,
    window_focused: bool,
    pane: &PaneRuntime,
    poll: PanePollStats,
) {
    if !config.enabled || (config.only_when_unfocused && window_focused) {
        return;
    }
    let (title, body, urgency) = if poll.error && config.transport_errors {
        (
            if pane.remote_session {
                "Panea SSH transport error"
            } else {
                "Panea terminal transport error"
            },
            format!(
                "The {} session for profile '{}' stopped with an error. Open Panea for details.",
                if pane.remote_session { "SSH" } else { "local" },
                pane.session_spec.profile_name
            ),
            NotificationUrgency::Critical,
        )
    } else if poll.closed && config.session_closed {
        (
            if pane.remote_session {
                "Panea SSH session disconnected"
            } else {
                "Panea terminal session exited"
            },
            format!(
                "The {} session for profile '{}' has closed.",
                if pane.remote_session { "SSH" } else { "local" },
                pane.session_spec.profile_name
            ),
            NotificationUrgency::Normal,
        )
    } else {
        return;
    };
    if let Err(diagnostic) =
        provider.notify(NotificationRequest::new(title, body).with_urgency(urgency))
    {
        eprintln!("notification fallback: {}", diagnostic.message);
    }
}
