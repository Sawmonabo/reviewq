use std::collections::VecDeque;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reviewq_app::access::AppError;
use reviewq_app::reading::table::paged::{self, Index};
use reviewq_app::reading::table::{self, Column, Inline, MIN_TABLE_BYTES};
use reviewq_app::reports::{DisplayBlockKind, DisplayIndex, build_display_index};
use reviewq_domain::identity::ReportHash;
use reviewq_protocol::reports::{DocumentDisplay, PreparedTable, TableColumn, TableInline};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex as AsyncMutex, Semaphore, mpsc};

const MAX_ENTRIES: usize = 4;
const RETENTION: Duration = Duration::from_secs(30);
const PACKET_BYTES: usize = 96 * 1024;
const MAX_LAYOUT_COST: usize = 300 * 1024;

#[derive(Clone, Copy, PartialEq)]
enum Shape {
    Block,
    Document,
}

type Key = (Shape, [u8; 32]);
type Entries = VecDeque<(Key, Option<Arc<PreparedTable>>, Instant)>;

pub(super) struct Cache {
    entries: Arc<Mutex<Entries>>,
    producer: Arc<Semaphore>,
    pages: Arc<AsyncMutex<Option<IndexedTable>>>,
}

struct IndexedTable {
    hash: ReportHash,
    at: Instant,
    index: Option<Arc<Index>>,
}

impl Cache {
    pub(super) fn new() -> Self {
        let entries = Arc::new(Mutex::new(Entries::new()));
        let expire = Arc::downgrade(&entries);
        let pages: Arc<AsyncMutex<Option<IndexedTable>>> = Arc::default();
        let expire_pages = Arc::downgrade(&pages);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let Some(entries) = expire.upgrade() else {
                    break;
                };
                if let Ok(mut entries) = entries.lock() {
                    entries.retain(|(_, _, at)| at.elapsed() < RETENTION);
                }
                if let Some(pages) = expire_pages.upgrade()
                    && let Ok(mut current) = pages.try_lock()
                    && current
                        .as_ref()
                        .is_some_and(|item| item.at.elapsed() >= RETENTION)
                {
                    *current = None;
                }
            }
        });
        Self {
            entries,
            producer: Arc::new(Semaphore::new(1)),
            pages,
        }
    }

    /// The caller has authorized these immutable report bytes and rechecks the
    /// receipt while resolving each chunk. The retained index holds only spans.
    pub(super) async fn ranges<F>(
        &self,
        hash: ReportHash,
        bytes: u64,
        mut read_chunk: impl FnMut(u64) -> F + Send,
    ) -> Result<Option<Arc<Index>>, AppError>
    where
        F: Future<Output = Result<Option<Vec<u8>>, AppError>> + Send,
    {
        if !(MIN_TABLE_BYTES as u64..=paged::MAX_INDEXED_BYTES).contains(&bytes) {
            return Ok(None);
        }
        let mut cache = self.pages.lock().await;
        if let Some(page) = cache
            .as_mut()
            .filter(|page| page.hash == hash && page.at.elapsed() < RETENTION)
        {
            page.at = Instant::now();
            return Ok(page.index.clone());
        }
        let (input, mut chunks) = mpsc::channel::<Vec<u8>>(2);
        let expected = hash.clone();
        let worker = tokio::task::spawn_blocking(move || {
            let mut builder = paged::Builder::default();
            let mut digest = Sha256::new();
            let mut seen = 0;
            while let Some(raw) = chunks.blocking_recv() {
                let text = std::str::from_utf8(&raw).map_err(|_| AppError::Internal)?;
                seen += raw.len() as u64;
                digest.update(&raw);
                builder.push(text);
                if builder.declined() {
                    return Ok(None);
                }
            }
            if seen != bytes || ReportHash::from_sha256(digest.finalize().into()) != expected {
                return Err(AppError::CursorExpired);
            }
            Ok(builder.finish().map(Arc::new))
        });
        let mut sent = 0;
        let mut ordinal = 0;
        while sent < bytes {
            let chunk = read_chunk(ordinal).await?.ok_or(AppError::CursorExpired)?;
            if chunk.is_empty() || sent + chunk.len() as u64 > bytes {
                return Err(AppError::CursorExpired);
            }
            sent += chunk.len() as u64;
            if input.send(chunk).await.is_err() {
                break;
            }
            ordinal += 1;
        }
        drop(input);
        let index = worker.await.map_err(|_| AppError::Unavailable)??;
        *cache = Some(IndexedTable {
            hash,
            at: Instant::now(),
            index: index.clone(),
        });
        Ok(index)
    }

    /// The caller supplies bytes from a verified, individually bounded Markdown block.
    pub(super) async fn read(&self, source: &str) -> Option<PreparedTable> {
        self.prepare(source, Shape::Block).await
    }

    pub(super) async fn document(&self, source: &str) -> Option<PreparedTable> {
        if !source.contains('|') {
            return None;
        }
        self.prepare(source, Shape::Document).await
    }

    async fn prepare(&self, source: &str, shape: Shape) -> Option<PreparedTable> {
        let ceiling = match shape {
            Shape::Block => 64 * 1024,
            Shape::Document => 128 * 1024,
        };
        if !(MIN_TABLE_BYTES..=ceiling).contains(&source.len()) {
            return None;
        }
        let key = (shape, Sha256::digest(source.as_bytes()).into());
        if let Some(cached) = lookup(&self.entries, &key) {
            return cached.as_deref().cloned();
        }
        let permit = self.producer.clone().try_acquire_owned().ok()?;
        let entries = self.entries.clone();
        let source = source.to_owned();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            if let Some(cached) = lookup(&entries, &key) {
                return cached.as_deref().cloned();
            }
            let result = match shape {
                Shape::Block => prepare(&source),
                Shape::Document => prepare_document(&source),
            }
            .map(Arc::new);
            let mut entries = entries.lock().ok()?;
            entries.push_back((key, result.clone(), Instant::now()));
            while entries.len() > MAX_ENTRIES {
                entries.pop_front();
            }
            result.as_deref().cloned()
        })
        .await
        .ok()
        .flatten()
    }
}

