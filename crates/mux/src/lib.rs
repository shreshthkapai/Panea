//! Transport-neutral multiplexer model for workspaces, tabs, panes, sessions,
//! and proportional layouts.

pub const LAYER: &str = "multiplexer structure";

use std::{collections::BTreeMap, error::Error, fmt};

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
        }
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
        window.active_tab = tab_id;
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
        Ok(pane_id)
    }

    pub fn close_pane(&mut self, pane_id: PaneId) -> MuxResult<()> {
        self.active_tab_mut().close_pane(pane_id)
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
        self.active_tab_mut().resize_active_pane(direction, delta)
    }

    pub fn toggle_zoom_active_pane(&mut self) -> PaneId {
        let tab = self.active_tab_mut();
        let pane_id = tab.active_pane;
        tab.zoomed_pane = if tab.zoomed_pane == Some(pane_id) {
            None
        } else {
            Some(pane_id)
        };
        pane_id
    }

    pub fn swap_panes(&mut self, first: PaneId, second: PaneId) -> MuxResult<()> {
        self.active_tab_mut().root.swap_panes(first, second)
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
        let assignments = self.layout(LogicalRect::unit());
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
        self.active_pane = pane_id;
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
                let mut offset = 0.0;
                for (index, child) in children.iter().enumerate() {
                    let ratio = ratios.get(index).copied().unwrap_or_default();
                    let rect = match axis {
                        SplitAxis::Horizontal => LogicalRect {
                            x: area.x + offset,
                            y: area.y,
                            width: if index + 1 == children.len() {
                                (area.width - offset).max(0.0)
                            } else {
                                area.width * ratio
                            },
                            height: area.height,
                        },
                        SplitAxis::Vertical => LogicalRect {
                            x: area.x,
                            y: area.y + offset,
                            width: area.width,
                            height: if index + 1 == children.len() {
                                (area.height - offset).max(0.0)
                            } else {
                                area.height * ratio
                            },
                        },
                    };
                    offset += match axis {
                        SplitAxis::Horizontal => rect.width,
                        SplitAxis::Vertical => rect.height,
                    };
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
    MovePane,
    SwapPane { other: PaneId },
}

impl MuxAction {
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        match name {
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
            "move_pane" => Some(Self::MovePane),
            _ => None,
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
    CannotCloseLastPane,
    CannotRemoveRootPane,
    NoPaneInDirection(FocusDirection),
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
            Self::CannotCloseLastPane => f.write_str("cannot close the last pane"),
            Self::CannotRemoveRootPane => f.write_str("cannot remove the root pane directly"),
            Self::NoPaneInDirection(direction) => write!(f, "no pane in direction {direction:?}"),
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
            "move_pane",
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
