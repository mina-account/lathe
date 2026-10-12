use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, ops::Range};

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

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientMessage {
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
    bail!("Concurrent edits overlap; the visual draft is retained for resolution")
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
    ) -> Result<Option<(Range<usize>, String)>> {
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
        if !writable {
            bail!("Document is read-only; the visual draft is retained");
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
        let range = rebase(base, self.current(), start..end)?;
        Ok(Some((range, change.text.clone())))
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
        if request <= self.last_request || base_revision != self.revision {
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
            Some((13..18, "WORLD".into()))
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
