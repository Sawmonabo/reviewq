use std::io::Read;

use reviewq_app::access::AppError;
use reviewq_app::changes::{PatchAvailability, PatchMetrics};
use struson::reader::{JsonReader, JsonStreamReader};

use super::super::search::{FileSearch, SearchBatch, SearchPage};
use super::{BodyScan, ResponseReader, invalid, read_item};

pub(in crate::gitlab::changes) fn find_matches(
    wire: ResponseReader,
    first: u32,
    limit: u16,
    search: SearchPage,
) -> Result<SearchBatch, AppError> {
    let mut reader = JsonStreamReader::new(wire);
    reader.begin_array().map_err(invalid)?;
    let mut batch = SearchBatch::default();
    while reader.has_next().map_err(invalid)? {
        if batch.files == u32::from(limit) {
            return Err(AppError::Unavailable);
        }
        let ordinal = first + batch.files;
        batch.files += 1;
        if !search.includes(ordinal) || batch.continuation.is_some() {
            reader.skip_value().map_err(invalid)?;
            continue;
        }
        let mut hunt = search.rows(ordinal, batch.remaining());
        let (file, metrics, ()) =
            read_item(&mut reader, ordinal, |string| scan(string, &mut hunt))?;
        batch.add(
            &search,
            FileSearch {
                file,
                metrics,
                ordinal,
                matches: hunt.finish(),
            },
        )?;
    }
    reader.end_array().map_err(invalid)?;
    reader.consume_trailing_whitespace().map_err(invalid)?;
    Ok(batch)
}

fn scan(
    mut string: impl Read,
    hunt: &mut super::super::search::Rows<'_>,
) -> Result<(PatchMetrics, (), PatchAvailability), AppError> {
    let mut metrics = BodyScan::new();
    let mut bytes = [0; 64 * 1024];
    let mut prefix = [0; 16];
    loop {
        let read = string.read(&mut bytes).map_err(invalid)?;
        if read == 0 {
            break;
        }
        let chunk = &bytes[..read];
        if metrics.length < prefix.len() {
            let n = (prefix.len() - metrics.length).min(read);
            prefix[metrics.length..metrics.length + n].copy_from_slice(&chunk[..n]);
        }
        metrics.inspect(chunk);
        hunt.part(chunk);
    }
    let (metrics, _, coverage) = metrics.finish(&prefix);
    Ok((metrics, (), coverage))
}
