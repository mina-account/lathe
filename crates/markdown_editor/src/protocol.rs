use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    ops::Range,
};

pub const MAX_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_MESSAGE_BYTES: usize = MAX_DOCUMENT_BYTES * 2 + 4096;
pub const ORIGIN: &str = "https://lathe-markdown.invalid";

pub fn within_document_budget(text: &str) -> bool {
    text.len() <= MAX_DOCUMENT_BYTES
        && serde_json::to_string(text)
            .is_ok_and(|serialized| serialized.len() <= MAX_DOCUMENT_BYTES)
}

mod document_identifier {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(value: &u64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(value)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
        let value = String::deserialize(deserializer)?;
        let identifier = value.parse::<u64>().map_err(D::Error::custom)?;
        if identifier.to_string() != value {
            return Err(D::Error::custom("Noncanonical document identifier"));
        }
        Ok(identifier)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Change {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Ir,
    Wysiwyg,
    Sv,
    Preview,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ViewPreferences {
    pub mode: Mode,
    pub outline: bool,
    pub wrap: bool,
    pub scroll: u32,
}

impl Default for ViewPreferences {
    fn default() -> Self {
        Self {
            mode: Mode::Ir,
            outline: false,
            wrap: true,
            scroll: 0,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientMessage {
    InputState {
        session: String,
        #[serde(with = "document_identifier")]
        document: u64,
        text: String,
        start: usize,
        end: usize,
    },
    Preferences {
        session: String,
        #[serde(with = "document_identifier")]
        document: u64,
        preferences: ViewPreferences,
    },
    Copy {
        session: String,
        #[serde(with = "document_identifier")]
        document: u64,
        text: String,
    },
    OpenLink {
        session: String,
        #[serde(with = "document_identifier")]
        document: u64,
        url: String,
    },
    Ready {
        session: String,
        #[serde(with = "document_identifier")]
        document: u64,
    },
    Draft {
        session: String,
        #[serde(with = "document_identifier")]
        document: u64,
        sequence: u64,
        base_revision: u64,
        text: String,
    },
    Edit {
        session: String,
        #[serde(with = "document_identifier")]
        document: u64,
        request: u64,
        sequence: u64,
        base_revision: u64,
        change: Change,
    },
    Flushed {
        session: String,
        #[serde(with = "document_identifier")]
        document: u64,
        request: u64,
        sequence: u64,
    },
    Undo {
        session: String,
        #[serde(with = "document_identifier")]
        document: u64,
        request: u64,
        base_revision: u64,
    },
    Redo {
        session: String,
        #[serde(with = "document_identifier")]
        document: u64,
        request: u64,
        base_revision: u64,
    },
    Selection {
        session: String,
        #[serde(with = "document_identifier")]
        document: u64,
        start: usize,
        end: usize,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HostMessage {
    Notice {
        text: String,
    },
    Preferences {
        preferences: ViewPreferences,
    },
    HistoryAccepted {
        request: u64,
        revision: u64,
        text: String,
        writable: bool,
    },
    Document {
        revision: u64,
        text: String,
        writable: bool,
        draft: Option<String>,
    },
    Accepted {
        request: u64,
        sequence: u64,
        revision: u64,
        text: String,
    },
    Conflict {
        request: u64,
        revision: u64,
        text: String,
        draft: String,
        reason: String,
    },
    Flush {
        request: u64,
    },
}

#[derive(Clone, Debug)]
pub struct Draft {
    pub sequence: u64,
    pub base_revision: u64,
    pub text: String,
}

#[derive(Debug)]
pub struct Protocol {
    pub session: String,
    pub document: u64,
    pub revision: u64,
    pub draft: Option<Draft>,
    pub conflict: Option<String>,
    snapshots: BTreeMap<u64, String>,
    accepted: BTreeMap<u64, (ClientMessage, HostMessage)>,
    last_request: u64,
    last_sequence: u64,
    history_requests: BTreeMap<u64, ClientMessage>,
    drafts: BTreeMap<u64, Draft>,
}

pub fn document_url(session: &str) -> String {
    format!("{ORIGIN}/index.html?session={session}")
}

pub fn application_url(session: &str, document: u64) -> String {
    format!("{}&document={document}", document_url(session))
}

pub fn decode(origin: &str, body: &str, session: &str, document: u64) -> Result<ClientMessage> {
    if origin != application_url(session, document) || body.len() > MAX_MESSAGE_BYTES {
        bail!("Foreign origin or oversized Markdown bridge message");
    }
    let message: ClientMessage = serde_json::from_str(body)?;
    let (actual_session, actual_document) = match &message {
        ClientMessage::Ready { session, document }
        | ClientMessage::Preferences {
            session, document, ..
        }
        | ClientMessage::Copy {
            session, document, ..
        }
        | ClientMessage::OpenLink {
            session, document, ..
        }
        | ClientMessage::InputState {
            session, document, ..
        }
        | ClientMessage::Draft {
            session, document, ..
        }
        | ClientMessage::Edit {
            session, document, ..
        }
        | ClientMessage::Flushed {
            session, document, ..
        }
        | ClientMessage::Undo {
            session, document, ..
        }
        | ClientMessage::Redo {
            session, document, ..
        }
        | ClientMessage::Selection {
            session, document, ..
        } => (session, document),
    };
    if actual_session != session || *actual_document != document {
        bail!("Foreign Markdown document/session");
    }
    match &message {
        ClientMessage::InputState {
            text, start, end, ..
        } => {
            if !within_document_budget(text) || start > end {
                bail!("Invalid browser input state");
            }
            utf16_to_byte(text, *start)?;
            utf16_to_byte(text, *end)?;
        }
        ClientMessage::Copy { text, .. } if !within_document_budget(text) => {
            bail!("Clipboard text exceeds its limit")
        }
        ClientMessage::OpenLink { url, .. } => {
            let url = urlencoding::decode(url)?;
            if url.is_empty() || url.len() > 8192 || url.chars().any(char::is_control) {
                bail!("Invalid Markdown link");
            }
            if let Some((scheme, _)) = url.split_once(':') {
                if !["https", "http", "mailto"].contains(&scheme.to_ascii_lowercase().as_str()) {
                    bail!("Forbidden Markdown link scheme");
                }
            }
            if url.starts_with("//") || url.starts_with('\\') {
                bail!("Forbidden Markdown link");
            }
        }
        ClientMessage::Preferences { preferences, .. } if preferences.scroll > 100_000_000 => {
            bail!("Invalid reading position")
        }
        _ => {}
    }
    Ok(message)
}

pub fn utf16_to_byte(text: &str, offset: usize) -> Result<usize> {
    let mut units = 0;
    for (index, character) in text.char_indices() {
        if units == offset {
            return Ok(index);
        }
        units += character.len_utf16();
        if units > offset {
            bail!("Offset splits a UTF-16 surrogate pair");
        }
    }
    if units == offset {
        Ok(text.len())
    } else {
        bail!("Offset outside document")
    }
}

pub fn minimal_change(before: &str, after: &str) -> Option<(Range<usize>, String)> {
    if before == after {
        return None;
    }
    let prefix = before
        .chars()
        .zip(after.chars())
        .take_while(|(left, right)| left == right)
        .map(|(character, _)| character.len_utf8())
        .sum::<usize>();
    let suffix = before
        .get(prefix..)?
        .chars()
        .rev()
        .zip(after.get(prefix..)?.chars().rev())
        .take_while(|(left, right)| left == right)
        .map(|(character, _)| character.len_utf8())
        .sum::<usize>();
    Some((
        prefix..before.len() - suffix,
        after.get(prefix..after.len() - suffix)?.to_owned(),
    ))
}

pub fn rebase(base: &str, current: &str, range: Range<usize>) -> Result<Range<usize>> {
    let Some((external, inserted)) = minimal_change(base, current) else {
        return Ok(range);
    };
    if external.end < range.start || (external.end == range.start && external.start != range.start)
    {
        let start = range.start - external.len() + inserted.len();
        let end = range.end - external.len() + inserted.len();
        return Ok(start..end);
    }
    if range.end < external.start || (range.end == external.start && range.start != external.start)
    {
        return Ok(range);
    }
    rebase_across(&separate_changes(base, current)?, range)
}

fn rebase_across(changes: &[(Range<usize>, String)], range: Range<usize>) -> Result<Range<usize>> {
    let mut removed = 0;
    let mut inserted = 0;
    for (external, text) in changes {
        if external.end < range.start
            || (external.end == range.start && external.start != range.start)
        {
            removed += external.len();
            inserted += text.len();
        } else if !(range.end < external.start
            || (range.end == external.start && range.start != external.start))
        {
            bail!("Concurrent edits overlap; the visual draft is retained for resolution");
        }
    }
    Ok(range.start - removed + inserted..range.end - removed + inserted)
}

fn unchanged_anchor(before: &str, after: &str) -> Option<(Range<usize>, Range<usize>)> {
    fn find<'a>(
        before: impl Iterator<Item = (usize, &'a str)>,
        after: impl Iterator<Item = (usize, &'a str)>,
    ) -> Option<(Range<usize>, Range<usize>)> {
        let mut candidates = HashMap::<&str, (usize, usize, usize, usize)>::new();
        for (offset, text) in before {
            if let Some(candidate) = candidates.get_mut(text) {
                candidate.2 += 1;
            } else if candidates.len() < 65536 {
                candidates.insert(text, (offset, 0, 1, 0));
            }
        }
        for (offset, text) in after {
            if let Some(candidate) = candidates.get_mut(text) {
                candidate.1 = offset;
                candidate.3 += 1;
            }
        }
        let mut matching: Vec<_> = candidates
            .into_iter()
            .filter(|(_, candidate)| candidate.2 == 1 && candidate.3 == 1)
            .collect();
        matching.sort_by_key(|(_, candidate)| candidate.0);
        matching.get(matching.len() / 2).map(|(text, candidate)| {
            (
                candidate.0..candidate.0 + text.len(),
                candidate.1..candidate.1 + text.len(),
            )
        })
    }
    fn lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
        let mut offset = 0;
        text.split_inclusive('\n').map(move |line| {
            let start = offset;
            offset += line.len();
            (start, line)
        })
    }
    fn characters(text: &str) -> impl Iterator<Item = (usize, &str)> {
        text.char_indices().filter_map(|(offset, character)| {
            text.get(offset..offset + character.len_utf8())
                .map(|character| (offset, character))
        })
    }
    let (old, new) = find(lines(before), lines(after))
        .or_else(|| find(characters(before), characters(after)))?;
    let prefix = before
        .get(..old.start)?
        .chars()
        .rev()
        .zip(after.get(..new.start)?.chars().rev())
        .take_while(|(left, right)| left == right)
        .map(|(character, _)| character.len_utf8())
        .sum::<usize>();
    let suffix = before
        .get(old.end..)?
        .chars()
        .zip(after.get(new.end..)?.chars())
        .take_while(|(left, right)| left == right)
        .map(|(character, _)| character.len_utf8())
        .sum::<usize>();
    Some((
        old.start - prefix..old.end + suffix,
        new.start - prefix..new.end + suffix,
    ))
}

fn separate_changes(before: &str, after: &str) -> Result<Vec<(Range<usize>, String)>> {
    reconcile_changes(before, after, &mut (64 * 1024 * 1024), 0)
}

fn reconcile_changes(
    before: &str,
    after: &str,
    budget: &mut usize,
    depth: usize,
) -> Result<Vec<(Range<usize>, String)>> {
    let Some((enclosing, replacement)) = minimal_change(before, after) else {
        return Ok(Vec::new());
    };
    if enclosing.is_empty() || replacement.is_empty() {
        return Ok(vec![(enclosing, replacement)]);
    }
    let left: Vec<char> = before
        .get(enclosing.clone())
        .ok_or_else(|| anyhow::anyhow!("Invalid diff range"))?
        .chars()
        .collect();
    let right: Vec<char> = replacement.chars().collect();
    #[derive(Clone, Default)]
    struct Path {
        left: usize,
        right: usize,
        changes: Vec<(Range<usize>, Range<usize>)>,
    }
    let mut frontier = BTreeMap::<i32, Path>::new();
    let mut remaining = (*budget).min(32 * 1024 * 1024);
    // Small character diffs keep fine-grained spans; larger replacements split at unchanged anchors.
    'search: for distance in 0..=128 {
        let mut next = BTreeMap::new();
        for diagonal in (-distance..=distance).step_by(2) {
            let deletion = frontier
                .get(&(diagonal - 1))
                .filter(|path| path.left < left.len());
            let insertion = frontier
                .get(&(diagonal + 1))
                .filter(|path| path.right < right.len());
            let mut path = if distance == 0 {
                Path::default()
            } else {
                let remove = deletion.is_some_and(|deletion| {
                    insertion.is_none_or(|insertion| deletion.left >= insertion.left)
                });
                let Some(previous) = (if remove { deletion } else { insertion }) else {
                    continue;
                };
                let mut path = previous.clone();
                let change = (
                    path.left..path.left + usize::from(remove),
                    path.right..path.right + usize::from(!remove),
                );
                if let Some(last) = path
                    .changes
                    .last_mut()
                    .filter(|last| last.0.end == change.0.start && last.1.end == change.1.start)
                {
                    last.0.end = change.0.end;
                    last.1.end = change.1.end;
                } else {
                    path.changes.push(change.clone());
                }
                path.left = change.0.end;
                path.right = change.1.end;
                path
            };
            while let (Some(left), Some(right)) = (left.get(path.left), right.get(path.right)) {
                let Some(next_remaining) = remaining.checked_sub(1) else {
                    break 'search;
                };
                remaining = next_remaining;
                *budget -= 1;
                if left != right {
                    break;
                }
                path.left += 1;
                path.right += 1;
            }
            if path.left == left.len() && path.right == right.len() {
                let mut left_characters = left.iter();
                let mut right_characters = right.iter();
                let mut left_index = 0;
                let mut right_index = 0;
                let mut left_offset = enclosing.start;
                let mut right_offset = 0;
                let mut changes = Vec::new();
                for (old, new) in path.changes {
                    left_offset += left_characters
                        .by_ref()
                        .take(old.start - left_index)
                        .map(|character| character.len_utf8())
                        .sum::<usize>();
                    let start = left_offset;
                    left_offset += left_characters
                        .by_ref()
                        .take(old.len())
                        .map(|character| character.len_utf8())
                        .sum::<usize>();
                    right_offset += right_characters
                        .by_ref()
                        .take(new.start - right_index)
                        .map(|character| character.len_utf8())
                        .sum::<usize>();
                    let inserted_start = right_offset;
                    right_offset += right_characters
                        .by_ref()
                        .take(new.len())
                        .map(|character| character.len_utf8())
                        .sum::<usize>();
                    let inserted = replacement
                        .get(inserted_start..right_offset)
                        .ok_or_else(|| anyhow::anyhow!("Invalid inserted diff range"))?
                        .to_owned();
                    changes.push((start..left_offset, inserted));
                    left_index = old.end;
                    right_index = new.end;
                }
                return Ok(changes);
            }
            next.insert(diagonal, path);
        }
        frontier = next;
    }
    drop(left);
    drop(right);
    drop(frontier);
    let old = before
        .get(enclosing.clone())
        .ok_or_else(|| anyhow::anyhow!("Invalid diff range"))?;
    *budget = budget.checked_sub(3 * (old.len() + replacement.len())).ok_or_else(|| anyhow::anyhow!("Concurrent change reconciliation exceeds its work limit; the visual draft is retained"))?;
    if depth >= 64 {
        return Ok(vec![(enclosing, replacement)]);
    }
    let Some((old_anchor, new_anchor)) = unchanged_anchor(old, &replacement) else {
        return Ok(vec![(enclosing, replacement)]);
    };
    let mut changes = reconcile_changes(
        old.get(..old_anchor.start)
            .ok_or_else(|| anyhow::anyhow!("Invalid anchor start"))?,
        replacement
            .get(..new_anchor.start)
            .ok_or_else(|| anyhow::anyhow!("Invalid replacement anchor start"))?,
        budget,
        depth + 1,
    )?;
    let later = reconcile_changes(
        old.get(old_anchor.end..)
            .ok_or_else(|| anyhow::anyhow!("Invalid anchor end"))?,
        replacement
            .get(new_anchor.end..)
            .ok_or_else(|| anyhow::anyhow!("Invalid replacement anchor end"))?,
        budget,
        depth + 1,
    )?;
    for (range, _) in &mut changes {
        range.start += enclosing.start;
        range.end += enclosing.start;
    }
    changes.extend(later.into_iter().map(|(range, text)| {
        (
            range.start + enclosing.start + old_anchor.end
                ..range.end + enclosing.start + old_anchor.end,
            text,
        )
    }));
    Ok(changes)
}

impl Protocol {
    pub fn validate_flush(&self, sequence: u64) -> Result<()> {
        if self.draft.is_some() || self.conflict.is_some() || sequence != self.last_sequence {
            bail!(
                "Visual input has not been fully acknowledged; resolve or recover it before saving/closing"
            );
        }
        Ok(())
    }

    pub fn new(session: String, document: u64, text: String) -> Self {
        Self {
            session,
            document,
            revision: 0,
            draft: None,
            conflict: None,
            snapshots: BTreeMap::from([(0, text)]),
            accepted: BTreeMap::new(),
            last_request: 0,
            last_sequence: 0,
            history_requests: BTreeMap::new(),
            drafts: BTreeMap::new(),
        }
    }

    pub fn current(&self) -> &str {
        self.snapshots
            .get(&self.revision)
            .map(String::as_str)
            .unwrap_or_default()
    }

    pub fn update(&mut self, text: String) {
        if text == self.current() {
            return;
        }
        self.revision += 1;
        self.snapshots.insert(self.revision, text);
        while self.snapshots.len() > 128 {
            let Some(oldest) = self.snapshots.keys().copied().find(|revision| {
                *revision != self.revision
                    && self
                        .draft
                        .as_ref()
                        .is_none_or(|draft| draft.base_revision != *revision)
            }) else {
                break;
            };
            self.snapshots.remove(&oldest);
        }
    }

    pub fn document_message(&self, writable: bool) -> HostMessage {
        HostMessage::Document {
            revision: self.revision,
            text: self.current().to_owned(),
            writable: writable && within_document_budget(self.current()),
            draft: self.draft.as_ref().map(|draft| draft.text.clone()),
        }
    }

    pub fn retain_draft(&mut self, sequence: u64, base_revision: u64, text: String) -> Result<()> {
        let oversized = !within_document_budget(&text);
        if sequence == 0 || sequence == self.last_sequence && self.draft.is_none() {
            bail!("Visual draft sequence was already acknowledged");
        }
        if sequence < self.last_sequence {
            bail!("Out-of-order visual draft");
        }
        if sequence == self.last_sequence
            && self.draft.as_ref().is_some_and(|draft| draft.text != text)
        {
            bail!("Repeated sequence changed its draft");
        }
        self.last_sequence = sequence;
        self.draft = Some(Draft {
            sequence,
            base_revision,
            text,
        });
        if let Some(draft) = &self.draft {
            self.drafts.insert(sequence, draft.clone());
        }
        while self.drafts.len() > 2 {
            if let Some(middle) = self.drafts.keys().nth(1).copied() {
                self.drafts.remove(&middle);
            }
        }
        if oversized {
            bail!("Visual draft exceeds the document size limit; input retained for recovery");
        }
        Ok(())
    }

    pub fn prepare(
        &self,
        message: &ClientMessage,
        writable: bool,
    ) -> Result<Option<Vec<(Range<usize>, String)>>> {
        let ClientMessage::Edit {
            request,
            sequence,
            base_revision,
            change,
            ..
        } = message
        else {
            bail!("Expected edit");
        };
        if let Some((original, _)) = self.accepted.get(request) {
            if original != message {
                bail!("Request identifier reused with different content");
            }
            return Ok(None);
        }
        if *request <= self.last_request {
            bail!("Out-of-order edit request");
        }
        let draft = self
            .drafts
            .get(sequence)
            .ok_or_else(|| anyhow::anyhow!("Edit sequence has no matching recovery draft"))?;
        if *base_revision != draft.base_revision {
            bail!("Edit and recovery draft refer to different revisions");
        }
        let base = self
            .snapshots
            .get(base_revision)
            .ok_or_else(|| anyhow::anyhow!("Edit base expired; the draft is retained"))?;
        let start = utf16_to_byte(base, change.start)?;
        let end = utf16_to_byte(base, change.end)?;
        if start > end || change.text.len() > MAX_DOCUMENT_BYTES {
            bail!("Invalid change range or size");
        }
        {
            let mut expected = base.clone();
            expected.replace_range(start..end, &change.text);
            if expected != draft.text {
                bail!("Edit does not match its recovery draft");
            }
        }
        let local = separate_changes(base, &draft.text)?;
        if !writable && (!local.is_empty() || draft.text != self.current()) {
            bail!("Document is read-only; the visual draft is retained");
        }
        let external = separate_changes(base, self.current())?;
        let mut changes = Vec::new();
        for (range, text) in local {
            changes.push((rebase_across(&external, range)?, text));
        }
        Ok(Some(changes))
    }

    pub fn accept(&mut self, message: ClientMessage, text: String) -> Result<HostMessage> {
        let ClientMessage::Edit {
            request, sequence, ..
        } = &message
        else {
            bail!("Expected edit");
        };
        if let Some((original, acknowledgment)) = self.accepted.get(request) {
            if original != &message {
                bail!("Request identifier reused with different content");
            }
            return Ok(acknowledgment.clone());
        }
        let (request, sequence) = (*request, *sequence);
        self.update(text);
        self.last_request = request;
        if self
            .draft
            .as_ref()
            .is_some_and(|draft| draft.sequence == sequence)
        {
            self.draft = None;
        }
        self.drafts.retain(|pending, _| *pending > sequence);
        self.conflict = None;
        let acknowledgment = HostMessage::Accepted {
            request,
            sequence,
            revision: self.revision,
            text: self.current().to_owned(),
        };
        self.accepted
            .insert(request, (message, acknowledgment.clone()));
        while self.accepted.len() > 128 {
            self.accepted.pop_first();
        }
        Ok(acknowledgment)
    }

    pub fn reject(&mut self, request: u64, reason: String) -> HostMessage {
        self.conflict = Some(reason.clone());
        HostMessage::Conflict {
            request,
            revision: self.revision,
            text: self.current().to_owned(),
            draft: self
                .draft
                .as_ref()
                .map(|draft| draft.text.clone())
                .unwrap_or_default(),
            reason,
        }
    }

    pub fn history(&mut self, message: &ClientMessage) -> Result<bool> {
        let (request, base_revision) = match message {
            ClientMessage::Undo {
                request,
                base_revision,
                ..
            }
            | ClientMessage::Redo {
                request,
                base_revision,
                ..
            } => (*request, *base_revision),
            _ => bail!("Expected history request"),
        };
        if let Some(previous) = self.history_requests.get(&request) {
            if previous == message {
                return Ok(false);
            }
            bail!("History request reused with different content");
        }
        if request <= self.last_request || base_revision > self.revision {
            bail!("Stale or out-of-order history request");
        }
        self.last_request = request;
        self.history_requests.insert(request, message.clone());
        while self.history_requests.len() > 128 {
            self.history_requests.pop_first();
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_and_input_messages_are_typed_bounded_and_decoded_before_scheme_validation() {
        let origin = application_url("session", 1);
        for url in [
            "https://example.com",
            "mailto:user@example.com",
            "other%20file.markdown#heading",
            "assets/%E4%B8%AD%E6%96%87.png",
            "#heading",
        ] {
            let message = ClientMessage::OpenLink {
                session: "session".into(),
                document: 1,
                url: url.into(),
            };
            assert!(
                decode(
                    &origin,
                    &serde_json::to_string(&message).expect("message"),
                    "session",
                    1
                )
                .is_ok(),
                "{url}"
            );
        }
        for url in [
            "javascript:evil()",
            "javascript%3Aevil()",
            "data:text/html,evil",
            "file:///secret",
            "//foreign.invalid/x",
            "%0Aevil",
            "\\\\server\\share",
        ] {
            let message = ClientMessage::OpenLink {
                session: "session".into(),
                document: 1,
                url: url.into(),
            };
            assert!(
                decode(
                    &origin,
                    &serde_json::to_string(&message).expect("message"),
                    "session",
                    1
                )
                .is_err(),
                "{url}"
            );
        }
        let message = ClientMessage::InputState {
            session: "session".into(),
            document: 1,
            text: "😀".into(),
            start: 1,
            end: 2,
        };
        assert!(
            decode(
                &origin,
                &serde_json::to_string(&message).expect("message"),
                "session",
                1
            )
            .is_err()
        );
    }

    fn edit(protocol: &Protocol, request: u64, sequence: u64, change: Change) -> ClientMessage {
        ClientMessage::Edit {
            session: protocol.session.clone(),
            document: protocol.document,
            request,
            sequence,
            base_revision: 0,
            change,
        }
    }

    #[test]
    fn oversized_documents_are_not_editable_and_oversized_input_is_retained() {
        let text = "x".repeat(MAX_DOCUMENT_BYTES + 1);
        let mut protocol = Protocol::new("session".into(), 1, text.clone());
        assert!(matches!(
            protocol.document_message(true),
            HostMessage::Document {
                writable: false,
                ..
            }
        ));
        assert!(
            protocol
                .retain_draft(1, 0, format!("{text} input"))
                .is_err()
        );
        assert_eq!(
            protocol
                .draft
                .as_ref()
                .expect("oversized input recovery")
                .text,
            format!("{text} input")
        );
    }

    #[test]
    fn opaque_document_identifiers_round_trip_above_javascript_integer_precision() -> Result<()> {
        for document in [9_007_199_254_740_993, u64::MAX] {
            let message = ClientMessage::Ready {
                session: "session".into(),
                document,
            };
            let body = serde_json::to_string(&message)?;
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&body)?["document"],
                document.to_string()
            );
            assert_eq!(
                decode(
                    &application_url("session", document),
                    &body,
                    "session",
                    document
                )?,
                message
            );
        }
        assert!(
            serde_json::from_str::<ClientMessage>(
                r#"{"kind":"ready","session":"session","document":9007199254740993}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<ClientMessage>(
                r#"{"kind":"ready","session":"session","document":"01"}"#
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn surrogate_boundaries_and_minimal_unicode_delta() -> Result<()> {
        assert_eq!(utf16_to_byte("a😀é", 3)?, 5);
        assert!(utf16_to_byte("a😀é", 2).is_err());
        assert!(utf16_to_byte("a😀é", 5).is_err());
        assert_eq!(minimal_change("a😀é", "a😀ê"), Some((5..7, "ê".into())));
        Ok(())
    }

    #[test]
    fn rebase_and_overlap_preserve_draft() -> Result<()> {
        let mut protocol = Protocol::new("session".into(), 1, "hello world".into());
        protocol.retain_draft(1, 0, "hello WORLD".into())?;
        let message = edit(
            &protocol,
            1,
            1,
            Change {
                start: 6,
                end: 11,
                text: "WORLD".into(),
            },
        );
        protocol.update("prefix hello world".into());
        assert_eq!(
            protocol.prepare(&message, true)?,
            Some(vec![(13..18, "WORLD".into())])
        );
        protocol.update("prefix hello other".into());
        assert!(protocol.prepare(&message, true).is_err());
        assert_eq!(
            protocol.draft.as_ref().map(|draft| draft.text.as_str()),
            Some("hello WORLD")
        );
        Ok(())
    }

    #[test]
    fn separate_external_changes_rebase_and_real_overlap_keeps_draft() -> Result<()> {
        let mut protocol = Protocol::new("session".into(), 1, "0123456789".into());
        protocol.retain_draft(1, 0, "01234L6789".into())?;
        let message = edit(
            &protocol,
            1,
            1,
            Change {
                start: 5,
                end: 6,
                text: "L".into(),
            },
        );
        protocol.update("X12345678Y".into());
        assert_eq!(
            protocol.prepare(&message, true)?,
            Some(vec![(5..6, "L".into())])
        );
        protocol.update("prefix 012345678 suffix".into());
        assert_eq!(
            protocol.prepare(&message, true)?,
            Some(vec![(12..13, "L".into())])
        );
        protocol.update("X1234R678Y".into());
        assert!(protocol.prepare(&message, true).is_err());
        assert_eq!(
            protocol.draft.as_ref().map(|draft| draft.text.as_str()),
            Some("01234L6789")
        );
        assert!(protocol.validate_flush(1).is_err());
        assert!(rebase("0123456789", "01234X56789", 5..5).is_err());
        let base = "😀\r\n0123456789\r\n[ref]: /a";
        let current = base.replace("😀", "😁😁").replace("/a", "/long");
        assert_eq!(rebase(base, &current, 11..12)?, 15..16);
        Ok(())
    }

    #[test]
    fn no_op_acknowledgment_validates_draft_revision_sequence_and_permissions() -> Result<()> {
        let mut protocol = Protocol::new("session".into(), 1, "a".into());
        protocol.retain_draft(1, 0, "ab".into())?;
        let first = edit(
            &protocol,
            1,
            1,
            Change {
                start: 1,
                end: 1,
                text: "b".into(),
            },
        );
        protocol.retain_draft(2, 0, "abc".into())?;
        protocol.retain_draft(3, 0, "ab".into())?;
        protocol.prepare(&first, true)?;
        protocol.accept(first, "ab".into())?;
        assert!(protocol.validate_flush(3).is_err());
        let mut no_op = edit(
            &protocol,
            2,
            3,
            Change {
                start: 0,
                end: 0,
                text: String::new(),
            },
        );
        assert!(protocol.prepare(&no_op, true).is_err());
        protocol.retain_draft(4, 1, "ab".into())?;
        if let ClientMessage::Edit {
            sequence,
            base_revision,
            ..
        } = &mut no_op
        {
            *sequence = 4;
            *base_revision = 1;
        }
        assert_eq!(protocol.prepare(&no_op, false)?, Some(Vec::new()));
        assert!(protocol.validate_flush(4).is_err());
        assert_eq!(protocol.prepare(&no_op, true)?, Some(Vec::new()));
        let accepted = protocol.accept(no_op.clone(), "ab".into())?;
        assert_eq!(protocol.revision, 1);
        protocol.validate_flush(4)?;
        assert!(protocol.validate_flush(3).is_err());
        assert_eq!(protocol.accept(no_op, "ab".into())?, accepted);
        assert!(protocol.retain_draft(4, 1, "ab".into()).is_err());
        protocol.retain_draft(5, 1, "abc".into())?;
        assert!(protocol.validate_flush(5).is_err());
        let mut real_edit = edit(
            &protocol,
            3,
            5,
            Change {
                start: 2,
                end: 2,
                text: "c".into(),
            },
        );
        if let ClientMessage::Edit { base_revision, .. } = &mut real_edit {
            *base_revision = 1;
        }
        assert!(protocol.prepare(&real_edit, false).is_err());
        Ok(())
    }

    #[test]
    fn large_and_symmetric_disjoint_changes_preserve_both_change_sets() -> Result<()> {
        for (base, sides) in [
            ("0123456789".to_owned(), "X12345678Y".to_owned()),
            (
                format!("{}0123456789{}", "A".repeat(200), "B".repeat(200)),
                format!("{}0123456789{}", "C".repeat(200), "D".repeat(200)),
            ),
            (
                format!(
                    "{}\nthe unchanged paragraph contains ordinary repeated letters\n{}",
                    "first ".repeat(200),
                    "last ".repeat(200)
                ),
                format!(
                    "{}\nthe unchanged paragraph contains ordinary repeated letters\n{}",
                    "external first ".repeat(200),
                    "external last ".repeat(200)
                ),
            ),
        ] {
            let middle = if base.contains('5') {
                base.replace('5', "L")
            } else {
                base.replace("ordinary", "local")
            };
            let expected = if sides.contains('5') {
                sides.replace('5', "L")
            } else {
                sides.replace("ordinary", "local")
            };
            for (current, draft) in [(&sides, &middle), (&middle, &sides)] {
                let mut protocol = Protocol::new("session".into(), 1, base.clone());
                protocol.retain_draft(1, 0, draft.clone())?;
                let (range, text) = minimal_change(&base, draft).expect("local change");
                let message = edit(
                    &protocol,
                    1,
                    1,
                    Change {
                        start: base
                            .get(..range.start)
                            .expect("start")
                            .encode_utf16()
                            .count(),
                        end: base.get(..range.end).expect("end").encode_utf16().count(),
                        text,
                    },
                );
                protocol.update(current.clone());
                let changes = protocol.prepare(&message, true)?.expect("new edit");
                let mut result = current.clone();
                for (range, text) in changes.into_iter().rev() {
                    result.replace_range(range, &text);
                }
                assert_eq!(result, expected);
                protocol.accept(message, result)?;
                protocol.validate_flush(1)?;
            }
        }
        Ok(())
    }

    #[test]
    fn bounded_reconciliation_supports_sparse_large_changes_and_retains_divergent_drafts()
    -> Result<()> {
        let base = format!(
            "0{}5{}9",
            "a".repeat(MAX_DOCUMENT_BYTES / 2 - 1),
            "b".repeat(MAX_DOCUMENT_BYTES / 2 - 2)
        );
        let current = format!("X{}Y", &base[1..base.len() - 1]);
        assert_eq!(
            rebase(
                &base,
                &current,
                MAX_DOCUMENT_BYTES / 2..MAX_DOCUMENT_BYTES / 2 + 1
            )?,
            MAX_DOCUMENT_BYTES / 2..MAX_DOCUMENT_BYTES / 2 + 1
        );
        let mut protocol = Protocol::new("session".into(), 1, "a".repeat(256));
        let draft = format!("{}L{}", "a".repeat(128), "a".repeat(127));
        protocol.retain_draft(1, 0, draft.clone())?;
        protocol.update("b".repeat(256));
        let message = edit(
            &protocol,
            1,
            1,
            Change {
                start: 128,
                end: 129,
                text: "L".into(),
            },
        );
        assert!(
            protocol
                .prepare(&message, true)
                .expect_err("bounded conflict")
                .to_string()
                .contains("overlap")
        );
        assert_eq!(
            protocol.draft.as_ref().map(|draft| draft.text.as_str()),
            Some(draft.as_str())
        );
        assert!(protocol.validate_flush(1).is_err());
        Ok(())
    }

    #[test]
    fn duplicate_acknowledgments_and_out_of_order_requests() -> Result<()> {
        let mut protocol = Protocol::new("session".into(), 1, "a".into());
        protocol.retain_draft(1, 0, "ab".into())?;
        let message = edit(
            &protocol,
            2,
            1,
            Change {
                start: 1,
                end: 1,
                text: "b".into(),
            },
        );
        protocol.prepare(&message, true)?;
        let accepted = protocol.accept(message.clone(), "ab".into())?;
        assert_eq!(protocol.prepare(&message, true)?, None);
        assert_eq!(protocol.accept(message.clone(), "ab".into())?, accepted);
        let mut changed = message;
        if let ClientMessage::Edit { change, .. } = &mut changed {
            change.text = "c".into();
        }
        assert!(protocol.prepare(&changed, true).is_err());
        assert!(
            protocol
                .prepare(
                    &edit(
                        &protocol,
                        1,
                        1,
                        Change {
                            start: 1,
                            end: 1,
                            text: "b".into()
                        }
                    ),
                    true
                )
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn newer_queued_draft_survives_older_ack_and_readonly_rejection() -> Result<()> {
        let mut protocol = Protocol::new("session".into(), 1, "a".into());
        protocol.retain_draft(1, 0, "ab".into())?;
        let message = edit(
            &protocol,
            1,
            1,
            Change {
                start: 1,
                end: 1,
                text: "b".into(),
            },
        );
        protocol.retain_draft(2, 0, "abc".into())?;
        assert!(protocol.prepare(&message, false).is_err());
        protocol.accept(message, "ab".into())?;
        assert_eq!(
            protocol.draft.as_ref().map(|draft| draft.text.as_str()),
            Some("abc")
        );
        Ok(())
    }

    #[test]
    fn exact_source_preservation_and_foreign_bridge_rejection() -> Result<()> {
        let source = "---\r\nx: 1\r\n---\r\n[ref]: /a\r\n<script>alert(1)</script>\r\n```unknown\r\nx\r\n```";
        let mut protocol = Protocol::new("session".into(), 1, source.into());
        protocol.update(source.into());
        assert_eq!(protocol.revision, 0);
        assert_eq!(protocol.current(), source);
        let body = r#"{"kind":"ready","session":"session","document":"1"}"#;
        assert!(decode(&application_url("session", 1), body, "session", 1).is_ok());
        assert!(decode(&application_url("other", 1), body, "session", 1).is_err());
        assert!(decode(&application_url("session", 1), body, "session", 2).is_err());
        assert!(
            decode(
                &application_url("session", 1),
                &body.replace(r#""document":"1""#, r#""document":"1","script":"evil""#),
                "session",
                1
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn acknowledged_drafts_cannot_reappear_and_pinned_snapshots_are_bounded() -> Result<()> {
        let mut protocol = Protocol::new("session".into(), 1, "a".into());
        protocol.retain_draft(1, 0, "ab".into())?;
        let message = edit(
            &protocol,
            1,
            1,
            Change {
                start: 1,
                end: 1,
                text: "b".into(),
            },
        );
        protocol.accept(message, "ab".into())?;
        assert!(protocol.retain_draft(1, 0, "ab".into()).is_err());
        protocol.retain_draft(2, 0, "abc".into())?;
        for revision in 0..300 {
            protocol.update(format!("external {revision}"));
        }
        assert!(protocol.snapshots.len() <= 128);
        assert!(protocol.snapshots.contains_key(&0));
        Ok(())
    }
}
