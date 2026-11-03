// Adapted from herdr-core b63ecbd (crates/herdr-core/src/holder_proto.rs, the frame layout and reader).
// Copyright herdrdev and the herdr-core contributors, Apache-2.0. See NOTICE.
//
//! The framed wire: `[kind u8][id u32 LE][len u32 LE][payload]`. It reads and writes any byte stream (a Unix socket,
//! a named pipe, ssh's stdio), so nothing here knows what carries it. Control payloads are JSON; terminal bytes are
//! raw. `id` names a held process (0 for none).

use serde::Serialize;
use std::io::{self, Read, Write};

/// Largest payload a peer may announce; anything bigger is a broken or hostile stream.
pub const MAX_PAYLOAD: usize = 16 << 20;

/// One frame as read: (kind, id, payload).
pub type Frame = (u8, u32, Vec<u8>);

pub fn frame(kind: u8, id: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(9 + payload.len());
    out.push(kind);
    out.extend_from_slice(&id.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

pub fn json_frame(kind: u8, id: u32, msg: &impl Serialize) -> Vec<u8> {
    frame(kind, id, &serde_json::to_vec(msg).expect("serializable"))
}

pub fn write_frame(w: &mut impl Write, kind: u8, id: u32, payload: &[u8]) -> io::Result<()> {
    w.write_all(&frame(kind, id, payload))
}

pub fn write_json(w: &mut impl Write, kind: u8, id: u32, msg: &impl Serialize) -> io::Result<()> {
    w.write_all(&json_frame(kind, id, msg))
}

/// Reads one frame; `Ok(None)` at a clean end of stream.
pub fn read(r: &mut impl Read) -> io::Result<Option<Frame>> {
    let mut header = [0u8; 9];
    match r.read_exact(&mut header) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let id = u32::from_le_bytes(header[1..5].try_into().expect("4 bytes"));
    let len = u32::from_le_bytes(header[5..9].try_into().expect("4 bytes")) as usize;
    if len > MAX_PAYLOAD {
        return Err(io::Error::other(format!("holder frame of {len} bytes")));
    }
    let mut payload = vec![0; len];
    r.read_exact(&mut payload)?;
    Ok(Some((header[0], id, payload)))
}

pub fn parse<T: serde::de::DeserializeOwned>(payload: &[u8]) -> io::Result<T> {
    serde_json::from_slice(payload).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_over_any_stream() {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, 7, 42, b"abc").unwrap();
        write_frame(&mut bytes, 8, 0, b"").unwrap();
        let mut r = io::Cursor::new(bytes);
        assert_eq!(read(&mut r).unwrap(), Some((7, 42, b"abc".to_vec())));
        assert_eq!(read(&mut r).unwrap(), Some((8, 0, Vec::new())));
        assert_eq!(read(&mut r).unwrap(), None);
    }

    #[test]
    fn an_oversized_frame_is_refused_before_it_is_read() {
        let mut header = vec![1u8, 0, 0, 0, 0];
        header.extend_from_slice(&((MAX_PAYLOAD as u32) + 1).to_le_bytes());
        assert!(read(&mut io::Cursor::new(header)).is_err());
    }
}
