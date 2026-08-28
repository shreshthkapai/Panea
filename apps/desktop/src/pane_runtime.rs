// Per-pane terminal state, transport ownership, semantic state, and polling.

struct PaneTextProvider<'a> {
    terminal: &'a TerminalEmulator,
}

impl TerminalTextProvider for PaneTextProvider<'_> {
    fn text_for_span(&self, span: SemanticSpan) -> Option<String> {
        Some(self.terminal.state().text_for_selection(Selection {
            start: GridPosition::new(span.start.row, span.start.col),
            end: GridPosition::new(span.end.row, span.end.col),
            kind: SelectionKind::Normal,
        }))
    }
}

struct PaneRuntime {
    terminal: TerminalEmulator,
    semantic_parser: SemanticEscapeParser,
    semantic_timeline: SemanticTimelineStore,
    heuristic_detector: Option<HeuristicCommandDetector>,
    parse_semantic_events: bool,
    remote_session: bool,
    session_spec: SessionSpec,
    last_size: TerminalGridSize,
    connection_state: PaneConnectionState,
    exit_code: Option<i32>,
    disconnect_notified: bool,
    ssh_prompt: Option<SshPromptState>,
    osc52_prompt: Option<Osc52PromptState>,
    ime_preedit: String,
    ime_preedit_cursor: Option<(usize, usize)>,
    ime_preedit_cells: usize,
    transport: Option<PaneTransport>,
    mouse_protocol: MouseProtocolState,
    wheel_remainder: f64,
    selection_anchor: Option<GridPosition>,
    selection_kind: SelectionKind,
    keyboard_selection: Option<KeyboardSelection>,
    search: PaneSearch,
    /// Per-command presentation override. `true` is collapsed, `false` keeps
    /// an otherwise auto-collapsed block expanded. Raw terminal data is untouched.
    command_output_collapsed: HashMap<u64, bool>,
    command_overlay_revision: u64,
    output_waker: TransportWakeHandle,
    synchronized_output_since: Option<Instant>,
    /// Automatic reconnection for a dropped remote session.
    /// `TerminalState::scrollback_dropped` as of the last semantic rebase.
    semantic_rows_dropped: u64,
    /// Region ids recorded since entering the alternate screen, if active. Those
    /// rows are screen-relative to the alternate buffer, so they must not be
    /// rebased against primary evictions and are dropped on return.
    alternate_screen_semantics: Option<u64>,
    reconnect_policy: SshReconnectPolicy,
    /// Retries already made since this session was last established.
    reconnect_attempts: u32,
    /// When the next automatic retry is due.
    reconnect_at: Option<Instant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PaneConnectionState {
    Connecting,
    Connected,
    Disconnected(String),
}

enum SshPromptState {
    HostTrust {
        request: HostKeyTrustRequest,
        response: Option<SyncSender<HostKeyTrustAction>>,
    },
    Secret {
        request: SecretRequest,
        keychain: KeychainProviderCapability,
        response: Option<SyncSender<Option<SecretPromptResponse>>>,
        input: String,
        save_to_keychain: bool,
    },
}

#[derive(Debug, Clone)]
struct Osc52PromptState {
    request: SecurityOsc52Request,
    reason: String,
    bytes: usize,
}

impl SshPromptState {
    fn from_request(request: SshInteractionRequest) -> Self {
        match request {
            SshInteractionRequest::HostTrust { request, response } => Self::HostTrust {
                request,
                response: Some(response),
            },
            SshInteractionRequest::Secret {
                request,
                keychain,
                response,
            } => Self::Secret {
                request,
                keychain,
                response: Some(response),
                input: String::new(),
                save_to_keychain: false,
            },
        }
    }

    fn handle_key(&mut self, event: &KeyEvent) -> bool {
        match self {
            Self::HostTrust { request, response } => {
                let action = match event.logical_key.to_ascii_lowercase().as_str() {
                    "escape" => Some(HostKeyTrustAction::Reject),
                    "o" if request.reason == HostKeyTrustReason::UnknownHost => {
                        Some(HostKeyTrustAction::TrustOnce)
                    }
                    "s" if request.reason == HostKeyTrustReason::UnknownHost => {
                        Some(HostKeyTrustAction::TrustAndStore)
                    }
                    "r" if request.reason == HostKeyTrustReason::ChangedHostKey => {
                        Some(HostKeyTrustAction::ReplaceStoredKey)
                    }
                    _ => None,
                };
                if let Some(action) = action
                    && let Some(response) = response.take()
                {
                    let _ = response.send(action);
                }
                action.is_some()
            }
            Self::Secret {
                response,
                input,
                keychain,
                save_to_keychain,
                ..
            } => match event.logical_key.as_str() {
                "Escape" => {
                    if let Some(response) = response.take() {
                        let _ = response.send(None);
                    }
                    true
                }
                "Enter" if !input.is_empty() => {
                    if let Some(response) = response.take() {
                        let secret = SecretString::new(std::mem::take(input));
                        let response_value = if *save_to_keychain {
                            SecretPromptResponse::persistent(secret)
                        } else {
                            SecretPromptResponse::transient(secret)
                        };
                        let _ = response.send(Some(response_value));
                    }
                    true
                }
                "Tab" => {
                    if keychain.available {
                        *save_to_keychain = !*save_to_keychain;
                    }
                    false
                }
                "Backspace" => {
                    if let Some((index, _)) = input.grapheme_indices(true).next_back() {
                        input.truncate(index);
                    }
                    false
                }
                _ if !event.modifiers.ctrl
                    && !event.modifiers.super_key
                    && (!event.modifiers.alt || event.modifiers.alt_graph) =>
                {
                    if let Some(text) = event.text.as_deref() {
                        append_secret_input(input, text);
                    }
                    false
                }
                _ => false,
            },
        }
    }

