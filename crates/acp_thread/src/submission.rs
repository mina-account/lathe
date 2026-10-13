use crate::ReceiptSessionSubmissions;
use agent_client_protocol::schema::{v1 as acp_v1, v2 as acp_v2};
use futures::{channel::oneshot, future::BoxFuture};
use gpui::{SharedString, Task};
use std::collections::{BTreeMap, HashSet};
use std::{
    fmt,
    future::Future,
    pin::Pin,
    rc::Rc,
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, Instant},
};

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SubmissionId(pub(crate) u64);

impl SubmissionId {
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

#[derive(Debug)]
pub enum SubmissionResponse {
    LegacyCompleted(acp_v1::PromptResponse),
    Accepted(acp_v2::PromptResponse),
}

pub struct Submission {
    pub id: SubmissionId,
    pub response: BoxFuture<'static, anyhow::Result<Option<SubmissionResponse>>>,
}

impl Submission {
    pub(crate) fn new(
        id: SubmissionId,
        response: BoxFuture<'static, anyhow::Result<Option<SubmissionResponse>>>,
    ) -> Self {
        Self { id, response }
    }
}

impl Future for Submission {
    type Output = anyhow::Result<Option<SubmissionResponse>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.get_mut().response.as_mut().poll(cx)
    }
}

