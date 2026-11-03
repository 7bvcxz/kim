//! Step 08 **prototype** IPC between the Host and its clients (ADR 0005, docs/CONPTY_PROOF.md).
//!
//! It reuses the framing of the IPC v1 draft (docs/contracts/IPC_PROTOCOL.md §1) so Step 09 can
//! harden it instead of replacing it, but it is *not* IPC v1: the protocol major is `0`, there is
//! one pane and a handful of methods, and peer identity checks are missing (Step 09).
//!
//! Frame: `u32 LE length | u8 kind | payload`, `length` covering kind + payload.
//! Control payload: one UTF-8 JSON object. Data payload: `u32 LE stream | u64 LE seq | u8 flags |
//! bytes`; `seq` is the byte offset of the first byte in the pane's output stream.

use serde_json::Value;
use std::io::{self, Read};

/// Protocol major of the Step 08 prototype. IPC v1 (Step 09) starts at 1, so a v1 Host rejects
/// a prototype client cleanly (`protocol_unsupported`) and vice versa.
pub const PROTOCOL: u64 = 0;
pub const KIND_CONTROL: u8 = 0x01;
pub const KIND_DATA: u8 = 0x02;
pub const MAX_CONTROL: usize = 256 * 1024;
pub const MAX_DATA: usize = 1024 * 1024;
/// Data flag: the bytes replay recent output (prototype: raw history, not a VT snapshot; Step 13).
pub const FLAG_SNAPSHOT: u8 = 0x01;
/// Data flag: client → Host input.
pub const FLAG_INPUT: u8 = 0x02;
/// The single pane's stream id.
pub const STREAM: u32 = 1;
/// Recent output kept for replay on attach.
pub const HISTORY_BYTES: usize = 64 * 1024;

/// One decoded frame.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Control(Value),
    Data {
        stream: u32,
        seq: u64,
        flags: u8,
        bytes: Vec<u8>,
    },
}

fn header(kind: u8, payload_len: usize) -> Vec<u8> {
    let len = u32::try_from(payload_len + 1).expect("frame limits keep lengths far below 4 GiB");
    let mut out = Vec::with_capacity(5 + payload_len);
    out.extend_from_slice(&len.to_le_bytes());
    out.push(kind);
    out
}

pub fn encode_control(msg: &Value) -> Vec<u8> {
    let body = msg.to_string();
    let mut out = header(KIND_CONTROL, body.len());
    out.extend_from_slice(body.as_bytes());
    out
}

pub fn encode_data(stream: u32, seq: u64, flags: u8, bytes: &[u8]) -> Vec<u8> {
    let mut out = header(KIND_DATA, 13 + bytes.len());
    out.extend_from_slice(&stream.to_le_bytes());
    out.extend_from_slice(&seq.to_le_bytes());
    out.push(flags);
    out.extend_from_slice(bytes);
    out
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

/// Reads one frame. `Ok(None)` is a clean end of stream at a frame boundary; malformed or
/// oversized frames are `InvalidData` (the caller drops that connection only).
pub fn read_frame(r: &mut impl Read) -> io::Result<Option<Frame>> {
    let mut len = [0u8; 4];
    let mut got = 0;
    while got < 4 {
        let n = r.read(&mut len[got..])?;
        if n == 0 {
            return if got == 0 {
                Ok(None)
            } else {
                Err(invalid("truncated frame header"))
            };
        }
        got += n;
    }
    let len = u32::from_le_bytes(len) as usize;
    if len == 0 || len > MAX_DATA + 14 {
        return Err(invalid("frame length out of range"));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            invalid("truncated frame")
        } else {
            e
        }
    })?;
    match body[0] {
        KIND_CONTROL => {
            if len - 1 > MAX_CONTROL {
                return Err(invalid("control frame too large"));
            }
            let v: Value =
                serde_json::from_slice(&body[1..]).map_err(|_| invalid("control frame is not JSON"))?;
            if !v.is_object() {
                return Err(invalid("control frame is not a JSON object"));
            }
            Ok(Some(Frame::Control(v)))
        }
        KIND_DATA => {
            if len < 14 {
                return Err(invalid("data frame header truncated"));
            }
            let stream = u32::from_le_bytes(body[1..5].try_into().expect("4 bytes"));
            let seq = u64::from_le_bytes(body[5..13].try_into().expect("8 bytes"));
            let flags = body[13];
            if flags & !(FLAG_SNAPSHOT | FLAG_INPUT) != 0 {
                return Err(invalid("unknown data flags"));
            }
            Ok(Some(Frame::Data {
                stream,
                seq,
                flags,
                bytes: body[14..].to_vec(),
            }))
        }
        _ => Err(invalid("unknown frame kind")),
    }
}

/// Bounded history of recent pane output for replay on attach.
#[derive(Debug)]
pub struct Ring {
    buf: std::collections::VecDeque<u8>,
    cap: usize,
    wrapped: bool,
}

