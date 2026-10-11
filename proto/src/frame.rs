//! Length-prefixed JSON framing: `u32` little-endian length, then that many bytes of JSON.
//!
//! Two readers: [`read_frame`] blocks until a whole frame has arrived (fine for files, buffers and
//! tests). [`FrameReader`] never blocks: it is fed only bytes that are already waiting, so a peer
//! that stops halfway through a frame cannot stall the thread that serves it.

use std::fmt;
use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use serde::{de::DeserializeOwned, Serialize};

/// Upper bound on one frame. The pipe peer is trusted to be the same user, but a bug (or a
/// squatter that slipped past the client checks) must not be able to make us allocate gigabytes.
pub const MAX_FRAME: u32 = 16 * 1024 * 1024;

/// The length prefix announced more than [`MAX_FRAME`]. The body is *not* skipped (it may be
/// gigabytes), so the stream is no longer at a frame boundary: the only safe response is to close
/// the connection. Check with [`is_desync`].
#[derive(Debug)]
pub struct FrameTooLarge(pub u32);

impl fmt::Display for FrameTooLarge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "frame of {} bytes exceeds MAX_FRAME ({MAX_FRAME})", self.0)
    }
}

impl std::error::Error for FrameTooLarge {}

/// True when `e` means the byte stream can no longer be trusted to sit on a frame boundary, so the
/// connection must be closed. Other `InvalidData` errors (malformed JSON inside a complete frame)
/// leave the stream intact and can be answered with an error message.
pub fn is_desync(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|inner| inner.is::<FrameTooLarge>())
}

fn too_large(len: u32) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, FrameTooLarge(len))
}

pub fn write_frame<W: Write, T: Serialize>(w: &mut W, value: &T) -> io::Result<()> {
    let data = serde_json::to_vec(value).map_err(io::Error::other)?;
    if data.len() as u64 > MAX_FRAME as u64 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "frame too large"));
    }
    w.write_all(&(data.len() as u32).to_le_bytes())?;
    w.write_all(&data)?;
    w.flush()
}

