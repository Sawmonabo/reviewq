//! Native calls may share a physical transcript record. Keep its byte offset intact and
//! distinguish siblings by the host's ordinal inside that record. Older records have ordinal zero.

use std::borrow::Cow;
use std::cmp::Ordering;

use reviewq_app::workflow::NativeCall;
use rusqlite::Connection;
use rusqlite::functions::FunctionFlags;

pub(super) fn encode(call: &NativeCall) -> Cow<'_, str> {
    if call.source_ordinal == 0 {
        Cow::Borrowed(&call.source_generation)
    } else {
        Cow::Owned(format!(
            "{}#v1:{:08x}",
            call.source_generation, call.source_ordinal
        ))
    }
}

pub(super) fn decode(value: &str) -> (&str, u32) {
    if let Some((generation, suffix)) = value.rsplit_once("#v1:")
        && suffix.len() == 8
        && let Ok(ordinal) = u32::from_str_radix(suffix, 16)
    {
        return (generation, ordinal);
    }
    (value, 0)
}

pub(crate) fn register(db: &Connection) -> rusqlite::Result<()> {
    let flags = FunctionFlags::SQLITE_UTF8
        | FunctionFlags::SQLITE_DETERMINISTIC
        | FunctionFlags::SQLITE_INNOCUOUS;
    db.create_scalar_function("reviewq_report_order", 4, flags, |ctx| {
        let (left, ordinal) = decode(ctx.get_raw(0).as_str()?);
        let (right, other) = decode(ctx.get_raw(2).as_str()?);
        let position: i64 = ctx.get(1)?;
        let other_position: i64 = ctx.get(3)?;
        Ok(
            (left == right).then(|| match (position, ordinal).cmp(&(other_position, other)) {
                Ordering::Less => -1,
                Ordering::Equal => 0,
                Ordering::Greater => 1,
            }),
        )
    })
}
