//! SSE stream helpers: line framing, backpressure buffer pool, and
//! shared constants used by all SSE-based streaming paths.

use std::sync::{Mutex as StdMutex, OnceLock};

pub(super) const SSE_BACKPRESSURE_HIGH_WATERMARK: usize = 8 * 1024 * 1024; // 8 MB
pub(super) const SSE_BACKPRESSURE_SLEEP_MS: u64 = 10;
pub(super) const SSE_MAX_LINES_PER_CHUNK: usize = 256;

fn buffer_pool() -> &'static StdMutex<Vec<Vec<u8>>> {
    static POOL: OnceLock<StdMutex<Vec<Vec<u8>>>> = OnceLock::new();
    POOL.get_or_init(|| StdMutex::new(Vec::new()))
}

pub(super) fn acquire_stream_buffer() -> Vec<u8> {
    if let Ok(mut pool) = buffer_pool().lock() {
        pool.pop().unwrap_or_else(|| Vec::with_capacity(8192))
    } else {
        Vec::with_capacity(8192)
    }
}

pub(super) fn release_stream_buffer(mut buf: Vec<u8>) {
    buf.clear();
    if buf.capacity() > 256 * 1024 {
        buf.shrink_to(256 * 1024);
    }
    if let Ok(mut pool) = buffer_pool().lock()
        && pool.len() < 8
    {
        pool.push(buf);
    }
}

pub(super) fn extract_sse_data_value(line: &str) -> Option<&str> {
    line.strip_prefix("data:")
        .map(|value| value.strip_prefix(' ').unwrap_or(value))
}

/// Take the next COMPLETE line (up to the first `\n`) off a raw byte buffer,
/// draining it, and return it trimmed. Returns `None` when no full line is
/// buffered yet. Decoding only complete lines (never an arbitrary network-read
/// boundary) means a multi-byte UTF-8 char — CJK, emoji, accented letter —
/// split across two reads is never corrupted to U+FFFD, since the `\n`
/// delimiter is ASCII and can never fall inside a multi-byte sequence.
pub(super) fn take_sse_line(buffer: &mut Vec<u8>) -> Option<String> {
    let line_end = buffer.iter().position(|&b| b == b'\n')?;
    let line = String::from_utf8_lossy(&buffer[..line_end])
        .trim()
        .to_string();
    buffer.drain(..=line_end);
    Some(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_buffer_pool_reuses_released_buffers() {
        let mut first = acquire_stream_buffer();
        first.extend_from_slice(b"hello");
        let released_capacity = first.capacity();
        release_stream_buffer(first);

        let second = acquire_stream_buffer();
        assert!(second.is_empty());
        assert!(
            second.capacity() >= released_capacity,
            "pooled buffer capacity should be reused"
        );
    }

    #[test]
    fn take_sse_line_preserves_multibyte_split_across_reads() {
        // "你好" streamed so the 3-byte '好' straddles a read boundary.
        let full = "data: 你好\n";
        let bytes = full.as_bytes();
        let split = bytes.len() - 2; // mid '好'
        let mut buffer: Vec<u8> = Vec::new();
        // First read: no complete line yet.
        buffer.extend_from_slice(&bytes[..split]);
        assert_eq!(take_sse_line(&mut buffer), None);
        // Second read completes the line; '好' must be intact, not U+FFFD.
        buffer.extend_from_slice(&bytes[split..]);
        let line = take_sse_line(&mut buffer).expect("a complete line");
        assert_eq!(line, "data: 你好");
        assert!(!line.contains('\u{FFFD}'), "multibyte char was corrupted");
        assert_eq!(extract_sse_data_value(&line), Some("你好"));
        // Buffer fully drained.
        assert!(buffer.is_empty());
    }

    #[test]
    fn take_sse_line_returns_none_without_newline() {
        let mut buffer = b"data: partial".to_vec();
        assert_eq!(take_sse_line(&mut buffer), None);
        assert_eq!(buffer, b"data: partial");
    }

    #[test]
    fn extract_sse_data_value_accepts_optional_space() {
        assert_eq!(
            extract_sse_data_value("data: {\"ok\":true}"),
            Some("{\"ok\":true}")
        );
        assert_eq!(
            extract_sse_data_value("data:{\"ok\":true}"),
            Some("{\"ok\":true}")
        );
    }

    #[test]
    fn extract_sse_data_value_handles_done_marker() {
        assert_eq!(extract_sse_data_value("data: [DONE]"), Some("[DONE]"));
        assert_eq!(extract_sse_data_value("data:[DONE]"), Some("[DONE]"));
    }

    #[test]
    fn extract_sse_data_value_rejects_non_data_lines() {
        assert_eq!(extract_sse_data_value("event: message"), None);
        assert_eq!(extract_sse_data_value(": heartbeat"), None);
    }
}