#[derive(Debug)]
pub enum SubmissionState {
    Pending,
    Accepted {
        receipt: acp_v2::PromptResponse,
        echoed: bool,
    },
    Completed,
    Failed(SharedString),
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmissionKind {
    Prompt,
    Command,
    Retry,
}

#[derive(Clone, Debug)]
pub enum LegacySubmissionCompletion {
    Stopped(acp_v1::StopReason),
    Failed,
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct SubmissionActivity {
    pub generation: u64,
    pub completion: Option<Option<acp_v2::StopReason>>,
    pub boundary: (usize, usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentationOwner {
    Submission(SubmissionId),
    Activity(u64),
}

pub struct SubmissionRecord {
    pub content: Arc<[acp_v2::ContentBlock]>,
    pub kind: SubmissionKind,
    pub state: SubmissionState,
    task: Option<Task<()>>,
}

impl fmt::Debug for SubmissionRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SubmissionRecord")
            .field("content", &self.content)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl SubmissionRecord {
    fn is_unsettled(&self) -> bool {
        matches!(
            self.state,
            SubmissionState::Pending | SubmissionState::Accepted { echoed: false, .. }
        )
    }

    fn is_recoverable(&self) -> bool {
        self.is_unsettled()
            || matches!(
                self.state,
                SubmissionState::Failed(_) | SubmissionState::Cancelled
            )
    }
}

pub(crate) struct SessionSubmissions {
    receipt_transport: Option<Rc<dyn ReceiptSessionSubmissions>>,
    records: BTreeMap<SubmissionId, SubmissionRecord>,
    receipts: BTreeMap<SubmissionId, acp_v2::MessageId>,
    legacy_completions: BTreeMap<SubmissionId, (SubmissionKind, LegacySubmissionCompletion)>,
    legacy_prompt_entries: BTreeMap<SubmissionId, usize>,
    activities: BTreeMap<SubmissionId, SubmissionActivity>,
    activity_owner: Option<SubmissionId>,
    resumed_activity: Option<(SubmissionId, SubmissionActivity)>,
    unmatched_activity_prompts: Vec<acp_v2::MessageId>,
    external_activity_prompt: Option<acp_v2::MessageId>,
    activity_boundary: (usize, usize),
    content_owners: BTreeMap<(usize, Option<usize>), PresentationOwner>,
    generation_owners: BTreeMap<u64, SubmissionId>,
    submission_boundaries: BTreeMap<SubmissionId, (usize, usize)>,
    next_id: u64,
    latest_id: Option<SubmissionId>,
}

impl SessionSubmissions {
    pub(crate) fn new(receipt_transport: Option<Rc<dyn ReceiptSessionSubmissions>>) -> Self {
        Self {
            receipt_transport,
            records: BTreeMap::default(),
            receipts: BTreeMap::default(),
            legacy_completions: BTreeMap::default(),
            legacy_prompt_entries: BTreeMap::default(),
            activities: BTreeMap::default(),
            activity_owner: None,
            resumed_activity: None,
            unmatched_activity_prompts: Vec::default(),
            external_activity_prompt: None,
            activity_boundary: (0, 0),
            content_owners: BTreeMap::default(),
            generation_owners: BTreeMap::default(),
            submission_boundaries: BTreeMap::default(),
            next_id: 0,
            latest_id: None,
        }
    }

    pub(crate) fn receipt_transport(&self) -> Option<&Rc<dyn ReceiptSessionSubmissions>> {
        self.receipt_transport.as_ref()
    }

    pub(crate) fn get(&self, id: SubmissionId) -> Option<&SubmissionRecord> {
        self.records.get(&id)
    }

    pub(crate) fn receipts(&self) -> impl Iterator<Item = (SubmissionId, &acp_v2::MessageId)> {
        self.receipts
            .iter()
            .map(|(id, message_id)| (*id, message_id))
    }

    pub(crate) fn latest_id(&self) -> Option<SubmissionId> {
        self.latest_id
    }

    pub(crate) fn legacy_prompt_entry(&self, id: SubmissionId) -> Option<usize> {
        self.legacy_prompt_entries.get(&id).copied()
    }

    pub(crate) fn associate_legacy_prompt(&mut self, id: SubmissionId, entry_index: usize) {
        self.legacy_prompt_entries.insert(id, entry_index);
    }

    pub(crate) fn truncate_prompt_entries(&mut self, index: usize) {
        self.legacy_prompt_entries
            .retain(|_, entry_index| *entry_index < index);
        if self.activity_boundary.0 >= index {
            self.activity_boundary = (index, 0);
        }
        self.content_owners.retain(|(entry, _), _| *entry < index);
        for activity in self.activities.values_mut() {
            if activity.boundary.0 >= index {
                activity.boundary = (index, 0);
            }
        }
        for boundary in self.submission_boundaries.values_mut() {
            if boundary.0 >= index {
                *boundary = (index, 0);
            }
        }
    }

    pub(crate) fn activity(&self, id: SubmissionId) -> Option<&SubmissionActivity> {
        self.activities.get(&id)
    }

    pub(crate) fn associate_boundary(&mut self, id: SubmissionId, boundary: (usize, usize)) {
        self.submission_boundaries.insert(id, boundary);
    }

    pub(crate) fn submission_boundary(&self, id: SubmissionId) -> Option<(usize, usize)> {
        self.activities
            .get(&id)
            .map(|activity| activity.boundary)
            .or_else(|| self.submission_boundaries.get(&id).copied())
    }

    pub(crate) fn external_activity_prompt(&self) -> Option<&acp_v2::MessageId> {
        self.external_activity_prompt.as_ref()
    }

    pub(crate) fn activity_boundary(&self) -> (usize, usize) {
        self.activity_boundary
    }

    fn awaiting_activity(&self) -> Option<SubmissionId> {
        self.records
            .iter()
            .filter_map(|(id, record)| {
                (matches!(
                    record.state,
                    SubmissionState::Pending | SubmissionState::Accepted { .. }
                ) && !self.activities.contains_key(id))
                .then_some(*id)
            })
            .chain(self.receipts.keys().copied().filter(|id| {
                !self.activities.contains_key(id)
                    && self.records.get(id).is_none_or(|record| {
                        matches!(
                            record.state,
                            SubmissionState::Pending | SubmissionState::Accepted { .. }
                        )
                    })
            }))
            .min()
    }

    pub(crate) fn presentation_owner(
        &self,
        generation: u64,
        active: bool,
    ) -> Option<PresentationOwner> {
        if active {
            // A delayed external echo can disprove the tentative submission resumption.
            Some(PresentationOwner::Activity(generation))
        } else {
            self.awaiting_activity().map(PresentationOwner::Submission)
        }
    }

    pub(crate) fn resolve_owner(&self, owner: PresentationOwner) -> PresentationOwner {
        match owner {
            PresentationOwner::Activity(generation) => self
                .generation_owners
                .get(&generation)
                .copied()
                .map_or(owner, PresentationOwner::Submission),
            _ => owner,
        }
    }

    pub(crate) fn observe_content(
        &mut self,
        position: (usize, Option<usize>),
        generation: u64,
        active: bool,
    ) {
        if self.receipt_transport.is_none() {
            return;
        }
        if let Some(owner) = self.presentation_owner(generation, active) {
            // In-place updates retain the source's original turn.
            self.content_owners.entry(position).or_insert(owner);
        }
    }

    pub(crate) fn content_owner(
        &self,
        position: (usize, Option<usize>),
    ) -> Option<PresentationOwner> {
        self.content_owners
            .get(&position)
            .copied()
            .map(|owner| self.resolve_owner(owner))
    }

    pub(crate) fn retain_activity_prompts(
        &mut self,
        contains_prompt: impl Fn(&acp_v2::MessageId) -> bool,
    ) {
        if self
            .external_activity_prompt
            .as_ref()
            .is_some_and(|prompt| !contains_prompt(prompt))
        {
            self.external_activity_prompt = None;
        }
        self.unmatched_activity_prompts
            .retain(|prompt| contains_prompt(prompt));
        if self.resumed_activity.as_ref().is_some_and(|(id, _)| {
            self.receipts
                .get(id)
                .is_some_and(|prompt| !contains_prompt(prompt))
        }) {
            self.resumed_activity = None;
        }
    }

    pub(crate) fn start_activity(
        &mut self,
        generation: u64,
        latest_prompt: Option<&acp_v2::MessageId>,
        boundary: (usize, usize),
    ) {
        self.activity_boundary = boundary;
        let eligible = |id| {
            self.records.get(&id).map_or_else(
                || self.receipts.contains_key(&id),
                |record| {
                    matches!(
                        record.state,
                        SubmissionState::Pending | SubmissionState::Accepted { .. }
                    )
                },
            )
        };
        // An echoed receipt can lose its record before its first activity report.
        let awaiting_activity = self.awaiting_activity();
        let resumed_owner = self.activity_owner.filter(|id| {
            eligible(*id)
                && latest_prompt.is_none_or(|prompt| self.receipts.get(id) == Some(prompt))
        });
        self.resumed_activity = awaiting_activity
            .is_none()
            .then(|| {
                let id = resumed_owner?;
                Some((id, self.activities.get(&id)?.clone()))
            })
            .flatten();
        self.unmatched_activity_prompts.clear();
        self.activity_owner = awaiting_activity.or(resumed_owner);
        let previous_external_prompt = self.external_activity_prompt.take();
        // Existing assistant output can be restored history while a new external echo is delayed.
        self.external_activity_prompt = self
            .activity_owner
            .is_none()
            .then(|| {
                latest_prompt
                    .filter(|prompt| !self.receipts.values().any(|receipt| receipt == *prompt))
                    .filter(|prompt| {
                        boundary.1 == 0 || previous_external_prompt.as_ref() == Some(*prompt)
                    })
                    .cloned()
            })
            .flatten();
        if let Some(id) = self.activity_owner {
            self.generation_owners.insert(generation, id);
            let boundary = self
                .activities
                .get(&id)
                .map_or(boundary, |activity| activity.boundary.min(boundary));
            self.activities.insert(
                id,
                SubmissionActivity {
                    generation,
                    completion: None,
                    boundary,
                },
            );
        }
    }

    pub(crate) fn observe_prompt(&mut self, message_id: &acp_v2::MessageId, active: bool) {
        if self.resumed_activity.is_some() || (self.activity_owner.is_none() && active) {
            self.unmatched_activity_prompts.push(message_id.clone());
            self.reconcile_activity_prompt();
        }
    }

    fn reconcile_activity_prompt(&mut self) {
        if self.unmatched_activity_prompts.is_empty() {
            return;
        }
        let receipt_prompts: HashSet<_> = self.receipts.values().collect();
        self.unmatched_activity_prompts
            .retain(|prompt| !receipt_prompts.contains(prompt));
        if !self.unmatched_activity_prompts.is_empty()
            && !self
                .records
                .values()
                .any(|record| matches!(record.state, SubmissionState::Pending))
        {
            // A delayed new prompt disproves the apparent resumption of the previous receipt.
            if let Some((id, previous)) = self.resumed_activity.take() {
                if let Some(activity) = self.activities.get(&id) {
                    self.generation_owners.remove(&activity.generation);
                }
                self.activities.insert(id, previous);
                self.activity_owner = None;
            }
            self.external_activity_prompt = self.unmatched_activity_prompts.last().cloned();
            self.unmatched_activity_prompts.clear();
        }
    }

    pub(crate) fn finish_activity(&mut self, generation: u64, reason: Option<acp_v2::StopReason>) {
        if let Some(activity) = self
            .activity_owner
            .and_then(|id| self.activities.get_mut(&id))
            && activity.generation == generation
        {
            activity.completion = Some(reason);
        }
    }

    pub(crate) fn legacy_completions(
        &self,
    ) -> impl Iterator<Item = (SubmissionId, SubmissionKind, &LegacySubmissionCompletion)> {
        self.legacy_completions
            .iter()
            .map(|(id, (kind, completion))| (*id, *kind, completion))
    }

    pub(crate) fn complete_legacy(
        &mut self,
        id: SubmissionId,
        completion: LegacySubmissionCompletion,
    ) {
        if let Some(record) = self.records.get(&id) {
            // A superseded prompt emits no stop event, and its record is pruned on settlement.
            self.legacy_completions
                .insert(id, (record.kind, completion));
        }
    }

    pub(crate) fn has_unsettled(&self) -> bool {
        self.records.values().any(SubmissionRecord::is_unsettled)
    }

    pub(crate) fn recoverable(&self) -> impl Iterator<Item = (SubmissionId, &SubmissionRecord)> {
        self.records.iter().filter_map(|(id, record)| {
            (self.receipt_transport.is_some() && record.is_recoverable()).then_some((*id, record))
        })
    }

    pub(crate) fn register(
        &mut self,
        content: Arc<[acp_v2::ContentBlock]>,
        kind: SubmissionKind,
    ) -> SubmissionId {
        self.next_id += 1;
        let id = SubmissionId(self.next_id);
        self.latest_id = Some(id);
        self.prune();
        self.records.insert(
            id,
            SubmissionRecord {
                content,
                kind,
                state: SubmissionState::Pending,
                task: None,
            },
        );
        id
    }

    pub(crate) fn track(&mut self, id: SubmissionId, task: Task<()>) {
        if let Some(record) = self.records.get_mut(&id) {
            record.task = Some(task);
        }
    }

    pub(crate) fn settle(&mut self, id: SubmissionId, state: SubmissionState) {
        if let Some(record) = self.records.get_mut(&id) {
            // Observers may run after an echoed older record is pruned; keep its acceptance evidence.
            if let SubmissionState::Accepted { receipt, .. } = &state {
                self.receipts.insert(id, receipt.message_id.clone());
            }
            record.state = state;
        }
        self.reconcile_activity_prompt();
        self.prune();
    }

    pub(crate) fn observe_echo(&mut self, message_id: &acp_v2::MessageId) -> Vec<SubmissionId> {
        let mut changed = Vec::new();
        for (id, record) in &mut self.records {
            if let SubmissionState::Accepted { receipt, echoed } = &mut record.state
                && &receipt.message_id == message_id
                && !*echoed
            {
                *echoed = true;
                changed.push(*id);
            }
        }
        self.prune();
        changed
    }

    pub(crate) fn forget(&mut self, id: SubmissionId) -> bool {
        if self
            .records
            .get(&id)
            .is_some_and(SubmissionRecord::is_unsettled)
        {
            return false;
        }
        if self.records.remove(&id).is_none() {
            return false;
        }
        self.receipts.remove(&id);
        self.legacy_completions.remove(&id);
        self.legacy_prompt_entries.remove(&id);
        self.activities.remove(&id);
        self.submission_boundaries.remove(&id);
        self.generation_owners.retain(|_, owner| *owner != id);
        self.content_owners
            .retain(|_, owner| *owner != PresentationOwner::Submission(id));
        if self.activity_owner == Some(id) {
            self.activity_owner = None;
        }
        if self
            .resumed_activity
            .as_ref()
            .is_some_and(|(owner, _)| *owner == id)
        {
            self.resumed_activity = None;
            self.unmatched_activity_prompts.clear();
        }
        if self.latest_id == Some(id) {
            self.latest_id = None;
        }
        true
    }

    fn prune(&mut self) {
        self.records.retain(|id, record| {
            Some(*id) == self.latest_id
                || record.is_unsettled()
                || (self.receipt_transport.is_some() && record.is_recoverable())
        });
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ForegroundActivity {
    Idle,
    Running,
    RequiresAction,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submission_activity_tracks_queued_generations_and_resumption() {
        let mut submissions = SessionSubmissions::new(None);
        let first = submissions.register(Arc::from([]), SubmissionKind::Prompt);
        submissions.settle(
            first,
            SubmissionState::Accepted {
                receipt: acp_v2::PromptResponse::new("first"),
                echoed: true,
            },
        );
        let second = submissions.register(Arc::from([]), SubmissionKind::Prompt);
        assert!(submissions.get(first).is_none());
        submissions.start_activity(1, None, (0, 0));
        submissions.finish_activity(0, Some(acp_v2::StopReason::Cancelled));
        assert!(
            submissions
                .activity(first)
                .expect("first activity")
                .completion
                .is_none()
        );
        assert!(submissions.activity(second).is_none());
        submissions.finish_activity(1, Some(acp_v2::StopReason::EndTurn));
        submissions.settle(first, SubmissionState::Completed);
        assert!(submissions.get(first).is_none());
        assert!(matches!(
            submissions
                .activity(first)
                .expect("retained activity")
                .completion,
            Some(Some(acp_v2::StopReason::EndTurn))
        ));
        submissions.start_activity(2, None, (0, 0));
        assert_eq!(
            submissions
                .activity(second)
                .expect("second activity")
                .generation,
            2
        );
        submissions.finish_activity(2, Some(acp_v2::StopReason::Cancelled));
        submissions.start_activity(3, None, (0, 0));
        assert_eq!(
            submissions
                .activity(second)
                .expect("resumed activity")
                .generation,
            3
        );
        assert!(
            submissions
                .activity(second)
                .expect("resumed activity")
                .completion
                .is_none()
        );
        assert!(matches!(
            submissions
                .activity(first)
                .expect("first stays settled")
                .completion,
            Some(Some(acp_v2::StopReason::EndTurn))
        ));
        submissions.settle(second, SubmissionState::Failed("rejected".into()));
        let third = submissions.register(Arc::from([]), SubmissionKind::Prompt);
        submissions.settle(third, SubmissionState::Cancelled);
        submissions.start_activity(4, None, (0, 0));
        assert_eq!(
            submissions
                .activity(second)
                .expect("prior activity")
                .generation,
            3
        );
        assert!(submissions.activity(third).is_none());
    }

    #[test]
    fn distinct_external_prompts_preserve_completed_receipt_activity() {
        for delayed_prompt in [false, true] {
            let mut submissions = SessionSubmissions::new(None);
            let id = submissions.register(Arc::from([]), SubmissionKind::Prompt);
            let receipt = acp_v2::PromptResponse::new("local");
            submissions.settle(
                id,
                SubmissionState::Accepted {
                    receipt: receipt.clone(),
                    echoed: true,
                },
            );
            submissions.start_activity(1, Some(&receipt.message_id), (0, 0));
            submissions.finish_activity(1, Some(acp_v2::StopReason::EndTurn));
            let external_prompt = acp_v2::PromptResponse::new("external").message_id;
            submissions.start_activity(
                2,
                Some(if delayed_prompt {
                    &receipt.message_id
                } else {
                    &external_prompt
                }),
                (0, 0),
            );
            submissions.finish_activity(2, Some(acp_v2::StopReason::Cancelled));
            if delayed_prompt {
                submissions.observe_prompt(&external_prompt, false);
            }
            let historical = submissions.activity(id).expect("historical receipt");
            assert_eq!(historical.generation, 1);
            assert!(matches!(
                historical.completion,
                Some(Some(acp_v2::StopReason::EndTurn))
            ));
            submissions.start_activity(3, Some(&external_prompt), (0, 0));
            assert_eq!(
                submissions
                    .activity(id)
                    .expect("historical receipt")
                    .generation,
                1
            );
            submissions.truncate_prompt_entries(0);
            submissions.retain_activity_prompts(|_| false);
            assert!(submissions.external_activity_prompt().is_none());
            assert_eq!(submissions.activity_boundary(), (0, 0));
        }
    }

    #[test]
    fn deferred_external_prompt_survives_interleaved_queued_receipt_echo() {
        for echo_before_acceptance in [false, true] {
            let mut submissions = SessionSubmissions::new(None);
            let local = submissions.register(Arc::from([]), SubmissionKind::Prompt);
            let receipt = acp_v2::PromptResponse::new("local");
            submissions.settle(
                local,
                SubmissionState::Accepted {
                    receipt: receipt.clone(),
                    echoed: true,
                },
            );
            submissions.start_activity(1, Some(&receipt.message_id), (0, 0));
            submissions.finish_activity(1, Some(acp_v2::StopReason::EndTurn));
            submissions.start_activity(2, Some(&receipt.message_id), (0, 0));
            let queued = submissions.register(Arc::from([]), SubmissionKind::Prompt);
            let queued_receipt = acp_v2::PromptResponse::new("queued");
            let external_prompt = acp_v2::PromptResponse::new("external").message_id;
            submissions.observe_prompt(&external_prompt, true);
            if echo_before_acceptance {
                submissions.observe_prompt(&queued_receipt.message_id, true);
            }
            assert_eq!(
                submissions
                    .activity(local)
                    .expect("unresolved resumption")
                    .generation,
                2
            );
            submissions.settle(
                queued,
                SubmissionState::Accepted {
                    receipt: queued_receipt.clone(),
                    echoed: echo_before_acceptance,
                },
            );
            if !echo_before_acceptance {
                submissions.observe_prompt(&queued_receipt.message_id, true);
            }
            let historical = submissions.activity(local).expect("historical receipt");
            assert_eq!(historical.generation, 1);
            assert!(matches!(
                historical.completion,
                Some(Some(acp_v2::StopReason::EndTurn))
            ));
            assert!(submissions.activity(queued).is_none());
            submissions.start_activity(3, Some(&queued_receipt.message_id), (0, 0));
            assert_eq!(
                submissions
                    .activity(queued)
                    .expect("queued activity")
                    .generation,
                3
            );
        }
    }

    #[test]
    fn legacy_prompt_locators_are_invalidated_on_rollback() {
        let mut submissions = SessionSubmissions::new(None);
        let first = submissions.register(Arc::from([]), SubmissionKind::Prompt);
        submissions.associate_legacy_prompt(first, 0);
        submissions.complete_legacy(first, LegacySubmissionCompletion::Cancelled);
        submissions.settle(first, SubmissionState::Completed);
        let second = submissions.register(Arc::from([]), SubmissionKind::Prompt);
        submissions.associate_legacy_prompt(second, 2);
        assert_eq!(submissions.legacy_prompt_entry(first), Some(0));
        assert_eq!(submissions.legacy_prompt_entry(second), Some(2));
        submissions.truncate_prompt_entries(2);
        assert_eq!(submissions.legacy_prompt_entry(first), Some(0));
        assert_eq!(submissions.legacy_prompt_entry(second), None);
        submissions.associate_legacy_prompt(second, 2);
        submissions.truncate_prompt_entries(0);
        assert_eq!(submissions.legacy_prompt_entry(first), None);
        assert_eq!(submissions.legacy_prompt_entry(second), None);
    }

    #[test]
    fn legacy_submission_completions_survive_record_pruning() {
        for kind in [
            SubmissionKind::Prompt,
            SubmissionKind::Command,
            SubmissionKind::Retry,
        ] {
            let mut submissions = SessionSubmissions::new(None);
            let first = submissions.register(Arc::from([]), kind);
            let second = submissions.register(Arc::from([]), SubmissionKind::Prompt);
            assert!(submissions.get(first).is_some());
            submissions.complete_legacy(
                first,
                LegacySubmissionCompletion::Stopped(acp_v1::StopReason::Cancelled),
            );
            submissions.settle(first, SubmissionState::Completed);
            assert!(submissions.get(first).is_none());
            assert!(submissions.get(second).is_some());
            let completions: Vec<_> = submissions.legacy_completions().collect();
            assert_eq!(completions.len(), 1);
            let (id, observed_kind, completion) = completions.first().expect("legacy completion");
            assert_eq!(*id, first);
            assert_eq!(*observed_kind, kind);
            assert!(matches!(
                completion,
                LegacySubmissionCompletion::Stopped(acp_v1::StopReason::Cancelled)
            ));
            assert!(submissions.recoverable().next().is_none());
            assert!(submissions.has_unsettled());
            submissions.settle(second, SubmissionState::Completed);
            assert!(!submissions.has_unsettled());
        }
    }
}

pub(crate) struct SessionActivity {
    state: acp_v2::StateUpdate,
    generation: u64,
    started_at: Option<Instant>,
    duration: Option<Duration>,
    first_entry_index: usize,
    cancel_waiters: Vec<oneshot::Sender<()>>,
}

impl Default for SessionActivity {
    fn default() -> Self {
        Self {
            state: acp_v2::StateUpdate::Idle(acp_v2::IdleStateUpdate::new()),
            generation: 0,
            started_at: None,
            duration: None,
            first_entry_index: 0,
            cancel_waiters: Vec::new(),
        }
    }
}

impl SessionActivity {
    pub(crate) fn state(&self) -> &acp_v2::StateUpdate {
        &self.state
    }

    pub(crate) fn phase(&self) -> ForegroundActivity {
        match self.state {
            acp_v2::StateUpdate::Idle(_) => ForegroundActivity::Idle,
            acp_v2::StateUpdate::RequiresAction(_) => ForegroundActivity::RequiresAction,
            _ => ForegroundActivity::Running,
        }
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn started_at(&self) -> Option<Instant> {
        self.started_at
    }

    pub(crate) fn duration(&self) -> Option<Duration> {
        if self.phase() == ForegroundActivity::Idle {
            self.duration
        } else {
            self.started_at.map(|started| started.elapsed())
        }
    }

    pub(crate) fn first_entry_index(&self) -> usize {
        self.first_entry_index
    }

    pub(crate) fn update(&mut self, state: acp_v2::StateUpdate, first_entry_index: usize) {
        let previous = self.phase();
        self.state = state;
        let current = self.phase();
        if previous == ForegroundActivity::Idle && current != ForegroundActivity::Idle {
            self.generation += 1;
            self.started_at = Some(Instant::now());
            self.duration = None;
            self.first_entry_index = first_entry_index;
        } else if previous != ForegroundActivity::Idle && current == ForegroundActivity::Idle {
            self.duration = self.started_at.map(|started| started.elapsed());
            for waiter in self.cancel_waiters.drain(..) {
                if waiter.send(()).is_err() {
                    log::debug!("Foreground cancellation observer was dropped");
                }
            }
        }
    }

    pub(crate) fn wait_for_idle(&mut self) -> Option<oneshot::Receiver<()>> {
        if self.phase() == ForegroundActivity::Idle {
            return None;
        }
        let (sender, receiver) = oneshot::channel();
        self.cancel_waiters.push(sender);
        Some(receiver)
    }
}
