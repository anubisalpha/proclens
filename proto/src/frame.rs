//! Length-prefixed JSON framing: `u32` little-endian length, then that many bytes of JSON.

use std::io::{self, Read, Write};

use serde::{de::DeserializeOwned, Serialize};

/// Upper bound on one frame. The pipe peer is trusted to be the same user, but a bug (or a
/// squatter that slipped past the client checks) must not be able to make us allocate gigabytes.
pub const MAX_FRAME: u32 = 16 * 1024 * 1024;

pub fn write_frame<W: Write, T: Serialize>(w: &mut W, value: &T) -> io::Result<()> {
    let data = serde_json::to_vec(value).map_err(io::Error::other)?;
    if data.len() as u64 > MAX_FRAME as u64 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "frame too large"));
    }
    w.write_all(&(data.len() as u32).to_le_bytes())?;
    w.write_all(&data)?;
    w.flush()
}

/// Returns `Ok(None)` on a clean EOF at a frame boundary (peer disconnected).
pub fn read_frame<R: Read, T: DeserializeOwned>(r: &mut R) -> io::Result<Option<T>> {
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len_buf);
    if len > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame exceeds MAX_FRAME"));
    }
    let mut data = vec![0u8; len as usize];
    r.read_exact(&mut data)?;
    serde_json::from_slice(&data).map(Some).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
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
    }

    #[test]
    fn truncated_frame_is_an_error() {
        let mut bytes = 100u32.to_le_bytes().to_vec();
        bytes.extend_from_slice(b"{\"type\"");
        assert!(read_frame::<_, Request>(&mut io::Cursor::new(bytes)).is_err());
    }

    #[test]
    fn garbage_json_is_invalid_data() {
        let mut bytes = 5u32.to_le_bytes().to_vec();
        bytes.extend_from_slice(b"hello");
        let err = read_frame::<_, Request>(&mut io::Cursor::new(bytes)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }
}
