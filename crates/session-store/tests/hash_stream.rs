//! The streamed digest holds one fixed buffer, whatever the source length.

use std::io::{self, Read};

use horon_agent_session_store::{HASH_STREAM_BUFFER, sha256_hex, sha256_hex_stream};

/// Yields `remaining` bytes of a repeating pattern, recording the largest read
/// request the hasher issues and how many bytes it consumed.
struct Probe {
    remaining: usize,
    max_request: usize,
    total: u64,
}

impl Probe {
    fn new(remaining: usize) -> Self {
        Self {
            remaining,
            max_request: 0,
            total: 0,
        }
    }
}

impl Read for Probe {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.max_request = self.max_request.max(buf.len());
        let read = buf.len().min(self.remaining);
        for (offset, slot) in buf[..read].iter_mut().enumerate() {
            *slot = u8::try_from((self.total as usize + offset) % 251).unwrap_or(0);
        }
        self.remaining -= read;
        self.total += u64::try_from(read).unwrap_or(0);
        Ok(read)
    }
}

/// The buffer the hasher allocates is the buffer it asks the reader to fill, so
/// the largest read request is the peak temporary buffer of the digest. It must
/// be the same for one byte and for two hundred mebibytes: only the number of
/// passes may grow, never the buffer.
#[test]
fn the_stream_buffer_is_fixed_and_the_source_length_only_adds_passes() {
    let mut small = Probe::new(1);
    let small_digest = sha256_hex_stream(&mut small).expect("a one-byte source streams");
    let mut large = Probe::new(200 * 1024 * 1024);
    let large_digest = sha256_hex_stream(&mut large).expect("a 200 MiB source streams");

    assert_eq!(small_digest.bytes, 1);
    assert_eq!(large_digest.bytes, 200 * 1024 * 1024);
    assert_eq!(
        small.max_request, HASH_STREAM_BUFFER,
        "the one-byte source must be read through the documented buffer"
    );
    assert_eq!(
        large.max_request, HASH_STREAM_BUFFER,
        "a 200 MiB source must not enlarge the buffer: {} bytes requested",
        large.max_request
    );
    // Read by scripts/perf/check_structural.py: the peak buffer is independent of
    // the source length, and never above the documented constant.
    println!(
        "STRUCTURAL_PERF section=hash buffer_bytes={} small_request={} large_request={} large_source_bytes={}",
        HASH_STREAM_BUFFER,
        small.max_request,
        large.max_request,
        200 * 1024 * 1024
    );
}

/// An empty source and a source smaller than the buffer both hash as the same
/// bytes a whole-file read would produce, including the empty-string digest.
#[test]
fn the_streamed_digest_equals_the_whole_bytes_digest_at_every_boundary() {
    for size in [
        0,
        1,
        2,
        HASH_STREAM_BUFFER - 1,
        HASH_STREAM_BUFFER,
        HASH_STREAM_BUFFER + 1,
        HASH_STREAM_BUFFER * 2 + 17,
    ] {
        let mut probe = Probe::new(size);
        let digest = sha256_hex_stream(&mut probe).expect("streams");
        let whole: Vec<u8> = (0..size)
            .map(|offset| u8::try_from(offset % 251).unwrap_or(0))
            .collect();
        assert_eq!(digest.hex, sha256_hex(&whole), "size {size}");
        assert_eq!(digest.bytes, u64::try_from(size).unwrap());
    }
    assert_eq!(
        sha256_hex_stream(&mut Probe::new(0))
            .expect("an empty source streams")
            .hex,
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        "an empty log must keep the empty-string SHA-256"
    );
}
