use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use reviewq_app::access::AppError;
use reviewq_app::reports::ReportReference;
use reviewq_domain::identity::RunId;
use reviewq_protocol::ResourceIdDto;
use reviewq_protocol::reports::{
    ReportHashDto, ReportTable, ReportTableRange, ReportTableResponse,
};
use serde::Deserialize;

use crate::entry::document_display::project_table;
use crate::entry::error::{ApiPath as Path, ApiQuery as Query};
use crate::entry::pages::{count, parse, success};
use crate::entry::{ApiError, Entry};

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(in crate::entry) struct TableQuery {
    sha256: String,
    #[serde(default)]
    byte_offset: u64,
}

pub(in crate::entry) async fn read(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path((run, receipt)): Path<(String, String)>,
    Query(query): Query<TableQuery>,
) -> Result<Json<ReportTableResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let run: RunId = parse(&run)?;
    let reference = ReportReference {
        receipt_id: parse(&receipt)?,
        hash: parse(&query.sha256)?,
    };
    let authorized = entry
        .store
        .read_report(
            actor.clone(),
            run.clone(),
            Some(reference.receipt_id.clone()),
            false,
        )
        .await?;
    let report = authorized.value.report.ok_or(AppError::NotFound)?;
    if report.reference != reference {
        return Err(AppError::PreconditionFailed.into());
    }
    if query.byte_offset > report.stats.byte_length {
        return Err(AppError::InvalidRequest.into());
    }
    let mut cursor = authorized.cursor;
    let mut range = None;
    if report.stats.complete
        && let Some(index) = entry
            .tables
            .ranges(
                reference.hash.clone(),
                report.stats.byte_length,
                |ordinal| {
                    let (entry, actor, run, reference) =
                        (entry.clone(), actor.clone(), run.clone(), reference.clone());
                    async move {
                        entry
                            .store
                            .read_report_chunk(actor, run, reference, ordinal)
                            .await
                    }
                },
            )
            .await?
    {
        let group = index.group(query.byte_offset);
        let source = entry
            .store
            .read_report_bytes(actor, run, reference.clone(), group.bytes.clone())
            .await?;
        cursor = source.cursor;
        range = Some(ReportTableRange {
            byte_start: count(group.bytes.start)?,
            byte_end: count(group.bytes.end)?,
            scalar_start: count(source.value.scalar_start)?,
            previous_byte: group.previous.map(count).transpose()?,
            next_byte: group.next.map(count).transpose()?,
            table: project_table(index.page(&group, source.value.markdown))?,
        });
    }
    Ok(Json(success(
        ReportTable {
            receipt_id: ResourceIdDto::new(receipt).ok_or(AppError::Internal)?,
            sha256: ReportHashDto::new(reference.hash.as_str()).ok_or(AppError::Internal)?,
            range,
        },
        cursor,
    )?))
}
