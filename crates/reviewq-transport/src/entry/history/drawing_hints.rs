use std::sync::Arc;

use reviewq_app::access::Actor;
use reviewq_app::history::{
    ContentReference, FileContentRef, RpcContentRef, VisibleEntry, VisiblePart,
};
use reviewq_domain::identity::{ReportHash, RunId, SessionKey};
use reviewq_protocol::history::HistoryPart;

use crate::entry::Entry;
use crate::entry::diagrams::{self, Owner};

use super::content;

pub(super) struct InlineDisplay<'a> {
    pub entry: &'a Arc<Entry>,
    pub actor: &'a Actor,
    pub run: &'a RunId,
    pub session: &'a SessionKey,
    pub font: Option<ReportHash>,
}

impl InlineDisplay<'_> {
    pub(super) fn entry(
        &self,
        projected: &mut HistoryPart,
        entry: &VisibleEntry,
        generation: &str,
        part: Option<&VisiblePart>,
    ) {
        let full = part.map_or(
            entry.text_bytes == Some(entry.text.len() as u64),
            |visible| visible.content.is_none(),
        );
        if !full {
            let reference = part
                .and_then(|part| part.content.as_ref())
                .or(entry.content.as_ref());
            let bytes = part.map(|part| part.text_bytes).or(entry.text_bytes);
            if bytes.is_some_and(|bytes| bytes <= 64 * 1024) {
                self.enrich(projected, false, |_| reference.cloned());
            }
            return;
        }
        self.enrich(projected, true, |text| {
            let index = part.map(|part| part.native_index);
            if let Some(file) = entry.content.as_ref().and_then(ContentReference::file) {
                return Some(ContentReference::File(FileContentRef {
                    part: index,
                    ..file.clone()
                }));
            }
            Some(ContentReference::Rpc(RpcContentRef {
                generation: generation.to_owned(),
                entry: entry.id.clone(),
                turn: entry.turn.clone()?,
                part: index,
                hash: diagrams::digest(text),
            }))
        });
    }

    pub(super) fn part(
        &self,
        projected: &mut HistoryPart,
        file: &FileContentRef,
        part: &VisiblePart,
    ) {
        if part.content.is_none() {
            self.enrich(projected, true, |_| {
                Some(ContentReference::File(FileContentRef {
                    part: Some(part.native_index),
                    ..file.clone()
                }))
            });
        }
    }

    fn enrich(
        &self,
        projected: &mut HistoryPart,
        complete: bool,
        witness: impl FnOnce(&str) -> Option<ContentReference>,
    ) {
        if !self.entry.drawings.enabled() {
            return;
        }
        let (text, display) = match projected {
            HistoryPart::AssistantText { text, display, .. }
            | HistoryPart::UserPrompt { text, display, .. }
            | HistoryPart::Assignment { text, display, .. }
            | HistoryPart::Plan { text, display, .. } => (text.as_str(), display),
            HistoryPart::ToolCall { .. } | HistoryPart::ToolResult { .. } => return,
        };
        if !text.contains("mermaid") {
            return;
        }
        if let Some(reference) = witness(text).and_then(|source| {
            content::reference(&self.entry.auth.endpoint, self.run, self.session, &source).ok()
        }) {
            let owner = Owner::Native {
                run: self.run.to_string(),
                session: self.session.to_string(),
                source: reference.as_str().to_owned(),
                window: None,
            };
            *display = if complete {
                diagrams::observe(self.entry, self.actor, owner, text, self.font.as_ref())
            } else {
                diagrams::known(self.entry, self.actor, owner, text)
            };
        }
    }
}
