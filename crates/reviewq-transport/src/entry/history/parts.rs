use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use reviewq_app::access::AppError;
use reviewq_app::history::FileContentRef;
use reviewq_domain::identity::{RunId, SessionKey};
use reviewq_protocol::history::{HistoryPartPage, HistoryPartPageResponse};
use reviewq_protocol::{HistoryCursorDto, HistoryEntryIdDto};
use serde::{Deserialize, Serialize};

use super::drawing_hints::InlineDisplay;
use super::{Entry, Point, ServiceEndpoint, visible_part};
use crate::entry::error::{ApiError, ApiPath as Path, ApiQuery as Query};
use crate::entry::pages::{parse, success};

const KIND: &[u8] = b"reviewq-native-part-window-v1";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::entry) struct Request {
    cursor: String,
}

#[derive(Deserialize, Serialize)]
struct Address {
    run: String,
    session: String,
    generation: String,
    item: String,
    point: Point,
    first: u16,
}

pub(super) fn cursor(
    endpoint: &ServiceEndpoint,
    run: &RunId,
    session: &SessionKey,
    reference: &FileContentRef,
    first: u16,
) -> Result<HistoryCursorDto, AppError> {
    let value = Address {
        run: run.to_string(),
        session: session.to_string(),
        generation: reference.generation.clone(),
        item: reference.entry.to_string(),
        point: reference.position.clone().into(),
        first,
    };
    HistoryCursorDto::new(endpoint.encode_cursor(KIND, &value)?).ok_or(AppError::Internal)
}

pub(in crate::entry) async fn read(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path((run, session)): Path<(String, String)>,
    Query(request): Query<Request>,
) -> Result<Json<HistoryPartPageResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let run: RunId = parse(&run)?;
    let session: SessionKey = parse(&session)?;
    let endpoint = &entry.auth.endpoint;
    let address: Address = endpoint.decode_cursor(KIND, &request.cursor)?;
    if address.run != run.as_str() || address.session != session.as_str() {
        return Err(AppError::CursorExpired.into());
    }
    let source = entry
        .store
        .history_session(actor.clone(), run.clone(), session.clone())
        .await?;
    let reference = FileContentRef {
        generation: address.generation,
        entry: parse(&address.item)?,
        part: None,
        position: address.point.into(),
    };
    let viewed = entry
        .file_history
        .parts(source.value.clone(), reference.clone(), address.first)
        .await?;
    if entry
        .store
        .history_session(actor.clone(), run.clone(), session.clone())
        .await?
        .value
        != source.value
    {
        return Err(AppError::CursorExpired.into());
    }
    let next_cursor = viewed
        .next
        .map(|first| cursor(endpoint, &run, &session, &reference, first))
        .transpose()?;
    let display = InlineDisplay {
        entry: &entry,
        actor: &actor,
        run: &run,
        session: &session,
        font: crate::entry::diagrams::client_font(&headers),
    };
    let parts = viewed
        .parts
        .iter()
        .map(|part| {
            let mut projected = visible_part(endpoint, &run, &session, part)?;
            display.part(&mut projected, &reference, part);
            Ok::<_, AppError>(projected)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(success(
        HistoryPartPage {
            entry_id: HistoryEntryIdDto::new(reference.entry.as_str()).ok_or(AppError::Internal)?,
            parts,
            next_cursor,
        },
        source.cursor,
    )?))
}
