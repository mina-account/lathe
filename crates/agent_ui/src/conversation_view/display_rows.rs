use std::collections::HashMap;

use gpui::{EntityId, ListOffset, Pixels};

use super::response_model::{
    ClassifiedTurn, SelectionConfidence, SourceReference, TurnIdentity, TurnLifecycle,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum DisplayMode {
    #[default]
    Normal,
    Compact,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SourceRole {
    Prompt,
    Activity,
    Response,
    RequiredAction,
    Status,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum DisplayRow {
    Transcript(usize),
    ActivityHeader(TurnIdentity),
    Content {
        turn: TurnIdentity,
        source: SourceReference,
        role: SourceRole,
        response_end: bool,
    },
    Generating,
}

impl DisplayRow {
    pub fn entry_index(&self) -> Option<usize> {
        match self {
            Self::Transcript(index) => Some(*index),
            Self::Content { source, .. } => Some(source.entry_index),
            Self::ActivityHeader(_) | Self::Generating => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GroupState {
    lifecycle: TurnLifecycle,
    confidence: SelectionConfidence,
    sources: Vec<SourceReference>,
    response: Vec<SourceReference>,
    activity: Vec<SourceReference>,
    required_actions: Vec<SourceReference>,
    statuses: Vec<SourceReference>,
    prompt: Option<SourceReference>,
    expanded: bool,
}

impl GroupState {
    fn matches(&self, turn: &ClassifiedTurn, expanded: bool) -> bool {
        self.lifecycle == turn.lifecycle
            && self.confidence == turn.confidence
            && self.response == turn.response
            && self.activity == turn.activity
            && self.required_actions == turn.required_actions
            && self.statuses == turn.statuses
            && self.prompt == turn.prompt
            && self.expanded == expanded
    }
}

#[derive(Default)]
pub(super) struct DisplayRows {
    pub rows: Vec<DisplayRow>,
    mode: DisplayMode,
    entry_count: usize,
    turn_order: Vec<TurnIdentity>,
    source_rows: HashMap<SourceReference, usize>,
    entry_rows: HashMap<usize, usize>,
    group_rows: HashMap<TurnIdentity, Vec<usize>>,
    groups: HashMap<TurnIdentity, GroupState>,
    header_rows: HashMap<TurnIdentity, usize>,
}

impl DisplayRows {
    pub fn new(
        mode: DisplayMode,
        entry_count: usize,
        turns: &[ClassifiedTurn],
        expanded: impl Fn(&TurnIdentity) -> bool,
        generating: bool,
    ) -> Self {
        let mut projection = Self {
            mode,
            entry_count,
            turn_order: turns.iter().map(|turn| turn.identity.clone()).collect(),
            ..Self::default()
        };
        if mode == DisplayMode::Normal {
            projection
                .rows
                .extend((0..entry_count).map(DisplayRow::Transcript));
            projection
                .entry_rows
                .extend((0..entry_count).map(|index| (index, index)));
        }
        for turn in turns {
            let mut sources = turn.activity.clone();
            sources.extend(turn.response.iter().cloned());
            sources.extend(turn.required_actions.iter().cloned());
            sources.extend(turn.statuses.iter().cloned());
            sources.extend(turn.prompt.iter().cloned());
            sources.sort_by_key(|source| (source.entry_index, source.chunk_index));
            sources.dedup();
            projection.groups.insert(
                turn.identity.clone(),
                GroupState {
                    lifecycle: turn.lifecycle,
                    confidence: turn.confidence,
                    sources: sources.clone(),
                    response: turn.response.clone(),
                    activity: turn.activity.clone(),
                    required_actions: turn.required_actions.clone(),
                    statuses: turn.statuses.clone(),
                    prompt: turn.prompt.clone(),
                    expanded: expanded(&turn.identity),
                },
            );
            if mode == DisplayMode::Normal {
                let mut rows = Vec::new();
                for source in &sources {
                    projection
                        .source_rows
                        .insert(source.clone(), source.entry_index);
                    rows.push(source.entry_index);
                }
                rows.sort_unstable();
                rows.dedup();
                projection.group_rows.insert(turn.identity.clone(), rows);
                continue;
            }
            if let Some(prompt) = &turn.prompt {
                projection.push_source(turn, prompt, SourceRole::Prompt, false);
            }
            let activity: Vec<_> = turn
                .activity
                .iter()
                .filter(|source| {
                    !turn.required_actions.contains(source) && !turn.statuses.contains(source)
                })
                .collect();
            if !activity.is_empty() {
                let header = projection.rows.len();
                projection
                    .rows
                    .push(DisplayRow::ActivityHeader(turn.identity.clone()));
                projection.header_rows.insert(turn.identity.clone(), header);
                projection
                    .group_rows
                    .entry(turn.identity.clone())
                    .or_default()
                    .push(header);
                for source in activity {
                    if expanded(&turn.identity) {
                        projection.push_source(turn, source, SourceRole::Activity, false);
                    } else {
                        projection.source_rows.insert(source.clone(), header);
                        projection
                            .entry_rows
                            .entry(source.entry_index)
                            .or_insert(header);
                    }
                }
            }
            for source in &sources {
                if turn.required_actions.contains(source) {
                    projection.push_source(turn, source, SourceRole::RequiredAction, false);
                } else if turn.statuses.contains(source) {
                    projection.push_source(turn, source, SourceRole::Status, false);
                } else if turn.response.contains(source) {
                    projection.push_source(
                        turn,
                        source,
                        SourceRole::Response,
                        turn.response.last() == Some(source),
                    );
                }
            }
        }
        if generating {
            projection.rows.push(DisplayRow::Generating);
        }
        projection
    }

    fn push_source(
        &mut self,
        turn: &ClassifiedTurn,
        source: &SourceReference,
        role: SourceRole,
        response_end: bool,
    ) {
        let row = self.rows.len();
        self.rows.push(DisplayRow::Content {
            turn: turn.identity.clone(),
            source: source.clone(),
            role,
            response_end,
        });
        self.source_rows.insert(source.clone(), row);
        // A visible response/action is a better entry-level locator than its hidden thought.
        self.entry_rows
            .entry(source.entry_index)
            .and_modify(|current| {
                if matches!(self.rows.get(*current), Some(DisplayRow::ActivityHeader(_))) {
                    *current = row;
                }
            })
            .or_insert(row);
        self.group_rows
            .entry(turn.identity.clone())
            .or_default()
            .push(row);
    }

    pub fn row_for_source(&self, source: &SourceReference) -> Option<usize> {
        self.source_rows.get(source).copied()
    }

    pub fn sources_for_row(&self, row: usize) -> impl Iterator<Item = &SourceReference> {
        self.source_rows
            .iter()
            .filter_map(move |(source, &source_row)| (source_row == row).then_some(source))
    }

    pub fn row_for_entry(&self, entry: usize) -> Option<usize> {
        self.entry_rows.get(&entry).copied()
    }

    pub fn entry_for_row(&self, row: usize) -> Option<usize> {
        self.rows.get(row)?.entry_index().or_else(|| {
            if let DisplayRow::ActivityHeader(identity) = self.rows.get(row)? {
                return self
                    .groups
                    .get(identity)?
                    .activity
                    .iter()
                    .map(|source| source.entry_index)
                    .min();
            }
            self.source_rows
                .iter()
                .filter_map(|(source, &source_row)| {
                    (source_row == row).then_some(source.entry_index)
                })
                .min()
        })
    }

    pub fn to_transcript_offset(&self, offset: ListOffset, entry_count: usize) -> ListOffset {
        ListOffset {
            item_ix: self.entry_for_row(offset.item_ix).unwrap_or(entry_count),
            offset_in_item: offset.offset_in_item,
        }
    }

    pub fn from_transcript_offset(&self, offset: ListOffset) -> ListOffset {
        ListOffset {
            item_ix: self
                .row_for_entry(offset.item_ix)
                .unwrap_or(self.rows.len()),
            offset_in_item: offset.offset_in_item,
        }
    }

    pub fn sync(
        &mut self,
        mode: DisplayMode,
        entry_count: usize,
        turns: &[ClassifiedTurn],
        expanded: impl Fn(&TurnIdentity) -> bool,
        generating: bool,
    ) -> (Vec<(std::ops::Range<usize>, Vec<DisplayRow>)>, Vec<usize>) {
        let stable_order = self.turn_order.len() <= turns.len()
            && self
                .turn_order
                .iter()
                .zip(turns)
                .all(|(identity, turn)| identity == &turn.identity);
        if self.mode != mode || !stable_order || self.entry_count != entry_count {
            let next = Self::new(mode, entry_count, turns, expanded, generating);
            let prefix = self
                .rows
                .iter()
                .zip(&next.rows)
                .take_while(|(old, new)| old == new)
                .count();
            let suffix = self.rows[prefix..]
                .iter()
                .rev()
                .zip(next.rows[prefix..].iter().rev())
                .take_while(|(old, new)| old == new)
                .count();
            let remeasure = next.changed_rows(self);
            let end = next.rows.len().saturating_sub(suffix);
            let edits = if prefix != self.rows.len() || prefix != next.rows.len() {
                vec![(
                    prefix..self.rows.len().saturating_sub(suffix),
                    next.rows[prefix..end].to_vec(),
                )]
            } else {
                Vec::new()
            };
            *self = next;
            return (edits, remeasure);
        }
        let mut edits = Vec::new();
        let mut remeasure = Vec::new();
        for turn in turns {
            let is_expanded = expanded(&turn.identity);
            if self
                .groups
                .get(&turn.identity)
                .is_some_and(|group| group.matches(turn, is_expanded))
            {
                continue;
            }
            let fragment = Self::new(mode, 0, std::slice::from_ref(turn), |_| is_expanded, false);
            if mode == DisplayMode::Compact {
                let range = self
                    .group_rows
                    .get(&turn.identity)
                    .and_then(|rows| Some(*rows.first()?..rows.last()? + 1))
                    .unwrap_or_else(|| {
                        let end = turns
                            .iter()
                            .skip_while(|candidate| candidate.identity != turn.identity)
                            .skip(1)
                            .find_map(|candidate| {
                                self.group_rows
                                    .get(&candidate.identity)
                                    .and_then(|rows| rows.first().copied())
                            })
                            .unwrap_or_else(|| {
                                self.rows.len()
                                    - usize::from(matches!(
                                        self.rows.last(),
                                        Some(DisplayRow::Generating)
                                    ))
                            });
                        end..end
                    });
                let old_rows = self.rows.get(range.clone()).unwrap_or_default();
                let prefix = old_rows
                    .iter()
                    .zip(&fragment.rows)
                    .take_while(|(old, new)| old == new)
                    .count();
                let suffix = old_rows[prefix..]
                    .iter()
                    .rev()
                    .zip(fragment.rows[prefix..].iter().rev())
                    .take_while(|(old, new)| old == new)
                    .count();
                let replacement =
                    fragment.rows[prefix..fragment.rows.len().saturating_sub(suffix)].to_vec();
                let edit = range.start + prefix..range.end.saturating_sub(suffix);
                if !edit.is_empty() || !replacement.is_empty() {
                    self.rows.splice(edit.clone(), replacement.clone());
                    let delta = replacement.len() as isize - edit.len() as isize;
                    for rows in self.group_rows.values_mut() {
                        for row in rows {
                            if *row >= range.end {
                                *row = row.saturating_add_signed(delta);
                            }
                        }
                    }
                    for row in self.source_rows.values_mut() {
                        if *row >= range.end {
                            *row = row.saturating_add_signed(delta);
                        }
                    }
                    for row in self.header_rows.values_mut() {
                        if *row >= range.end {
                            *row = row.saturating_add_signed(delta);
                        }
                    }
                    edits.push((edit, replacement));
                }
                self.source_rows.retain(|source, _| {
                    !self
                        .groups
                        .get(&turn.identity)
                        .is_some_and(|group| group.sources.contains(source))
                });
                self.source_rows.extend(
                    fragment
                        .source_rows
                        .iter()
                        .map(|(source, row)| (source.clone(), range.start + row)),
                );
                self.group_rows.insert(
                    turn.identity.clone(),
                    (range.start..range.start + fragment.rows.len()).collect(),
                );
                self.header_rows.remove(&turn.identity);
                if let Some(header) = fragment.header_rows.get(&turn.identity) {
                    self.header_rows
                        .insert(turn.identity.clone(), range.start + header);
                }
            } else {
                self.source_rows.retain(|source, _| {
                    !self
                        .groups
                        .get(&turn.identity)
                        .is_some_and(|group| group.sources.contains(source))
                });
                self.source_rows.extend(fragment.source_rows);
                if let Some(rows) = fragment.group_rows.get(&turn.identity) {
                    self.group_rows.insert(turn.identity.clone(), rows.clone());
                }
            }
            if let Some(group) = fragment.groups.get(&turn.identity) {
                self.groups.insert(turn.identity.clone(), group.clone());
            }
            remeasure.extend(
                self.group_rows
                    .get(&turn.identity)
                    .into_iter()
                    .flatten()
                    .copied(),
            );
        }
        if self.turn_order.len() != turns.len() {
            self.turn_order = turns.iter().map(|turn| turn.identity.clone()).collect();
        }
        if !edits.is_empty() {
            self.entry_rows.clear();
            for (row, display) in self.rows.iter().enumerate() {
                if let Some(entry) = display.entry_index() {
                    self.entry_rows.entry(entry).or_insert(row);
                }
            }
            for (source, &row) in &self.source_rows {
                self.entry_rows.entry(source.entry_index).or_insert(row);
            }
        }
        let was_generating = matches!(self.rows.last(), Some(DisplayRow::Generating));
        if was_generating != generating {
            let index = self.rows.len() - usize::from(was_generating);
            let replacement = if generating {
                vec![DisplayRow::Generating]
            } else {
                Vec::new()
            };
            self.rows
                .splice(index..self.rows.len(), replacement.clone());
            edits.push((index..index + usize::from(was_generating), replacement));
        }
        (edits, remeasure)
    }

    pub fn changed_rows(&self, previous: &Self) -> Vec<usize> {
        self.groups
            .iter()
            .filter(|(identity, group)| previous.groups.get(*identity) != Some(*group))
            .flat_map(|(identity, _)| self.group_rows.get(identity).into_iter().flatten().copied())
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum ContentBlockIdentity {
    Entity(EntityId),
    Image(u64),
    Resource(std::sync::Arc<str>),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct ContentTarget {
    pub turn: TurnIdentity,
    pub source: SourceReference,
    pub block_index: Option<usize>,
    pub block_identity: Option<ContentBlockIdentity>,
    pub text_offset: Option<usize>,
    pub response_only: bool,
}

#[derive(Clone)]
pub(super) struct PendingContentScroll {
    pub request: u64,
    pub target: ContentTarget,
    pub relative_offset: Pixels,
    pub reveal: Option<std::sync::Arc<dyn Fn(&mut gpui::Window, &mut gpui::App)>>,
}

#[cfg(test)]
mod tests {
    use super::super::response_model::{ProviderIdentity, SourceIdentity, TurnOutcome};
    use super::*;
    use std::sync::Arc;

    fn source(entry: usize, chunk: Option<usize>, identity: SourceIdentity) -> SourceReference {
        SourceReference {
            entry_index: entry,
            chunk_index: chunk,
            identity,
        }
    }

    fn turn() -> ClassifiedTurn {
        let prompt = source(
            0,
            None,
            SourceIdentity::Prompt(ProviderIdentity::Keyed(Arc::from("prompt"))),
        );
        ClassifiedTurn {
            identity: TurnIdentity::Prompt(prompt.clone()),
            prompt: Some(prompt),
            lifecycle: TurnLifecycle::Settled(TurnOutcome::Unknown),
            confidence: SelectionConfidence::Structural,
            activity: vec![source(
                1,
                Some(0),
                SourceIdentity::Thought(ProviderIdentity::Keyed(Arc::from("thought"))),
            )],
            response: (1..4)
                .map(|chunk| {
                    source(
                        1,
                        Some(chunk),
                        SourceIdentity::Message(ProviderIdentity::Keyed(Arc::from(format!(
                            "answer-{chunk}"
                        )))),
                    )
                })
                .collect(),
            required_actions: Vec::new(),
            statuses: Vec::new(),
            inspect_activity: false,
            predecessor: None,
        }
    }

    #[test]
    fn normal_rows_preserve_transcript_and_synthetic_generation() {
        let turn = turn();
        let projection = DisplayRows::new(
            DisplayMode::Normal,
            2,
            std::slice::from_ref(&turn),
            |_| false,
            true,
        );
        assert_eq!(
            projection.rows,
            vec![
                DisplayRow::Transcript(0),
                DisplayRow::Transcript(1),
                DisplayRow::Generating
            ]
        );
        for source in turn.activity.iter().chain(&turn.response) {
            assert_eq!(projection.row_for_source(source), Some(1));
        }
        let offset = ListOffset {
            item_ix: 1,
            offset_in_item: gpui::px(37.),
        };
        let persisted = projection.to_transcript_offset(offset, 2);
        assert_eq!(persisted.item_ix, 1);
        assert_eq!(
            projection.from_transcript_offset(persisted).offset_in_item,
            gpui::px(37.)
        );
    }

    #[test]
    fn compact_rows_virtualize_sources_with_one_header_and_footer() {
        let turn = turn();
        let collapsed = DisplayRows::new(
            DisplayMode::Compact,
            2,
            std::slice::from_ref(&turn),
            |_| false,
            true,
        );
        assert_eq!(collapsed.rows.len(), 6);
        assert_eq!(collapsed.row_for_source(&turn.activity[0]), Some(1));
        assert!(matches!(
            collapsed.rows.get(1),
            Some(DisplayRow::ActivityHeader(_))
        ));
        assert!(collapsed.rows.iter().all(|row| !matches!(
            row,
            DisplayRow::Content {
                role: SourceRole::Activity,
                ..
            }
        )));
        let expanded = DisplayRows::new(DisplayMode::Compact, 2, &[turn], |_| true, true);
        assert_eq!(expanded.rows.len(), 7);
        assert_eq!(
            expanded
                .rows
                .iter()
                .filter(|row| matches!(row, DisplayRow::ActivityHeader(_)))
                .count(),
            1
        );
        assert_eq!(
            expanded
                .rows
                .iter()
                .filter(|row| matches!(
                    row,
                    DisplayRow::Content {
                        response_end: true,
                        ..
                    }
                ))
                .count(),
            1
        );
        assert_eq!(expanded.entry_for_row(1), Some(1));
        assert_eq!(
            expanded
                .to_transcript_offset(
                    ListOffset {
                        item_ix: 6,
                        offset_in_item: gpui::px(0.)
                    },
                    2
                )
                .item_ix,
            2
        );
    }

    #[test]
    fn in_place_lifecycle_changes_invalidate_rows_without_splicing() {
        let mut turn = turn();
        let previous = DisplayRows::new(
            DisplayMode::Normal,
            2,
            std::slice::from_ref(&turn),
            |_| false,
            false,
        );
        turn.lifecycle = TurnLifecycle::Running;
        turn.activity.extend(std::mem::take(&mut turn.response));
        let next = DisplayRows::new(DisplayMode::Normal, 2, &[turn], |_| false, false);
        assert_eq!(next.rows, previous.rows);
        let mut changed = next.changed_rows(&previous);
        changed.sort_unstable();
        assert_eq!(changed, vec![0, 1]);
    }

    #[test]
    fn incremental_updates_reuse_unchanged_descriptors_and_splice_only_the_group() {
        let mut first = turn();
        let mut second = turn();
        second.identity = TurnIdentity::Submission(2);
        for source in second
            .activity
            .iter_mut()
            .chain(&mut second.response)
            .chain(&mut second.prompt)
        {
            source.entry_index += 2;
        }
        let mut projection = DisplayRows::new(
            DisplayMode::Compact,
            4,
            &[first.clone(), second.clone()],
            |_| false,
            false,
        );
        let allocation = projection.rows.as_ptr();
        let (edits, remeasure) = projection.sync(
            DisplayMode::Compact,
            4,
            &[first.clone(), second.clone()],
            |_| false,
            false,
        );
        assert!(edits.is_empty());
        assert!(remeasure.is_empty());
        assert_eq!(projection.rows.as_ptr(), allocation);
        let historical = projection.rows[..5].to_vec();
        second.lifecycle = TurnLifecycle::Running;
        second.activity.extend(std::mem::take(&mut second.response));
        let (edits, _) = projection.sync(
            DisplayMode::Compact,
            4,
            &[first.clone(), second],
            |_| false,
            true,
        );
        assert_eq!(&projection.rows[..5], historical.as_slice());
        assert!(edits.iter().all(|(range, _)| range.start >= 5));
        first.confidence = SelectionConfidence::Uncertain;
        let (_, remeasure) = projection.sync(DisplayMode::Compact, 4, &[first], |_| false, false);
        assert!(!remeasure.is_empty());
    }

    #[test]
    fn required_actions_and_statuses_remain_visible_when_activity_is_closed() {
        let mut turn = turn();
        let action = source(2, None, SourceIdentity::Tool(Arc::from("permission")));
        let status = source(3, None, SourceIdentity::Compaction(Arc::from("failed")));
        turn.activity.extend([action.clone(), status.clone()]);
        turn.required_actions.push(action.clone());
        turn.statuses.push(status.clone());
        let projection = DisplayRows::new(DisplayMode::Compact, 4, &[turn], |_| false, false);
        assert!(matches!(
            projection
                .rows
                .get(projection.row_for_source(&action).expect("action row")),
            Some(DisplayRow::Content {
                role: SourceRole::RequiredAction,
                ..
            })
        ));
        assert!(matches!(
            projection
                .rows
                .get(projection.row_for_source(&status).expect("status row")),
            Some(DisplayRow::Content {
                role: SourceRole::Status,
                ..
            })
        ));
    }
}