fn lookup(entries: &Mutex<Entries>, key: &Key) -> Option<Option<Arc<PreparedTable>>> {
    let mut entries = entries.lock().ok()?;
    entries.retain(|(_, _, at)| at.elapsed() < RETENTION);
    let index = entries.iter().position(|(digest, _, _)| digest == key)?;
    let mut item = entries.remove(index)?;
    item.2 = Instant::now();
    let result = item.1.clone();
    entries.push_back(item);
    Some(result)
}

fn prepare_document(source: &str) -> Option<PreparedTable> {
    let DisplayIndex::Indexed(layout) = build_display_index(source) else {
        return None;
    };
    for block in layout.blocks {
        if block.kind != DisplayBlockKind::Table {
            continue;
        }
        let markdown = source.get(block.byte_start as usize..block.byte_end as usize)?;
        let Some(mut packet) = prepare(markdown) else {
            continue;
        };
        let base = block.utf16_start;
        packet.start += base;
        packet.end += base;
        for token in packet.rows.iter_mut().flatten().flatten() {
            if let TableInline::Text { start, end } | TableInline::Code { start, end, .. } = token {
                *start += base;
                *end += base;
            }
        }
        update_budget(&mut packet, source.len())?;
        return Some(packet);
    }
    None
}

fn prepare(source: &str) -> Option<PreparedTable> {
    let table = table::prepare_table(source)?;
    let mut nodes = 10 + table.rows.len() * (table.columns.len() * 2 + 3);
    let rows = table
        .rows
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|cell| {
                    cell.into_iter()
                        .map(|operation| {
                            nodes += match operation {
                                Inline::Text { .. } | Inline::Code { .. } => 2,
                                Inline::Close => 0,
                                _ => 1,
                            };
                            match operation {
                                Inline::Text { start, end } => TableInline::Text { start, end },
                                Inline::Code { start, end, text } => {
                                    TableInline::Code { start, end, text }
                                }
                                Inline::Emphasis => TableInline::Emphasis,
                                Inline::Strong => TableInline::Strong,
                                Inline::Strike => TableInline::Strike,
                                Inline::Link { href, title } => TableInline::Link { href, title },
                                Inline::Close => TableInline::Close,
                            }
                        })
                        .collect()
                })
                .collect()
        })
        .collect();
    let mut packet = PreparedTable {
        start: table.start,
        end: table.end,
        columns: table
            .columns
            .into_iter()
            .map(|column| match column {
                Column::Default => TableColumn::Default,
                Column::Left => TableColumn::Left,
                Column::Center => TableColumn::Center,
                Column::Right => TableColumn::Right,
            })
            .collect(),
        rows,
        encoded_bytes: 0,
        nodes: nodes as u32,
    };
    update_budget(&mut packet, source.len())?;
    Some(packet)
}

fn update_budget(packet: &mut PreparedTable, source_bytes: usize) -> Option<()> {
    packet.encoded_bytes = 0;
    let encoded = serde_json::to_vec(&packet).ok()?.len() + 8;
    if encoded > PACKET_BYTES
        || source_bytes + encoded + packet.nodes as usize * 40 > MAX_LAYOUT_COST
    {
        return None;
    }
    packet.encoded_bytes = encoded as u32;
    Some(())
}

pub(super) fn attach(display: &mut Option<DocumentDisplay>, table: PreparedTable) {
    display
        .get_or_insert_with(|| DocumentDisplay {
            drawings: Vec::new(),
            table: None,
        })
        .table = Some(table);
}
