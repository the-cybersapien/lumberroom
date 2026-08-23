//! A line reader over a byte range, bounded.
//!
//! `readFile` on the 96.2MB transcript in this corpus means a 96MB string plus an array of every
//! line. This reads forward from the watermark to the frozen ceiling and hands each line back with
//! the byte range it occupied, because the hold-back rule in spec §8.3 needs the first byte of a
//! span and nothing else can supply it.

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use crate::client::{err, Result};

#[derive(Debug, Clone, Default)]
pub struct LineStats {
    pub lines: i64,
    /// Lines longer than the bound, skipped rather than buffered, and counted so a truncated file
    /// is visible instead of silently short.
    pub oversized: i64,
    /// The byte offset one past the last complete line read. Never mid-line.
    pub consumed: i64,
}

/// Read `[start, end)` line by line. The callback receives the line text without its newline, the
/// byte offset it started at, and the offset one past its newline.
///
/// A line that does not fit `max_line_bytes` is skipped, counted and never handed to the callback.
/// A trailing fragment with no newline is not a complete entry and is left for the next run.
pub fn for_each_line<F>(
    path: &Path,
    start: u64,
    end: u64,
    max_line_bytes: usize,
    f: F,
) -> Result<LineStats>
where
    F: FnMut(&str, i64, i64) -> Result<()>,
{
    let mut f = f;
    let mut stats = LineStats { consumed: start as i64, ..LineStats::default() };
    if end <= start {
        return Ok(stats);
    }

    let mut file = std::fs::File::open(path)
        .map_err(|e| err(format!("could not open {}: {e}", path.display())))?;
    file.seek(SeekFrom::Start(start))
        .map_err(|e| err(format!("could not seek {} to {start}: {e}", path.display())))?;
    // `take` is the ceiling. A line whose newline falls past `end` reaches this loop without one and
    // stays a fragment, which is the same answer as a half-written line at the tail of a live file.
    let mut reader = BufReader::with_capacity(64 * 1024, file.take(end - start));

    // Absolute offset of the line being assembled. `raw_len` counts every byte of it, including the
    // ones an oversized line never buffers, so the offsets a caller sees stay honest either way.
    let mut line_start = start as i64;
    let mut raw_len: usize = 0;
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut oversized = false;

    loop {
        let (eaten, hit_newline) = {
            let available = reader
                .fill_buf()
                .map_err(|e| err(format!("could not read {}: {e}", path.display())))?;
            if available.is_empty() {
                break;
            }
            match available.iter().position(|&b| b == b'\n') {
                Some(i) => {
                    take_bytes(
                        &available[..i],
                        &mut buf,
                        &mut raw_len,
                        &mut oversized,
                        max_line_bytes,
                    );
                    (i + 1, true)
                }
                None => {
                    let n = available.len();
                    take_bytes(available, &mut buf, &mut raw_len, &mut oversized, max_line_bytes);
                    (n, false)
                }
            }
        };
        reader.consume(eaten);
        if !hit_newline {
            continue;
        }

        let line_end = line_start + raw_len as i64 + 1;
        if oversized {
            stats.oversized += 1;
        } else {
            stats.lines += 1;
            let text = String::from_utf8_lossy(&buf);
            f(&text, line_start, line_end)?;
        }
        stats.consumed = line_end;
        line_start = line_end;
        raw_len = 0;
        oversized = false;
        buf.clear();
    }

    Ok(stats)
}

/// Append while the line still fits. Past the bound the bytes are dropped on the floor and the
/// buffer released: a single 96MB line held in memory is the failure this bound exists to stop.
fn take_bytes(
    part: &[u8],
    buf: &mut Vec<u8>,
    raw_len: &mut usize,
    oversized: &mut bool,
    max: usize,
) {
    if !*oversized {
        if *raw_len + part.len() > max {
            *oversized = true;
            buf.clear();
            buf.shrink_to_fit();
        } else {
            buf.extend_from_slice(part);
        }
    }
    *raw_len += part.len();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str, body: &[u8]) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lumberroom-reader-{}-{}", std::process::id(), name));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        std::fs::write(&path, body).unwrap();
        path
    }

    fn collect(
        path: &Path,
        start: u64,
        end: u64,
        max: usize,
    ) -> (Vec<(String, i64, i64)>, LineStats) {
        let mut seen = vec![];
        let stats = for_each_line(path, start, end, max, |line, s, e| {
            seen.push((line.to_string(), s, e));
            Ok(())
        })
        .unwrap();
        (seen, stats)
    }

    #[test]
    fn hands_back_offsets_and_leaves_the_fragment() {
        let path = fixture("frag", b"aa\nbbb\ncc");
        let (seen, stats) = collect(&path, 0, 9, 1024);
        assert_eq!(seen, vec![("aa".into(), 0, 3), ("bbb".into(), 3, 7)]);
        assert_eq!(stats.lines, 2);
        assert_eq!(stats.consumed, 7);
    }

    #[test]
    fn starts_at_the_watermark_with_absolute_offsets() {
        let path = fixture("mid", b"aa\nbbb\ncc\n");
        let (seen, stats) = collect(&path, 3, 10, 1024);
        assert_eq!(seen, vec![("bbb".into(), 3, 7), ("cc".into(), 7, 10)]);
        assert_eq!(stats.consumed, 10);
    }

    #[test]
    fn a_line_crossing_the_ceiling_is_a_fragment() {
        let path = fixture("ceil", b"aa\nbbb\ncc\n");
        let (seen, stats) = collect(&path, 0, 8, 1024);
        assert_eq!(seen, vec![("aa".into(), 0, 3), ("bbb".into(), 3, 7)]);
        assert_eq!(stats.consumed, 7);
    }

    #[test]
    fn oversized_is_skipped_and_counted_without_shifting_offsets() {
        let path = fixture("big", b"aa\nXXXXXXXXXX\nbb\n");
        let (seen, stats) = collect(&path, 0, 17, 4);
        assert_eq!(seen, vec![("aa".into(), 0, 3), ("bb".into(), 14, 17)]);
        assert_eq!(stats.oversized, 1);
        assert_eq!(stats.lines, 2);
        assert_eq!(stats.consumed, 17);
    }

    #[test]
    fn an_empty_range_reads_nothing() {
        let path = fixture("empty", b"aa\n");
        let (seen, stats) = collect(&path, 3, 3, 1024);
        assert!(seen.is_empty());
        assert_eq!(stats.consumed, 3);
    }

    #[test]
    fn invalid_utf8_does_not_stop_the_read() {
        let path = fixture("utf8", b"a\xffb\ncc\n");
        let (seen, stats) = collect(&path, 0, 8, 1024);
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[1], ("cc".to_string(), 4, 7));
        assert_eq!(stats.consumed, 7);
    }
}
