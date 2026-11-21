use std::collections::VecDeque;

use reviewq_app::access::AppError;
use reviewq_app::history::document::{MAX_INDEX_BYTES, Query as DocumentQuery};
use reviewq_app::history::search::document::project;
use reviewq_app::history::search::rpc::{
    RpcSearchEntry, RpcSearchPage, RpcSearchPoint, RpcSearchQuery,
};
use reviewq_app::history::search::{DocumentMatch, MAX_SEARCH_HITS, SearchHit};
use reviewq_app::history::{
    HistorySession, NativePosition, RpcContentRef, VisibleEntry, VisibleKind, VisiblePartKind,
};
use reviewq_app::reading::{FoldedText, Literal, TextMatch, displayed_markdown};
use reviewq_domain::identity::{NativeInputId, ReportHash};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

use super::{Codex, history_reads::digest};

const INPUT_ITEMS: u16 = 4;
const BODY_BLOCK: usize = 16 * 1024;
static READS: Semaphore = Semaphore::const_new(2);

impl Codex {
    pub(super) async fn search_rpc_history(
        &self,
        session: HistorySession,
        query: RpcSearchQuery,
    ) -> Result<RpcSearchPage, AppError> {
        let _slot = READS.try_acquire().map_err(|_| AppError::Busy)?;
        let observation = self.read_history_stamp(session.clone(), true).await?;
        let at = if let Some((saved, position)) = query.from {
            if saved != observation {
                return Err(AppError::CursorExpired);
            }
            position
        } else {
            at_page(NativePosition {
                ascending: !query.reverse,
                ..NativePosition::default()
            })
        };
        let mut limit = at
            .entry
            .as_ref()
            .map_or(INPUT_ITEMS, |entry| entry.page_size);
        let projected = loop {
            let result = self
                .read_history_projected(session.clone(), at.page.clone(), limit, false)
                .await;
            match result {
                Err(AppError::Unavailable) if at.entry.is_none() && limit > 1 => limit /= 2,
                result => break result?,
            }
        };
        let page = projected.page;
        if page.generation != observation.generation {
            return Err(AppError::CursorExpired);
        }
        let mut entries = page.entries.iter().collect::<Vec<_>>();
        if query.reverse {
            entries.reverse();
        }
        let start = at
            .entry
            .as_ref()
            .map(|entry| {
                entries
                    .iter()
                    .position(|item| item.id == entry.id)
                    .ok_or(AppError::CursorExpired)
            })
            .transpose()?
            .unwrap_or(0);
        let continuation = if query.reverse {
            page.older
        } else {
            page.newer
        };
        if continuation.as_ref() == Some(&at.page) {
            return Err(AppError::CursorExpired);
        }
        let mut output = RpcSearchPage {
            observation,
            hits: Vec::with_capacity(MAX_SEARCH_HITS),
            next: continuation.map(at_page),
            partial: projected.partial,
        };
        for entry in &entries[start..] {
            if let Some(next) = search_entry(
                self,
                entry,
                &at,
                limit,
                &query.literal,
                query.reverse,
                &mut output,
            )
            .await?
            {
                output.next = Some(next);
                break;
            }
        }
        if self.read_history_stamp(session, true).await? != output.observation {
            return Err(AppError::CursorExpired);
        }
        Ok(output)
    }
}

fn at_page(page: NativePosition) -> RpcSearchPoint {
    RpcSearchPoint {
        page,
        entry: None,
        part: 0,
        window: None,
        byte: 0,
    }
}

struct Body<'a> {
    part: Option<u16>,
    text: &'a str,
    markdown: bool,
    tool: Option<&'a NativeInputId>,
}

impl Body<'_> {
    fn key(&self) -> u16 {
        self.part.map_or(0, |part| part + 1)
    }
}