    fn append_text(&mut self, text: &str) -> bool {
        let Self::Secret { input, .. } = self else {
            return false;
        };
        append_secret_input(input, text)
    }
}

fn append_secret_input(input: &mut String, text: &str) -> bool {
    const MAX_SECRET_BYTES: usize = 4096;
    if text.is_empty() || input.len().saturating_add(text.len()) > MAX_SECRET_BYTES {
        return false;
    }
    input.push_str(text);
    true
}

#[derive(Debug, Default)]
struct MouseHandling {
    changed: bool,
    open_url: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct KeyboardSelection {
    anchor: GridPosition,
    focus: GridPosition,
    kind: SelectionKind,
}

#[derive(Debug, Default)]
struct PaneSearch {
    input_active: bool,
    query: String,
    matches: Vec<Selection>,
    rows: BTreeMap<i64, Vec<SearchRowSpan>>,
    active_match: usize,
    revision: u64,
    /// Query and buffer state the current `matches` were produced from. Search
    /// walks the whole buffer, and it used to run again on every keystroke and
    /// every refresh even when neither had changed.
    searched: Option<(String, ContentRevision)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SearchRowSpan {
    match_index: usize,
    start_col: u16,
    end_col: u16,
}

fn refresh_search_state(search: &mut PaneSearch, terminal: &mut TerminalEmulator) {
    let revision = terminal.state().content_revision();
    if search
        .searched
        .as_ref()
        .is_some_and(|(query, seen)| *query == search.query && *seen == revision)
    {
        // Same query over the same buffer yields the same hits; re-running it
        // would also fight the user's scrolling by revealing the match again.
        return;
    }
    search.matches = terminal.state().search(&search.query, false);
    search.searched = Some((search.query.clone(), revision));
    search.rebuild_rows(terminal.state().viewport().size.cols);
    search.active_match = search
        .active_match
        .min(search.matches.len().saturating_sub(1));
    if let Some(selection) = search.matches.get(search.active_match) {
        terminal.state_mut().reveal_position(selection.start);
    }
}

impl PaneSearch {
    fn start(&mut self) {
        self.input_active = true;
        self.query.clear();
        self.matches.clear();
        self.rows.clear();
        self.active_match = 0;
        self.bump_revision();
    }

    fn bump_revision(&mut self) {
        self.revision = self.revision.wrapping_add(1).max(1);
    }

