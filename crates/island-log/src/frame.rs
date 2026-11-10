//! How records sit in a log file.
//!
//! ```text
//! file  = MAGIC frame*
//! frame = len:u32le  !len:u32le  record[len]
//! ```
//!
//! A record carries its own hash and signatures, so the frame only has to
//! say where it ends. The length is stored twice, the second time with
//! every bit flipped, so a damaged length is seen as damage and not read
//! as "the rest of the file is one unfinished record".
//!
//! One thing separates a crash from damage. The appender writes one frame
//! and syncs it before the next, so a crash can leave at most one
//! unfinished frame, and only at the very end: a header or a body cut
//! short by the end of the file. That is a torn tail; it was never
//! acknowledged, and opening the log for appending drops it. Anything else
//! that does not add up is damage and an error. Nothing is ever dropped to
//! make damage go away.

use std::fmt;

/// The first eight bytes of every log file.
pub const MAGIC: [u8; 8] = *b"islelog1";

/// Bytes of frame header: the length and its complement.
pub const HEADER_LEN: usize = 8;

/// The largest record a frame may hold. Far above any real record; it
/// keeps a length field from ever asking for gigabytes.
pub const MAX_RECORD_LEN: usize = 16 * 1024 * 1024;

/// Why a file is not a log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// The file does not start with [`MAGIC`].
    NotALog,
    /// A frame header's two copies of the length disagree, or the length
    /// is over [`MAX_RECORD_LEN`].
    BadHeader {
        /// Which frame, counting from 0.
        index: u64,
        /// Its byte offset in the file.
        offset: u64,
    },
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotALog => {
                f.write_str("not an Island log: the file does not start with the log magic")
            }
            Self::BadHeader { index, offset } => {
                write!(
                    f,
                    "damaged frame header for record {index} at byte {offset}"
                )
            }
        }
    }
}

impl std::error::Error for FrameError {}

/// A log file split into its frames.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scan<'a> {
    /// The bytes of each complete record, in order.
    pub records: Vec<&'a [u8]>,
    /// How many leading bytes hold the magic and the complete frames.
    pub valid_len: usize,
    /// How many bytes follow them: an unfinished last frame, or 0.
    pub torn: usize,
}

/// The bytes that start a new log file.
pub fn header() -> Vec<u8> {
    MAGIC.to_vec()
}

/// One record, framed for appending. `None` if it is over
/// [`MAX_RECORD_LEN`].
pub fn frame(record: &[u8]) -> Option<Vec<u8>> {
    if record.len() > MAX_RECORD_LEN {
        return None;
    }
    let len = u32::try_from(record.len()).ok()?;
    let mut out = Vec::with_capacity(HEADER_LEN + record.len());
    out.extend(len.to_le_bytes());
    out.extend((!len).to_le_bytes());
    out.extend(record);
    Some(out)
}

/// Split a log file's bytes into records.
///
/// An empty file, or one that ends inside the magic, is a log with no
/// records whose creation was cut short: all of it is a torn tail.
pub fn scan(bytes: &[u8]) -> Result<Scan<'_>, FrameError> {
    if bytes.len() < MAGIC.len() {
        return if MAGIC.starts_with(bytes) {
            Ok(Scan {
                records: Vec::new(),
                valid_len: 0,
                torn: bytes.len(),
            })
        } else {
            Err(FrameError::NotALog)
        };
    }
    if bytes[..MAGIC.len()] != MAGIC {
        return Err(FrameError::NotALog);
    }

    let mut records = Vec::new();
    let mut at = MAGIC.len();
    loop {
        let rest = &bytes[at..];
        if rest.len() < HEADER_LEN {
            break; // a clean end, or a header cut short
        }
        let len = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]);
        let check = u32::from_le_bytes([rest[4], rest[5], rest[6], rest[7]]);
        let len_usize = len as usize;
        if check != !len || len_usize > MAX_RECORD_LEN {
            return Err(FrameError::BadHeader {
                index: records.len() as u64,
                offset: at as u64,
            });
        }
        let body = &rest[HEADER_LEN..];
        if body.len() < len_usize {
            break; // a body cut short
        }
        records.push(&body[..len_usize]);
        at += HEADER_LEN + len_usize;
    }
    Ok(Scan {
        records,
        valid_len: at,
        torn: bytes.len() - at,
    })
}
