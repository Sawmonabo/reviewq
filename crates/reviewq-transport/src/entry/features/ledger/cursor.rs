//! The `read_draft` cursor: the app's `LedgerCursor` signed with the service's cursor key,
//! so an agent passes back only a cursor ReviewQ issued. The store checks that it belongs to
//! the run and selection it continues.

use reviewq_app::access::{AppError, FieldError};
use reviewq_app::ledger::{LedgerCursor, LedgerSelection};
use reviewq_domain::identity::Revision;
use reviewq_protocol::PageCursorDto;
use reviewq_protocol::ledger::LedgerTool;
use serde::{Deserialize, Serialize};

use crate::entry::ServiceEndpoint;
use crate::entry::pages::parse;

/// Separates ledger cursors from every other cursor the service signs.
const SCOPE: &[u8] = b"reviewq-ledger-cursor-v1";

/// A `LedgerCursor` as signed text: finding IDs and note sections by their labels.
#[derive(Deserialize, Serialize)]
struct Position {
    run: String,
    selection: Selection,
    sequence: u64,
    position: u32,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Selection {
    Short,
    Findings(Vec<String>),
    Notes(Vec<String>),
}

/// The cursor a page returns for the rest of its read.
pub(super) fn encode(
    endpoint: &ServiceEndpoint,
    cursor: LedgerCursor,
) -> Result<PageCursorDto, AppError> {
    let position = Position {
        run: cursor.run.as_str().to_owned(),
        selection: match cursor.selection {
            LedgerSelection::Short => Selection::Short,
            LedgerSelection::Findings(ids) => {
                Selection::Findings(ids.iter().map(ToString::to_string).collect())
            }
            LedgerSelection::Notes(sections) => {
                Selection::Notes(sections.iter().map(ToString::to_string).collect())
            }
        },
        sequence: cursor.sequence.get(),
        position: cursor.position,
    };
    PageCursorDto::new(endpoint.encode_cursor(SCOPE, &position)?).ok_or(AppError::Internal)
}

/// The cursor an agent passed back. A cursor ReviewQ did not issue is a field error at
/// `/cursor`, so the agent reads again from the first page.
pub(super) fn decode(
    endpoint: &ServiceEndpoint,
    value: &PageCursorDto,
) -> Result<LedgerCursor, AppError> {
    let refused = || {
        AppError::InvalidFields(vec![FieldError::new(
            "/cursor",
            format!(
                "This is not a cursor {} returned. Read again without cursor to start from the first page.",
                LedgerTool::ReadDraft.name()
            ),
        )])
    };
    let position: Position = endpoint
        .decode_cursor(SCOPE, value.as_str())
        .map_err(|_| refused())?;
    let selection = match position.selection {
        Selection::Short => LedgerSelection::Short,
        Selection::Findings(ids) => LedgerSelection::Findings(
            ids.iter()
                .map(|id| parse(id))
                .collect::<Result<_, _>>()
                .map_err(|_| refused())?,
        ),
        Selection::Notes(sections) => LedgerSelection::Notes(
            sections
                .iter()
                .map(|section| section.parse())
                .collect::<Result<_, _>>()
                .map_err(|_| refused())?,
        ),
    };
    Ok(LedgerCursor {
        run: parse(&position.run).map_err(|_| refused())?,
        selection,
        sequence: Revision::try_from(position.sequence).map_err(|_| refused())?,
        position: position.position,
    })
}
