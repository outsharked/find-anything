/// FTS5 rowid multiplier: rowid = file_id * MAX_LINES_PER_FILE + line_number.
/// Hardcoded — must not change after any index has been built.
/// A file with >= MAX_LINES_PER_FILE lines has its excess lines dropped from
/// the FTS index (logged as a warning).
pub const MAX_LINES_PER_FILE: i64 = 1_000_000;

pub fn encode_fts_rowid(file_id: i64, line_number: i64) -> i64 {
    debug_assert!(line_number < MAX_LINES_PER_FILE, "line_number {line_number} would overflow FTS rowid");
    file_id * MAX_LINES_PER_FILE + line_number
}

pub fn decode_fts_rowid(rowid: i64) -> (i64, i64) {
    (rowid / MAX_LINES_PER_FILE, rowid % MAX_LINES_PER_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        for (file_id, line_number) in [(1, 0), (1, 1), (42, 999999), (100, 500000)] {
            let rowid = encode_fts_rowid(file_id, line_number);
            let (dec_file_id, dec_line) = decode_fts_rowid(rowid);
            assert_eq!(dec_file_id, file_id, "file_id mismatch for ({file_id}, {line_number})");
            assert_eq!(dec_line, line_number, "line_number mismatch for ({file_id}, {line_number})");
        }
    }
}