async fn search_entry(
    codex: &Codex,
    entry: &VisibleEntry,
    at: &RpcSearchPoint,
    page_size: u16,
    literal: &Literal,
    reverse: bool,
    output: &mut RpcSearchPage,
) -> Result<Option<RpcSearchPoint>, AppError> {
    let turn = entry.turn.as_ref().ok_or(AppError::Unavailable)?;
    let mut bodies = if entry.parts.is_empty() {
        vec![Body {
            part: None,
            text: &entry.text,
            markdown: true,
            tool: None,
        }]
    } else {
        entry
            .parts
            .iter()
            .map(|part| {
                let tool = match &part.kind {
                    VisiblePartKind::ToolCall { call, .. }
                    | VisiblePartKind::ToolResult { call, .. } => Some(call),
                    _ => None,
                };
                Body {
                    part: Some(part.native_index),
                    text: &part.text,
                    markdown: tool.is_none(),
                    tool,
                }
            })
            .collect()
    };
    if reverse {
        bodies.reverse();
    }
    let witness = RpcSearchEntry {
        id: entry.id.clone(),
        hash: visible_digest(entry),
        page_size,
    };
    let same_entry = at.entry.as_ref().is_some_and(|saved| saved.id == entry.id);
    if same_entry && at.entry.as_ref() != Some(&witness) {
        return Err(AppError::CursorExpired);
    }
    for body in bodies {
        let key = body.key();
        if same_entry
            && if reverse {
                key > at.part
            } else {
                key < at.part
            }
        {
            continue;
        }
        let boundary = if same_entry && key == at.part {
            at.byte
        } else if reverse {
            u64::MAX
        } else {
            0
        };
        let mut point = RpcSearchPoint {
            page: at.page.clone(),
            entry: Some(witness.clone()),
            part: key,
            window: if same_entry && key == at.part {
                at.window
            } else {
                None
            },
            byte: boundary,
        };
        let available = MAX_SEARCH_HITS - output.hits.len();
        if available == 0 {
            return Ok(Some(point));
        }
        let reference = RpcContentRef {
            generation: output.observation.generation.clone(),
            turn: turn.clone(),
            entry: entry.id.clone(),
            part: body.part,
            hash: digest(body.text),
        };
        let mut document = None;
        let mut next_window = None;
        let projected = if body.markdown && body.text.len() > 65_536 {
            if body.text.len() > MAX_INDEX_BYTES
                || (entry.kind != VisibleKind::User && entry.at_ms.is_none())
            {
                output.partial = true;
                continue;
            }
            let offset = point.window.unwrap_or_else(|| {
                if reverse {
                    body.text.len() as u64 - 1
                } else {
                    0
                }
            });
            let projection = project(
                codex
                    .project_item_document(
                        body.text.to_owned(),
                        reference.hash.clone(),
                        DocumentQuery {
                            offset,
                            expected: Some(reference.hash.clone()),
                        },
                    )
                    .await?,
            );
            if point
                .window
                .is_some_and(|at| at != projection.proof.byte_start)
            {
                return Err(AppError::CursorExpired);
            }
            point.window = Some(projection.proof.byte_start);
            document = Some(projection.proof);
            next_window = if reverse {
                projection.previous
            } else {
                projection.next
            };
            projection.text
        } else if body.markdown {
            displayed_markdown(body.text)
        } else {
            None
        };
        if body.markdown && projected.is_none() {
            output.partial = true;
        }
        let text = if body.markdown {
            projected.as_deref().unwrap_or_default()
        } else {
            body.text
        };
        let mut matcher = RpcMatches {
            point,
            reference,
            literal,
            available,
            reverse,
            markdown: body.markdown,
            document,
            ranged: !body.markdown && body.text.len() > 65_536,
            output_bytes: body.text.len() as u64,
            tool: body.tool,
            hits: VecDeque::new(),
        };
        let partial = matcher.collect(text);
        output.partial |= partial;
        let next = if matcher.hits.len() == available {
            let hit = if reverse {
                matcher.hits.front()
            } else {
                matcher.hits.back()
            }
            .ok_or(AppError::Internal)?;
            let mut point = hit.point.clone();
            if !reverse {
                point.byte += 1;
            }
            Some(point)
        } else {
            next_window.map(|window| RpcSearchPoint {
                window: Some(window),
                byte: if reverse { u64::MAX } else { 0 },
                ..matcher.point.clone()
            })
        };
        if reverse {
            output.hits.extend(matcher.hits.into_iter().rev());
        } else {
            output.hits.extend(matcher.hits);
        }
        if next.is_some() {
            return Ok(next);
        }
    }
    Ok(None)
}

