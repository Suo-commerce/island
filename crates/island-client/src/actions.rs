//! The actions a Vessel may declare whose request bytes Island reads
//! (docs/slice-2.md, "Actions and the sender").
//!
//! | Provider | Request bytes | Approval |
//! | --- | --- | --- |
//! | [`VERSE`] | canonical `{ stream, verse }`: [`verse_request`] | required |
//!
//! The agent declares the bytes; `island-sender` reads them back to sign
//! the verse with the Vessel's key once it is approved; a client shows
//! them to Jarmo and checks that their [`request_hash`](crate::approval::request_hash)
//! is the one he approves.

use island_core::{DecodeError, Reader, Verse};
use taso_core::canon::{Canon, Canonical};

/// A verse of the Vessel's own, on a stream its grant covers.
pub const VERSE: &str = "island.verse";

/// The request bytes of [`VERSE`]: the stream's name, then the verse, in
/// the log's canonical encoding.
pub fn verse_request(stream: &str, verse: &Verse) -> Vec<u8> {
    let mut out = Canon::new();
    out.str(stream);
    verse.canon(&mut out);
    out.into_bytes()
}

/// The stream and the verse of [`VERSE`] request bytes; nothing may
/// follow them.
pub fn parse_verse_request(bytes: &[u8]) -> Result<(String, Verse), DecodeError> {
    let mut reader = Reader::new(bytes);
    let stream = reader.string()?;
    let evidence = reader.u8()?;
    let text = reader.string()?;
    let refs = reader.seq(Reader::fixed)?;
    reader.finish()?;
    Ok((
        stream,
        Verse {
            evidence,
            text,
            refs,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use island_core::Evidence;

    #[test]
    fn a_verse_request_reads_back_and_nothing_may_follow() {
        let verse = Verse::new(Evidence::Artefact, "drafted the step 5 spec", vec![[7; 32]]);
        let bytes = verse_request("flare", &verse);
        assert_eq!(
            parse_verse_request(&bytes).unwrap(),
            ("flare".to_owned(), verse)
        );
        let mut longer = bytes.clone();
        longer.push(0);
        assert!(parse_verse_request(&longer).is_err());
        assert!(parse_verse_request(&bytes[..bytes.len() - 1]).is_err());
    }
}
