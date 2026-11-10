//! Strict decoding: the exact inverse of the canonical encoding.
//!
//! The encoding is one-to-one. Every input either decodes to exactly one
//! value whose canonical encoding is the input itself, or is refused.
//! Refused are: an unknown tag, invalid UTF-8, truncation, trailing bytes,
//! and a set whose elements are not in strictly ascending order of their
//! encodings. Counts are checked against the remaining input before
//! anything is allocated.
//!
//! The rules are `taso-core`'s. Its reader is private to that crate, so
//! Island carries its own, with one more rule for sets, which Island's
//! records use and Taso's decoded payloads do not.

use std::collections::BTreeSet;
use std::fmt;

use crate::record::{
    Body, Bootstrap, Entry, Genesis, Grant, Invariant, Kind, Policy, Preference, Record, Refusal,
    Revoke, StreamId, Submission, Verdict, Verse,
};

/// Why bytes are not a canonical encoding. `at` is the byte offset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// The input ended early.
    Truncated {
        /// Where the read started.
        at: usize,
        /// How many bytes it needed.
        need: u64,
    },
    /// A tag is not valid for its type.
    BadTag {
        /// Where.
        at: usize,
        /// Which type.
        what: &'static str,
        /// The tag found.
        tag: u8,
    },
    /// A string is not UTF-8.
    BadUtf8 {
        /// Where the string's bytes start.
        at: usize,
    },
    /// A set's element does not sort strictly after the one before it.
    UnorderedSet {
        /// Where the offending element starts.
        at: usize,
        /// Which set.
        what: &'static str,
    },
    /// Bytes remain after a complete value.
    Trailing {
        /// Where the value ended.
        at: usize,
        /// How many bytes remain.
        extra: usize,
    },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated { at, need } => write!(f, "truncated at {at}: need {need} more bytes"),
            Self::BadTag { at, what, tag } => write!(f, "bad {what} tag {tag} at {at}"),
            Self::BadUtf8 { at } => write!(f, "invalid UTF-8 at {at}"),
            Self::UnorderedSet { at, what } => {
                write!(f, "{what} set element at {at} is out of order or repeated")
            }
            Self::Trailing { at, extra } => write!(f, "{extra} trailing bytes after {at}"),
        }
    }
}

impl std::error::Error for DecodeError {}

type R<T> = Result<T, DecodeError>;

/// The most elements a sequence reserves room for before it has read them.
const RESERVE_MAX: usize = 1024;

