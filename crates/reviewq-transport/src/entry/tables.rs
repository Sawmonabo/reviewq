use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reviewq_app::reading::table::{self, Column, Inline, MIN_TABLE_BYTES};
use reviewq_protocol::reports::{DocumentDisplay, PreparedTable, TableColumn, TableInline};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

const MAX_ENTRIES: usize = 4;
const RETENTION: Duration = Duration::from_secs(30);
const PACKET_BYTES: usize = 96 * 1024;
const MAX_LAYOUT_COST: usize = 300 * 1024;

type Key = [u8; 32];
type Entries = VecDeque<(Key, Option<Arc<PreparedTable>>, Instant)>;

pub(super) struct Cache {
    entries: Arc<Mutex<Entries>>,
    producer: Arc<Semaphore>,
}

impl Cache {
    pub(super) fn new() -> Self {
        let entries = Arc::new(Mutex::new(Entries::new()));
        let expire = Arc::downgrade(&entries);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let Some(entries) = expire.upgrade() else {
                    break;
                };
                if let Ok(mut entries) = entries.lock() {
                    entries.retain(|(_, _, at)| at.elapsed() < RETENTION);
                }
            }
        });
        Self {
            entries,
            producer: Arc::new(Semaphore::new(1)),
        }
    }

    /// The caller supplies bytes from a verified, individually bounded Markdown block.
    pub(super) async fn read(&self, source: &str) -> Option<PreparedTable> {
        if source.len() < MIN_TABLE_BYTES || source.len() > 64 * 1024 {
            return None;
        }
        let key = Sha256::digest(source.as_bytes()).into();
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
            let result = prepare(&source).map(Arc::new);
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
    let encoded = serde_json::to_vec(&packet).ok()?.len() + 8;
    if encoded > PACKET_BYTES || source.len() + encoded + nodes * 40 > MAX_LAYOUT_COST {
        return None;
    }
    packet.encoded_bytes = encoded as u32;
    Some(packet)
}

pub(super) fn attach(display: &mut Option<DocumentDisplay>, table: PreparedTable) {
    display
        .get_or_insert_with(|| DocumentDisplay {
            drawings: Vec::new(),
            table: None,
        })
        .table = Some(table);
}
