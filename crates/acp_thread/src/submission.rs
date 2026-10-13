use crate::ReceiptSessionSubmissions;
use agent_client_protocol::schema::{v1 as acp_v1, v2 as acp_v2};
use futures::{channel::oneshot, future::BoxFuture};
use gpui::{SharedString, Task};
use std::collections::BTreeMap;
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
    }

    pub(crate) fn activity(&self, id: SubmissionId) -> Option<&SubmissionActivity> {
        self.activities.get(&id)
    }

    pub(crate) fn start_activity(&mut self, generation: u64) {
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
        self.activity_owner = self
            .records
            .iter()
            .find_map(|(id, record)| {
                (matches!(
                    record.state,
                    SubmissionState::Pending | SubmissionState::Accepted { .. }
                ) && !self.activities.contains_key(id))
                .then_some(*id)
            })
            .into_iter()
            .chain(
                self.receipts
                    .keys()
                    .copied()
                    .filter(|id| !self.activities.contains_key(id) && eligible(*id)),
            )
            .min()
            .or(self.activity_owner.filter(|id| eligible(*id)));
        if let Some(id) = self.activity_owner {
            self.activities.insert(
                id,
                SubmissionActivity {
                    generation,
                    completion: None,
                },
            );
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
        if self.activity_owner == Some(id) {
            self.activity_owner = None;
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
        submissions.start_activity(1);
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
        submissions.start_activity(2);
        assert_eq!(
            submissions
                .activity(second)
                .expect("second activity")
                .generation,
            2
        );
        submissions.finish_activity(2, Some(acp_v2::StopReason::Cancelled));
        submissions.start_activity(3);
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
        submissions.start_activity(4);
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
