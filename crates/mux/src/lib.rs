//! Transport-neutral multiplexer model for workspaces, tabs, panes, sessions,
//! and proportional layouts.

pub const LAYER: &str = "multiplexer structure";

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
};

use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
pub struct WorkspaceId(pub u64);

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
pub struct WindowId(pub u64);

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
pub struct TabId(pub u64);

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
pub struct PaneId(pub u64);

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
pub struct SessionId(pub u64);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MuxModel {
    pub workspaces: BTreeMap<WorkspaceId, Workspace>,
    pub active_workspace: WorkspaceId,
    counters: IdCounters,
    #[serde(skip, default = "initial_layout_revision")]
    layout_revision: u64,
}

const fn initial_layout_revision() -> u64 {
    1
}

impl MuxModel {
    #[must_use]
    pub fn new(default_session: SessionSpec) -> Self {
        let mut counters = IdCounters::default();
        let workspace_id = counters.next_workspace();
        let window_id = counters.next_window();
        let tab_id = counters.next_tab();
        let pane_id = counters.next_pane();
        let session_id = counters.next_session();

        let session = Session::new(session_id, default_session);
        let pane = Pane::new(pane_id, session_id);
        let tab = Tab::single_pane(tab_id, "1", pane, session);
        let window = WindowModel {
            id: window_id,
            tabs: vec![tab],
            active_tab: tab_id,
        };
        let workspace = Workspace {
            id: workspace_id,
            name: "default".to_owned(),
            windows: vec![window],
            active_window: window_id,
        };

        Self {
            workspaces: BTreeMap::from([(workspace_id, workspace)]),
            active_workspace: workspace_id,
            counters,
            layout_revision: initial_layout_revision(),
        }
    }

    #[must_use]
    pub const fn layout_revision(&self) -> u64 {
        self.layout_revision
    }

    fn bump_layout_revision(&mut self) {
        self.layout_revision = self.layout_revision.wrapping_add(1).max(1);
    }

    #[must_use]
    pub fn active_workspace(&self) -> &Workspace {
        self.workspaces
            .get(&self.active_workspace)
            .expect("active workspace must exist")
    }

    pub fn active_workspace_mut(&mut self) -> &mut Workspace {
        self.workspaces
            .get_mut(&self.active_workspace)
            .expect("active workspace must exist")
    }

    #[must_use]
    pub fn active_tab(&self) -> &Tab {
        self.active_workspace().active_window().active_tab()
    }

    pub fn active_tab_mut(&mut self) -> &mut Tab {
        self.active_workspace_mut()
            .active_window_mut()
            .active_tab_mut()
    }

    pub fn session_for_pane(&self, pane_id: PaneId) -> MuxResult<&Session> {
        for workspace in self.workspaces.values() {
            for window in &workspace.windows {
                for tab in &window.tabs {
                    if let Some(pane) = tab.panes.get(&pane_id) {
                        return tab
                            .sessions
                            .get(&pane.session_id)
                            .ok_or(MuxError::SessionNotFound(pane.session_id));
                    }
                }
            }
        }
        Err(MuxError::PaneNotFound(pane_id))
    }

    pub fn session_for_pane_mut(&mut self, pane_id: PaneId) -> MuxResult<&mut Session> {
        for workspace in self.workspaces.values_mut() {
            for window in &mut workspace.windows {
                for tab in &mut window.tabs {
                    if let Some(pane) = tab.panes.get(&pane_id) {
                        return tab
                            .sessions
                            .get_mut(&pane.session_id)
                            .ok_or(MuxError::SessionNotFound(pane.session_id));
                    }
                }
            }
        }
        Err(MuxError::PaneNotFound(pane_id))
    }

    pub fn update_pane_title(
        &mut self,
        pane_id: PaneId,
        title: impl Into<String>,
    ) -> MuxResult<()> {
        let title = title.into();
        for workspace in self.workspaces.values_mut() {
            for window in &mut workspace.windows {
                for tab in &mut window.tabs {
                    if let Some(pane) = tab.panes.get_mut(&pane_id) {
                        pane.title = Some(title.clone());
                        if let Some(session) = tab.sessions.get_mut(&pane.session_id) {
                            session.title = Some(title.clone());
                        }
                        if tab.active_pane == pane_id
                            && !matches!(tab.title_source, TabTitleSource::User)
                        {
                            tab.name = title;
                            tab.title_source = TabTitleSource::Session;
                        }
                        return Ok(());
                    }
                }
            }
        }
        Err(MuxError::PaneNotFound(pane_id))
    }

    pub fn new_workspace(&mut self, name: impl Into<String>, session: SessionSpec) -> WorkspaceId {
        let workspace_id = self.counters.next_workspace();
        let window_id = self.counters.next_window();
        let tab_id = self.counters.next_tab();
        let pane_id = self.counters.next_pane();
        let session_id = self.counters.next_session();
        let workspace = Workspace {
            id: workspace_id,
            name: name.into(),
            windows: vec![WindowModel {
                id: window_id,
                tabs: vec![Tab::single_pane(
                    tab_id,
                    "1",
                    Pane::new(pane_id, session_id),
                    Session::new(session_id, session),
                )],
                active_tab: tab_id,
            }],
            active_window: window_id,
        };
        self.workspaces.insert(workspace_id, workspace);
        self.active_workspace = workspace_id;
        self.bump_layout_revision();
        workspace_id
    }

    pub fn switch_workspace(&mut self, workspace_id: WorkspaceId) -> MuxResult<()> {
        if !self.workspaces.contains_key(&workspace_id) {
            return Err(MuxError::WorkspaceNotFound(workspace_id));
        }
        if self.active_workspace != workspace_id {
            self.active_workspace = workspace_id;
            self.bump_layout_revision();
        }
        Ok(())
    }

