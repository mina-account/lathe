use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderIdentity {
    Legacy(Option<Arc<str>>),
    Keyed(Arc<str>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceIdentity {
    Prompt(ProviderIdentity),
    Message(ProviderIdentity),
    Thought(ProviderIdentity),
    Tool(Arc<str>),
    Elicitation(Arc<str>),
    Compaction(Arc<str>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceReference {
    pub entry_index: usize,
    pub chunk_index: Option<usize>,
    pub identity: SourceIdentity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentKind {
    Prompt,
    Message,
    Thought,
    Tool,
    Elicitation,
    Compaction,
}

#[derive(Clone, Debug)]
pub struct ContentObservation {
    pub source: SourceReference,
    pub kind: ContentKind,
    pub visible: bool,
    pub pending_action: bool,
    pub failed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivityPhase {
    Idle,
    Running,
    RequiresAction,
}

#[derive(Clone, Debug)]
pub enum SubmissionPhase {
    Pending,
    Accepted {
        prompt: ProviderIdentity,
        echoed: bool,
    },
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct SubmissionObservation {
    pub id: u64,
    pub phase: SubmissionPhase,
}

#[derive(Clone, Debug)]
pub struct ThreadObservation {
    pub contents: Vec<ContentObservation>,
    pub entry_count: usize,
    pub submission: Option<SubmissionObservation>,
    pub submission_updates: Vec<SubmissionObservation>,
    pub reported_activity: bool,
    pub activity: ActivityPhase,
    pub activity_generation: u64,
    pub pending_permission: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnOutcome {
    Success,
    Interrupted,
    Failed,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnLifecycle {
    Submitting,
    Running,
    RequiresAction,
    Settled(TurnOutcome),
    Unaccepted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionConfidence {
    Structural,
    Uncertain,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnIdentity {
    Submission(u64),
    Activity(u64),
    Prompt(SourceReference),
    Orphan,
}

#[derive(Clone, Debug)]
pub struct ClassifiedTurn {
    pub identity: TurnIdentity,
    pub prompt: Option<SourceReference>,
    pub lifecycle: TurnLifecycle,
    pub confidence: SelectionConfidence,
    pub activity: Vec<SourceReference>,
    pub response: Vec<SourceReference>,
    pub required_actions: Vec<SourceReference>,
    pub statuses: Vec<SourceReference>,
    pub inspect_activity: bool,
    pub predecessor: Option<TurnIdentity>,
}

#[derive(Clone, Copy, Debug)]
pub enum ObservationEvent {
    Refresh,
    EntryUpdated(usize),
    EntriesRemoved {
        start: usize,
        end: usize,
    },
    Stopped {
        generation: u64,
        outcome: TurnOutcome,
    },
    Failed,
}

#[derive(Default)]
pub struct ResponseModel {
    pub turns: Vec<ClassifiedTurn>,
    associations: Vec<TurnAssociation>,
    uncertain_sources: Vec<SourceReference>,
    previous_activity: Option<(ActivityPhase, u64)>,
}

struct TurnAssociation {
    identity: TurnIdentity,
    first_entry: usize,
    first_chunk: usize,
    prompt: Option<SourceReference>,
    generation: Option<u64>,
    baseline_generation: u64,
    submission: Option<SubmissionPhase>,
    accepted: bool,
    terminal: Option<TurnOutcome>,
}

impl ResponseModel {
    pub fn observe(&mut self, thread: &ThreadObservation, event: ObservationEvent) {
        if let ObservationEvent::EntriesRemoved { start, end } = event {
            self.associations.retain_mut(|association| {
                if association
                    .prompt
                    .as_ref()
                    .is_some_and(|prompt| (start..end).contains(&prompt.entry_index))
                    || (association.prompt.is_none()
                        && (start..end).contains(&association.first_entry))
                {
                    return false;
                }
                if association.first_entry >= end {
                    association.first_entry = association.first_entry.saturating_sub(end - start);
                }
                if let Some(prompt) = &mut association.prompt
                    && prompt.entry_index >= end
                {
                    prompt.entry_index = prompt.entry_index.saturating_sub(end - start);
                }
                true
            });
            self.uncertain_sources.retain_mut(|source| {
                if (start..end).contains(&source.entry_index) {
                    return false;
                }
                if source.entry_index >= end {
                    source.entry_index = source.entry_index.saturating_sub(end - start);
                }
                true
            });
        }

        let last_prompt = thread
            .contents
            .iter()
            .rev()
            .find(|content| content.kind == ContentKind::Prompt);
        let mut registered_submission = false;
        if let Some(submission) = &thread.submission {
            let identity = TurnIdentity::Submission(submission.id);
            if self
                .associations
                .iter()
                .all(|association| association.identity != identity)
            {
                registered_submission = true;
                let prompt = match &submission.phase {
                    SubmissionPhase::Accepted { prompt, .. } => {
                        thread.contents.iter().find(|content| {
                            content.source.identity == SourceIdentity::Prompt(prompt.clone())
                        })
                    }
                    _ => None,
                };
                let (first_entry, first_chunk) = prompt.map_or_else(
                    || provisional_boundary(thread),
                    |content| (content.source.entry_index, 0),
                );
                self.associations.push(TurnAssociation {
                    identity,
                    first_entry,
                    first_chunk,
                    prompt: prompt.map(|content| content.source.clone()),
                    generation: None,
                    baseline_generation: thread.activity_generation,
                    submission: Some(submission.phase.clone()),
                    accepted: matches!(
                        submission.phase,
                        SubmissionPhase::Accepted { .. } | SubmissionPhase::Completed
                    ),
                    terminal: None,
                });
            }
        }
        for association in &mut self.associations {
            if let Some(submission) = thread
                .submission_updates
                .iter()
                .chain(thread.submission.iter())
                .find(|submission| association.identity == TurnIdentity::Submission(submission.id))
            {
                association.submission = Some(submission.phase.clone());
                association.accepted |= matches!(
                    submission.phase,
                    SubmissionPhase::Accepted { .. } | SubmissionPhase::Completed
                );
            }
            // Echoed older records can be pruned before their update event reaches the view.
            if let Some(SubmissionPhase::Accepted { prompt, echoed }) = &mut association.submission
                && let Some(content) = thread.contents.iter().find(|content| {
                    content.source.identity == SourceIdentity::Prompt(prompt.clone())
                })
            {
                *echoed = true;
                association.prompt = Some(content.source.clone());
                if content.source.entry_index < association.first_entry {
                    association.first_entry = content.source.entry_index;
                    association.first_chunk = 0;
                }
            }
        }
        let new_external_prompt = self.associations.last().is_some_and(|association| {
            association.prompt.as_ref().is_some_and(|prompt| {
                last_prompt.is_some_and(|last| {
                    last.source.entry_index > prompt.entry_index
                        && self
                            .associations
                            .iter()
                            .all(|association| association.prompt.as_ref() != Some(&last.source))
                })
            })
        });
        let matching_stop = matches!(event, ObservationEvent::Stopped { generation, .. }
            if generation == thread.activity_generation && thread.activity == ActivityPhase::Idle);
        if (thread.activity != ActivityPhase::Idle || matching_stop)
            && !registered_submission
            && (new_external_prompt
                || (thread.submission.is_none()
                    && self.associations.last().is_none_or(|association| {
                        matches!(association.identity, TurnIdentity::Submission(_))
                    })))
        {
            self.associations.push(TurnAssociation {
                identity: TurnIdentity::Activity(thread.activity_generation),
                first_entry: last_prompt
                    .map_or(thread.entry_count, |content| content.source.entry_index),
                first_chunk: 0,
                prompt: last_prompt.map(|content| content.source.clone()),
                generation: Some(thread.activity_generation),
                baseline_generation: thread.activity_generation,
                submission: None,
                accepted: false,
                terminal: None,
            });
        }

        let other_prompts: Vec<_> = self
            .associations
            .iter()
            .rev()
            .skip(1)
            .filter_map(|association| association.prompt.as_ref())
            .cloned()
            .collect();
        if let Some(association) = self.associations.last_mut()
            && match association.identity {
                TurnIdentity::Submission(id) => thread
                    .submission
                    .as_ref()
                    .is_some_and(|submission| submission.id == id),
                _ => true,
            }
            && (!new_external_prompt
                || registered_submission
                || matches!(association.identity, TurnIdentity::Activity(_)))
        {
            if matches!(association.identity, TurnIdentity::Activity(_))
                && association.prompt.is_none()
                && let Some(prompt) = thread.contents.iter().find(|content| {
                    content.kind == ContentKind::Prompt
                        && content.source.entry_index >= association.first_entry
                        && !other_prompts.contains(&content.source)
                })
            {
                association.prompt = Some(prompt.source.clone());
            }
            if let Some(submission) = &thread.submission
                && association.identity == TurnIdentity::Submission(submission.id)
            {
                let prompt = match &submission.phase {
                    SubmissionPhase::Accepted { prompt, .. } => {
                        thread.contents.iter().find(|content| {
                            content.source.identity == SourceIdentity::Prompt(prompt.clone())
                        })
                    }
                    _ if !thread.reported_activity
                        || matches!(submission.phase, SubmissionPhase::Pending) =>
                    {
                        thread.contents.iter().find(|content| {
                            content.kind == ContentKind::Prompt
                                && content.source.entry_index >= association.first_entry
                                && !other_prompts.contains(&content.source)
                        })
                    }
                    _ => None,
                };
                if let Some(prompt) = prompt {
                    association.prompt = Some(prompt.source.clone());
                    if prompt.source.entry_index < association.first_entry {
                        association.first_entry = prompt.source.entry_index;
                        association.first_chunk = 0;
                    }
                } else if !thread.reported_activity
                    && association.generation.is_some()
                    && matches!(
                        submission.phase,
                        SubmissionPhase::Completed
                            | SubmissionPhase::Failed
                            | SubmissionPhase::Cancelled
                    )
                {
                    // Legacy retry continues an existing prompt instead of echoing a new one.
                    if let Some(prompt) = last_prompt {
                        association.prompt = Some(prompt.source.clone());
                        association.first_entry = prompt.source.entry_index;
                        association.first_chunk = 0;
                    }
                }
            }
            if thread.activity != ActivityPhase::Idle {
                if association.generation != Some(thread.activity_generation) {
                    association.terminal = None;
                }
                association.generation = Some(thread.activity_generation);
            } else if thread.activity_generation
                > association
                    .generation
                    .unwrap_or(association.baseline_generation)
            {
                // GPUI may deliver a running notification after the same update has already stopped.
                association.generation = Some(thread.activity_generation);
                association.terminal = Some(TurnOutcome::Unknown);
            }
            if let ObservationEvent::Stopped {
                generation,
                outcome,
            } = event
                && generation == thread.activity_generation
                && thread.activity == ActivityPhase::Idle
                && association.generation == Some(generation)
            {
                association.terminal = Some(outcome);
            } else if matches!(event, ObservationEvent::Failed)
                && matches!(association.terminal, None | Some(TurnOutcome::Unknown))
                && association.generation == Some(thread.activity_generation)
            {
                association.terminal = Some(TurnOutcome::Failed);
            } else if thread.activity == ActivityPhase::Idle
                && self.previous_activity.is_some_and(|(phase, generation)| {
                    phase != ActivityPhase::Idle && generation == thread.activity_generation
                })
                && association.generation == Some(thread.activity_generation)
                && association.terminal.is_none()
            {
                association.terminal = Some(TurnOutcome::Unknown);
            }
        }
        self.previous_activity = Some((thread.activity, thread.activity_generation));

        if let ObservationEvent::EntryUpdated(entry_index) = event {
            for (position, content) in thread.contents.iter().enumerate().filter(|(_, content)| {
                content.source.entry_index == entry_index
                    && content.kind == ContentKind::Message
                    && content.visible
            }) {
                if thread
                    .contents
                    .iter()
                    .skip(position + 1)
                    .take_while(|next| next.kind != ContentKind::Prompt)
                    .any(|next| interrupts(next))
                    && !self.uncertain_sources.contains(&content.source)
                {
                    self.uncertain_sources.push(content.source.clone());
                }
            }
        }
        self.turns = self.classify(thread);
    }

    fn classify(&self, thread: &ThreadObservation) -> Vec<ClassifiedTurn> {
        let mut groups: Vec<(
            TurnIdentity,
            Option<SourceReference>,
            Vec<&ContentObservation>,
        )> = Vec::new();
        let mut content_associations = vec![None; thread.contents.len()];
        for (association_index, association) in self.associations.iter().enumerate() {
            let first_position = (association.first_entry, association.first_chunk);
            let next_prompt = thread
                .contents
                .iter()
                .find(|content| {
                    content.kind == ContentKind::Prompt
                        && source_position(&content.source) >= first_position
                        && self
                            .associations
                            .iter()
                            .all(|association| association.prompt.as_ref() != Some(&content.source))
                })
                .map_or((thread.entry_count, 0), |content| {
                    source_position(&content.source)
                });
            let next_association = self
                .associations
                .iter()
                .map(|next| (next.first_entry, next.first_chunk))
                .filter(|position| *position > first_position)
                .min()
                .unwrap_or((thread.entry_count, 0));
            for (content, owner) in thread.contents.iter().zip(&mut content_associations) {
                let position = source_position(&content.source);
                if position >= first_position && position < next_prompt.min(next_association) {
                    *owner = Some(association_index);
                }
            }
        }
        for (content, owner) in thread.contents.iter().zip(content_associations) {
            let association = self
                .associations
                .iter()
                .find(|association| association.prompt.as_ref() == Some(&content.source))
                .or_else(|| owner.and_then(|index| self.associations.get(index)));
            if let Some(association) = association
                && let Some((_, _, contents)) = groups
                    .iter_mut()
                    .find(|(identity, _, _)| identity == &association.identity)
            {
                contents.push(content);
                continue;
            }
            let associated_identity = association.map(|association| association.identity.clone());
            let starts_group = groups.last().is_none_or(|(identity, _, _)| {
                associated_identity
                    .as_ref()
                    .is_some_and(|associated| associated != identity)
                    || (content.kind == ContentKind::Prompt && associated_identity.is_none())
            });
            if starts_group {
                let identity = associated_identity.unwrap_or_else(|| {
                    if content.kind == ContentKind::Prompt {
                        TurnIdentity::Prompt(content.source.clone())
                    } else {
                        TurnIdentity::Orphan
                    }
                });
                let prompt = association
                    .and_then(|association| association.prompt.clone())
                    .or_else(|| {
                        (content.kind == ContentKind::Prompt).then(|| content.source.clone())
                    });
                groups.push((identity, prompt, Vec::new()));
            }
            if let Some((_, _, contents)) = groups.last_mut() {
                contents.push(content);
            }
        }
        for association in &self.associations {
            if association.prompt.is_none()
                && (thread.submission.as_ref().is_some_and(|submission| {
                    association.identity == TurnIdentity::Submission(submission.id)
                }) || (self
                    .associations
                    .last()
                    .is_some_and(|last| last.identity == association.identity)
                    && matches!(association.identity, TurnIdentity::Activity(_))))
                && groups
                    .iter()
                    .all(|(identity, _, _)| identity != &association.identity)
            {
                groups.push((
                    association.identity.clone(),
                    association.prompt.clone(),
                    Vec::new(),
                ));
            }
        }

        let mut predecessor = None;
        let last_identity = groups.last().map(|(identity, _, _)| identity.clone());
        groups
            .into_iter()
            .map(|(identity, prompt, contents)| {
                let association = self
                    .associations
                    .iter()
                    .find(|association| association.identity == identity);
                let current = Some(&identity) == last_identity.as_ref();
                let pending = contents.iter().any(|content| content.pending_action)
                    || (current && thread.pending_permission);
                let failed = contents.iter().any(|content| content.failed);
                let lifecycle =
                    if pending || (current && thread.activity == ActivityPhase::RequiresAction) {
                        TurnLifecycle::RequiresAction
                    } else if current && thread.activity == ActivityPhase::Running {
                        TurnLifecycle::Running
                    } else if let Some(association) = association {
                        match &association.submission {
                            Some(SubmissionPhase::Failed | SubmissionPhase::Cancelled)
                                if association.prompt.is_none()
                                    || (thread.reported_activity && !association.accepted) =>
                            {
                                TurnLifecycle::Unaccepted
                            }
                            Some(
                                SubmissionPhase::Pending
                                | SubmissionPhase::Accepted { echoed: false, .. },
                            ) => TurnLifecycle::Submitting,
                            Some(SubmissionPhase::Accepted { .. })
                                if association.terminal.is_none() =>
                            {
                                TurnLifecycle::Submitting
                            }
                            Some(SubmissionPhase::Failed) => {
                                TurnLifecycle::Settled(TurnOutcome::Failed)
                            }
                            Some(SubmissionPhase::Cancelled) => {
                                TurnLifecycle::Settled(TurnOutcome::Interrupted)
                            }
                            _ => TurnLifecycle::Settled(if failed {
                                TurnOutcome::Failed
                            } else {
                                association.terminal.unwrap_or(TurnOutcome::Unknown)
                            }),
                        }
                    } else {
                        TurnLifecycle::Settled(if failed {
                            TurnOutcome::Failed
                        } else {
                            TurnOutcome::Unknown
                        })
                    };
                let mut selected = Vec::new();
                for content in &contents {
                    if interrupts(content) {
                        selected.clear();
                    } else if content.kind == ContentKind::Message && content.visible {
                        selected.push(content.source.clone());
                    }
                }
                if !matches!(lifecycle, TurnLifecycle::Settled(_))
                    || identity == TurnIdentity::Orphan
                {
                    selected.clear();
                }
                let uncertain = contents
                    .iter()
                    .any(|content| self.uncertain_sources.contains(&content.source));
                let inspect_activity = selected.is_empty()
                    && contents
                        .iter()
                        .any(|content| content.kind == ContentKind::Message && content.visible)
                    && matches!(lifecycle, TurnLifecycle::Settled(_));
                let activity = contents
                    .iter()
                    .filter(|content| {
                        content.kind != ContentKind::Prompt
                            && !content.pending_action
                            && !content.failed
                            && !selected.contains(&content.source)
                    })
                    .map(|content| content.source.clone())
                    .collect();
                let required_actions = contents
                    .iter()
                    .filter(|content| content.pending_action)
                    .map(|content| content.source.clone())
                    .collect();
                let statuses = contents
                    .iter()
                    .filter(|content| content.failed)
                    .map(|content| content.source.clone())
                    .collect();
                let turn = ClassifiedTurn {
                    identity: identity.clone(),
                    prompt,
                    lifecycle,
                    confidence: if uncertain {
                        SelectionConfidence::Uncertain
                    } else {
                        SelectionConfidence::Structural
                    },
                    activity,
                    response: selected,
                    required_actions,
                    statuses,
                    inspect_activity,
                    predecessor: predecessor.clone(),
                };
                if !turn.response.is_empty() {
                    predecessor = Some(identity);
                }
                turn
            })
            .collect()
    }
}

fn source_position(source: &SourceReference) -> (usize, usize) {
    (source.entry_index, source.chunk_index.unwrap_or(0))
}

fn provisional_boundary(thread: &ThreadObservation) -> (usize, usize) {
    thread
        .contents
        .last()
        .and_then(|content| {
            content
                .source
                .chunk_index
                .map(|chunk_index| (content.source.entry_index, chunk_index + 1))
        })
        .unwrap_or((thread.entry_count, 0))
}

fn interrupts(content: &ContentObservation) -> bool {
    matches!(content.kind, ContentKind::Tool | ContentKind::Elicitation)
        || (content.kind == ContentKind::Thought && content.visible)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content(
        entry_index: usize,
        chunk_index: Option<usize>,
        kind: ContentKind,
        name: &str,
    ) -> ContentObservation {
        let provider = ProviderIdentity::Keyed(name.into());
        let identity = match kind {
            ContentKind::Prompt => SourceIdentity::Prompt(provider),
            ContentKind::Message => SourceIdentity::Message(provider),
            ContentKind::Thought => SourceIdentity::Thought(provider),
            ContentKind::Tool => SourceIdentity::Tool(name.into()),
            ContentKind::Elicitation => SourceIdentity::Elicitation(name.into()),
            ContentKind::Compaction => SourceIdentity::Compaction(name.into()),
        };
        ContentObservation {
            source: SourceReference {
                entry_index,
                chunk_index,
                identity,
            },
            kind,
            visible: true,
            pending_action: false,
            failed: false,
        }
    }

    fn history(kinds: &[ContentKind]) -> ThreadObservation {
        ThreadObservation {
            contents: kinds
                .iter()
                .enumerate()
                .map(|(index, kind)| content(index, None, *kind, &format!("source-{index}")))
                .collect(),
            entry_count: kinds.len(),
            submission: None,
            submission_updates: Vec::new(),
            reported_activity: true,
            activity: ActivityPhase::Idle,
            activity_generation: 0,
            pending_permission: false,
        }
    }

    fn classified(thread: &ThreadObservation) -> Vec<ClassifiedTurn> {
        let mut model = ResponseModel::default();
        model.observe(thread, ObservationEvent::Refresh);
        model.turns
    }

    #[test]
    fn adjacent_commentary_is_structural_output_but_thought_interrupts() {
        use ContentKind::*;
        let thread = history(&[Prompt, Message, Message]);
        let turns = classified(&thread);
        let turn = turns.first().expect("prompted turn");
        assert_eq!(turn.response.len(), 2);
        assert_eq!(turn.lifecycle, TurnLifecycle::Settled(TurnOutcome::Unknown));
        assert_eq!(turn.confidence, SelectionConfidence::Structural);
        let turns = classified(&history(&[Prompt, Message, Thought, Message]));
        let turn = turns.first().expect("prompted turn");
        assert_eq!(turn.response.len(), 1);
        assert_eq!(
            turn.response.first().map(|source| source.entry_index),
            Some(3)
        );
        assert_eq!(turn.activity.len(), 2);
    }

    #[test]
    fn trailing_compaction_and_empty_chunks_are_transparent() {
        use ContentKind::*;
        let mut thread = history(&[Prompt, Message, Thought, Compaction]);
        thread.contents.get_mut(2).expect("thought").visible = false;
        let turns = classified(&thread);
        let turn = turns.first().expect("turn");
        assert_eq!(
            turn.response.first().map(|source| source.entry_index),
            Some(1)
        );
        assert_eq!(turn.activity.len(), 2);
        thread.contents.get_mut(3).expect("compaction").failed = true;
        let turns = classified(&thread);
        let turn = turns.first().expect("turn");
        assert_eq!(turn.lifecycle, TurnLifecycle::Settled(TurnOutcome::Failed));
        assert_eq!(turn.statuses.len(), 1);
    }

    #[test]
    fn pending_and_resolved_elicitations_interrupt_output() {
        use ContentKind::*;
        let mut thread = history(&[Prompt, Message, Elicitation]);
        thread
            .contents
            .get_mut(2)
            .expect("elicitation")
            .pending_action = true;
        let turns = classified(&thread);
        let turn = turns.first().expect("turn");
        assert_eq!(turn.lifecycle, TurnLifecycle::RequiresAction);
        assert!(turn.response.is_empty());
        assert_eq!(turn.required_actions.len(), 1);
        thread
            .contents
            .get_mut(2)
            .expect("elicitation")
            .pending_action = false;
        let turns = classified(&thread);
        let turn = turns.first().expect("turn");
        assert_eq!(turn.lifecycle, TurnLifecycle::Settled(TurnOutcome::Unknown));
        assert!(turn.response.is_empty());
        assert!(turn.inspect_activity);
        assert_eq!(turn.activity.len(), 2);
    }

    #[test]
    fn keyed_updates_do_not_move_messages_across_tools() {
        use ContentKind::*;
        let thread = history(&[Prompt, Message, Tool]);
        let mut model = ResponseModel::default();
        model.observe(&thread, ObservationEvent::Refresh);
        model.observe(&thread, ObservationEvent::EntryUpdated(1));
        let turn = model.turns.first().expect("turn");
        assert!(turn.response.is_empty());
        assert_eq!(turn.confidence, SelectionConfidence::Uncertain);
        assert!(turn.inspect_activity);
        let mut thread = thread;
        thread.contents.push(content(3, Some(0), Message, "answer"));
        thread.entry_count += 1;
        model.observe(&thread, ObservationEvent::EntryUpdated(3));
        let turn = model.turns.first().expect("turn");
        assert_eq!(
            turn.response.first().map(|source| source.entry_index),
            Some(3)
        );
        assert_eq!(turn.confidence, SelectionConfidence::Uncertain);
        let restored = classified(&thread);
        assert_eq!(
            restored.first().expect("turn").confidence,
            SelectionConfidence::Structural
        );
    }

    #[test]
    fn response_preserves_chunk_identities_and_skips_empty_predecessors() {
        use ContentKind::*;
        let mut thread = history(&[Thought, Prompt, Message, Prompt, Tool, Prompt]);
        thread.contents.extend([
            content(6, Some(0), Message, "answer-a"),
            content(6, Some(1), Message, "answer-b"),
        ]);
        thread.entry_count = 7;
        let turns = classified(&thread);
        assert_eq!(turns.len(), 4);
        let response = turns.last().expect("last turn");
        assert_eq!(response.response.len(), 2);
        assert_eq!(
            response.response,
            thread
                .contents
                .iter()
                .skip(6)
                .map(|item| item.source.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            response.predecessor,
            turns.get(1).map(|turn| turn.identity.clone())
        );
        assert!(turns.get(2).expect("tool-only turn").response.is_empty());
        assert!(classified(&history(&[])).is_empty());
        assert!(
            classified(&history(&[Prompt]))
                .first()
                .expect("empty turn")
                .response
                .is_empty()
        );
    }

    fn submitting() -> ThreadObservation {
        let mut thread = history(&[]);
        thread.submission = Some(SubmissionObservation {
            id: 1,
            phase: SubmissionPhase::Pending,
        });
        thread
    }

    fn accept(thread: &mut ThreadObservation, echoed: bool) {
        thread.submission.as_mut().expect("submission").phase = SubmissionPhase::Accepted {
            prompt: ProviderIdentity::Keyed("prompt".into()),
            echoed,
        };
    }

    fn stop(
        model: &mut ResponseModel,
        thread: &ThreadObservation,
        generation: u64,
        outcome: TurnOutcome,
    ) {
        model.observe(
            thread,
            ObservationEvent::Stopped {
                generation,
                outcome,
            },
        );
    }

    #[test]
    fn accepted_echoed_submission_waits_for_activity_and_matching_stop() {
        use ContentKind::*;
        let mut model = ResponseModel::default();
        let mut thread = submitting();
        model.observe(&thread, ObservationEvent::Refresh);
        accept(&mut thread, true);
        thread.contents = vec![
            content(0, None, Prompt, "prompt"),
            content(1, Some(0), Message, "answer"),
        ];
        thread.entry_count = 2;
        model.observe(&thread, ObservationEvent::Refresh);
        assert_eq!(
            model.turns.first().expect("turn").lifecycle,
            TurnLifecycle::Submitting
        );
        thread.activity = ActivityPhase::Running;
        thread.activity_generation = 1;
        model.observe(&thread, ObservationEvent::Refresh);
        stop(&mut model, &thread, 1, TurnOutcome::Success);
        assert_eq!(
            model.turns.first().expect("turn").lifecycle,
            TurnLifecycle::Running
        );
        thread.activity = ActivityPhase::Idle;
        stop(&mut model, &thread, 0, TurnOutcome::Success);
        assert_ne!(
            model.turns.first().expect("turn").lifecycle,
            TurnLifecycle::Settled(TurnOutcome::Success)
        );
        stop(&mut model, &thread, 1, TurnOutcome::Success);
        assert_eq!(
            model.turns.first().expect("turn").lifecycle,
            TurnLifecycle::Settled(TurnOutcome::Success)
        );
    }

    #[test]
    fn delayed_echo_reconciles_provisional_activity_without_duplicate_answer() {
        use ContentKind::*;
        let mut model = ResponseModel::default();
        let mut thread = submitting();
        model.observe(&thread, ObservationEvent::Refresh);
        accept(&mut thread, false);
        thread.activity = ActivityPhase::Running;
        thread.activity_generation = 1;
        thread.contents.push(content(0, Some(0), Message, "answer"));
        thread.entry_count = 1;
        model.observe(&thread, ObservationEvent::Refresh);
        thread.activity = ActivityPhase::Idle;
        stop(&mut model, &thread, 1, TurnOutcome::Success);
        assert_eq!(
            model.turns.first().expect("turn").lifecycle,
            TurnLifecycle::Submitting
        );
        accept(&mut thread, true);
        thread.contents.push(content(1, None, Prompt, "prompt"));
        thread.entry_count = 2;
        model.observe(&thread, ObservationEvent::Refresh);
        assert_eq!(model.turns.len(), 1);
        let turn = model.turns.first().expect("turn");
        assert_eq!(turn.identity, TurnIdentity::Submission(1));
        assert_eq!(
            turn.prompt.as_ref().map(|source| source.entry_index),
            Some(1)
        );
        assert_eq!(turn.lifecycle, TurnLifecycle::Settled(TurnOutcome::Success));
        assert_eq!(
            turn.response.first().map(|source| source.entry_index),
            Some(0)
        );
        thread.activity = ActivityPhase::Running;
        thread.activity_generation = 2;
        model.observe(&thread, ObservationEvent::Refresh);
        assert!(model.turns.first().expect("turn").response.is_empty());
        assert_eq!(
            model.turns.first().expect("turn").lifecycle,
            TurnLifecycle::Running
        );
        thread.activity = ActivityPhase::Idle;
        stop(&mut model, &thread, 1, TurnOutcome::Success);
        assert_ne!(
            model.turns.first().expect("turn").lifecycle,
            TurnLifecycle::Settled(TurnOutcome::Success)
        );
        stop(&mut model, &thread, 2, TurnOutcome::Interrupted);
        assert_eq!(
            model.turns.first().expect("turn").lifecycle,
            TurnLifecycle::Settled(TurnOutcome::Interrupted)
        );
    }

    #[test]
    fn action_resolution_does_not_complete_running_work() {
        let mut model = ResponseModel::default();
        let mut thread = submitting();
        accept(&mut thread, true);
        thread
            .contents
            .push(content(0, None, ContentKind::Prompt, "prompt"));
        thread.entry_count = 1;
        thread.activity = ActivityPhase::RequiresAction;
        thread.activity_generation = 1;
        model.observe(&thread, ObservationEvent::Refresh);
        assert_eq!(
            model.turns.first().expect("turn").lifecycle,
            TurnLifecycle::RequiresAction
        );
        thread.activity = ActivityPhase::Running;
        model.observe(&thread, ObservationEvent::Refresh);
        assert_eq!(
            model.turns.first().expect("turn").lifecycle,
            TurnLifecycle::Running
        );
        thread.pending_permission = true;
        thread.activity = ActivityPhase::Idle;
        stop(&mut model, &thread, 1, TurnOutcome::Success);
        assert_eq!(
            model.turns.first().expect("turn").lifecycle,
            TurnLifecycle::RequiresAction
        );
        thread.pending_permission = false;
        model.observe(&thread, ObservationEvent::Refresh);
        assert_eq!(
            model.turns.first().expect("turn").lifecycle,
            TurnLifecycle::Settled(TurnOutcome::Success)
        );
    }

    #[test]
    fn legacy_terminal_transition_without_stop_is_unknown_until_guarded_completion() {
        use ContentKind::*;
        let mut model = ResponseModel::default();
        let mut thread = submitting();
        thread.reported_activity = false;
        model.observe(&thread, ObservationEvent::Refresh);
        thread.activity = ActivityPhase::Running;
        thread.activity_generation = 1;
        thread.contents = vec![
            content(0, None, Prompt, "prompt"),
            content(1, Some(0), Message, "answer"),
        ];
        thread.entry_count = 2;
        model.observe(&thread, ObservationEvent::Refresh);
        thread.activity = ActivityPhase::Idle;
        model.observe(&thread, ObservationEvent::Refresh);
        thread.submission.as_mut().expect("submission").phase = SubmissionPhase::Completed;
        model.observe(&thread, ObservationEvent::Refresh);
        assert_eq!(
            model.turns.first().expect("turn").lifecycle,
            TurnLifecycle::Settled(TurnOutcome::Unknown)
        );
        stop(&mut model, &thread, 1, TurnOutcome::Success);
        assert_eq!(
            model.turns.first().expect("turn").lifecycle,
            TurnLifecycle::Settled(TurnOutcome::Success)
        );
        thread.submission.as_mut().expect("submission").phase = SubmissionPhase::Cancelled;
        model.observe(&thread, ObservationEvent::Refresh);
        assert_eq!(
            model.turns.first().expect("turn").lifecycle,
            TurnLifecycle::Settled(TurnOutcome::Interrupted)
        );
    }

    #[test]
    fn failed_unaccepted_submission_does_not_invent_response() {
        let mut model = ResponseModel::default();
        let mut thread = submitting();
        model.observe(&thread, ObservationEvent::Refresh);
        thread.submission.as_mut().expect("submission").phase = SubmissionPhase::Failed;
        model.observe(&thread, ObservationEvent::Failed);
        let turn = model.turns.first().expect("submission");
        assert_eq!(turn.lifecycle, TurnLifecycle::Unaccepted);
        assert!(turn.response.is_empty());
    }

    #[test]
    fn later_failure_preserves_earlier_success_and_removal_invalidates_association() {
        use ContentKind::*;
        let mut thread = submitting();
        let mut model = ResponseModel::default();
        model.observe(&thread, ObservationEvent::Refresh);
        accept(&mut thread, true);
        thread.contents = vec![
            content(0, None, Prompt, "prompt"),
            content(1, Some(0), Message, "answer"),
        ];
        thread.entry_count = 2;
        thread.activity = ActivityPhase::Running;
        thread.activity_generation = 1;
        model.observe(&thread, ObservationEvent::Refresh);
        thread.activity = ActivityPhase::Idle;
        stop(&mut model, &thread, 1, TurnOutcome::Success);
        thread.submission = Some(SubmissionObservation {
            id: 2,
            phase: SubmissionPhase::Pending,
        });
        model.observe(&thread, ObservationEvent::Refresh);
        thread.contents.extend([
            content(2, None, Prompt, "prompt-2"),
            content(3, Some(0), Message, "partial"),
        ]);
        thread.entry_count = 4;
        thread.submission.as_mut().expect("submission").phase = SubmissionPhase::Accepted {
            prompt: ProviderIdentity::Keyed("prompt-2".into()),
            echoed: true,
        };
        thread.activity_generation = 2;
        thread.activity = ActivityPhase::Running;
        model.observe(&thread, ObservationEvent::Refresh);
        thread.activity = ActivityPhase::Idle;
        model.observe(&thread, ObservationEvent::Failed);
        assert_eq!(
            model.turns.first().expect("earlier turn").lifecycle,
            TurnLifecycle::Settled(TurnOutcome::Success)
        );
        let later = model.turns.last().expect("later turn");
        assert_eq!(later.lifecycle, TurnLifecycle::Settled(TurnOutcome::Failed));
        assert_eq!(later.predecessor, Some(TurnIdentity::Submission(1)));
        thread.contents.truncate(2);
        thread.entry_count = 2;
        thread.submission = None;
        model.observe(
            &thread,
            ObservationEvent::EntriesRemoved { start: 2, end: 4 },
        );
        assert_eq!(model.turns.len(), 1);
        assert_eq!(
            model.turns.first().expect("remaining turn").lifecycle,
            TurnLifecycle::Settled(TurnOutcome::Success)
        );
    }

    #[test]
    fn externally_added_prompt_bounds_an_earlier_live_association() {
        use ContentKind::*;
        let mut thread = submitting();
        let mut model = ResponseModel::default();
        model.observe(&thread, ObservationEvent::Refresh);
        accept(&mut thread, true);
        thread.contents = vec![
            content(0, None, Prompt, "prompt"),
            content(1, Some(0), Message, "answer"),
        ];
        thread.entry_count = 2;
        thread.activity = ActivityPhase::Running;
        thread.activity_generation = 1;
        model.observe(&thread, ObservationEvent::Refresh);
        thread.activity = ActivityPhase::Idle;
        stop(&mut model, &thread, 1, TurnOutcome::Success);
        thread.contents.extend([
            content(2, None, Prompt, "external-prompt"),
            content(3, Some(0), Message, "external-answer"),
        ]);
        thread.entry_count = 4;
        model.observe(&thread, ObservationEvent::Refresh);
        assert_eq!(model.turns.len(), 2);
        assert_eq!(
            model.turns.first().expect("earlier").lifecycle,
            TurnLifecycle::Settled(TurnOutcome::Success)
        );
        let external = model.turns.last().expect("external");
        assert_eq!(
            external.lifecycle,
            TurnLifecycle::Settled(TurnOutcome::Unknown)
        );
        assert_eq!(
            external.response.first().map(|source| source.entry_index),
            Some(3)
        );
        thread.activity = ActivityPhase::Running;
        thread.activity_generation = 2;
        model.observe(&thread, ObservationEvent::Refresh);
        assert_eq!(model.turns.len(), 2);
        assert_eq!(
            model.turns.first().expect("earlier").lifecycle,
            TurnLifecycle::Settled(TurnOutcome::Success)
        );
        assert_eq!(
            model.turns.last().expect("external").lifecycle,
            TurnLifecycle::Running
        );
        assert!(model.turns.last().expect("external").response.is_empty());
    }

    #[test]
    fn in_place_updates_consider_chunk_order_within_shared_entries() {
        use ContentKind::*;
        let mut thread = history(&[Prompt]);
        thread.contents.extend([
            content(1, Some(0), Message, "commentary"),
            content(1, Some(1), Thought, "thought"),
            content(1, Some(2), Message, "answer"),
        ]);
        thread.entry_count = 2;
        let mut model = ResponseModel::default();
        model.observe(&thread, ObservationEvent::Refresh);
        model.observe(&thread, ObservationEvent::EntryUpdated(1));
        let turn = model.turns.first().expect("turn");
        assert_eq!(turn.confidence, SelectionConfidence::Uncertain);
        assert_eq!(
            turn.response,
            vec![thread.contents.last().expect("answer").source.clone()]
        );
        assert_eq!(turn.activity.len(), 2);
    }

    #[test]
    fn echo_before_acceptance_and_before_idle_is_one_running_turn() {
        use ContentKind::*;
        let mut thread = submitting();
        let mut model = ResponseModel::default();
        model.observe(&thread, ObservationEvent::Refresh);
        thread.contents.extend([
            content(0, None, Prompt, "prompt"),
            content(1, Some(0), Message, "answer"),
        ]);
        thread.entry_count = 2;
        thread.activity = ActivityPhase::Running;
        thread.activity_generation = 1;
        model.observe(&thread, ObservationEvent::Refresh);
        assert!(model.turns.iter().all(|turn| turn.response.is_empty()));
        accept(&mut thread, true);
        model.observe(&thread, ObservationEvent::Refresh);
        assert_eq!(model.turns.len(), 1);
        assert_eq!(
            model.turns.first().expect("turn").lifecycle,
            TurnLifecycle::Running
        );
        assert!(model.turns.first().expect("turn").response.is_empty());
        thread.activity = ActivityPhase::Idle;
        stop(&mut model, &thread, 1, TurnOutcome::Unknown);
        assert_eq!(
            model.turns.first().expect("turn").lifecycle,
            TurnLifecycle::Settled(TurnOutcome::Unknown)
        );
    }

    #[test]
    fn restored_orphan_activity_and_unaccepted_legacy_failure_stay_out_of_answers() {
        use ContentKind::*;
        let thread = history(&[Message, Tool, Message]);
        let turns = classified(&thread);
        let orphan = turns.first().expect("orphan");
        assert_eq!(orphan.identity, TurnIdentity::Orphan);
        assert!(orphan.response.is_empty());
        assert_eq!(orphan.activity.len(), 3);
        let mut thread = history(&[Prompt, Message]);
        thread.reported_activity = false;
        thread.submission = Some(SubmissionObservation {
            id: 1,
            phase: SubmissionPhase::Pending,
        });
        let mut model = ResponseModel::default();
        model.observe(&thread, ObservationEvent::Refresh);
        thread.submission.as_mut().expect("submission").phase = SubmissionPhase::Failed;
        model.observe(&thread, ObservationEvent::Failed);
        assert_eq!(model.turns.len(), 2);
        assert_eq!(
            model.turns.first().expect("historical").lifecycle,
            TurnLifecycle::Settled(TurnOutcome::Unknown)
        );
        assert_eq!(
            model.turns.last().expect("failed submission").lifecycle,
            TurnLifecycle::Unaccepted
        );
        assert!(
            model
                .turns
                .last()
                .expect("failed submission")
                .response
                .is_empty()
        );
    }

    #[test]
    fn batched_running_and_idle_notifications_use_authoritative_generation() {
        use ContentKind::*;
        let mut thread = submitting();
        let mut model = ResponseModel::default();
        model.observe(&thread, ObservationEvent::Refresh);
        accept(&mut thread, true);
        thread.contents = vec![
            content(0, None, Prompt, "prompt"),
            content(1, Some(0), Message, "answer"),
        ];
        thread.entry_count = 2;
        thread.activity_generation = 1;
        stop(&mut model, &thread, 1, TurnOutcome::Success);
        assert_eq!(
            model.turns.first().expect("batched turn").lifecycle,
            TurnLifecycle::Settled(TurnOutcome::Success)
        );
        thread.activity_generation = 2;
        stop(&mut model, &thread, 1, TurnOutcome::Success);
        assert_eq!(
            model.turns.first().expect("resumed turn").lifecycle,
            TurnLifecycle::Settled(TurnOutcome::Unknown)
        );
        stop(&mut model, &thread, 2, TurnOutcome::Interrupted);
        assert_eq!(
            model.turns.first().expect("resumed turn").lifecycle,
            TurnLifecycle::Settled(TurnOutcome::Interrupted)
        );
    }

    #[test]
    fn idle_notification_before_error_does_not_hide_generation_failure() {
        use ContentKind::*;
        let mut thread = submitting();
        let mut model = ResponseModel::default();
        model.observe(&thread, ObservationEvent::Refresh);
        thread.reported_activity = false;
        thread.contents = vec![
            content(0, None, Prompt, "prompt"),
            content(1, Some(0), Message, "partial"),
        ];
        thread.entry_count = 2;
        thread.activity_generation = 1;
        thread.activity = ActivityPhase::Running;
        model.observe(&thread, ObservationEvent::Refresh);
        thread.activity = ActivityPhase::Idle;
        model.observe(&thread, ObservationEvent::Refresh);
        model.observe(&thread, ObservationEvent::Failed);
        thread.submission.as_mut().expect("submission").phase = SubmissionPhase::Failed;
        model.observe(&thread, ObservationEvent::Refresh);
        assert_eq!(
            model.turns.first().expect("failed turn").lifecycle,
            TurnLifecycle::Settled(TurnOutcome::Failed)
        );
        assert_eq!(model.turns.first().expect("partial").response.len(), 1);
    }

    #[test]
    fn external_terminal_events_require_current_idle_generation() {
        use ContentKind::*;
        let mut thread = history(&[Prompt, Message]);
        thread.activity_generation = 1;
        let mut model = ResponseModel::default();
        model.observe(&thread, ObservationEvent::Refresh);
        stop(&mut model, &thread, 0, TurnOutcome::Success);
        assert!(model.associations.is_empty());
        assert_eq!(
            model.turns.first().expect("history").lifecycle,
            TurnLifecycle::Settled(TurnOutcome::Unknown)
        );
        stop(&mut model, &thread, 1, TurnOutcome::Success);
        assert_eq!(model.associations.len(), 1);
        assert_eq!(
            model.turns.first().expect("external").lifecycle,
            TurnLifecycle::Settled(TurnOutcome::Success)
        );
        thread.activity = ActivityPhase::Running;
        thread.activity_generation = 2;
        stop(&mut model, &thread, 1, TurnOutcome::Success);
        assert_eq!(model.associations.len(), 1);
        assert_eq!(
            model.turns.first().expect("running").lifecycle,
            TurnLifecycle::Running
        );
        stop(&mut model, &thread, 2, TurnOutcome::Success);
        assert_eq!(
            model.turns.first().expect("still running").lifecycle,
            TurnLifecycle::Running
        );
        assert!(
            model
                .turns
                .first()
                .expect("still running")
                .response
                .is_empty()
        );
        thread.activity = ActivityPhase::Idle;
        stop(&mut model, &thread, 2, TurnOutcome::Interrupted);
        assert_eq!(
            model.turns.first().expect("cancelled").lifecycle,
            TurnLifecycle::Settled(TurnOutcome::Interrupted)
        );
    }

    #[test]
    fn activity_before_any_prompt_has_a_provisional_group() {
        let mut thread = history(&[]);
        thread.activity = ActivityPhase::Running;
        thread.activity_generation = 1;
        let mut model = ResponseModel::default();
        model.observe(&thread, ObservationEvent::Refresh);
        assert_eq!(model.turns.len(), 1);
        let turn = model.turns.first().expect("provisional activity");
        assert_eq!(turn.identity, TurnIdentity::Activity(1));
        assert_eq!(turn.lifecycle, TurnLifecycle::Running);
        assert!(turn.response.is_empty());
    }

    #[test]
    fn activity_without_local_submission_reconciles_its_first_prompt() {
        use ContentKind::*;
        let mut thread = history(&[]);
        thread.activity = ActivityPhase::Running;
        thread.activity_generation = 1;
        let mut model = ResponseModel::default();
        model.observe(&thread, ObservationEvent::Refresh);
        thread.contents.push(content(0, Some(0), Message, "answer"));
        thread.entry_count = 1;
        model.observe(&thread, ObservationEvent::Refresh);
        thread
            .contents
            .push(content(1, None, Prompt, "remote-prompt"));
        thread.entry_count = 2;
        model.observe(&thread, ObservationEvent::Refresh);
        assert_eq!(model.turns.len(), 1);
        let turn = model.turns.first().expect("remote turn");
        assert_eq!(turn.identity, TurnIdentity::Activity(1));
        assert_eq!(turn.lifecycle, TurnLifecycle::Running);
        assert!(turn.response.is_empty());
        thread.activity = ActivityPhase::Idle;
        stop(&mut model, &thread, 1, TurnOutcome::Success);
        assert_eq!(model.turns.first().expect("remote turn").response.len(), 1);
    }

    #[test]
    fn unaccepted_receipt_failure_does_not_publish_a_provisional_echo() {
        use ContentKind::*;
        let mut thread = submitting();
        let mut model = ResponseModel::default();
        model.observe(&thread, ObservationEvent::Refresh);
        thread.contents = vec![
            content(0, None, Prompt, "prompt"),
            content(1, Some(0), Message, "text"),
        ];
        thread.entry_count = 2;
        model.observe(&thread, ObservationEvent::Refresh);
        thread.submission.as_mut().expect("submission").phase = SubmissionPhase::Failed;
        model.observe(&thread, ObservationEvent::Failed);
        let turn = model.turns.first().expect("provisional turn");
        assert_eq!(turn.lifecycle, TurnLifecycle::Unaccepted);
        assert!(turn.response.is_empty());
    }
}