/// Returns `Ok(None)` on a clean EOF at a frame boundary (peer disconnected). EOF in the middle of
/// a frame, including a partly received length prefix, is an `UnexpectedEof` error.
pub fn read_frame<R: Read, T: DeserializeOwned>(r: &mut R) -> io::Result<Option<T>> {
    let mut len_buf = [0u8; 4];
    let mut got = 0;
    while got < len_buf.len() {
        match r.read(&mut len_buf[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "connection ended inside a length prefix")),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    let len = u32::from_le_bytes(len_buf);
    if len > MAX_FRAME {
        return Err(too_large(len));
    }
    let mut data = vec![0u8; len as usize];
    r.read_exact(&mut data)?;
    serde_json::from_slice(&data).map(Some).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Assembles frames from bytes that are already available, without ever blocking.
///
/// The caller learns how many bytes can be read right now (on a Windows pipe:
/// `PeekNamedPipe`) and passes that to [`poll`](Self::poll). A frame that arrives in pieces is
/// kept here between calls; [`stalled_for`](Self::stalled_for) says how long the current partial
/// frame has been waiting so the owner can give up on a peer that stops mid-frame.
#[derive(Default)]
pub struct FrameReader {
    buf: Vec<u8>,
    since: Option<Instant>,
}

impl FrameReader {
    pub fn new() -> Self {
        Self::default()
    }

    /// How long ago the first byte of the frame being assembled arrived; `None` between frames.
    pub fn stalled_for(&self) -> Option<Duration> {
        self.since.map(|t| t.elapsed())
    }

    /// Reads at most `avail` bytes from `r` (which must be able to supply that many without
    /// blocking) and returns a frame once one is complete. `Ok(None)` means "need more bytes".
    ///
    /// Errors: [`FrameTooLarge`] (see [`is_desync`]; the connection must be closed) and
    /// `InvalidData` for malformed JSON in an otherwise complete frame (the stream is still fine).
    pub fn poll<R: Read, T: DeserializeOwned>(&mut self, r: &mut R, mut avail: usize) -> io::Result<Option<T>> {
        loop {
            let need = if self.buf.len() < 4 {
                4 - self.buf.len()
            } else {
                let len = u32::from_le_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]);
                if len > MAX_FRAME {
                    return Err(too_large(len));
                }
                4 + len as usize - self.buf.len()
            };
            if need == 0 {
                let parsed = serde_json::from_slice(&self.buf[4..]);
                self.buf.clear();
                self.since = None;
                return parsed.map(Some).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e));
            }
            let take = need.min(avail);
            if take == 0 {
                return Ok(None);
            }
            let start = self.buf.len();
            self.buf.resize(start + take, 0);
            r.read_exact(&mut self.buf[start..])?;
            self.since.get_or_insert_with(Instant::now);
            avail -= take;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Request, Topic};

    #[test]
    fn roundtrip() {
        let mut buf = Vec::new();
        let req = Request::Snapshot { topic: Topic::Processes };
        write_frame(&mut buf, &req).unwrap();
        write_frame(&mut buf, &Request::Shutdown).unwrap();
        let mut cur = io::Cursor::new(buf);
        assert_eq!(read_frame::<_, Request>(&mut cur).unwrap(), Some(req));
        assert_eq!(read_frame::<_, Request>(&mut cur).unwrap(), Some(Request::Shutdown));
        assert_eq!(read_frame::<_, Request>(&mut cur).unwrap(), None); // clean EOF
    }

    #[test]
    fn oversized_length_is_rejected_without_allocating() {
        let mut bytes = (MAX_FRAME + 1).to_le_bytes().to_vec();
        bytes.extend_from_slice(b"{}");
        let err = read_frame::<_, Request>(&mut io::Cursor::new(bytes)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(is_desync(&err), "an oversized frame leaves the stream unusable");
    }

    #[test]
    fn truncated_frame_is_an_error() {
        let mut bytes = 100u32.to_le_bytes().to_vec();
        bytes.extend_from_slice(b"{\"type\"");
        assert!(read_frame::<_, Request>(&mut io::Cursor::new(bytes)).is_err());
    }

    #[test]
    fn partial_length_prefix_is_an_error_not_a_clean_eof() {
        let err = read_frame::<_, Request>(&mut io::Cursor::new(vec![7u8, 0])).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn garbage_json_is_invalid_data_and_keeps_the_stream_usable() {
        let mut bytes = 5u32.to_le_bytes().to_vec();
        bytes.extend_from_slice(b"hello");
        let err = read_frame::<_, Request>(&mut io::Cursor::new(bytes)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(!is_desync(&err));
    }

    fn two_frames() -> Vec<u8> {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &Request::Snapshot { topic: Topic::Memory }).unwrap();
        write_frame(&mut bytes, &Request::Shutdown).unwrap();
        bytes
    }

    #[test]
    fn reader_assembles_frames_from_tiny_pieces() {
        let bytes = two_frames();
        let mut src = io::Cursor::new(bytes.clone());
        let mut fr = FrameReader::new();
        let mut got = Vec::new();
        // Offer 3 bytes at a time, as a slow peer would; the real caller re-checks how many bytes
        // are waiting before every call, which is what the cursor position stands in for here.
        while (src.position() as usize) < bytes.len() {
            let avail = (bytes.len() - src.position() as usize).min(3);
            if let Some(r) = fr.poll::<_, Request>(&mut src, avail).unwrap() {
                got.push(r);
            }
        }
        assert_eq!(got, vec![Request::Snapshot { topic: Topic::Memory }, Request::Shutdown]);
        assert!(fr.stalled_for().is_none(), "nothing is half-received any more");
    }

    #[test]
    fn reader_returns_none_and_reports_a_stall_while_a_frame_is_incomplete() {
        let bytes = two_frames();
        let mut src = io::Cursor::new(bytes);
        let mut fr = FrameReader::new();
        assert!(fr.stalled_for().is_none());
        assert!(fr.poll::<_, Request>(&mut src, 2).unwrap().is_none()); // half a length prefix
        assert!(fr.stalled_for().is_some());
        assert!(fr.poll::<_, Request>(&mut src, 0).unwrap().is_none()); // nothing new: still waiting
    }

    #[test]
    fn reader_rejects_an_oversized_prefix_before_reading_any_body() {
        let mut bytes = (MAX_FRAME + 1).to_le_bytes().to_vec();
        bytes.extend_from_slice(b"{}");
        let mut src = io::Cursor::new(bytes);
        let err = FrameReader::new().poll::<_, Request>(&mut src, 6).unwrap_err();
        assert!(is_desync(&err));
    }

    #[test]
    fn reader_survives_a_malformed_frame_and_reads_the_next_one() {
        let mut bytes = 5u32.to_le_bytes().to_vec();
        bytes.extend_from_slice(b"hello");
        write_frame(&mut bytes, &Request::Shutdown).unwrap();
        let total = bytes.len();
        let mut src = io::Cursor::new(bytes);
        let mut fr = FrameReader::new();
        let err = fr.poll::<_, Request>(&mut src, total).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(!is_desync(&err));
        let next = fr.poll::<_, Request>(&mut src, total - 9).unwrap();
        assert_eq!(next, Some(Request::Shutdown));
    }
}
