use reviewq_app::changes::{MAX_DISCUSSION_TEXT_SOURCE_BYTES, discussion_text};
use reviewq_protocol::changes::{DiscussionResolution, PatchDiscussion, PatchDiscussionText};
use tokio::sync::Semaphore;

const MAX_INPUT_BYTES: usize = 128 * 1024;
const MAX_WIRE_BYTES: usize = 64 * 1024;
const MAX_NOTES: usize = 64;
static PROJECTION_SLOTS: Semaphore = Semaphore::const_new(2);

pub(super) async fn project_page(items: &[PatchDiscussion]) -> Option<Vec<PatchDiscussionText>> {
    let mut bytes = 0;
    let mut candidates = Vec::new();
    let open_notes = items
        .iter()
        .filter(|item| !matches!(item.resolution, DiscussionResolution::Resolved))
        .flat_map(|item| &item.notes);

    for note in open_notes {
        let size = note.body.len();
        if size > MAX_DISCUSSION_TEXT_SOURCE_BYTES || bytes + size > MAX_INPUT_BYTES {
            continue;
        }
        bytes += size;
        candidates.push((note.note_id.clone(), note.body.clone()));
        if candidates.len() == MAX_NOTES {
            break;
        }
    }
    if candidates.is_empty() {
        return None;
    }

    let permit = reviewq_app::wait::permit(&PROJECTION_SLOTS).await.ok()?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let mut items = Vec::new();
        // Count the brackets and separators of the optional JSON array too.
        let mut bytes = 2;
        for (note_id, body) in candidates {
            if let Some(text) = discussion_text(&body) {
                let item = PatchDiscussionText { note_id, text };
                let size = serde_json::to_string(&item).ok()?.len();
                if bytes + size < MAX_WIRE_BYTES {
                    bytes += size + 1;
                    items.push(item);
                }
            }
        }
        (!items.is_empty()).then_some(items)
    })
    .await
    .ok()
    .flatten()
}