    pub fn rename_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        name: impl Into<String>,
    ) -> MuxResult<()> {
        let workspace = self
            .workspaces
            .get_mut(&workspace_id)
            .ok_or(MuxError::WorkspaceNotFound(workspace_id))?;
        workspace.name = name.into();
        Ok(())
    }

    pub fn close_workspace(&mut self, workspace_id: WorkspaceId) -> MuxResult<()> {
        if self.workspaces.len() == 1 {
            return Err(MuxError::CannotCloseLastWorkspace);
        }
        self.workspaces
            .remove(&workspace_id)
            .ok_or(MuxError::WorkspaceNotFound(workspace_id))?;
        if self.active_workspace == workspace_id {
            self.active_workspace = *self
                .workspaces
                .keys()
                .next()
                .expect("closing one of multiple workspaces leaves a workspace");
        }
        self.bump_layout_revision();
        Ok(())
    }

    pub fn new_tab(&mut self, name: impl Into<String>, session: SessionSpec) -> MuxResult<TabId> {
        let tab_id = self.counters.next_tab();
        let pane_id = self.counters.next_pane();
        let session_id = self.counters.next_session();
        let tab = Tab::single_pane(
            tab_id,
            name,
            Pane::new(pane_id, session_id),
            Session::new(session_id, session),
        );
        let window = self.active_workspace_mut().active_window_mut();
        window.active_tab = tab_id;
        window.tabs.push(tab);
        self.bump_layout_revision();
        Ok(tab_id)
    }

    pub fn close_tab(&mut self, tab_id: TabId) -> MuxResult<()> {
        let window = self.active_workspace_mut().active_window_mut();
        if window.tabs.len() == 1 {
            return Err(MuxError::CannotCloseLastTab);
        }

        let index = window
            .tabs
            .iter()
            .position(|tab| tab.id == tab_id)
            .ok_or(MuxError::TabNotFound(tab_id))?;
        window.tabs.remove(index);

        if window.active_tab == tab_id {
            let next_index = index.saturating_sub(1).min(window.tabs.len() - 1);
            window.active_tab = window.tabs[next_index].id;
        }
        self.bump_layout_revision();
        Ok(())
    }

    pub fn rename_tab(&mut self, tab_id: TabId, name: impl Into<String>) -> MuxResult<()> {
        let tab = self
            .active_workspace_mut()
            .active_window_mut()
            .tab_mut(tab_id)?;
        tab.name = name.into();
        tab.title_source = TabTitleSource::User;
        Ok(())
    }

    pub fn switch_tab(&mut self, tab_id: TabId) -> MuxResult<()> {
        let window = self.active_workspace_mut().active_window_mut();
        if !window.tabs.iter().any(|tab| tab.id == tab_id) {
            return Err(MuxError::TabNotFound(tab_id));
        }
        if window.active_tab != tab_id {
            window.active_tab = tab_id;
            self.bump_layout_revision();
        }
        Ok(())
    }

    pub fn move_tab(&mut self, tab_id: TabId, target_index: usize) -> MuxResult<()> {
        let window = self.active_workspace_mut().active_window_mut();
        let index = window
            .tabs
            .iter()
            .position(|tab| tab.id == tab_id)
            .ok_or(MuxError::TabNotFound(tab_id))?;
        let tab = window.tabs.remove(index);
        window.tabs.insert(target_index.min(window.tabs.len()), tab);
        self.bump_layout_revision();
        Ok(())
    }

    pub fn split_active_pane(
        &mut self,
        axis: SplitAxis,
        session: SessionSpec,
    ) -> MuxResult<PaneId> {
        let session_id = self.counters.next_session();
        let pane_id = self.counters.next_pane();
        let tab = self.active_tab_mut();
        let active_pane = tab.active_pane;
        let pane = Pane::new(pane_id, session_id);

        tab.sessions
            .insert(session_id, Session::new(session_id, session));
        tab.panes.insert(pane_id, pane);
        tab.root.split_leaf(active_pane, pane_id, axis)?;
        tab.active_pane = pane_id;
        self.bump_layout_revision();
        Ok(pane_id)
    }

    pub fn close_pane(&mut self, pane_id: PaneId) -> MuxResult<()> {
        self.active_tab_mut().close_pane(pane_id)?;
        self.bump_layout_revision();
        Ok(())
    }

    /// Removes a cleanly exited pane while preserving the mux invariant that
    /// every retained window has at least one workspace, tab, and pane.
    /// The final session is left in place so the application can perform its
    /// own bounded shutdown before closing the native window.
    pub fn close_exited_pane(&mut self, pane_id: PaneId) -> MuxResult<PaneExitDisposition> {
        let location = self
            .workspaces
            .iter()
            .find_map(|(workspace_id, workspace)| {
                workspace.windows.iter().find_map(|window| {
                    window.tabs.iter().find_map(|tab| {
                        tab.panes.contains_key(&pane_id).then_some((
                            *workspace_id,
                            window.id,
                            tab.id,
                            tab.panes.len(),
                            window.tabs.len(),
                            workspace.windows.len(),
                        ))
                    })
                })
            })
            .ok_or(MuxError::PaneNotFound(pane_id))?;
        let (workspace_id, window_id, tab_id, pane_count, tab_count, window_count) = location;

        if pane_count > 1 {
            let workspace = self
                .workspaces
                .get_mut(&workspace_id)
                .ok_or(MuxError::WorkspaceNotFound(workspace_id))?;
            let window = workspace
                .windows
                .iter_mut()
                .find(|window| window.id == window_id)
                .ok_or(MuxError::WindowNotFound(window_id))?;
            window.tab_mut(tab_id)?.close_pane(pane_id)?;
            self.bump_layout_revision();
            return Ok(PaneExitDisposition::PaneClosed);
        }

        if tab_count > 1 {
            let workspace = self
                .workspaces
                .get_mut(&workspace_id)
                .ok_or(MuxError::WorkspaceNotFound(workspace_id))?;
            let window = workspace
                .windows
                .iter_mut()
                .find(|window| window.id == window_id)
                .ok_or(MuxError::WindowNotFound(window_id))?;
            let index = window
                .tabs
                .iter()
                .position(|tab| tab.id == tab_id)
                .ok_or(MuxError::TabNotFound(tab_id))?;
            window.tabs.remove(index);
            if window.active_tab == tab_id {
                window.active_tab =
                    window.tabs[index.saturating_sub(1).min(window.tabs.len() - 1)].id;
            }
            self.bump_layout_revision();
            return Ok(PaneExitDisposition::TabClosed);
        }

        if window_count > 1 {
            let workspace = self
                .workspaces
                .get_mut(&workspace_id)
                .ok_or(MuxError::WorkspaceNotFound(workspace_id))?;
            let index = workspace
                .windows
                .iter()
                .position(|window| window.id == window_id)
                .ok_or(MuxError::WindowNotFound(window_id))?;
            workspace.windows.remove(index);
            if workspace.active_window == window_id {
                workspace.active_window =
                    workspace.windows[index.saturating_sub(1).min(workspace.windows.len() - 1)].id;
            }
            self.bump_layout_revision();
            return Ok(PaneExitDisposition::WindowClosed);
        }

        if self.workspaces.len() > 1 {
            self.workspaces
                .remove(&workspace_id)
                .ok_or(MuxError::WorkspaceNotFound(workspace_id))?;
            if self.active_workspace == workspace_id {
                self.active_workspace = *self
                    .workspaces
                    .keys()
                    .next()
                    .expect("closing one of multiple workspaces leaves a workspace");
            }
            self.bump_layout_revision();
            return Ok(PaneExitDisposition::WorkspaceClosed);
        }

        Ok(PaneExitDisposition::ExitApplication)
    }

    pub fn focus_pane(&mut self, pane_id: PaneId) -> MuxResult<()> {
        let tab = self.active_tab_mut();
        if !tab.panes.contains_key(&pane_id) {
            return Err(MuxError::PaneNotFound(pane_id));
        }
        tab.active_pane = pane_id;
        Ok(())
    }

    pub fn focus_direction(&mut self, direction: FocusDirection) -> MuxResult<PaneId> {
        self.active_tab_mut().focus_direction(direction)
    }

    pub fn resize_active_pane(&mut self, direction: ResizeDirection, delta: f32) -> MuxResult<()> {
        self.active_tab_mut().resize_active_pane(direction, delta)?;
        self.bump_layout_revision();
        Ok(())
    }

    pub fn toggle_zoom_active_pane(&mut self) -> PaneId {
        let tab = self.active_tab_mut();
        let pane_id = tab.active_pane;
        tab.zoomed_pane = if tab.zoomed_pane == Some(pane_id) {
            None
        } else {
            Some(pane_id)
        };
        self.bump_layout_revision();
        pane_id
    }

    pub fn swap_panes(&mut self, first: PaneId, second: PaneId) -> MuxResult<()> {
        self.active_tab_mut().root.swap_panes(first, second)?;
        self.bump_layout_revision();
        Ok(())
    }

    pub fn move_active_pane(&mut self, direction: FocusDirection) -> MuxResult<PaneId> {
        let tab = self.active_tab_mut();
        let active = tab.active_pane;
        let target = tab.pane_in_direction(direction)?;
        tab.root.swap_panes(active, target)?;
        self.bump_layout_revision();
        Ok(target)
    }

    pub fn from_restore_snapshot(
        snapshot: &RestoreSnapshot,
        fallback_session: SessionSpec,
    ) -> MuxResult<Self> {
        let mut counters = IdCounters::default();
        let mut workspaces = BTreeMap::new();

        for workspace_restore in &snapshot.workspaces {
            if workspace_restore.windows.is_empty() {
                return Err(MuxError::InvalidSnapshot(
                    "workspace contains no windows".to_owned(),
                ));
            }
            let workspace_id = counters.next_workspace();
            let mut windows = Vec::new();
            for window_restore in &workspace_restore.windows {
                if window_restore.tabs.is_empty() {
                    return Err(MuxError::InvalidSnapshot(
                        "window contains no tabs".to_owned(),
                    ));
                }
                let window_id = counters.next_window();
                let mut tabs = Vec::new();
                for tab_restore in &window_restore.tabs {
                    tabs.push(restore_tab(tab_restore, &mut counters, &fallback_session)?);
                }
                let active_tab = window_restore
                    .active_tab_name
                    .as_ref()
                    .and_then(|name| tabs.iter().find(|tab| &tab.name == name))
                    .map_or(tabs[0].id, |tab| tab.id);
                windows.push(WindowModel {
                    id: window_id,
                    tabs,
                    active_tab,
                });
            }
            let active_window = windows[0].id;
            workspaces.insert(
                workspace_id,
                Workspace {
                    id: workspace_id,
                    name: workspace_restore.name.clone(),
                    windows,
                    active_window,
                },
            );
        }

        if workspaces.is_empty() {
            return Ok(Self::new(fallback_session));
        }
        let active_workspace = *workspaces
            .keys()
            .next()
            .expect("non-empty restored workspace map");
        Ok(Self {
            workspaces,
            active_workspace,
            counters,
            layout_revision: initial_layout_revision(),
        })
    }

    #[must_use]
    pub fn restore_snapshot(&self) -> RestoreSnapshot {
        RestoreSnapshot {
            workspaces: self
                .workspaces
                .values()
                .map(WorkspaceRestore::from_workspace)
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub name: String,
    pub windows: Vec<WindowModel>,
    pub active_window: WindowId,
}

impl Workspace {
    #[must_use]
    pub fn active_window(&self) -> &WindowModel {
        self.windows
            .iter()
            .find(|window| window.id == self.active_window)
            .expect("active window must exist")
    }

    pub fn active_window_mut(&mut self) -> &mut WindowModel {
        self.windows
            .iter_mut()
            .find(|window| window.id == self.active_window)
            .expect("active window must exist")
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowModel {
    pub id: WindowId,
    pub tabs: Vec<Tab>,
    pub active_tab: TabId,
}

impl WindowModel {
    #[must_use]
    pub fn active_tab(&self) -> &Tab {
        self.tabs
            .iter()
            .find(|tab| tab.id == self.active_tab)
            .expect("active tab must exist")
    }

    pub fn active_tab_mut(&mut self) -> &mut Tab {
        self.tabs
            .iter_mut()
            .find(|tab| tab.id == self.active_tab)
            .expect("active tab must exist")
    }

    pub fn tab_mut(&mut self, id: TabId) -> MuxResult<&mut Tab> {
        self.tabs
            .iter_mut()
            .find(|tab| tab.id == id)
            .ok_or(MuxError::TabNotFound(id))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tab {
    pub id: TabId,
    pub name: String,
    pub title_source: TabTitleSource,
    pub panes: BTreeMap<PaneId, Pane>,
    pub sessions: BTreeMap<SessionId, Session>,
    pub root: SplitTree,
    pub active_pane: PaneId,
    pub zoomed_pane: Option<PaneId>,
}

impl Tab {
    #[must_use]
    pub fn single_pane(id: TabId, name: impl Into<String>, pane: Pane, session: Session) -> Self {
        Self {
            id,
            name: name.into(),
            title_source: TabTitleSource::Default,
            root: SplitTree::Pane(pane.id),
            active_pane: pane.id,
            panes: BTreeMap::from([(pane.id, pane)]),
            sessions: BTreeMap::from([(session.id, session)]),
            zoomed_pane: None,
        }
    }

    #[must_use]
    pub fn active_pane(&self) -> &Pane {
        self.panes
            .get(&self.active_pane)
            .expect("active pane must exist")
    }

    pub fn close_pane(&mut self, pane_id: PaneId) -> MuxResult<()> {
        if self.panes.len() == 1 {
            return Err(MuxError::CannotCloseLastPane);
        }
        let pane = self
            .panes
            .remove(&pane_id)
            .ok_or(MuxError::PaneNotFound(pane_id))?;
        self.sessions.remove(&pane.session_id);
        self.root.remove_pane(pane_id)?;
        self.root.normalize();

        if self.active_pane == pane_id {
            self.active_pane = self
                .root
                .first_pane()
                .expect("closing a pane from a multi-pane tab leaves at least one pane");
        }
        if self.zoomed_pane == Some(pane_id) {
            self.zoomed_pane = None;
        }
        Ok(())
    }

    pub fn focus_direction(&mut self, direction: FocusDirection) -> MuxResult<PaneId> {
        let pane_id = self.pane_in_direction(direction)?;
        self.active_pane = pane_id;
        Ok(pane_id)
    }

    fn pane_in_direction(&self, direction: FocusDirection) -> MuxResult<PaneId> {
        // Navigation needs enough integral cells for every split to retain a
        // distinct centre. A unit rectangle collapses same-axis children once
        // integer layout rounding is applied.
        let assignments = self.layout(LogicalRect::new(0.0, 0.0, 10_000.0, 10_000.0));
        let active = assignments
            .iter()
            .find(|assignment| assignment.pane_id == self.active_pane)
            .ok_or(MuxError::PaneNotFound(self.active_pane))?;
        let active_center = active.rect.center();

        let mut candidates = assignments
            .iter()
            .filter(|candidate| candidate.pane_id != self.active_pane)
            .filter_map(|candidate| {
                let center = candidate.rect.center();
                let primary = match direction {
                    FocusDirection::Left if center.x < active_center.x => {
                        active_center.x - center.x
                    }
                    FocusDirection::Right if center.x > active_center.x => {
                        center.x - active_center.x
                    }
                    FocusDirection::Up if center.y < active_center.y => active_center.y - center.y,
                    FocusDirection::Down if center.y > active_center.y => {
                        center.y - active_center.y
                    }
                    _ => return None,
                };
                let secondary = match direction {
                    FocusDirection::Left | FocusDirection::Right => {
                        (active_center.y - center.y).abs()
                    }
                    FocusDirection::Up | FocusDirection::Down => (active_center.x - center.x).abs(),
                };
                Some((candidate.pane_id, primary, secondary))
            })
            .collect::<Vec<_>>();

        candidates.sort_by(|a, b| {
            a.1.total_cmp(&b.1)
                .then_with(|| a.2.total_cmp(&b.2))
                .then_with(|| a.0.cmp(&b.0))
        });

        let pane_id = candidates
            .first()
            .map(|candidate| candidate.0)
            .ok_or(MuxError::NoPaneInDirection(direction))?;
        Ok(pane_id)
    }

    pub fn resize_active_pane(&mut self, direction: ResizeDirection, delta: f32) -> MuxResult<()> {
        self.root
            .resize_pane(self.active_pane, direction.axis(), direction.sign() * delta)?;
        self.root.normalize();
        Ok(())
    }

    #[must_use]
    pub fn layout(&self, area: LogicalRect) -> Vec<PaneLayout> {
        if let Some(pane_id) = self.zoomed_pane {
            return vec![PaneLayout {
                pane_id,
                rect: area,
                terminal_size: TerminalGridSize::from_rect(area),
            }];
        }

        let mut assignments = Vec::new();
        self.root.assign_layout(area, &mut assignments);
        assignments
    }

    pub fn update_title_from_session(
        &mut self,
        pane_id: PaneId,
        title: impl Into<String>,
    ) -> MuxResult<()> {
        if !self.panes.contains_key(&pane_id) {
            return Err(MuxError::PaneNotFound(pane_id));
        }
        if !matches!(self.title_source, TabTitleSource::User) {
            self.name = title.into();
            self.title_source = TabTitleSource::Session;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TabTitleSource {
    Default,
    User,
    Session,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pane {
    pub id: PaneId,
    pub session_id: SessionId,
    pub title: Option<String>,
    pub last_size: Option<TerminalGridSize>,
}

impl Pane {
    #[must_use]
    pub const fn new(id: PaneId, session_id: SessionId) -> Self {
        Self {
            id,
            session_id,
            title: None,
            last_size: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub spec: SessionSpec,
    pub status: SessionStatus,
    pub title: Option<String>,
    pub current_working_directory: Option<String>,
}

impl Session {
    #[must_use]
    pub fn new(id: SessionId, spec: SessionSpec) -> Self {
        Self {
            id,
            spec,
            status: SessionStatus::Pending,
            title: None,
            current_working_directory: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Pending,
    Running,
    Exited { exit_code: Option<i32> },
    Failed { message: String },
    Detached,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneExitDisposition {
    PaneClosed,
    TabClosed,
    WindowClosed,
    WorkspaceClosed,
    ExitApplication,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSpec {
    pub profile_name: String,
    pub transport: SessionTransportKind,
    pub working_directory: Option<String>,
    pub startup_command: Option<String>,
}

impl SessionSpec {
    #[must_use]
    pub fn local(profile_name: impl Into<String>) -> Self {
        Self {
            profile_name: profile_name.into(),
            transport: if cfg!(windows) {
                SessionTransportKind::WindowsPseudoconsole
            } else {
                SessionTransportKind::LocalPty
            },
            working_directory: None,
            startup_command: None,
        }
    }

    #[must_use]
    pub fn ssh(profile_name: impl Into<String>) -> Self {
        Self {
            profile_name: profile_name.into(),
            transport: SessionTransportKind::Ssh,
            working_directory: None,
            startup_command: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionTransportKind {
    LocalPty,
    WindowsPseudoconsole,
    Ssh,
    FutureMobileSsh,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SplitTree {
    Pane(PaneId),
    Split {
        axis: SplitAxis,
        children: Vec<SplitTree>,
        ratios: Vec<f32>,
    },
}

impl SplitTree {
    pub fn split_leaf(
        &mut self,
        target: PaneId,
        new_pane: PaneId,
        axis: SplitAxis,
    ) -> MuxResult<()> {
        match self {
            Self::Pane(id) if *id == target => {
                *self = Self::Split {
                    axis,
                    children: vec![Self::Pane(target), Self::Pane(new_pane)],
                    ratios: vec![0.5, 0.5],
                };
                Ok(())
            }
            Self::Pane(_) => Err(MuxError::PaneNotFound(target)),
            Self::Split {
                axis: existing_axis,
                children,
                ratios,
            } => {
                for child in children.iter_mut() {
                    if child.split_leaf(target, new_pane, axis).is_ok() {
                        if *existing_axis == axis {
                            child.flatten_same_axis(axis);
                            Self::renormalize_ratios(ratios);
                        }
                        return Ok(());
                    }
                }
                Err(MuxError::PaneNotFound(target))
            }
        }
    }

    pub fn remove_pane(&mut self, pane_id: PaneId) -> MuxResult<()> {
        if matches!(self, Self::Pane(id) if *id == pane_id) {
            return Err(MuxError::CannotRemoveRootPane);
        }

        match self {
            Self::Pane(_) => Err(MuxError::PaneNotFound(pane_id)),
            Self::Split {
                children, ratios, ..
            } => {
                let mut removed = false;
                let mut index = 0;
                while index < children.len() {
                    if matches!(children[index], Self::Pane(id) if id == pane_id) {
                        children.remove(index);
                        ratios.remove(index);
                        removed = true;
                    } else {
                        if children[index].remove_pane(pane_id).is_ok() {
                            removed = true;
                        }
                        index += 1;
                    }
                }

                if !removed {
                    return Err(MuxError::PaneNotFound(pane_id));
                }
                Self::renormalize_ratios(ratios);
                Ok(())
            }
        }
    }

    pub fn swap_panes(&mut self, first: PaneId, second: PaneId) -> MuxResult<()> {
        let mut found_first = false;
        let mut found_second = false;
        self.replace_panes(first, second, &mut found_first, &mut found_second);

        if found_first && found_second {
            Ok(())
        } else if !found_first {
            Err(MuxError::PaneNotFound(first))
        } else {
            Err(MuxError::PaneNotFound(second))
        }
    }

    pub fn normalize(&mut self) {
        match self {
            Self::Pane(_) => {}
            Self::Split {
                axis,
                children,
                ratios,
            } => {
                for child in children.iter_mut() {
                    child.normalize();
                }

                let mut flattened = Vec::new();
                let mut flattened_ratios = Vec::new();
                for (child, ratio) in children.drain(..).zip(ratios.drain(..)) {
                    match child {
                        Self::Split {
                            axis: child_axis,
                            children: grand_children,
                            ratios: grand_ratios,
                        } if child_axis == *axis => {
                            for (grand_child, grand_ratio) in
                                grand_children.into_iter().zip(grand_ratios)
                            {
                                flattened.push(grand_child);
                                flattened_ratios.push(ratio * grand_ratio);
                            }
                        }
                        other => {
                            flattened.push(other);
                            flattened_ratios.push(ratio);
                        }
                    }
                }

                if flattened.len() == 1 {
                    *self = flattened.remove(0);
                } else {
                    *children = flattened;
                    *ratios = flattened_ratios;
                    Self::renormalize_ratios(ratios);
                }
            }
        }
    }

    #[must_use]
    pub fn first_pane(&self) -> Option<PaneId> {
        match self {
            Self::Pane(id) => Some(*id),
            Self::Split { children, .. } => children.iter().find_map(Self::first_pane),
        }
    }

    fn collect_panes(&self, output: &mut Vec<PaneId>) {
        match self {
            Self::Pane(pane_id) => output.push(*pane_id),
            Self::Split { children, .. } => {
                for child in children {
                    child.collect_panes(output);
                }
            }
        }
    }

    fn resize_pane(&mut self, target: PaneId, axis: SplitAxis, delta: f32) -> MuxResult<()> {
        match self {
            Self::Pane(_) => Err(MuxError::PaneNotFound(target)),
            Self::Split {
                axis: split_axis,
                children,
                ratios,
            } => {
                if let Some(index) = children
                    .iter()
                    .position(|child| child.contains_pane(target))
                    && *split_axis == axis
                    && ratios.len() > 1
                {
                    let neighbor = if delta >= 0.0 {
                        (index + 1 < ratios.len()).then_some(index + 1)
                    } else {
                        index.checked_sub(1)
                    };

                    if let Some(neighbor) = neighbor {
                        let amount = delta.abs().clamp(0.01, 0.25);
                        let available = ratios[neighbor] - 0.05;
                        let applied = amount.min(available.max(0.0));
                        ratios[index] += applied;
                        ratios[neighbor] -= applied;
                        Self::renormalize_ratios(ratios);
                        return Ok(());
                    }
                }

                for child in children {
                    if child.resize_pane(target, axis, delta).is_ok() {
                        return Ok(());
                    }
                }
                Err(MuxError::PaneNotFound(target))
            }
        }
    }

    fn assign_layout(&self, area: LogicalRect, output: &mut Vec<PaneLayout>) {
        match self {
            Self::Pane(pane_id) => output.push(PaneLayout {
                pane_id: *pane_id,
                rect: area,
                terminal_size: TerminalGridSize::from_rect(area),
            }),
            Self::Split {
                axis,
                children,
                ratios,
            } => {
                let extent = match axis {
                    SplitAxis::Horizontal => area.width,
                    SplitAxis::Vertical => area.height,
                }
                .floor()
                .max(0.0) as u32;
                let reserve_one_cell = extent >= children.len() as u32;
                let mut consumed = 0u32;
                let mut cumulative_ratio = 0.0f32;
                for (index, child) in children.iter().enumerate() {
                    let ratio = ratios.get(index).copied().unwrap_or_default();
                    cumulative_ratio += ratio;
                    let remaining = children.len().saturating_sub(index + 1) as u32;
                    let end = if remaining == 0 {
                        extent
                    } else {
                        let desired =
                            (extent as f32 * cumulative_ratio.clamp(0.0, 1.0)).round() as u32;
                        let minimum = if reserve_one_cell {
                            consumed.saturating_add(1)
                        } else {
                            consumed
                        };
                        let maximum = if reserve_one_cell {
                            extent.saturating_sub(remaining)
                        } else {
                            extent
                        };
                        desired.clamp(minimum.min(maximum), maximum)
                    };
                    let length = end.saturating_sub(consumed) as f32;
                    let offset = consumed as f32;
                    let rect = match axis {
                        SplitAxis::Horizontal => LogicalRect {
                            x: area.x + offset,
                            y: area.y,
                            width: length,
                            height: area.height,
                        },
                        SplitAxis::Vertical => LogicalRect {
                            x: area.x,
                            y: area.y + offset,
                            width: area.width,
                            height: length,
                        },
                    };
                    consumed = end;
                    child.assign_layout(rect, output);
                }
            }
        }
    }

    fn flatten_same_axis(&mut self, axis_to_flatten: SplitAxis) {
        let Self::Split {
            axis,
            children,
            ratios,
        } = self
        else {
            return;
        };

        if *axis != axis_to_flatten {
            return;
        }

        let mut flattened = Vec::new();
        let mut flattened_ratios = Vec::new();
        for (child, ratio) in children.drain(..).zip(ratios.drain(..)) {
            match child {
                Self::Split {
                    axis: child_axis,
                    children: grand_children,
                    ratios: grand_ratios,
                } if child_axis == axis_to_flatten => {
                    for (grand_child, grand_ratio) in grand_children.into_iter().zip(grand_ratios) {
                        flattened.push(grand_child);
                        flattened_ratios.push(ratio * grand_ratio);
                    }
                }
                other => {
                    flattened.push(other);
                    flattened_ratios.push(ratio);
                }
            }
        }
        *children = flattened;
        *ratios = flattened_ratios;
        Self::renormalize_ratios(ratios);
    }

    fn replace_panes(
        &mut self,
        first: PaneId,
        second: PaneId,
        found_first: &mut bool,
        found_second: &mut bool,
    ) {
        match self {
            Self::Pane(id) if *id == first => {
                *id = second;
                *found_first = true;
            }
            Self::Pane(id) if *id == second => {
                *id = first;
                *found_second = true;
            }
            Self::Pane(_) => {}
            Self::Split { children, .. } => {
                for child in children {
                    child.replace_panes(first, second, found_first, found_second);
                }
            }
        }
    }

    fn contains_pane(&self, pane_id: PaneId) -> bool {
        match self {
            Self::Pane(id) => *id == pane_id,
            Self::Split { children, .. } => {
                children.iter().any(|child| child.contains_pane(pane_id))
            }
        }
    }

    fn renormalize_ratios(ratios: &mut [f32]) {
        if ratios.is_empty() {
            return;
        }
        let total: f32 = ratios.iter().sum();
        if total <= f32::EPSILON {
            let equal = 1.0 / ratios.len() as f32;
            ratios.fill(equal);
            return;
        }
        for ratio in ratios {
            *ratio = (*ratio / total).clamp(0.05, 0.95);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitAxis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FocusDirection {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResizeDirection {
    Left,
    Right,
    Up,
    Down,
}

impl ResizeDirection {
    const fn axis(self) -> SplitAxis {
        match self {
            Self::Left | Self::Right => SplitAxis::Horizontal,
            Self::Up | Self::Down => SplitAxis::Vertical,
        }
    }

    const fn sign(self) -> f32 {
        match self {
            Self::Left | Self::Up => -1.0,
            Self::Right | Self::Down => 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LogicalRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl LogicalRect {
    #[must_use]
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    #[must_use]
    pub const fn unit() -> Self {
        Self::new(0.0, 0.0, 1.0, 1.0)
    }

    fn center(self) -> Point {
        Point {
            x: self.x + self.width / 2.0,
            y: self.y + self.height / 2.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Point {
    x: f32,
    y: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalGridSize {
    pub cols: u16,
    pub rows: u16,
}

impl TerminalGridSize {
    #[must_use]
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols: if cols == 0 { 1 } else { cols },
            rows: if rows == 0 { 1 } else { rows },
        }
    }

    fn from_rect(rect: LogicalRect) -> Self {
        Self::new(rect.width.floor() as u16, rect.height.floor() as u16)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PaneLayout {
    pub pane_id: PaneId,
    pub rect: LogicalRect,
    pub terminal_size: TerminalGridSize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MuxAction {
    NewWorkspace,
    CloseWorkspace,
    NextWorkspace,
    PreviousWorkspace,
    NewTab,
    CloseTab,
    NextTab,
    PreviousTab,
    RenameTab { name: String },
    MoveTab { target_index: usize },
    SplitHorizontal,
    SplitVertical,
    ClosePane,
    FocusDirection(FocusDirection),
    ResizePane(ResizeDirection),
    ZoomPane,
    MovePane(FocusDirection),
    SwapPaneDirection(FocusDirection),
    SwapPane { other: PaneId },
}

impl MuxAction {
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        match name {
            "new_workspace" => Some(Self::NewWorkspace),
            "close_workspace" => Some(Self::CloseWorkspace),
            "next_workspace" => Some(Self::NextWorkspace),
            "previous_workspace" => Some(Self::PreviousWorkspace),
            "new_tab" => Some(Self::NewTab),
            "close_tab" => Some(Self::CloseTab),
            "next_tab" => Some(Self::NextTab),
            "previous_tab" => Some(Self::PreviousTab),
            "split_horizontal" => Some(Self::SplitHorizontal),
            "split_vertical" => Some(Self::SplitVertical),
            "close_pane" => Some(Self::ClosePane),
            "focus_left" => Some(Self::FocusDirection(FocusDirection::Left)),
            "focus_right" => Some(Self::FocusDirection(FocusDirection::Right)),
            "focus_up" => Some(Self::FocusDirection(FocusDirection::Up)),
            "focus_down" => Some(Self::FocusDirection(FocusDirection::Down)),
            "resize_pane_left" => Some(Self::ResizePane(ResizeDirection::Left)),
            "resize_pane_right" => Some(Self::ResizePane(ResizeDirection::Right)),
            "resize_pane_up" => Some(Self::ResizePane(ResizeDirection::Up)),
            "resize_pane_down" => Some(Self::ResizePane(ResizeDirection::Down)),
            "zoom_pane" => Some(Self::ZoomPane),
            "rename_tab" => Some(Self::RenameTab {
                name: String::new(),
            }),
            "move_pane_left" => Some(Self::MovePane(FocusDirection::Left)),
            "move_pane_right" => Some(Self::MovePane(FocusDirection::Right)),
            "move_pane_up" => Some(Self::MovePane(FocusDirection::Up)),
            "move_pane_down" => Some(Self::MovePane(FocusDirection::Down)),
            "move_pane" => Some(Self::MovePane(FocusDirection::Right)),
            "swap_pane_left" => Some(Self::SwapPaneDirection(FocusDirection::Left)),
            "swap_pane_right" => Some(Self::SwapPaneDirection(FocusDirection::Right)),
            "swap_pane_up" => Some(Self::SwapPaneDirection(FocusDirection::Up)),
            "swap_pane_down" => Some(Self::SwapPaneDirection(FocusDirection::Down)),
            _ => None,
        }
    }
}

fn restore_tab(
    restore: &TabRestore,
    counters: &mut IdCounters,
    fallback_session: &SessionSpec,
) -> MuxResult<Tab> {
    if restore.panes.is_empty() {
        return Err(MuxError::InvalidSnapshot(
            "tab contains no panes".to_owned(),
        ));
    }
    let tab_id = counters.next_tab();
    let mut pane_ids = BTreeMap::new();
    let mut panes = BTreeMap::new();
    let mut sessions = BTreeMap::new();
    for pane_restore in &restore.panes {
        if pane_restore.session_profile.trim().is_empty() {
            return Err(MuxError::InvalidSnapshot(
                "pane session profile is empty".to_owned(),
            ));
        }
        let pane_id = counters.next_pane();
        let session_id = counters.next_session();
        if pane_ids.insert(pane_restore.pane_id, pane_id).is_some() {
            return Err(MuxError::InvalidSnapshot(
                "tab contains duplicate pane identifiers".to_owned(),
            ));
        }
        let mut spec = fallback_session.clone();
        spec.profile_name = pane_restore.session_profile.clone();
        spec.transport = pane_restore.transport;
        spec.working_directory = pane_restore.working_directory.clone();
        panes.insert(pane_id, Pane::new(pane_id, session_id));
        sessions.insert(session_id, Session::new(session_id, spec));
    }
    let root = remap_split_tree(&restore.layout, &pane_ids)?;
    let mut leaves = Vec::new();
    root.collect_panes(&mut leaves);
    let unique_leaves = leaves.iter().copied().collect::<BTreeSet<_>>();
    let restored_panes = panes.keys().copied().collect::<BTreeSet<_>>();
    if unique_leaves.len() != leaves.len() || unique_leaves != restored_panes {
        return Err(MuxError::InvalidSnapshot(
            "layout must reference every pane exactly once".to_owned(),
        ));
    }
    let active_pane = pane_ids
        .get(&restore.active_pane)
        .copied()
        .or_else(|| root.first_pane())
        .ok_or_else(|| MuxError::InvalidSnapshot("tab layout has no panes".to_owned()))?;
    Ok(Tab {
        id: tab_id,
        name: restore.name.clone(),
        title_source: TabTitleSource::Default,
        panes,
        sessions,
        root,
        active_pane,
        zoomed_pane: None,
    })
}

fn remap_split_tree(tree: &SplitTree, pane_ids: &BTreeMap<PaneId, PaneId>) -> MuxResult<SplitTree> {
    match tree {
        SplitTree::Pane(old) => pane_ids
            .get(old)
            .copied()
            .map(SplitTree::Pane)
            .ok_or_else(|| {
                MuxError::InvalidSnapshot(format!("layout references missing pane {old:?}"))
            }),
        SplitTree::Split {
            axis,
            children,
            ratios,
        } => {
            if children.len() < 2
                || children.len() != ratios.len()
                || ratios
                    .iter()
                    .any(|ratio| !ratio.is_finite() || *ratio <= 0.0)
            {
                return Err(MuxError::InvalidSnapshot(
                    "split children and ratios are inconsistent".to_owned(),
                ));
            }
            let children = children
                .iter()
                .map(|child| remap_split_tree(child, pane_ids))
                .collect::<MuxResult<Vec<_>>>()?;
            let mut restored = SplitTree::Split {
                axis: *axis,
                children,
                ratios: ratios.clone(),
            };
            restored.normalize();
            Ok(restored)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RestoreSnapshot {
    pub workspaces: Vec<WorkspaceRestore>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceRestore {
    pub name: String,
    pub windows: Vec<WindowRestore>,
}

impl WorkspaceRestore {
    fn from_workspace(workspace: &Workspace) -> Self {
        Self {
            name: workspace.name.clone(),
            windows: workspace
                .windows
                .iter()
                .map(WindowRestore::from_window)
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowRestore {
    pub tabs: Vec<TabRestore>,
    pub active_tab_name: Option<String>,
}

impl WindowRestore {
    fn from_window(window: &WindowModel) -> Self {
        Self {
            tabs: window.tabs.iter().map(TabRestore::from_tab).collect(),
            active_tab_name: window
                .tabs
                .iter()
                .find(|tab| tab.id == window.active_tab)
                .map(|tab| tab.name.clone()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TabRestore {
    pub name: String,
    pub layout: SplitTree,
    pub active_pane: PaneId,
    pub panes: Vec<PaneRestore>,
}

impl TabRestore {
    fn from_tab(tab: &Tab) -> Self {
        Self {
            name: tab.name.clone(),
            layout: tab.root.clone(),
            active_pane: tab.active_pane,
            panes: tab
                .panes
                .values()
                .filter_map(|pane| {
                    tab.sessions
                        .get(&pane.session_id)
                        .map(|session| PaneRestore {
                            pane_id: pane.id,
                            session_profile: session.spec.profile_name.clone(),
                            transport: session.spec.transport,
                            working_directory: session
                                .current_working_directory
                                .clone()
                                .or_else(|| session.spec.working_directory.clone()),
                        })
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneRestore {
    pub pane_id: PaneId,
    pub session_profile: String,
    pub transport: SessionTransportKind,
    pub working_directory: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct IdCounters {
    workspace: u64,
    window: u64,
    tab: u64,
    pane: u64,
    session: u64,
}

impl Default for IdCounters {
    fn default() -> Self {
        Self {
            workspace: 1,
            window: 1,
            tab: 1,
            pane: 1,
            session: 1,
        }
    }
}

impl IdCounters {
    fn next_workspace(&mut self) -> WorkspaceId {
        let id = WorkspaceId(self.workspace);
        self.workspace += 1;
        id
    }

    fn next_window(&mut self) -> WindowId {
        let id = WindowId(self.window);
        self.window += 1;
        id
    }

    fn next_tab(&mut self) -> TabId {
        let id = TabId(self.tab);
        self.tab += 1;
        id
    }

    fn next_pane(&mut self) -> PaneId {
        let id = PaneId(self.pane);
        self.pane += 1;
        id
    }

    fn next_session(&mut self) -> SessionId {
        let id = SessionId(self.session);
        self.session += 1;
        id
    }
}

pub type MuxResult<T> = Result<T, MuxError>;

#[derive(Debug, Clone, PartialEq)]
pub enum MuxError {
    WorkspaceNotFound(WorkspaceId),
    WindowNotFound(WindowId),
    TabNotFound(TabId),
    PaneNotFound(PaneId),
    SessionNotFound(SessionId),
    CannotCloseLastTab,
    CannotCloseLastWorkspace,
    CannotCloseLastPane,
    CannotRemoveRootPane,
    NoPaneInDirection(FocusDirection),
    InvalidSnapshot(String),
}

impl fmt::Display for MuxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WorkspaceNotFound(id) => write!(f, "workspace not found: {id:?}"),
            Self::WindowNotFound(id) => write!(f, "window not found: {id:?}"),
            Self::TabNotFound(id) => write!(f, "tab not found: {id:?}"),
            Self::PaneNotFound(id) => write!(f, "pane not found: {id:?}"),
            Self::SessionNotFound(id) => write!(f, "session not found: {id:?}"),
            Self::CannotCloseLastTab => f.write_str("cannot close the last tab"),
            Self::CannotCloseLastWorkspace => f.write_str("cannot close the last workspace"),
            Self::CannotCloseLastPane => f.write_str("cannot close the last pane"),
            Self::CannotRemoveRootPane => f.write_str("cannot remove the root pane directly"),
            Self::NoPaneInDirection(direction) => write!(f, "no pane in direction {direction:?}"),
            Self::InvalidSnapshot(message) => write!(f, "invalid mux snapshot: {message}"),
        }
    }
}

impl Error for MuxError {}

#[must_use]
pub fn external_mux_compatibility_policy() -> &'static str {
    "native mux wraps sessions; tmux, screen, and zellij remain ordinary terminal applications inside panes"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> MuxModel {
        MuxModel::new(SessionSpec::local("default"))
    }

    #[test]
    fn model_starts_with_workspace_tab_pane_and_session() {
        let model = model();
        let workspace = model.active_workspace();
        let tab = model.active_tab();

        assert_eq!(workspace.windows.len(), 1);
        assert_eq!(tab.panes.len(), 1);
        assert_eq!(tab.sessions.len(), 1);
        assert_eq!(tab.active_pane().session_id, SessionId(1));
    }

    #[test]
    fn tabs_can_be_created_renamed_moved_switched_and_closed() {
        let mut model = model();
        let first = model.active_tab().id;
        let second = model
            .new_tab("build", SessionSpec::local("default"))
            .expect("new tab");

        assert_eq!(model.active_tab().id, second);
        model.rename_tab(second, "server").expect("rename");
        assert_eq!(model.active_tab().name, "server");

        model.move_tab(second, 0).expect("move");
        assert_eq!(model.active_workspace().active_window().tabs[0].id, second);

        model.switch_tab(first).expect("switch");
        assert_eq!(model.active_tab().id, first);

        model.close_tab(first).expect("close");
        assert_eq!(model.active_tab().id, second);
    }

    #[test]
    fn active_pane_title_updates_tab_without_overriding_user_title() {
        let mut model = model();
        let pane = model.active_tab().active_pane;
        model.update_pane_title(pane, "vim").expect("title");
        assert_eq!(model.active_tab().name, "vim");
        model
            .rename_tab(model.active_tab().id, "editor")
            .expect("user title");
        model.update_pane_title(pane, "nvim").expect("title");
        assert_eq!(model.active_tab().name, "editor");
    }

    #[test]
    fn panes_split_focus_resize_zoom_swap_and_close() {
        let mut model = model();
        let first = model.active_tab().active_pane;
        let second = model
            .split_active_pane(SplitAxis::Horizontal, SessionSpec::local("default"))
            .expect("split");

        assert_eq!(model.active_tab().panes.len(), 2);
        assert_eq!(model.active_tab().active_pane, second);

        let focused = model
            .focus_direction(FocusDirection::Left)
            .expect("focus left");
        assert_eq!(focused, first);

        model
            .resize_active_pane(ResizeDirection::Right, 0.1)
            .expect("resize");
        let layout = model
            .active_tab()
            .layout(LogicalRect::new(0.0, 0.0, 100.0, 24.0));
        assert_eq!(layout.len(), 2);
        assert!(layout.iter().all(|pane| pane.terminal_size.rows == 24));

        assert_eq!(model.toggle_zoom_active_pane(), first);
        assert_eq!(
            model
                .active_tab()
                .layout(LogicalRect::new(0.0, 0.0, 100.0, 24.0))
                .len(),
            1
        );
        model.toggle_zoom_active_pane();

        model.swap_panes(first, second).expect("swap");
        model.close_pane(second).expect("close pane");
        assert_eq!(model.active_tab().panes.len(), 1);
    }

    #[test]
    fn active_pane_can_move_directionally_without_changing_identity() {
        let mut model = model();
        let first = model.active_tab().active_pane;
        let second = model
            .split_active_pane(SplitAxis::Horizontal, SessionSpec::local("default"))
            .expect("split");

        assert_eq!(model.move_active_pane(FocusDirection::Left), Ok(first));
        assert_eq!(model.active_tab().active_pane, second);
        assert_eq!(model.active_tab().root.first_pane(), Some(second));
    }

    #[test]
    fn directional_focus_distinguishes_three_same_axis_panes() {
        let mut model = model();
        let first = model.active_tab().active_pane;
        let second = model
            .split_active_pane(SplitAxis::Horizontal, SessionSpec::local("default"))
            .expect("second pane");
        let third = model
            .split_active_pane(SplitAxis::Horizontal, SessionSpec::local("default"))
            .expect("third pane");
        assert_eq!(model.active_tab().active_pane, third);

        assert_eq!(model.focus_direction(FocusDirection::Left), Ok(second));
        assert_eq!(model.focus_direction(FocusDirection::Left), Ok(first));
        assert_eq!(model.focus_direction(FocusDirection::Right), Ok(second));
        assert_eq!(model.focus_direction(FocusDirection::Right), Ok(third));
    }

    #[test]
    fn workspaces_can_be_created_switched_renamed_and_closed() {
        let mut model = model();
        let first = model.active_workspace;
        let second = model.new_workspace("remote", SessionSpec::ssh("prod"));

        assert_eq!(model.active_workspace, second);
        assert_eq!(model.active_tab().active_pane().session_id, SessionId(2));
        model.rename_workspace(second, "ops").expect("rename");
        assert_eq!(model.active_workspace().name, "ops");
        model.switch_workspace(first).expect("switch");
        model.close_workspace(second).expect("close");
        assert_eq!(model.workspaces.len(), 1);
        assert_eq!(
            model.close_workspace(first),
            Err(MuxError::CannotCloseLastWorkspace)
        );
    }

    #[test]
    fn nested_layouts_are_proportional_and_dpi_independent() {
        let mut model = model();
        let _right = model
            .split_active_pane(SplitAxis::Horizontal, SessionSpec::local("default"))
            .expect("horizontal split");
        let _bottom_right = model
            .split_active_pane(SplitAxis::Vertical, SessionSpec::local("default"))
            .expect("vertical split");

        let layout = model
            .active_tab()
            .layout(LogicalRect::new(0.0, 0.0, 120.0, 40.0));

        assert_eq!(layout.len(), 3);
        assert!(layout.iter().all(|assignment| assignment.rect.width > 0.0));
        assert!(layout.iter().all(|assignment| assignment.rect.height > 0.0));
    }

    #[test]
    fn odd_cell_extents_produce_integral_gapless_nested_layouts() {
        let mut model = model();
        let _right = model
            .split_active_pane(SplitAxis::Horizontal, SessionSpec::local("default"))
            .expect("horizontal split");
        let _bottom_right = model
            .split_active_pane(SplitAxis::Vertical, SessionSpec::local("default"))
            .expect("vertical split");

        let width = 121usize;
        let height = 41usize;
        let layout =
            model
                .active_tab()
                .layout(LogicalRect::new(0.0, 0.0, width as f32, height as f32));
        let mut coverage = vec![0u8; width * height];

        for assignment in layout {
            let rect = assignment.rect;
            assert_eq!(rect.x.fract(), 0.0, "pane x must align to a cell");
            assert_eq!(rect.y.fract(), 0.0, "pane y must align to a cell");
            assert_eq!(rect.width.fract(), 0.0, "pane width must use whole cells");
            assert_eq!(rect.height.fract(), 0.0, "pane height must use whole cells");
            assert_eq!(assignment.terminal_size.cols, rect.width as u16);
            assert_eq!(assignment.terminal_size.rows, rect.height as u16);

            for row in rect.y as usize..(rect.y + rect.height) as usize {
                for col in rect.x as usize..(rect.x + rect.width) as usize {
                    coverage[row * width + col] += 1;
                }
            }
        }

        assert!(
            coverage.iter().all(|count| *count == 1),
            "nested pane layouts must cover every terminal cell exactly once"
        );
    }

    #[test]
    fn restore_snapshot_persists_layout_but_not_process_resurrection() {
        let mut model = model();
        model
            .split_active_pane(SplitAxis::Horizontal, SessionSpec::local("default"))
            .expect("split");

        let snapshot = model.restore_snapshot();

        assert_eq!(snapshot.workspaces.len(), 1);
        assert_eq!(snapshot.workspaces[0].windows[0].tabs[0].panes.len(), 2);
        assert_eq!(
            snapshot.workspaces[0].windows[0].tabs[0].panes[0].session_profile,
            "default"
        );

        let restored = MuxModel::from_restore_snapshot(&snapshot, SessionSpec::local("fallback"))
            .expect("restore");
        assert_eq!(restored.active_tab().panes.len(), 2);
        assert_eq!(restored.active_tab().layout(LogicalRect::unit()).len(), 2);
        assert!(
            restored
                .active_tab()
                .sessions
                .values()
                .all(|session| session.spec.profile_name == "default")
        );
    }

    #[test]
    fn clean_exit_removes_only_the_exited_pane_from_a_split() {
        let mut model = model();
        let original = model.active_tab().active_pane;
        let exited = model
            .split_active_pane(SplitAxis::Horizontal, SessionSpec::local("default"))
            .expect("split pane");

        let disposition = model.close_exited_pane(exited).expect("close exited pane");

        assert_eq!(disposition, PaneExitDisposition::PaneClosed);
        assert_eq!(model.active_tab().panes.len(), 1);
        assert_eq!(model.active_tab().active_pane, original);
    }

    #[test]
    fn clean_exit_removes_a_single_pane_tab_when_another_tab_exists() {
        let mut model = model();
        let first_tab = model.active_tab().id;
        model
            .new_tab("2", SessionSpec::local("default"))
            .expect("new tab");
        let exited = model.active_tab().active_pane;

        let disposition = model.close_exited_pane(exited).expect("close exited tab");

        assert_eq!(disposition, PaneExitDisposition::TabClosed);
        assert_eq!(model.active_tab().id, first_tab);
        assert_eq!(model.active_workspace().active_window().tabs.len(), 1);
    }

    #[test]
    fn clean_exit_of_the_final_session_requests_application_exit() {
        let mut model = model();
        let exited = model.active_tab().active_pane;

        let disposition = model
            .close_exited_pane(exited)
            .expect("classify final exit");

        assert_eq!(disposition, PaneExitDisposition::ExitApplication);
        assert_eq!(model.active_tab().active_pane, exited);
    }

    #[test]
    fn invalid_restore_snapshot_is_rejected_without_panicking() {
        let snapshot = RestoreSnapshot {
            workspaces: vec![WorkspaceRestore {
                name: "broken".to_owned(),
                windows: Vec::new(),
            }],
        };

        assert!(matches!(
            MuxModel::from_restore_snapshot(&snapshot, SessionSpec::local("default")),
            Err(MuxError::InvalidSnapshot(_))
        ));

        let pane_id = PaneId(7);
        let duplicate = RestoreSnapshot {
            workspaces: vec![WorkspaceRestore {
                name: "broken".to_owned(),
                windows: vec![WindowRestore {
                    active_tab_name: Some("tab".to_owned()),
                    tabs: vec![TabRestore {
                        name: "tab".to_owned(),
                        layout: SplitTree::Split {
                            axis: SplitAxis::Horizontal,
                            children: vec![SplitTree::Pane(pane_id), SplitTree::Pane(pane_id)],
                            ratios: vec![0.5, 0.5],
                        },
                        active_pane: pane_id,
                        panes: vec![PaneRestore {
                            pane_id,
                            session_profile: "default".to_owned(),
                            transport: SessionTransportKind::LocalPty,
                            working_directory: None,
                        }],
                    }],
                }],
            }],
        };
        assert!(matches!(
            MuxModel::from_restore_snapshot(&duplicate, SessionSpec::local("default")),
            Err(MuxError::InvalidSnapshot(_))
        ));
    }

    #[test]
    fn ssh_sessions_are_first_class_mux_specs() {
        let spec = SessionSpec::ssh("prod");

        assert_eq!(spec.profile_name, "prod");
        assert_eq!(spec.transport, SessionTransportKind::Ssh);
    }

    #[test]
    fn action_names_cover_default_mux_keybindings() {
        for action in [
            "new_tab",
            "close_tab",
            "next_tab",
            "previous_tab",
            "split_horizontal",
            "split_vertical",
            "close_pane",
            "focus_left",
            "focus_right",
            "focus_up",
            "focus_down",
            "resize_pane_left",
            "resize_pane_right",
            "resize_pane_up",
            "resize_pane_down",
            "zoom_pane",
            "rename_tab",
            "move_pane_left",
            "move_pane_right",
            "move_pane_up",
            "move_pane_down",
        ] {
            assert!(MuxAction::named(action).is_some(), "{action}");
        }
    }

    #[test]
    fn native_mux_policy_keeps_external_multiplexers_inside_panes() {
        assert!(
            external_mux_compatibility_policy()
                .contains("ordinary terminal applications inside panes")
        );
    }

    #[test]
    fn layout_revision_changes_only_for_layout_affecting_mutations() {
        let mut model = model();
        let initial = model.layout_revision();
        let tab_id = model.active_tab().id;

        model.rename_tab(tab_id, "renamed").expect("rename tab");
        assert_eq!(model.layout_revision(), initial);

        model
            .split_active_pane(SplitAxis::Horizontal, SessionSpec::local("default"))
            .expect("split pane");
        let after_split = model.layout_revision();
        assert_ne!(after_split, initial);

        model.toggle_zoom_active_pane();
        assert_ne!(model.layout_revision(), after_split);
    }

    #[test]
    fn mux_does_not_import_platform_renderer_or_transport_implementations() {
        let manifest = include_str!("../Cargo.toml");

        assert!(
            !manifest.contains("render-")
                && !manifest.contains("platform-")
                && !manifest.contains("transport-pty")
                && !manifest.contains("transport-ssh")
                && !manifest.contains("config-"),
            "mux must stay independent of renderer, platform backend, concrete transports, and config frontends"
        );
    }
}