    fn rebuild_rows(&mut self, cols: u16) {
        self.rows.clear();
        if cols == 0 {
            return;
        }
        let last_col = cols - 1;
        for (match_index, selection) in self.matches.iter().enumerate() {
            let (start, end) = if selection.start <= selection.end {
                (selection.start, selection.end)
            } else {
                (selection.end, selection.start)
            };
            for row in start.row..=end.row {
                let start_col = if row == start.row { start.col } else { 0 };
                let end_col = if row == end.row { end.col } else { last_col }.min(last_col);
                if start_col <= end_col {
                    self.rows.entry(row).or_default().push(SearchRowSpan {
                        match_index,
                        start_col,
                        end_col,
                    });
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct PanePollStats {
    content_changed: bool,
    pty_bytes: u64,
    parser_bytes: u64,
    closed: bool,
    clean_exit: bool,
    error: bool,
}

fn pane_poll_needs_metadata_refresh(poll: PanePollStats) -> bool {
    poll.content_changed || poll.closed || poll.clean_exit || poll.error
}

#[derive(Debug)]
struct RuntimePerformanceCounters {
    pty_read_bytes: u64,
    parser_bytes: u64,
    sample_started: Instant,
}

#[derive(Debug, Clone, Copy, Default)]
struct RuntimeThroughputSample {
    pty_read_bytes_per_second: u64,
    parser_bytes_per_second: u64,
}

impl RuntimePerformanceCounters {
    fn new() -> Self {
        Self {
            pty_read_bytes: 0,
            parser_bytes: 0,
            sample_started: Instant::now(),
        }
    }

    fn record_pty_bytes(&mut self, bytes: u64) {
        self.pty_read_bytes = self.pty_read_bytes.saturating_add(bytes);
    }

    fn record_parser_bytes(&mut self, bytes: u64) {
        self.parser_bytes = self.parser_bytes.saturating_add(bytes);
    }

    fn sample_throughput(&mut self) -> RuntimeThroughputSample {
        let elapsed = self.sample_started.elapsed();
        if elapsed.is_zero() {
            return RuntimeThroughputSample::default();
        }

        let pty_read_bytes_per_second =
            bytes_per_second(self.pty_read_bytes, elapsed).unwrap_or(u64::MAX);
        let parser_bytes_per_second =
            bytes_per_second(self.parser_bytes, elapsed).unwrap_or(u64::MAX);
        self.pty_read_bytes = 0;
        self.parser_bytes = 0;
        self.sample_started = Instant::now();

        RuntimeThroughputSample {
            pty_read_bytes_per_second,
            parser_bytes_per_second,
        }
    }
}

fn bytes_per_second(bytes: u64, elapsed: Duration) -> Option<u64> {
    let nanos = elapsed.as_nanos();
    if nanos == 0 {
        return None;
    }
    let per_second = (u128::from(bytes) * 1_000_000_000) / nanos;
    Some(u64::try_from(per_second).unwrap_or(u64::MAX))
}

impl PaneRuntime {
    fn new(
        config: &AppConfig,
        spec: &SessionSpec,
        size: TerminalGridSize,
        metrics: CellMetrics,
        output_waker: TransportWakeHandle,
    ) -> Self {
        // Honour the configured history depth. The default cap lives in
        // term-core, so an unwired pane silently ignored `scrollback.lines`.
        let mut terminal = TerminalEmulator::with_scrollback_limit(
            CoreTerminalSize::new(size.cols, size.rows),
            config.scrollback.lines,
        );
        terminal.state_mut().set_dynamic_colors(
            config_rgb(config.colors.foreground),
            config_rgb(config.colors.background),
        );
        let transport_size = terminal_transport_size(size, metrics);
        let mut semantic_timeline = SemanticTimelineStore::new();
        let (transport, parse_semantic_events) =
            match spawn_session_transport(config, spec, transport_size, &output_waker) {
                Ok(initial) => {
                    semantic_timeline.set_integration_mode(initial.semantic_mode);
                    if let Some(metadata) = initial.remote_metadata {
                        semantic_timeline.set_remote_session_metadata(metadata);
                    }
                    for diagnostic in initial.activation_diagnostics {
                        eprintln!("shell integration: {diagnostic}");
                    }
                    (Some(initial.transport), initial.parse_semantic_events)
                }
                Err(error) => {
                    let _ = terminal.apply_bytes(
                        format!("failed to start session transport: {error}\r\n").as_bytes(),
                    );
                    semantic_timeline.set_integration_mode(IntegrationMode::Disabled);
                    (None, false)
                }
            };

        let heuristic_detector = (semantic_timeline.integration_mode()
            == IntegrationMode::Heuristic)
            .then(HeuristicCommandDetector::default);
        Self {
            terminal,
            semantic_parser: SemanticEscapeParser::new(),
            semantic_timeline,
            heuristic_detector,
            parse_semantic_events,
            remote_session: matches!(spec.transport, SessionTransportKind::Ssh),
            session_spec: spec.clone(),
            last_size: size,
            connection_state: if matches!(spec.transport, SessionTransportKind::Ssh) {
                if transport.is_some() {
                    PaneConnectionState::Connecting
                } else {
                    PaneConnectionState::Disconnected("SSH transport failed to start".to_owned())
                }
            } else if transport.is_some() {
                PaneConnectionState::Connected
            } else {
                PaneConnectionState::Disconnected("transport failed to start".to_owned())
            },
            exit_code: None,
            disconnect_notified: false,
            ssh_prompt: None,
            osc52_prompt: None,
            ime_preedit: String::new(),
            ime_preedit_cursor: None,
            ime_preedit_cells: 0,
            transport,
            mouse_protocol: MouseProtocolState::default(),
            wheel_remainder: 0.0,
            selection_anchor: None,
            selection_kind: SelectionKind::Normal,
            keyboard_selection: None,
            search: PaneSearch::default(),
            command_output_collapsed: HashMap::new(),
            command_overlay_revision: 1,
            output_waker,
            synchronized_output_since: None,
            semantic_rows_dropped: 0,
            alternate_screen_semantics: None,
            reconnect_policy: SshReconnectPolicy::default(),
            reconnect_attempts: 0,
            reconnect_at: None,
        }
    }

    /// Keeps semantic rows aligned with the terminal after scrollback eviction.
    ///
    /// Regions and command blocks store absolute buffer rows. Once the
    /// scrollback cap starts evicting, every stored row refers to content one
    /// line further up per evicted line; left alone they would point at whatever
    /// text later occupies those coordinates. Rebasing first and then pruning is
    /// required in that order, because pruning compares against current rows.
    /// Takes its fields directly so it can be called while the transport is
    /// mutably borrowed by the polling loop.
    fn sync_semantic_rows(
        terminal: &TerminalEmulator,
        timeline: &mut SemanticTimelineStore,
        baseline: &mut u64,
        alternate_screen_semantics: &mut Option<u64>,
    ) {
        // Rows recorded on the alternate screen are relative to that buffer,
        // whose viewport origin is always 0, so they are correct as recorded and
        // must not be rebased. Nothing is appended to the primary scrollback
        // while the alternate screen is active, so there is nothing to miss.
        let alternate_active = terminal
            .modes_ref()
            .contains(&TerminalMode::AlternateScreen);
        match (alternate_active, *alternate_screen_semantics) {
            (true, None) => {
                *alternate_screen_semantics = Some(timeline.region_id_watermark());
                return;
            }
            (true, Some(_)) => return,
            (false, Some(watermark)) => {
                // Back on the primary screen: those rows describe a buffer that
                // no longer exists, so keeping them would place overlays over
                // unrelated scrollback.
                timeline.discard_regions_from(watermark);
                *alternate_screen_semantics = None;
            }
            (false, None) => {}
        }

        let dropped = terminal.state().scrollback_dropped();
        if dropped == *baseline {
            // The common case: one comparison per poll.
            return;
        }
        if dropped < *baseline {
            // The counter restarted: a full reset or a reconnect replaced the
            // terminal. There is nothing to rebase against, so just re-baseline.
            *baseline = dropped;
            return;
        }
        let evicted = dropped - *baseline;
        *baseline = dropped;
        timeline.rebase_rows(evicted);
        // Anything that ended above row 0 has left the buffer entirely.
        timeline.prune_before_row(0);
    }

    /// Decides whether a dropped remote session should be retried, and arms the
    /// timer if so.
    ///
    /// Only remote sessions are retried: a local shell that exited did so
    /// because the user asked it to.
    fn arm_automatic_reconnect(&mut self, failure: &str) -> Option<Duration> {
        if !self.remote_session {
            return None;
        }
        match self.reconnect_policy.decide(self.reconnect_attempts, failure) {
            SshReconnectDecision::Retry { attempt, after } => {
                self.reconnect_at = Some(Instant::now() + after);
                let _ = attempt;
                Some(after)
            }
            SshReconnectDecision::GiveUp(refusal) => {
                self.reconnect_at = None;
                if !matches!(refusal, SshReconnectRefusal::Disabled) {
                    let note: &[u8] = match refusal {
                        SshReconnectRefusal::Permanent => {
                            b"
[Panea will not reconnect automatically: resolve the error above]
"
                        }
                        _ => b"
[Panea stopped reconnecting; use the reconnect binding to retry]
",
                    };
                    let _ = self.terminal.apply_bytes(note);
                }
                None
            }
        }
    }

    /// Whether an armed automatic retry is due.
    fn automatic_reconnect_is_due(&self, now: Instant) -> bool {
        self.reconnect_at.is_some_and(|due| now >= due)
    }

    /// Runs a due automatic retry.
    fn run_automatic_reconnect(&mut self, config: &AppConfig, metrics: CellMetrics) -> bool {
        if !self.automatic_reconnect_is_due(Instant::now()) {
            return false;
        }
        self.reconnect_at = None;
        self.reconnect_attempts = self.reconnect_attempts.saturating_add(1);
        // `reconnect` reports failure through the connection state, which the
        // next poll turns back into a decision, so the backoff keeps widening.
        self.reconnect(config, metrics)
    }

    fn toggle_current_command_output(&mut self, config: &AppConfig) -> bool {
        if !config.command_blocks.enabled {
            return false;
        }
        let cursor = self.terminal.state().cursor_buffer_position();
        let Some(block) = self
            .semantic_timeline
            .current_command(BufferPosition::new(cursor.row, cursor.col))
            .or_else(|| {
                self.semantic_timeline
                    .previous_command(BufferPosition::new(cursor.row, cursor.col))
            })
        else {
            return false;
        };
        let output_rows = self
            .semantic_timeline
            .output_span_for_command(block)
            .map_or(0, semantic_span_rows);
        if output_rows <= u32::from(config.command_blocks.collapsed_preview_lines) {
            return false;
        }
        let auto_collapsed = config.command_blocks.collapse_long_output
            && output_rows > u32::from(config.command_blocks.collapse_after_lines);
        let currently_collapsed = self
            .command_output_collapsed
            .get(&block.region_id)
            .copied()
            .unwrap_or(auto_collapsed);
        self.command_output_collapsed
            .insert(block.region_id, !currently_collapsed);
        self.command_overlay_revision = self.command_overlay_revision.wrapping_add(1).max(1);
        true
    }

    fn start_keyboard_selection(&mut self, kind: SelectionKind) {
        let position = self.terminal.state().cursor_buffer_position();
        self.keyboard_selection = Some(KeyboardSelection {
            anchor: position,
            focus: position,
            kind,
        });
        self.terminal.state_mut().set_selection(Selection {
            start: position,
            end: position,
            kind,
        });
    }

    fn run_semantic_action(&mut self, action: SemanticAction) -> SemanticActionResult {
        let cursor = self.terminal.state().cursor_buffer_position();
        let position = BufferPosition::new(cursor.row, cursor.col);
        let provider = PaneTextProvider {
            terminal: &self.terminal,
        };
        let result = self
            .semantic_timeline
            .run_action(action, position, &provider);
        match result {
            SemanticActionResult::Position(position) => {
                self.terminal
                    .state_mut()
                    .reveal_position(GridPosition::new(position.row, position.col));
            }
            SemanticActionResult::Selection(span) => {
                self.terminal.state_mut().set_selection(Selection {
                    start: GridPosition::new(span.start.row, span.start.col),
                    end: GridPosition::new(span.end.row, span.end.col),
                    kind: SelectionKind::Normal,
                });
                self.terminal
                    .state_mut()
                    .reveal_position(GridPosition::new(span.start.row, span.start.col));
            }
            SemanticActionResult::Text(_) | SemanticActionResult::Noop => {}
        }
        result
    }

    fn handle_modal_key(
        &mut self,
        event: &KeyEvent,
        clipboard: &mut ClipboardBridge,
        policy: &Osc52ClipboardPolicy,
        clipboard_config: &ClipboardConfig,
    ) -> Option<bool> {
        if let Some(prompt) = self.ssh_prompt.as_mut() {
            let completed = prompt.handle_key(event);
            if completed {
                self.ssh_prompt = None;
            }
            return Some(true);
        }
        if self.osc52_prompt.is_some() {
            match event.logical_key.to_ascii_lowercase().as_str() {
                "y" => {
                    let Some(prompt) = self.osc52_prompt.take() else {
                        return Some(true);
                    };
                    match approve_osc52_clipboard_write(&prompt.request, policy) {
                        Osc52ClipboardDecision::Allow { text, bytes } => {
                            copy_osc52_text_with_diagnostics(
                                clipboard,
                                &text,
                                clipboard_config,
                                prompt.request.target,
                                "confirmed remote OSC 52",
                            );
                            eprintln!(
                                "clipboard OSC 52: user approved one remote {bytes} byte write"
                            );
                        }
                        Osc52ClipboardDecision::Deny { reason } => {
                            eprintln!(
                                "clipboard OSC 52: approved request failed policy recheck: {reason}"
                            );
                        }
                        Osc52ClipboardDecision::PromptRequired { .. } => {
                            eprintln!(
                                "clipboard OSC 52: approved request unexpectedly required another prompt"
                            );
                        }
                    }
                    return Some(true);
                }
                "n" | "escape" => {
                    self.osc52_prompt = None;
                    eprintln!("clipboard OSC 52: user denied remote clipboard write");
                    return Some(true);
                }
                _ => return Some(true),
            }
        }
        if self.search.input_active {
            return Some(self.handle_search_key(event));
        }
        if self.keyboard_selection.is_some() {
            return Some(self.handle_keyboard_selection_key(event));
        }
        None
    }

    fn append_modal_text(&mut self, text: &str) -> bool {
        self.ssh_prompt
            .as_mut()
            .is_some_and(|prompt| prompt.append_text(text))
    }

    fn update_ime_preedit(&mut self, text: String, cursor: Option<(usize, usize)>) -> bool {
        if self.ime_preedit == text && self.ime_preedit_cursor == cursor {
            return false;
        }
        self.ime_preedit = text;
        self.ime_preedit_cursor = cursor;
        self.ime_preedit_cells = UnicodeWidthStr::width(self.ime_preedit.as_str());
        true
    }

    fn append_search_text(&mut self, text: &str) -> bool {
        if !self.search.input_active || text.is_empty() {
            return false;
        }
        self.search.query.push_str(text);
        self.refresh_search();
        true
    }

    fn handle_search_key(&mut self, event: &KeyEvent) -> bool {
        match event.logical_key.as_str() {
            "Escape" => {
                self.search.input_active = false;
                self.search.matches.clear();
                self.search.rows.clear();
                self.search.bump_revision();
                true
            }
            "Backspace" => {
                if let Some((index, _)) = self.search.query.grapheme_indices(true).next_back() {
                    self.search.query.truncate(index);
                    self.refresh_search();
                }
                true
            }
            "Enter" | "ArrowDown" => {
                self.advance_search(!event.modifiers.shift);
                true
            }
            "ArrowUp" => {
                self.advance_search(false);
                true
            }
            _ if !event.modifiers.ctrl
                && !event.modifiers.super_key
                && (!event.modifiers.alt || event.modifiers.alt_graph) =>
            {
                if let Some(text) = event.text.as_deref().filter(|text| !text.is_empty()) {
                    self.search.query.push_str(text);
                    self.refresh_search();
                }
                true
            }
            _ => true,
        }
    }

    fn refresh_search(&mut self) {
        refresh_search_state(&mut self.search, &mut self.terminal);
        self.search.bump_revision();
    }

    fn advance_search(&mut self, forward: bool) {
        if self.search.matches.is_empty() {
            return;
        }
        if forward {
            self.search.active_match = (self.search.active_match + 1) % self.search.matches.len();
        } else {
            self.search.active_match = self
                .search
                .active_match
                .checked_sub(1)
                .unwrap_or(self.search.matches.len() - 1);
        }
        self.reveal_active_search_match();
        self.search.bump_revision();
    }

    fn reveal_active_search_match(&mut self) {
        if let Some(selection) = self.search.matches.get(self.search.active_match) {
            self.terminal.state_mut().reveal_position(selection.start);
        }
    }

    fn handle_keyboard_selection_key(&mut self, event: &KeyEvent) -> bool {
        if event.logical_key == "Escape" {
            self.keyboard_selection = None;
            self.terminal.state_mut().clear_selection();
            return true;
        }
        if event.logical_key == "Enter" {
            self.keyboard_selection = None;
            return true;
        }

        let Some(mut selection) = self.keyboard_selection else {
            return false;
        };
        let max_row = self.terminal.state().buffer_line_count().saturating_sub(1) as i64;
        let viewport = self.terminal.state().viewport();
        let max_col = viewport.size.cols.saturating_sub(1);
        let page = i64::from(viewport.size.rows).max(1);
        match event.logical_key.as_str() {
            "ArrowLeft" => selection.focus.col = selection.focus.col.saturating_sub(1),
            "ArrowRight" => {
                selection.focus.col = selection.focus.col.saturating_add(1).min(max_col)
            }
            "ArrowUp" => selection.focus.row = selection.focus.row.saturating_sub(1),
            "ArrowDown" => selection.focus.row = (selection.focus.row + 1).min(max_row),
            "Home" => selection.focus.col = 0,
            "End" => selection.focus.col = max_col,
            "PageUp" => selection.focus.row = selection.focus.row.saturating_sub(page),
            "PageDown" => selection.focus.row = (selection.focus.row + page).min(max_row),
            _ => return true,
        }
        self.keyboard_selection = Some(selection);
        self.terminal.state_mut().set_selection(Selection {
            start: selection.anchor,
            end: selection.focus,
            kind: selection.kind,
        });
        self.terminal.state_mut().reveal_position(selection.focus);
        true
    }

    fn url_at_mouse(&self, mouse: MouseEvent, metrics: CellMetrics) -> Option<String> {
        let viewport = self.terminal.state().viewport();
        let row = ((mouse.y / f64::from(metrics.cell_height)).floor() as u16)
            .min(viewport.size.rows.saturating_sub(1));
        let col = ((mouse.x / f64::from(metrics.cell_width)).floor() as u16)
            .min(viewport.size.cols.saturating_sub(1));
        visible_url_hints(&self.terminal, row)
            .into_iter()
            .find(|hint| col >= hint.start.col && col < hint.end.col)
            .map(|hint| hint.text)
    }

    fn handle_selection_or_scrollback(&mut self, mouse: MouseEvent, metrics: CellMetrics) -> bool {
        if let MouseEventKind::Wheel(delta) = mouse.kind {
            let lines = accumulated_scroll_lines(delta, metrics, &mut self.wheel_remainder);
            return self.terminal.state_mut().scroll_viewport(lines);
        }

        let visible = self.terminal.state().viewport();
        let row = ((mouse.y / f64::from(metrics.cell_height)).floor() as u16)
            .min(visible.size.rows.saturating_sub(1));
        let col = ((mouse.x / f64::from(metrics.cell_width)).floor() as u16)
            .min(visible.size.cols.saturating_sub(1));
        let position = self.terminal.state().viewport_position(row, col);

        match mouse.kind {
            MouseEventKind::Pressed(MouseButton::Left) => {
                self.selection_anchor = Some(position);
                self.selection_kind = if mouse.modifiers.alt {
                    SelectionKind::Rectangular
                } else {
                    SelectionKind::Normal
                };
                let had_selection = self.terminal.selection_state().is_some();
                self.terminal.state_mut().clear_selection();
                had_selection
            }
            MouseEventKind::Moved => {
                let Some(anchor) = self.selection_anchor else {
                    return false;
                };
                if anchor == position {
                    let had_selection = self.terminal.selection_state().is_some();
                    self.terminal.state_mut().clear_selection();
                    return had_selection;
                }
                self.terminal.state_mut().set_selection(Selection {
                    start: anchor,
                    end: position,
                    kind: self.selection_kind,
                });
                true
            }
            MouseEventKind::Released(MouseButton::Left) => {
                let Some(anchor) = self.selection_anchor.take() else {
                    return false;
                };
                if anchor == position {
                    let had_selection = self.terminal.selection_state().is_some();
                    self.terminal.state_mut().clear_selection();
                    return had_selection;
                }
                self.terminal.state_mut().set_selection(Selection {
                    start: anchor,
                    end: position,
                    kind: self.selection_kind,
                });
                true
            }
            _ => false,
        }
    }

    fn resize(&mut self, size: TerminalGridSize, metrics: CellMetrics) {
        self.last_size = size;
        let mut semantic_positions = self.semantic_timeline.position_entries();
        let semantic_count = semantic_positions.len();
        let mut grid_positions = semantic_positions
            .iter()
            .map(|entry| GridPosition::new(entry.position.row, entry.position.col))
            .collect::<Vec<_>>();
        let selection_anchor_index = self.selection_anchor.map(|position| {
            grid_positions.push(position);
            grid_positions.len() - 1
        });
        let keyboard_indices = self.keyboard_selection.map(|selection| {
            let anchor = grid_positions.len();
            grid_positions.push(selection.anchor);
            let focus = grid_positions.len();
            grid_positions.push(selection.focus);
            (anchor, focus)
        });
        let resized = self
            .terminal
            .resize_with_positions(
                CoreTerminalSize::new(size.cols, size.rows),
                &mut grid_positions,
            )
            .is_ok();
        if resized {
            for (entry, position) in semantic_positions
                .iter_mut()
                .zip(grid_positions.iter().take(semantic_count).copied())
            {
                entry.position = BufferPosition::new(position.row, position.col);
            }
            self.semantic_timeline
                .apply_position_entries(&semantic_positions[..semantic_count]);
            if let Some(index) = selection_anchor_index {
                self.selection_anchor = grid_positions.get(index).copied();
            }
            if let Some((anchor, focus)) = keyboard_indices
                && let Some(selection) = self.keyboard_selection.as_mut()
            {
                if let Some(position) = grid_positions.get(anchor).copied() {
                    selection.anchor = position;
                }
                if let Some(position) = grid_positions.get(focus).copied() {
                    selection.focus = position;
                }
            }
            if !self.search.query.is_empty() {
                refresh_search_state(&mut self.search, &mut self.terminal);
                self.search.bump_revision();
            }
        }
        if let Some(transport) = self.transport.as_mut() {
            resize_transport(transport, terminal_transport_size(size, metrics));
        }
    }

    fn write_input(&mut self, bytes: &[u8]) {
        let Some(transport) = self.transport.as_mut() else {
            return;
        };

        // Terminal input has priority over optional semantic bookkeeping. The
        // transport worker owns all potentially blocking backend I/O.
        write_terminal_input(&mut self.terminal, transport, bytes);

        if let Some(detector) = self.heuristic_detector.as_mut() {
            let cursor = self.terminal.state().cursor_buffer_position();
            let events = detector.observe_input(
                bytes,
                BufferPosition::new(cursor.row, cursor.col),
                self.terminal
                    .modes_ref()
                    .contains(&TerminalMode::AlternateScreen),
                Instant::now(),
            );
            for event in events {
                self.semantic_timeline.apply_event(event);
            }
        }
    }

    fn poll_output(
        &mut self,
        clipboard: &mut ClipboardBridge,
        policy: &Osc52ClipboardPolicy,
        clipboard_config: &ClipboardConfig,
    ) -> PanePollStats {
        let Some(transport) = self.transport.as_mut() else {
            return PanePollStats::default();
        };

        let mut stats = PanePollStats::default();
        if Self::update_synchronized_output(
            &mut self.terminal,
            &mut self.synchronized_output_since,
            Instant::now(),
            false,
        ) {
            stats.content_changed = true;
        }
        if self.ssh_prompt.is_none()
            && let Some(request) = transport.take_interaction()
        {
            self.ssh_prompt = Some(SshPromptState::from_request(request));
            stats.content_changed = true;
        }
        let drain_started = Instant::now();
        for pass in 0..MAX_OUTPUT_DRAIN_PASSES {
            // A pane that stops mid-backlog keeps its wake: the transport wakes
            // whenever it stopped at its own byte cap or still holds a split
            // chunk, so yielding here never leaves output unread.
            if pass > 0 && drain_started.elapsed() >= MAX_OUTPUT_DRAIN_BUDGET {
                break;
            }
            let output = match catch_unwind(AssertUnwindSafe(|| transport.poll_output())) {
                Ok(Ok(output)) => output,
                Ok(Err(error)) => {
                    eprintln!("transport poll error: {error}");
                    let failure = error.to_string();
                    self.arm_automatic_reconnect(&failure);
                    self.connection_state = PaneConnectionState::Disconnected(failure);
                    let _ = self
                        .terminal
                        .apply_bytes(format!("\r\ntransport error: {error}\r\n").as_bytes());
                    stats.content_changed = true;
                    stats.error = !self.disconnect_notified;
                    self.disconnect_notified = true;
                    break;
                }
                Err(panic) => {
                    eprintln!("transport poll panic boundary: {}", panic_payload(panic));
                    break;
                }
            };
            if transport.is_connected() {
                if self.reconnect_attempts > 0 {
                    // The session is back: spend the budget afresh next time and
                    // say so, since the outage was announced.
                    self.reconnect_attempts = 0;
                    let _ = self
                        .terminal
                        .apply_bytes(b"
[Panea reconnected the SSH session]
");
                }
                self.reconnect_at = None;
                self.connection_state = PaneConnectionState::Connected;
                self.disconnect_notified = false;
            }
            for lifecycle in &output.lifecycle {
                if let transport_core::TransportLifecycleEvent::Exited { exit_code } = lifecycle {
                    self.exit_code = *exit_code;
                }
            }
            if output.bytes.is_empty() && output.lifecycle.is_empty() && !output.closed {
                break;
            }

            if !output.bytes.is_empty() {
                self.connection_state = PaneConnectionState::Connected;
                self.disconnect_notified = false;
                let byte_count = u64::try_from(output.bytes.len()).unwrap_or(u64::MAX);
                stats.pty_bytes = stats.pty_bytes.saturating_add(byte_count);
                if self.parse_semantic_events {
                    // Semantic rows are absolute buffer rows: overlays subtract
                    // the viewport origin from them. Recording the screen-relative
                    // cursor row instead displaced every region by the scrollback
                    // length as soon as anything scrolled off.
                    let cursor = self.terminal.state().cursor_buffer_position();
                    let initial_position = BufferPosition::new(cursor.row, cursor.col);
                    let parsed = parse_semantic_markers(
                        &mut self.semantic_parser,
                        &output.bytes,
                        initial_position,
                    );
                    let mut applied = 0usize;
                    for parsed in parsed {
                        let source_end = parsed.source_end.min(output.bytes.len()).max(applied);
                        if !apply_terminal_bytes(
                            &mut self.terminal,
                            &output.bytes[applied..source_end],
                        ) {
                            break;
                        }
                        applied = source_end;
                        // Applying those bytes may have evicted scrollback, which
                        // moves every row already recorded.
                        Self::sync_semantic_rows(
                            &self.terminal,
                            &mut self.semantic_timeline,
                            &mut self.semantic_rows_dropped,
                            &mut self.alternate_screen_semantics,
                        );
                        let cursor = self.terminal.state().cursor_buffer_position();
                        let event = parsed
                            .event
                            .at_position(BufferPosition::new(cursor.row, cursor.col));
                        let event = if self.remote_session {
                            self.semantic_timeline.mark_remote_integration_active();
                            event.in_remote_session()
                        } else {
                            event
                        };
                        self.semantic_timeline.apply_event(event);
                    }
                    if !apply_terminal_bytes(&mut self.terminal, &output.bytes[applied..]) {
                        break;
                    }
                    Self::sync_semantic_rows(
                        &self.terminal,
                        &mut self.semantic_timeline,
                        &mut self.semantic_rows_dropped,
                        &mut self.alternate_screen_semantics,
                    );
                } else if !apply_terminal_bytes(&mut self.terminal, &output.bytes) {
                    break;
                } else {
                    Self::sync_semantic_rows(
                        &self.terminal,
                        &mut self.semantic_timeline,
                        &mut self.semantic_rows_dropped,
                        &mut self.alternate_screen_semantics,
                    );
                }
                stats.parser_bytes = stats.parser_bytes.saturating_add(byte_count);
                process_pending_clipboard_requests(
                    &mut self.terminal,
                    clipboard,
                    policy,
                    clipboard_config,
                    self.remote_session,
                    &mut self.osc52_prompt,
                );
                flush_terminal_responses(&mut self.terminal, transport);
                if Self::update_synchronized_output(
                    &mut self.terminal,
                    &mut self.synchronized_output_since,
                    Instant::now(),
                    true,
                ) {
                    stats.content_changed = true;
                }
            }
            if output.closed {
                if let Some(detector) = self.heuristic_detector.as_mut() {
                    let cursor = self.terminal.state().cursor_buffer_position();
                    for event in detector
                        .finish_session(BufferPosition::new(cursor.row, cursor.col), Instant::now())
                    {
                        self.semantic_timeline.apply_event(event);
                    }
                }
                let failure = if self.remote_session {
                    "SSH session disconnected".to_owned()
                } else {
                    "session exited".to_owned()
                };
                if let Some(after) = self.arm_automatic_reconnect(&failure) {
                    let _ = self.terminal.apply_bytes(
                        format!(
                            "
[Panea lost the SSH session; reconnecting in {}s...]
",
                            after.as_secs().max(1)
                        )
                        .as_bytes(),
                    );
                }
                self.connection_state = PaneConnectionState::Disconnected(failure);
                stats.content_changed = true;
                stats.closed = !self.disconnect_notified;
                stats.clean_exit = !self.remote_session && self.exit_code == Some(0);
                self.disconnect_notified = true;
                break;
            }
            if !output.bytes.is_empty() {
                break;
            }
        }
        stats
    }

    fn update_synchronized_output(
        terminal: &mut TerminalEmulator,
        synchronized_output_since: &mut Option<Instant>,
        now: Instant,
        content_arrived: bool,
    ) -> bool {
        if terminal
            .modes_ref()
            .contains(&TerminalMode::SynchronizedOutput)
        {
            let started = synchronized_output_since.get_or_insert(now);
            if now.saturating_duration_since(*started) < SYNCHRONIZED_OUTPUT_TIMEOUT {
                return false;
            }
            let _ = terminal.state_mut().apply_action(TerminalAction::SetMode {
                mode: TerminalMode::SynchronizedOutput,
                enabled: false,
            });
            *synchronized_output_since = None;
            return true;
        }

        let released = synchronized_output_since.take().is_some();
        content_arrived || released
    }

    fn synchronized_output_deadline(&self) -> Option<Instant> {
        self.synchronized_output_since
            .map(|started| started + SYNCHRONIZED_OUTPUT_TIMEOUT)
    }

    fn reconnect(&mut self, config: &AppConfig, metrics: CellMetrics) -> bool {
        self.shutdown();
        self.ssh_prompt = None;
        self.osc52_prompt = None;
        self.semantic_parser = SemanticEscapeParser::new();
        self.exit_code = None;
        let transport_size = terminal_transport_size(self.last_size, metrics);
        match spawn_session_transport(
            config,
            &self.session_spec,
            transport_size,
            &self.output_waker,
        ) {
            Ok(initial) => {
                self.transport = Some(initial.transport);
                self.parse_semantic_events = initial.parse_semantic_events;
                self.semantic_timeline
                    .set_integration_mode(initial.semantic_mode);
                self.heuristic_detector = (initial.semantic_mode == IntegrationMode::Heuristic)
                    .then(HeuristicCommandDetector::default);
                if let Some(metadata) = initial.remote_metadata {
                    self.semantic_timeline.set_remote_session_metadata(metadata);
                }
                for diagnostic in initial.activation_diagnostics {
                    eprintln!("shell integration: {diagnostic}");
                }
                self.connection_state = if self.remote_session {
                    PaneConnectionState::Connecting
                } else {
                    PaneConnectionState::Connected
                };
                self.disconnect_notified = false;
                let message = if self.remote_session {
                    b"\r\n[Panea reconnecting SSH session...]\r\n".as_slice()
                } else {
                    b"\r\n[Panea restarting local session...]\r\n".as_slice()
                };
                let _ = self.terminal.apply_bytes(message);
                true
            }
            Err(error) => {
                self.connection_state = PaneConnectionState::Disconnected(error.to_string());
                let operation = if self.remote_session {
                    "SSH reconnect"
                } else {
                    "local session restart"
                };
                let _ = self
                    .terminal
                    .apply_bytes(format!("\r\n{operation} failed: {error}\r\n").as_bytes());
                false
            }
        }
    }

    fn shutdown(&mut self) {
        self.osc52_prompt = None;
        if let Some(detector) = self.heuristic_detector.as_mut() {
            let cursor = self.terminal.state().cursor_buffer_position();
            for event in
                detector.finish_session(BufferPosition::new(cursor.row, cursor.col), Instant::now())
            {
                self.semantic_timeline.apply_event(event);
            }
        }
        shutdown_transport(self.transport.as_mut());
    }

    fn scrollback_memory_bytes(&self) -> u64 {
        self.terminal.scrollback_memory_bytes()
    }
}

fn mark_session_status(model: &mut MuxModel, pane_id: PaneId, status: SessionStatus) {
    if let Ok(session) = model.session_for_pane_mut(pane_id) {
        session.status = status;
    }
}

fn session_spec_for_config(config: &AppConfig) -> SessionSpec {
    let profile = selected_shell_profile(config).map(resolved_shell_profile);
    let mut spec = SessionSpec::local(
        profile
            .as_ref()
            .map(|profile| profile.name.clone())
            .unwrap_or_else(|| "default".to_owned()),
    );
    if let Some(profile) = profile {
        spec.working_directory = profile.working_directory.clone();
        spec.startup_command = profile.startup_command.clone();
    }
    spec
}

fn local_session_spec(profile_name: &str) -> SessionSpec {
    SessionSpec::local(profile_name)
}

fn terminal_transport_size(size: TerminalGridSize, metrics: CellMetrics) -> TransportSize {
    TransportSize::new(
        size.cols,
        size.rows,
        (f32::from(size.cols) * metrics.cell_width).ceil() as u32,
        (f32::from(size.rows) * metrics.cell_height).ceil() as u32,
    )
}

fn resize_transport(transport: &mut PaneTransport, size: TransportSize) {
    match catch_unwind(AssertUnwindSafe(|| transport.resize(size))) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => eprintln!("transport resize error: {error}"),
        Err(panic) => eprintln!("transport resize panic boundary: {}", panic_payload(panic)),
    }
}

fn apply_terminal_bytes(terminal: &mut TerminalEmulator, bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return true;
    }
    match catch_unwind(AssertUnwindSafe(|| terminal.apply_bytes(bytes))) {
        Ok(_) => true,
        Err(panic) => {
            eprintln!("terminal parser panic boundary: {}", panic_payload(panic));
            false
        }
    }
}

/// Parses shell-integration markers behind a panic boundary.
///
/// This runs its own scanner over untrusted terminal bytes, so it needs the same
/// guard as the terminal parser. It sat outside one, which is how a malformed
/// `OSC 7` payload could take down the pane rather than being dropped.
fn parse_semantic_markers(
    parser: &mut SemanticEscapeParser,
    bytes: &[u8],
    position: BufferPosition,
) -> Vec<shell_integration::ParsedSemanticEvent> {
    match catch_unwind(AssertUnwindSafe(|| parser.parse(bytes, position))) {
        Ok(events) => events,
        Err(panic) => {
            eprintln!(
                "shell integration parser panic boundary: {}",
                panic_payload(panic)
            );
            Vec::new()
        }
    }
}

fn tab_bar_rows(model: &MuxModel, config: &AppConfig) -> u16 {
    if config.mux.show_tab_bar && model.active_workspace().active_window().tabs.len() > 1 {
        1
    } else {
        0
    }
}

#[derive(Debug, Clone, Copy)]
struct CursorPresentation {
    blink_visible: bool,
    window_focused: bool,
}

fn surface_size_is_renderable(size: winit::dpi::PhysicalSize<u32>) -> bool {
    size.width > 0 && size.height > 0
}

const TERMINAL_RESIZE_SETTLE: Duration = Duration::from_millis(40);

#[derive(Debug, Clone, Copy, Default)]
struct PendingTerminalResize {
    size: Option<winit::dpi::PhysicalSize<u32>>,
    apply_at: Option<Instant>,
}

impl PendingTerminalResize {
    fn queue(&mut self, size: winit::dpi::PhysicalSize<u32>) {
        if !surface_size_is_renderable(size) {
            return;
        }
        self.size = Some(size);
        self.apply_at = Some(Instant::now() + TERMINAL_RESIZE_SETTLE);
    }

    fn deadline(self) -> Option<Instant> {
        self.apply_at
    }

    fn take_due(&mut self, now: Instant) -> Option<winit::dpi::PhysicalSize<u32>> {
        if self.apply_at.is_none_or(|deadline| now < deadline) {
            return None;
        }
        self.apply_at = None;
        self.size.take()
    }
}
