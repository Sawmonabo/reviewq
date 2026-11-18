//! Small SQLite WAL header preflight. SQLite validates and replays the frames.

use std::fs::File;
use std::io::{self, Read, Seek};
use std::path::Path;

use crate::StoreError;

const WAL_FORMAT: u32 = 3_007_000;
const CHECKSUM_LITTLE_ENDIAN: u32 = 0x377f_0682;
const CHECKSUM_BIG_ENDIAN: u32 = 0x377f_0683;

pub(super) fn verify_header(
    wal: &mut File,
    database: &[u8; 100],
    path: &Path,
) -> Result<(), StoreError> {
    let mut header = [0; 32];
    wal.read_exact(&mut header).map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            StoreError::path(path, "SQLite WAL header is incomplete")
        } else {
            error.into()
        }
    })?;
    wal.rewind()?;

    let big_endian = match field(&header, 0) {
        CHECKSUM_LITTLE_ENDIAN => false,
        CHECKSUM_BIG_ENDIAN => true,
        _ => return Err(StoreError::path(path, "unrecognized SQLite WAL header")),
    };
    let db_page = u16::from_be_bytes([database[16], database[17]]);
    let db_page = if db_page == 1 {
        65_536
    } else {
        u32::from(db_page)
    };
    if field(&header, 4) != WAL_FORMAT
        || !(512..=65_536).contains(&db_page)
        || !db_page.is_power_of_two()
        || field(&header, 8) != db_page
        || database[18..20] != [2, 2]
    {
        return Err(StoreError::path(
            path,
            "SQLite WAL format or page size does not match the database",
        ));
    }

    let mut first = 0u32;
    let mut second = 0u32;
    for pair in header[..24].as_chunks::<8>().0 {
        let word = |start| {
            let bytes = pair[start..start + 4]
                .try_into()
                .expect("whole checksum word");
            if big_endian {
                u32::from_be_bytes(bytes)
            } else {
                u32::from_le_bytes(bytes)
            }
        };
        first = first.wrapping_add(word(0)).wrapping_add(second);
        second = second.wrapping_add(word(4)).wrapping_add(first);
    }
    if (first, second) != (field(&header, 24), field(&header, 28)) {
        return Err(StoreError::path(
            path,
            "SQLite WAL header checksum does not match",
        ));
    }
    Ok(())
}

fn field(data: &[u8; 32], start: usize) -> u32 {
    u32::from_be_bytes(
        data[start..start + 4]
            .try_into()
            .expect("whole WAL header field"),
    )
}
