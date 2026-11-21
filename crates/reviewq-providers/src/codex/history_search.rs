use std::collections::VecDeque;

use reviewq_app::access::AppError;
use reviewq_app::history::search::rpc::{RpcSearchPage, RpcSearchPoint, RpcSearchQuery};
use reviewq_app::history::search::{MAX_SEARCH_HITS, SearchHit};
use reviewq_app::history::{
    HistorySession, NativePosition, RpcContentRef, VisibleEntry, VisiblePartKind,
};
use reviewq_app::reading::{FoldedText, Literal, TextMatch, displayed_markdown};
use reviewq_domain::identity::NativeInputId;
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
        let observation = self.read_history_stamp(session.clone()).await?;
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
        let page = self
            .read_history_projected(session.clone(), at.page.clone(), INPUT_ITEMS, false)
            .await?;
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
            .map(|id| {
                entries
                    .iter()
                    .position(|item| &item.id == id)
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
            partial: false,
        };
        for entry in &entries[start..] {
            if let Some(next) =
                search_entry(entry, &at, &query.literal, query.reverse, &mut output)?
            {
                output.next = Some(next);
                break;
            }
        }
        if self.read_history_stamp(session).await? != output.observation {
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

fn search_entry(
    entry: &VisibleEntry,
    at: &RpcSearchPoint,
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
    let same_entry = at.entry.as_ref() == Some(&entry.id);
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
        let point = RpcSearchPoint {
            page: at.page.clone(),
            entry: Some(entry.id.clone()),
            part: key,
            byte: boundary,
        };
        let available = MAX_SEARCH_HITS - output.hits.len();
        if available == 0 {
            return Ok(Some(point));
        }
        let markdown = body.markdown && body.text.len() <= 65_536;
        let projected = if markdown {
            match displayed_markdown(body.text) {
                Some(display) => Some(display),
                None => {
                    output.partial = true;
                    continue;
                }
            }
        } else {
            None
        };
        let text = projected.as_deref().unwrap_or(body.text);
        let reference = RpcContentRef {
            generation: output.observation.generation.clone(),
            turn: turn.clone(),
            entry: entry.id.clone(),
            part: body.part,
            hash: digest(body.text),
        };
        let mut matcher = RpcMatches {
            point,
            reference,
            literal,
            available,
            reverse,
            markdown,
            ranged: !markdown && body.text.len() > 65_536,
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
            None
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

struct RpcMatches<'a> {
    point: RpcSearchPoint,
    reference: RpcContentRef,
    literal: &'a Literal,
    available: usize,
    reverse: bool,
    markdown: bool,
    ranged: bool,
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
