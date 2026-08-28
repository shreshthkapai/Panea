use std::{
    cell::{Cell as Counter, RefCell},
    collections::{BTreeMap, VecDeque},
    ops::Range,
    sync::Arc,
};

use crate::{
    Cell, CellAttributes, GridPosition, Line, Selection, SelectionKind, line_content_len,
    push_cell_with_continuation, reflow_cell_width,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HistoryStoreConfig {
    pub(crate) max_cached_logical_lines: usize,
}

impl Default for HistoryStoreConfig {
    fn default() -> Self {
        Self {
            max_cached_logical_lines: 64,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HistoryStats {
    pub canonical_logical_lines: usize,
    pub canonical_cells: usize,
    pub materialized_logical_lines: usize,
    pub materialized_physical_rows: usize,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub cache_evictions: u64,
    pub row_count_cells_scanned: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LogicalAnchor {
    line_id: u64,
    cell_index: usize,
    cell_column: u8,
    trailing_columns: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SelectionAnchors {
    start: LogicalAnchor,
    end: LogicalAnchor,
    kind: SelectionKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LogicalLine {
    id: u64,
    cells: Vec<Cell>,
    terminated: bool,
    generation: u64,
    row_counts: RowCountSummaries,
}

const ROW_COUNT_SUMMARY_CAPACITY: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RowCountSummaries {
    entries: [(u16, usize); ROW_COUNT_SUMMARY_CAPACITY],
    len: u8,
    next: u8,
}

impl RowCountSummaries {
    fn new(cols: u16, rows: usize) -> Self {
        let mut summaries = Self {
            entries: [(0, 0); ROW_COUNT_SUMMARY_CAPACITY],
            len: 1,
            next: 1,
        };
        summaries.entries[0] = (cols, rows);
        summaries
    }

    fn get(&self, cols: u16) -> Option<usize> {
        self.entries[..usize::from(self.len)]
            .iter()
            .find_map(|(cached_cols, rows)| (*cached_cols == cols).then_some(*rows))
    }

    fn reset(&mut self, cols: u16, rows: usize) {
        *self = Self::new(cols, rows);
    }

    fn insert(&mut self, cols: u16, rows: usize) {
        if let Some(entry) = self.entries[..usize::from(self.len)]
            .iter_mut()
            .find(|(cached_cols, _)| *cached_cols == cols)
        {
            *entry = (cols, rows);
            return;
        }

        let index = usize::from(self.next);
        self.entries[index] = (cols, rows);
        if usize::from(self.len) < ROW_COUNT_SUMMARY_CAPACITY {
            self.len += 1;
        }
        self.next = ((index + 1) % ROW_COUNT_SUMMARY_CAPACITY) as u8;
    }
}

impl LogicalLine {
    fn row_count(&mut self, cols: u16) -> (usize, usize) {
        if let Some(rows) = self.row_counts.get(cols) {
            return (rows, 0);
        }

        let scanned = self.cells.len();
        let rows = measure_rows(&self.cells, cols);
        self.row_counts.insert(cols, rows);
        (rows, scanned)
    }
}

#[derive(Debug, Clone)]
struct CachedLayout {
    rows: Vec<Arc<Line>>,
    last_used: u64,
}

#[derive(Debug, Default)]
struct LayoutCache {
    entries: BTreeMap<u64, CachedLayout>,
    clock: u64,
}

fn cells_memory_bytes(cells: &[Cell]) -> u64 {
    cells
        .iter()
        .map(|cell| {
            u64::try_from(cell.text.len())
                .unwrap_or(u64::MAX)
                .saturating_add(16)
        })
        .sum()
}

#[derive(Debug)]
pub(crate) struct HistoryStore {
    cols: u16,
    logical: VecDeque<LogicalLine>,
    row_starts: VecDeque<usize>,
    next_line_id: u64,
    config: HistoryStoreConfig,
    cache: RefCell<LayoutCache>,
    cache_hits: Counter<u64>,
    cache_misses: Counter<u64>,
    cache_evictions: Counter<u64>,
    row_count_cells_scanned: Counter<u64>,
    /// Running total of [`retained_memory_bytes`]. Recomputing it walked every
    /// cell of every retained line, which the diagnostics overlay did once per
    /// frame.
    retained_bytes: u64,
}

impl Clone for HistoryStore {
    fn clone(&self) -> Self {
        Self {
            cols: self.cols,
            logical: self.logical.clone(),
            row_starts: self.row_starts.clone(),
            next_line_id: self.next_line_id,
            config: self.config,
            cache: RefCell::new(LayoutCache::default()),
            cache_hits: Counter::new(0),
            cache_misses: Counter::new(0),
            cache_evictions: Counter::new(0),
            row_count_cells_scanned: Counter::new(self.row_count_cells_scanned.get()),
            retained_bytes: self.retained_bytes,
        }
    }
}

impl PartialEq for HistoryStore {
    fn eq(&self, other: &Self) -> bool {
        self.cols == other.cols
            && self.logical == other.logical
            && self.row_starts == other.row_starts
            && self.next_line_id == other.next_line_id
            && self.config == other.config
    }
}

impl Eq for HistoryStore {}

impl HistoryStore {
    pub(crate) fn new(cols: u16, config: HistoryStoreConfig) -> Self {
        Self {
            cols: cols.max(1),
            logical: VecDeque::new(),
            row_starts: VecDeque::from([0]),
            next_line_id: 1,
            config,
            cache: RefCell::new(LayoutCache::default()),
            cache_hits: Counter::new(0),
            cache_misses: Counter::new(0),
            cache_evictions: Counter::new(0),
            row_count_cells_scanned: Counter::new(0),
            retained_bytes: 0,
        }
    }

    #[cfg(test)]
    pub(crate) fn logical_line_count(&self) -> usize {
        self.logical.len()
    }

    pub(crate) fn physical_row_count(&self) -> usize {
        self.row_starts
            .back()
            .copied()
            .unwrap_or(0)
            .saturating_sub(self.row_starts.front().copied().unwrap_or(0))
    }

    pub(crate) fn push_physical_line(&mut self, line: Line) {
        let wrapped = line.hard_wrapped;
        let generation = line.generation;
        let cells = canonical_cells(&line);
        self.retained_bytes = self
            .retained_bytes
            .saturating_add(cells_memory_bytes(&cells));
        let append = self.logical.back().is_some_and(|line| !line.terminated);
        if append {
            let (id, rows) = {
                let logical = self.logical.back_mut().expect("open logical history line");
                logical.cells.extend(cells);
                logical.terminated = !wrapped;
                logical.generation = logical.generation.max(generation);
                let rows = measure_rows(&logical.cells, self.cols);
                logical.row_counts.reset(self.cols, rows);
                (logical.id, rows)
            };
            self.cache.borrow_mut().entries.remove(&id);
            self.row_starts.pop_back();
            let start = self.row_starts.back().copied().unwrap_or(0);
            self.row_starts.push_back(start.saturating_add(rows));
        } else {
            let id = self.next_line_id;
            self.next_line_id = self.next_line_id.wrapping_add(1).max(1);
            let rows = measure_rows(&cells, self.cols);
            let start = self.row_starts.back().copied().unwrap_or(0);
            self.logical.push_back(LogicalLine {
                id,
                cells,
                terminated: !wrapped,
                generation,
                row_counts: RowCountSummaries::new(self.cols, rows),
            });
            self.row_starts.push_back(start.saturating_add(rows));
        }
    }

    pub(crate) fn set_width(&mut self, cols: u16) {
        let cols = cols.max(1);
        if cols == self.cols {
            return;
        }
        self.cols = cols;
        self.clear_materialized_cache();
        self.rebuild_row_index();
    }

    fn rebuild_row_index(&mut self) {
        self.row_starts.clear();
        self.row_starts
            .reserve(self.logical.len().saturating_add(1));
        self.row_starts.push_back(0);
        let mut rows = 0usize;
        let mut scanned = 0u64;
        for logical in &mut self.logical {
            let (logical_rows, logical_scanned) = logical.row_count(self.cols);
            scanned = scanned.saturating_add(logical_scanned as u64);
            rows = rows.saturating_add(logical_rows);
            self.row_starts.push_back(rows);
        }
        self.row_count_cells_scanned
            .set(self.row_count_cells_scanned.get().saturating_add(scanned));
    }

    pub(crate) fn clear_materialized_cache(&self) {
        self.cache.borrow_mut().entries.clear();
    }

    pub(crate) fn row(&self, physical_row: usize) -> Option<Arc<Line>> {
        let (logical_index, local_row) = self.locate_row(physical_row)?;
        let id = self.logical.get(logical_index)?.id;
        {
            let mut cache = self.cache.borrow_mut();
            cache.clock = cache.clock.wrapping_add(1).max(1);
            let now = cache.clock;
            if let Some(layout) = cache.entries.get_mut(&id) {
                layout.last_used = now;
                self.cache_hits.set(self.cache_hits.get().saturating_add(1));
                return layout.rows.get(local_row).cloned();
            }
        }

        self.cache_misses
            .set(self.cache_misses.get().saturating_add(1));
        let logical = self.logical.get(logical_index)?;
        let rows = materialize_line(logical, self.cols)
            .into_iter()
            .map(Arc::new)
            .collect::<Vec<_>>();
        let requested = rows.get(local_row).cloned();
        if self.config.max_cached_logical_lines == 0 {
            return requested;
        }

        let mut cache = self.cache.borrow_mut();
        while cache.entries.len() >= self.config.max_cached_logical_lines {
            let Some(evict_id) = cache
                .entries
                .iter()
                .min_by_key(|(id, layout)| (layout.last_used, **id))
                .map(|(id, _)| *id)
            else {
                break;
            };
            cache.entries.remove(&evict_id);
            self.cache_evictions
                .set(self.cache_evictions.get().saturating_add(1));
        }
        cache.clock = cache.clock.wrapping_add(1).max(1);
        let last_used = cache.clock;
        cache.entries.insert(id, CachedLayout { rows, last_used });
        requested
    }

    pub(crate) fn prefetch(&self, rows: Range<usize>) {
        let end = rows.end.min(self.physical_row_count());
        let mut logical_ids = Vec::new();
        for row in rows.start.min(end)..end {
            if let Some((logical_index, _)) = self.locate_row(row) {
                let id = self.logical[logical_index].id;
                if logical_ids.last() != Some(&id) {
                    logical_ids.push(id);
                    let _ = self.row(row);
                }
            }
        }
    }

    pub(crate) fn snapshot(&self) -> VecDeque<Line> {
        self.logical
            .iter()
            .flat_map(|logical| materialize_line(logical, self.cols))
            .collect()
    }

    pub(crate) fn clear(&mut self) {
        self.logical.clear();
        self.retained_bytes = 0;
        self.row_starts.clear();
        self.row_starts.push_back(0);
        self.clear_materialized_cache();
    }

    /// Removes physical rows from the front and returns the number removed.
    pub(crate) fn trim_to_rows(&mut self, limit: usize) -> usize {
        let mut excess = self.physical_row_count().saturating_sub(limit);
        let removed = excess;
        while excess > 0 {
            let Some(first_rows) = self
                .row_starts
                .get(1)
                .copied()
                .map(|end| end.saturating_sub(self.row_starts[0]))
            else {
                break;
            };
            if first_rows <= excess {
                let dropped = self.logical.pop_front().expect("indexed logical line");
                self.retained_bytes = self
                    .retained_bytes
                    .saturating_sub(cells_memory_bytes(&dropped.cells));
                self.cache.borrow_mut().entries.remove(&dropped.id);
                excess -= first_rows;
                self.row_starts.pop_front();
                continue;
            }

            let first = self.logical.pop_front().expect("indexed logical line");
            let rows = materialize_line(&first, self.cols);
            let retained_prefix = rows.into_iter().skip(excess).collect::<Vec<_>>();
            let mut retained_tail = std::mem::take(&mut self.logical);
            // `first` was popped and the rest moved out, so nothing is retained
            // until the lines below are pushed back in.
            self.retained_bytes = 0;
            self.row_starts.clear();
            self.row_starts.push_back(0);
            self.clear_materialized_cache();
            for row in retained_prefix {
                self.push_physical_line(row);
            }
            while let Some(mut logical) = retained_tail.pop_front() {
                let (rows, scanned) = logical.row_count(self.cols);
                self.row_count_cells_scanned.set(
                    self.row_count_cells_scanned
                        .get()
                        .saturating_add(scanned as u64),
                );
                let start = self.row_starts.back().copied().unwrap_or(0);
                self.row_starts.push_back(start.saturating_add(rows));
                self.retained_bytes = self
                    .retained_bytes
                    .saturating_add(cells_memory_bytes(&logical.cells));
                self.logical.push_back(logical);
            }
            excess = 0;
        }
        removed
    }

    /// Moves every physical row at and after `split` out of history. Only the
    /// logical line intersecting the split and lines after it are materialized.
    pub(crate) fn drain_tail_from(&mut self, split: usize) -> Vec<Line> {
        let split = split.min(self.physical_row_count());
        if split == self.physical_row_count() {
            return Vec::new();
        }
        let (logical_index, local_row) = self
            .locate_row(split)
            .expect("a split below row count resolves to a logical line");
        let mut tail = self.logical.split_off(logical_index);
        let first = tail.pop_front().expect("split logical line");
        self.row_starts.truncate(logical_index.saturating_add(1));
        self.clear_materialized_cache();

        let first_rows = materialize_line(&first, self.cols);
        for row in first_rows.iter().take(local_row).cloned() {
            self.push_physical_line(row);
        }

        let mut drained = first_rows.into_iter().skip(local_row).collect::<Vec<_>>();
        for logical in tail {
            drained.extend(materialize_line(&logical, self.cols));
        }
        drained
    }

    pub(crate) fn retained_memory_bytes(&self) -> u64 {
        self.retained_bytes
    }

    /// Full recount, for asserting the running total has not drifted.
    #[cfg(test)]
    pub(crate) fn recounted_memory_bytes(&self) -> u64 {
        self.logical
            .iter()
            .map(|line| cells_memory_bytes(&line.cells))
            .sum()
    }

    pub(crate) fn stats(&self) -> HistoryStats {
        let cache = self.cache.borrow();
        HistoryStats {
            canonical_logical_lines: self.logical.len(),
            canonical_cells: self.logical.iter().map(|line| line.cells.len()).sum(),
            materialized_logical_lines: cache.entries.len(),
            materialized_physical_rows: cache.entries.values().map(|entry| entry.rows.len()).sum(),
            cache_hits: self.cache_hits.get(),
            cache_misses: self.cache_misses.get(),
            cache_evictions: self.cache_evictions.get(),
            row_count_cells_scanned: self.row_count_cells_scanned.get(),
        }
    }

    pub(crate) fn anchor_for_position(&self, position: GridPosition) -> Option<LogicalAnchor> {
        let physical_row = usize::try_from(position.row).ok()?;
        let (logical_index, local_row) = self.locate_row(physical_row)?;
        let logical = self.logical.get(logical_index)?;
        let (cell_index, cell_column, trailing_columns) =
            anchor_offset(&logical.cells, self.cols, local_row, position.col);
        Some(LogicalAnchor {
            line_id: logical.id,
            cell_index,
            cell_column,
            trailing_columns,
        })
    }

    pub(crate) fn position_for_anchor(&self, anchor: LogicalAnchor) -> Option<GridPosition> {
        let logical_index = self
            .logical
            .iter()
            .position(|line| line.id == anchor.line_id)?;
        let local = position_for_offset(
            &self.logical[logical_index].cells,
            self.cols,
            anchor.cell_index,
            anchor.cell_column,
            anchor.trailing_columns,
        );
        Some(GridPosition::new(
            i64::try_from(
                self.row_starts[logical_index]
                    .saturating_sub(self.row_starts.front().copied().unwrap_or(0))
                    .saturating_add(local.0),
            )
            .unwrap_or(i64::MAX),
            local.1,
        ))
    }

    pub(crate) fn anchors_for_selection(&self, selection: Selection) -> Option<SelectionAnchors> {
        Some(SelectionAnchors {
            start: self.anchor_for_position(selection.start)?,
            end: self.anchor_for_position(selection.end)?,
            kind: selection.kind,
        })
    }

    pub(crate) fn selection_for_anchors(&self, anchors: SelectionAnchors) -> Option<Selection> {
        Some(Selection {
            start: self.position_for_anchor(anchors.start)?,
            end: self.position_for_anchor(anchors.end)?,
            kind: anchors.kind,
        })
    }

    fn locate_row(&self, physical_row: usize) -> Option<(usize, usize)> {
        if physical_row >= self.physical_row_count() {
            return None;
        }
        let absolute_row = self
            .row_starts
            .front()
            .copied()
            .unwrap_or(0)
            .saturating_add(physical_row);
        let mut low = 0usize;
        let mut high = self.row_starts.len();
        while low < high {
            let middle = low + (high - low) / 2;
            if self.row_starts[middle] <= absolute_row {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        let logical_index = low
            .saturating_sub(1)
            .min(self.logical.len().saturating_sub(1));
        Some((
            logical_index,
            absolute_row.saturating_sub(self.row_starts[logical_index]),
        ))
    }
}

fn canonical_cells(line: &Line) -> Vec<Cell> {
    let end = line_content_len(line);
    line.cells
        .iter()
        .take(end)
        .filter(|cell| !cell.wide_continuation)
        .cloned()
        .collect()
}

fn measure_rows(cells: &[Cell], cols: u16) -> usize {
    let cols = usize::from(cols.max(1));
    let mut rows = 1usize;
    let mut col = 0usize;
    for (index, cell) in cells.iter().enumerate() {
        let width = reflow_cell_width(cell, cols);
        if col > 0 && col.saturating_add(width) > cols {
            rows = rows.saturating_add(1);
            col = 0;
        }
        col = col.saturating_add(width);
        if col == cols {
            col = 0;
            if index + 1 < cells.len() {
                rows = rows.saturating_add(1);
            }
        }
    }
    rows
}

fn materialize_line(logical: &LogicalLine, cols: u16) -> Vec<Line> {
    let cols = usize::from(cols.max(1));
    let mut rows = Vec::new();
    let mut current = Line {
        cells: Vec::with_capacity(cols),
        hard_wrapped: false,
        generation: logical.generation,
    };
    for cell in &logical.cells {
        let width = reflow_cell_width(cell, cols);
        if !current.cells.is_empty() && current.cells.len().saturating_add(width) > cols {
            current.hard_wrapped = true;
            current.resize_to(cols as u16, CellAttributes::default());
            rows.push(current);
            current = Line {
                cells: Vec::with_capacity(cols),
                hard_wrapped: false,
                generation: logical.generation,
            };
        }
        push_cell_with_continuation(&mut current.cells, cell.clone(), cols, width);
        if current.cells.len() == cols {
            current.hard_wrapped = true;
            rows.push(current);
            current = Line {
                cells: Vec::with_capacity(cols),
                hard_wrapped: false,
                generation: logical.generation,
            };
        }
    }
    if !current.cells.is_empty() || rows.is_empty() {
        current.resize_to(cols as u16, CellAttributes::default());
        rows.push(current);
    }
    if let Some(last) = rows.last_mut() {
        last.hard_wrapped = !logical.terminated;
    }
    rows
}

fn anchor_offset(
    cells: &[Cell],
    cols: u16,
    target_row: usize,
    target_col: u16,
) -> (usize, u8, u16) {
    let cols = usize::from(cols.max(1));
    let target_col = usize::from(target_col);
    let mut row = 0usize;
    let mut col = 0usize;
    for (index, cell) in cells.iter().enumerate() {
        let width = reflow_cell_width(cell, cols);
        if col > 0 && col.saturating_add(width) > cols {
            row = row.saturating_add(1);
            col = 0;
        }
        if row == target_row && target_col < col.saturating_add(width) {
            return (
                index,
                target_col.saturating_sub(col).min(u8::MAX as usize) as u8,
                0,
            );
        }
        col = col.saturating_add(width);
        if col == cols && index + 1 < cells.len() {
            row = row.saturating_add(1);
            col = 0;
        }
    }
    let trailing = if row == target_row {
        target_col.saturating_sub(col)
    } else {
        0
    };
    (cells.len(), 0, trailing.min(u16::MAX as usize) as u16)
}

fn position_for_offset(
    cells: &[Cell],
    cols: u16,
    cell_index: usize,
    cell_column: u8,
    trailing_columns: u16,
) -> (usize, u16) {
    let cols = usize::from(cols.max(1));
    let mut row = 0usize;
    let mut col = 0usize;
    for (index, cell) in cells.iter().enumerate() {
        let width = reflow_cell_width(cell, cols);
        if col > 0 && col.saturating_add(width) > cols {
            row = row.saturating_add(1);
            col = 0;
        }
        if index == cell_index {
            return (
                row,
                (col + usize::from(cell_column).min(width.saturating_sub(1))) as u16,
            );
        }
        col = col.saturating_add(width);
        if col == cols && index + 1 < cells.len() {
            row = row.saturating_add(1);
            col = 0;
        }
    }
    let absolute = col.saturating_add(usize::from(trailing_columns));
    (
        row.saturating_add(absolute / cols),
        (absolute % cols) as u16,
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{HistoryStore, HistoryStoreConfig};
    use crate::{Cell, CellAttributes, Color, GridPosition, Line, Selection, SelectionKind};

    #[test]
    fn the_running_byte_total_never_drifts_from_a_recount() {
        let mut store = HistoryStore::new(8, HistoryStoreConfig::default());
        assert_eq!(store.retained_memory_bytes(), 0);

        // Growth, including soft-wrapped lines that extend an open logical line.
        for index in 0..40 {
            let wrapped = index % 3 == 0;
            store.push_physical_line(line(&format!("row{index}"), 8, wrapped));
            assert_eq!(
                store.retained_memory_bytes(),
                store.recounted_memory_bytes(),
                "drift after push {index}"
            );
        }

        // Eviction, which both drops whole lines and splits one.
        for target in [30usize, 12, 5, 1] {
            store.trim_to_rows(target);
            assert_eq!(
                store.retained_memory_bytes(),
                store.recounted_memory_bytes(),
                "drift after trimming to {target} rows"
            );
        }

        // A width change rebuilds row counts but not cell contents.
        store.set_width(4);
        assert_eq!(
            store.retained_memory_bytes(),
            store.recounted_memory_bytes(),
            "drift after a width change"
        );

        store.clear();
        assert_eq!(store.retained_memory_bytes(), 0);
        assert_eq!(store.recounted_memory_bytes(), 0);
    }

    fn text(line: &Line) -> String {
        line.cells
            .iter()
            .filter(|cell| !cell.wide_continuation)
            .map(|cell| cell.text.as_str())
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    fn line(contents: &str, cols: u16, wrapped: bool) -> Line {
        let mut line = Line::blank(cols);
        for (index, ch) in contents.chars().enumerate() {
            line.cells[index] = Cell::text(ch.to_string(), CellAttributes::default());
        }
        line.hard_wrapped = wrapped;
        line
    }

    #[test]
    fn canonical_history_round_trips_soft_wraps_and_hard_breaks() {
        let mut history = HistoryStore::new(4, HistoryStoreConfig::default());
        history.push_physical_line(line("abcd", 4, true));
        history.push_physical_line(line("ef", 4, false));
        history.push_physical_line(line("next", 4, false));

        assert_eq!(history.logical_line_count(), 2);
        assert_eq!(history.physical_row_count(), 3);
        assert_eq!(
            history.snapshot().iter().map(text).collect::<Vec<_>>(),
            ["abcd", "ef", "next"]
        );

        history.set_width(3);

        let rows = history.snapshot();
        assert_eq!(
            rows.iter().map(text).collect::<Vec<_>>(),
            ["abc", "def", "nex", "t"]
        );
        assert!(rows[0].hard_wrapped);
        assert!(!rows[1].hard_wrapped);
        assert!(rows[2].hard_wrapped);
        assert!(!rows[3].hard_wrapped);
    }

    #[test]
    fn canonical_history_preserves_wide_cells_and_background_blanks() {
        let attrs = CellAttributes {
            background: Some(Color::Indexed(5)),
            ..CellAttributes::default()
        };
        let mut first = Line::blank(4);
        first.cells[0] = Cell::text("界", CellAttributes::default());
        first.cells[0].width = 2;
        first.cells[1] = Cell::wide_continuation(CellAttributes::default());
        first.cells[2] = Cell::blank(attrs);
        first.cells[3] = Cell::blank(attrs);

        let mut history = HistoryStore::new(4, HistoryStoreConfig::default());
        history.push_physical_line(first);
        history.set_width(3);

        let rows = history.snapshot();
        assert_eq!(rows.len(), 2);
        assert_eq!(text(&rows[0]), "界");
        assert!(rows[0].cells[1].wide_continuation);
        assert_eq!(
            rows.iter()
                .flat_map(|line| &line.cells)
                .filter(|cell| cell.attributes.background == Some(Color::Indexed(5)))
                .count(),
            2
        );
    }

    #[test]
    fn width_change_counts_rows_without_materializing_cold_history() {
        let mut history = HistoryStore::new(
            8,
            HistoryStoreConfig {
                max_cached_logical_lines: 4,
            },
        );
        for index in 0..64 {
            history.push_physical_line(line(&format!("row{index:04}"), 8, false));
        }
        history.clear_materialized_cache();

        history.set_width(4);

        assert_eq!(history.physical_row_count(), 128);
        assert_eq!(history.stats().materialized_logical_lines, 0);

        history.prefetch(120..128);
        let stats = history.stats();
        assert!(stats.materialized_logical_lines <= 4);
        assert!(stats.materialized_physical_rows <= 8);
    }

    #[test]
    fn repeated_widths_reuse_row_count_summaries() {
        let mut history = HistoryStore::new(16, HistoryStoreConfig::default());
        for index in 0..64 {
            history.push_physical_line(line(&format!("row-{index:04}-abcdef"), 16, false));
        }

        history.set_width(8);
        history.set_width(4);
        let warmed_scans = history.stats().row_count_cells_scanned;
        history.set_width(16);
        history.set_width(8);
        history.set_width(4);

        assert_eq!(history.stats().row_count_cells_scanned, warmed_scans);
    }

    #[test]
    fn partial_front_trim_keeps_cold_tail_canonical() {
        let mut history = HistoryStore::new(4, HistoryStoreConfig::default());
        history.push_physical_line(line("abcd", 4, true));
        history.push_physical_line(line("efgh", 4, true));
        history.push_physical_line(line("ijkl", 4, false));
        for index in 0..100 {
            history.push_physical_line(line(&format!("{index:04}"), 4, false));
        }
        history.clear_materialized_cache();
        let original_rows = history.physical_row_count();

        assert_eq!(history.trim_to_rows(original_rows - 1), 1);

        let stats = history.stats();
        assert_eq!(stats.materialized_logical_lines, 0);
        assert_eq!(stats.canonical_logical_lines, 101);
        assert_eq!(text(&history.row(0).expect("retained wrapped row")), "efgh");
        assert_eq!(
            text(
                &history
                    .row(history.physical_row_count() - 1)
                    .expect("last retained row")
            ),
            "0099"
        );
    }

    #[test]
    fn repeated_row_access_hits_the_bounded_cache() {
        let mut history = HistoryStore::new(
            8,
            HistoryStoreConfig {
                max_cached_logical_lines: 2,
            },
        );
        for contents in ["alpha", "bravo", "charlie"] {
            history.push_physical_line(line(contents, 8, false));
        }
        history.clear_materialized_cache();

        let first = history.row(0).expect("first row");
        let again = history.row(0).expect("cached first row");
        assert!(Arc::ptr_eq(&first, &again));
        assert!(history.stats().cache_hits >= 1);

        let _ = history.row(1);
        let _ = history.row(2);
        assert!(history.stats().materialized_logical_lines <= 2);
        assert!(history.stats().cache_evictions >= 1);
    }

    #[test]
    fn logical_anchors_follow_text_across_width_changes() {
        let mut history = HistoryStore::new(6, HistoryStoreConfig::default());
        history.push_physical_line(line("abcdef", 6, true));
        history.push_physical_line(line("ghij", 6, false));
        let anchor = history
            .anchor_for_position(GridPosition::new(1, 2))
            .expect("logical anchor");

        history.set_width(4);

        assert_eq!(
            history.position_for_anchor(anchor),
            Some(GridPosition::new(2, 0))
        );
    }

    #[test]
    fn selection_endpoints_can_be_remapped_without_changing_selection_kind() {
        let mut history = HistoryStore::new(6, HistoryStoreConfig::default());
        history.push_physical_line(line("abcdef", 6, true));
        history.push_physical_line(line("ghij", 6, false));
        let selection = Selection::rectangular(GridPosition::new(0, 2), GridPosition::new(1, 3));
        let anchors = history
            .anchors_for_selection(selection)
            .expect("selection anchors");

        history.set_width(4);
        let remapped = history
            .selection_for_anchors(anchors)
            .expect("remapped selection");

        assert_eq!(remapped.kind, SelectionKind::Rectangular);
        assert_eq!(remapped.start, GridPosition::new(0, 2));
        assert_eq!(remapped.end, GridPosition::new(2, 1));
    }
}