fn visible_digest(entry: &VisibleEntry) -> ReportHash {
    let mut hash = Sha256::new();
    let fields = std::iter::once(entry.text.as_str())
        .chain(entry.parts.iter().map(|part| part.text.as_str()));
    for text in fields {
        hash.update((text.len() as u64).to_be_bytes());
        hash.update(text.as_bytes());
    }
    ReportHash::from_sha256(hash.finalize().into())
}

struct RpcMatches<'a> {
    point: RpcSearchPoint,
    reference: RpcContentRef,
    literal: &'a Literal,
    available: usize,
    reverse: bool,
    markdown: bool,
    document: Option<DocumentMatch>,
    ranged: bool,
    output_bytes: u64,
    tool: Option<&'a NativeInputId>,
    hits: VecDeque<SearchHit<RpcContentRef, RpcSearchPoint>>,
}

impl RpcMatches<'_> {
    fn collect(&mut self, text: &str) -> bool {
        let mut partial = false;
        let mut byte = 0;
        let mut utf16 = 0;
        while byte < text.len() {
            let mut end = if self.markdown {
                text.len()
            } else {
                text.floor_char_boundary((byte + BODY_BLOCK).min(text.len()))
            };
            let mut folded = FoldedText::new(&text[byte..end]);
            let done = end == text.len();
            let mut cut = if done {
                reviewq_app::reading::TextPoint {
                    byte: end - byte,
                    utf16: text[byte..end].encode_utf16().count(),
                }
            } else {
                folded.settled_prefix(self.literal.folded().len())
            };
            if cut.byte == 0 {
                end = text.floor_char_boundary((byte + 65_536).min(text.len()));
                folded = FoldedText::new(&text[byte..end]);
                cut = folded.settled_prefix(self.literal.folded().len());
                if cut.byte == 0 {
                    utf16 += text[byte..end].encode_utf16().count();
                    byte = end;
                    partial = true;
                    continue;
                }
            }
            let window = &text[byte..end];
            for span in folded
                .occurrences(self.literal)
                .take_while(|span| span.start.byte < cut.byte)
            {
                let anchor = (byte + span.start.byte) as u64;
                if if self.reverse {
                    anchor >= self.point.byte
                } else {
                    anchor < self.point.byte
                } {
                    continue;
                }
                let witness = &window[span.start.byte..span.end.byte];
                if witness.len() > 1024 {
                    partial = true;
                    continue;
                }
                if self.reverse && self.hits.len() == self.available {
                    self.hits.pop_front();
                }
                self.hits
                    .push_back(self.witness(window, witness, span, anchor, utf16 as u64));
                if !self.reverse && self.hits.len() == self.available {
                    return partial;
                }
            }
            byte += cut.byte;
            utf16 += cut.utf16;
            if self.reverse && (byte as u64) >= self.point.byte {
                break;
            }
        }
        partial
    }

    fn witness(
        &self,
        window: &str,
        found: &str,
        span: TextMatch,
        anchor: u64,
        utf16: u64,
    ) -> SearchHit<RpcContentRef, RpcSearchPoint> {
        let mut point = self.point.clone();
        point.byte = anchor;
        let before = &window[..span.start.byte];
        let from = before.char_indices().rev().nth(31).map_or(0, |(at, _)| at);
        SearchHit {
            reference: self.reference.clone(),
            point,
            tool_call: self.tool.cloned(),
            before: before[from..].to_owned(),
            matched: found.to_owned(),
            after: window[span.end.byte..].chars().take(32).collect(),
            output_offset: if self.ranged { anchor } else { 0 },
            output_bytes: self.output_bytes,
            start_utf16: if self.ranged {
                0
            } else {
                utf16 + span.start.utf16 as u64
            },
            end_utf16: if self.ranged {
                (span.end.utf16 - span.start.utf16) as u64
            } else {
                utf16 + span.end.utf16 as u64
            },
            markdown: self.markdown,
            document: self.document.clone(),
            ordinal: if self.markdown {
                before
                    .char_indices()
                    .filter(|(i, _)| window[*i..].starts_with(found))
                    .count() as u64
            } else {
                0
            },
        }
    }
}