/// A cursor over canonical bytes: the one strict reader every Island
/// format decodes with.
#[derive(Debug)]
pub struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    /// A reader at the start of `bytes`.
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    /// How many bytes are left.
    pub fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    /// The offset of the next byte.
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Take `n` bytes, or fail without consuming.
    pub fn take(&mut self, n: u64) -> R<&'a [u8]> {
        if n > self.remaining() as u64 {
            return Err(DecodeError::Truncated {
                at: self.pos,
                need: n,
            });
        }
        let start = self.pos;
        self.pos += n as usize;
        Ok(&self.bytes[start..self.pos])
    }

    /// One byte.
    pub fn u8(&mut self) -> R<u8> {
        Ok(self.take(1)?[0])
    }

    /// A little-endian `u32`.
    pub fn u32(&mut self) -> R<u32> {
        Ok(u32::from_le_bytes(self.fixed()?))
    }

    /// A little-endian `u64`.
    pub fn u64(&mut self) -> R<u64> {
        Ok(u64::from_le_bytes(self.fixed()?))
    }

    /// A fixed-width array, with no length prefix.
    pub fn fixed<const N: usize>(&mut self) -> R<[u8; N]> {
        let mut array = [0u8; N];
        array.copy_from_slice(self.take(N as u64)?);
        Ok(array)
    }

    /// A tag, with its offset for errors.
    pub fn tag(&mut self) -> R<(usize, u8)> {
        let at = self.pos;
        Ok((at, self.u8()?))
    }

    /// A length-prefixed byte string.
    pub fn bytes(&mut self) -> R<Vec<u8>> {
        let len = self.u64()?;
        Ok(self.take(len)?.to_vec())
    }

    /// A length-prefixed UTF-8 string.
    pub fn string(&mut self) -> R<String> {
        let len = self.u64()?;
        let at = self.pos;
        let raw = self.take(len)?;
        std::str::from_utf8(raw)
            .map(str::to_owned)
            .map_err(|_| DecodeError::BadUtf8 { at })
    }

    /// An optional value: tag `0`, or tag `1` then the value.
    pub fn option<T>(&mut self, f: impl FnOnce(&mut Self) -> R<T>) -> R<Option<T>> {
        match self.tag()? {
            (_, 0) => Ok(None),
            (_, 1) => f(self).map(Some),
            (at, tag) => Err(DecodeError::BadTag {
                at,
                what: "option",
                tag,
            }),
        }
    }

    /// The count that opens a sequence or set. Every element is at least
    /// one byte, so a count larger than the remaining input is refused
    /// before any allocation.
    fn count(&mut self) -> R<usize> {
        let at = self.pos;
        let n = self.u64()?;
        if n > self.remaining() as u64 {
            return Err(DecodeError::Truncated { at, need: n });
        }
        Ok(n as usize)
    }

    /// A count-prefixed sequence, in the order given.
    pub fn seq<T>(&mut self, mut f: impl FnMut(&mut Self) -> R<T>) -> R<Vec<T>> {
        let n = self.count()?;
        let mut out = Vec::with_capacity(n.min(RESERVE_MAX));
        for _ in 0..n {
            out.push(f(self)?);
        }
        Ok(out)
    }

    /// A count-prefixed set. Each element's encoding must sort strictly
    /// after the one before it, which is the only order the encoder ever
    /// writes; anything else is a second encoding of the same set, or a
    /// repeated element.
    pub fn set<T: Ord>(
        &mut self,
        what: &'static str,
        mut f: impl FnMut(&mut Self) -> R<T>,
    ) -> R<BTreeSet<T>> {
        let n = self.count()?;
        let mut out = BTreeSet::new();
        let mut previous: Option<&'a [u8]> = None;
        for _ in 0..n {
            let at = self.pos;
            let item = f(self)?;
            let encoded = &self.bytes[at..self.pos];
            if previous.is_some_and(|previous| previous >= encoded) {
                return Err(DecodeError::UnorderedSet { at, what });
            }
            previous = Some(encoded);
            out.insert(item);
        }
        Ok(out)
    }

    /// Succeeds only at the end of the input.
    pub fn finish(&self) -> R<()> {
        match self.remaining() {
            0 => Ok(()),
            extra => Err(DecodeError::Trailing {
                at: self.pos,
                extra,
            }),
        }
    }

    // ── Island's record types ───────────────────────────────────────

    /// A [`StreamId`].
    pub fn stream_id(&mut self) -> R<StreamId> {
        self.bytes().map(StreamId::new)
    }

    /// A [`Kind`].
    pub fn kind(&mut self) -> R<Kind> {
        let (at, tag) = self.tag()?;
        Kind::from_tag(tag).ok_or(DecodeError::BadTag {
            at,
            what: "kind",
            tag,
        })
    }

    /// An [`Invariant`].
    pub fn invariant(&mut self) -> R<Invariant> {
        Ok(match self.tag()? {
            (_, 0) => Invariant::VerseNamesEvidence,
            (_, 1) => Invariant::VerseTextMax { bytes: self.u32()? },
            (_, 2) => Invariant::ArtefactHasRef,
            (at, tag) => {
                return Err(DecodeError::BadTag {
                    at,
                    what: "invariant",
                    tag,
                });
            }
        })
    }

    /// A [`Refusal`].
    pub fn refusal(&mut self) -> R<Refusal> {
        Ok(match self.tag()? {
            (_, 0) => Refusal::Invariant(self.invariant()?),
            (at, tag) => {
                return Err(DecodeError::BadTag {
                    at,
                    what: "refusal",
                    tag,
                });
            }
        })
    }

    /// A [`Grant`].
    pub fn grant(&mut self) -> R<Grant> {
        Ok(Grant {
            holder: self.fixed()?,
            scope: self.set("scope", Self::stream_id)?,
            rights: self.set("rights", Self::kind)?,
            narrows: self.option(Self::u64)?,
        })
    }

    /// The fields of a body of the given kind. `nested` is true inside a
    /// verdict, where another verdict is refused.
    fn body(&mut self, at: usize, kind: Kind, nested: bool) -> R<Body> {
        Ok(match kind {
            Kind::Policy => Body::Policy(Policy {
                evaluator: self.string()?,
                invariants: self.seq(Self::invariant)?,
            }),
            Kind::Grant => Body::Grant(self.grant()?),
            Kind::Revoke => Body::Revoke(Revoke { grant: self.u64()? }),
            Kind::Verse => Body::Verse(Verse {
                evidence: self.u8()?,
                text: self.string()?,
                refs: self.seq(Self::fixed)?,
            }),
            Kind::Preference => Body::Preference(Preference {
                key: self.string()?,
                value: self.string()?,
            }),
            Kind::Verdict if nested => {
                return Err(DecodeError::BadTag {
                    at,
                    what: "rejected kind",
                    tag: kind.tag(),
                });
            }
            Kind::Verdict => Body::Verdict(Verdict {
                rejected: Box::new(self.submission_at(true)?),
                sig_writer: self.fixed()?,
                reason: self.refusal()?,
            }),
            Kind::Genesis => Body::Genesis(Genesis {
                root: self.fixed()?,
            }),
            Kind::Bootstrap => Body::Bootstrap(Bootstrap {
                grants: self.seq(Self::grant)?,
            }),
        })
    }

    fn submission_at(&mut self, nested: bool) -> R<Submission> {
        let stream = self.stream_id()?;
        let writer = self.fixed()?;
        let counter = self.u64()?;
        let at = self.pos;
        let kind = self.kind()?;
        let body = self.body(at, kind, nested)?;
        Ok(Submission {
            stream,
            writer,
            counter,
            body,
        })
    }

    /// A [`Submission`].
    pub fn submission(&mut self) -> R<Submission> {
        self.submission_at(false)
    }

    /// An [`Entry`].
    pub fn entry(&mut self) -> R<Entry> {
        Ok(Entry {
            seq: self.u64()?,
            at: self.u64()?,
            policy: self.u64()?,
            prev: self.fixed()?,
            submission: self.submission()?,
            sig_writer: self.fixed()?,
        })
    }

    /// A [`Record`]: an entry, its hash and the appender's signature.
    pub fn record(&mut self) -> R<Record> {
        Ok(Record {
            entry: self.entry()?,
            hash: self.fixed()?,
            sig_appender: self.fixed()?,
        })
    }
}

impl Submission {
    /// Decode a submission from exactly `bytes`.
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut reader = Reader::new(bytes);
        let submission = reader.submission()?;
        reader.finish()?;
        Ok(submission)
    }
}

impl Record {
    /// Decode a record from exactly `bytes`.
    ///
    /// The result is well-formed, not yet authentic: see
    /// [`Record::verify`].
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut reader = Reader::new(bytes);
        let record = reader.record()?;
        reader.finish()?;
        Ok(record)
    }
}