impl Ring {
    pub fn new(cap: usize) -> Self {
        Self {
            buf: std::collections::VecDeque::with_capacity(cap),
            cap,
            wrapped: false,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        let bytes = if bytes.len() > self.cap {
            self.wrapped = true;
            &bytes[bytes.len() - self.cap..]
        } else {
            bytes
        };
        let overflow = (self.buf.len() + bytes.len()).saturating_sub(self.cap);
        if overflow > 0 {
            self.wrapped = true;
            self.buf.drain(..overflow);
        }
        self.buf.extend(bytes);
    }

    /// The bytes to replay. Once old output was dropped, replay starts after the first line
    /// break so it does not begin inside an escape sequence or a UTF-8 character.
    pub fn replay(&self) -> Vec<u8> {
        let all: Vec<u8> = self.buf.iter().copied().collect();
        if !self.wrapped {
            return all;
        }
        match all.iter().position(|&b| b == b'\n') {
            Some(i) => all[i + 1..].to_vec(),
            None => Vec::new(),
        }
    }
}

/// tmux-style detach key: `Ctrl+B d`. `Ctrl+B Ctrl+B` sends one literal `Ctrl+B`; `Ctrl+B`
/// followed by anything else sends both bytes unchanged.
#[derive(Debug, Default)]
pub struct DetachKeys {
    prefix: bool,
}

pub const PREFIX: u8 = 0x02;

impl DetachKeys {
    /// Filters `input`; returns the bytes to forward and whether the user asked to detach.
    pub fn feed(&mut self, input: &[u8]) -> (Vec<u8>, bool) {
        let mut out = Vec::with_capacity(input.len());
        for &b in input {
            if self.prefix {
                self.prefix = false;
                match b {
                    b'd' => return (out, true),
                    PREFIX => out.push(PREFIX),
                    other => out.extend_from_slice(&[PREFIX, other]),
                }
            } else if b == PREFIX {
                self.prefix = true;
            } else {
                out.push(b);
            }
        }
        (out, false)
    }
}

/// RFC 3339 UTC timestamp with second precision, without a date crate.
pub fn rfc3339(t: std::time::SystemTime) -> String {
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn frames_round_trip() {
        let mut wire = encode_control(&json!({"type": "hello", "protocol": {"min": 0, "max": 0}}));
        wire.extend(encode_data(STREAM, 42, FLAG_INPUT, b"echo hi\r"));
        wire.extend(encode_data(STREAM, 0, 0, b""));
        let mut r = wire.as_slice();
        assert_eq!(
            read_frame(&mut r).unwrap(),
            Some(Frame::Control(json!({"type": "hello", "protocol": {"min": 0, "max": 0}})))
        );
        assert_eq!(
            read_frame(&mut r).unwrap(),
            Some(Frame::Data {
                stream: STREAM,
                seq: 42,
                flags: FLAG_INPUT,
                bytes: b"echo hi\r".to_vec()
            })
        );
        assert!(matches!(read_frame(&mut r).unwrap(), Some(Frame::Data { .. })));
        assert_eq!(read_frame(&mut r).unwrap(), None, "clean EOF");
    }

    #[test]
    fn malformed_frames_are_invalid_data_never_panics() {
        let cases: Vec<Vec<u8>> = vec![
            vec![1, 0],                                   // truncated header
            vec![0, 0, 0, 0],                             // zero length
            vec![255, 255, 255, 255, 1],                  // too long
            vec![3, 0, 0, 0, 1, b'{'],                    // truncated body
            vec![2, 0, 0, 0, 1, b'1'],                    // JSON but not an object
            vec![2, 0, 0, 0, 9, b'x'],                    // unknown kind
            vec![5, 0, 0, 0, 2, 1, 0, 0, 0],              // data header truncated
            encode_data(1, 0, 0x80, b"x"),                // unknown flag
            {
                let mut v = header(KIND_CONTROL, MAX_CONTROL + 1);
                v.resize(5 + MAX_CONTROL + 1, b' ');
                v
            },
        ];
        for (i, c) in cases.iter().enumerate() {
            let err = read_frame(&mut c.as_slice()).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidData, "case {i}");
        }
    }

    #[test]
    fn ring_keeps_recent_output_and_replays_from_a_line_start() {
        let mut r = Ring::new(16);
        r.push(b"abc\n");
        assert_eq!(r.replay(), b"abc\n");
        r.push(b"0123456789\nxyz");
        // 18 bytes pushed, cap 16: "c\n0123456789\nxyz" kept; replay starts after the first \n.
        assert_eq!(r.replay(), b"0123456789\nxyz");
        r.push(&[b'q'; 40]);
        assert_eq!(r.replay(), b"", "no line start left: replay nothing rather than garbage");
    }

    #[test]
    fn detach_keys() {
        let mut k = DetachKeys::default();
        assert_eq!(k.feed(b"ls\r"), (b"ls\r".to_vec(), false));
        assert_eq!(k.feed(&[PREFIX, PREFIX, b'x']), (vec![PREFIX, b'x'], false));
        assert_eq!(k.feed(&[PREFIX, b'c']), (vec![PREFIX, b'c'], false));
        assert_eq!(k.feed(&[b'a', PREFIX]), (b"a".to_vec(), false));
        assert_eq!(k.feed(b"d"), (Vec::new(), true), "prefix carried across reads");
        assert_eq!(k.feed(&[b'z', PREFIX, b'd', b'q']), (b"z".to_vec(), true));
    }

    #[test]
    fn timestamps() {
        use std::time::{Duration, UNIX_EPOCH};
        assert_eq!(rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        assert_eq!(
            rfc3339(UNIX_EPOCH + Duration::from_secs(1_793_750_400 + 3723)),
            "2026-11-04T01:02:03Z"
        );
        assert_eq!(
            rfc3339(UNIX_EPOCH + Duration::from_secs(951_782_400)),
            "2000-02-29T00:00:00Z"
        );
    }
}
