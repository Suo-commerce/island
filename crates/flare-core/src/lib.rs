//! Flare core: what Flare shows of the Island log (docs/spec.md, "`see`").
//!
//! Everything here works on a log that has already been verified from
//! genesis against the pinned root key: the records, and the chain they
//! replay to. Nothing reads a file, the network or the clock, so the link,
//! the server and the terminal all show the same thing for the same log.
//!
//! - [`Line`]: one entry on one line.
//! - [`Overview`]: the head, the policy in force, every stream with its
//!   newest entry, and the counts worth watching.
//! - [`StreamView`]: the newest entries of one stream.
//! - [`session`]: the protocol between `flare-server` and its clients.

#![warn(missing_docs)]

pub mod session;

use std::fmt;

use island_core::{
    AccessKind, Answered, Body, Evidence, Grant, Hash, Invariant, Kind, Policy, PublicKey, Record,
    Refusal,
};
use island_log::Chain;

/// The key of the `preference` entry that asks for an override.
pub const OVERRIDE_REQUEST: &str = "override-request";

/// `1 entry`, `2 entries`.
pub fn entries(count: u64) -> String {
    match count {
        1 => "1 entry".to_owned(),
        n => format!("{n} entries"),
    }
}

/// Lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The first eight hex digits of a key or hash: enough to tell them apart
/// by eye.
pub fn short(bytes: &[u8]) -> String {
    hex(&bytes[..bytes.len().min(4)])
}

/// Microseconds since the epoch as `2026-11-10T12:00:00Z`, to the second.
pub fn time(micros: u64) -> String {
    let seconds = micros / 1_000_000;
    let (days, of_day) = (seconds / 86_400, seconds % 86_400);

    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm),
    // on unsigned numbers since the epoch is the earliest time there is.
    let z = days + 719_468;
    let era = z / 146_097;
    let day_of_era = z % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);

    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        of_day / 3_600,
        of_day % 3_600 / 60,
        of_day % 60
    )
}

/// An access kind's name.
fn access_name(what: AccessKind) -> &'static str {
    match what {
        AccessKind::Inference => "inference",
        AccessKind::Read => "read",
        AccessKind::Task => "task",
        AccessKind::Note => "note",
    }
}

/// A kind's name.
pub fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Policy => "policy",
        Kind::Grant => "grant",
        Kind::Revoke => "revoke",
        Kind::Verse => "verse",
        Kind::Preference => "preference",
        Kind::Verdict => "verdict",
        Kind::Receipt => "receipt",
        Kind::Access => "access",
        Kind::Genesis => "genesis",
        Kind::Bootstrap => "bootstrap",
        Kind::Approval => "approval",
        Kind::Dispatch => "dispatch",
    }
}

fn invariant(invariant: &Invariant) -> String {
    match invariant {
        Invariant::VerseNamesEvidence => "VerseNamesEvidence".to_owned(),
        Invariant::VerseTextMax { bytes } => format!("VerseTextMax({bytes})"),
        Invariant::ArtefactHasRef => "ArtefactHasRef".to_owned(),
    }
}

fn invariants(list: &[Invariant]) -> String {
    if list.is_empty() {
        return "none".to_owned();
    }
    list.iter().map(invariant).collect::<Vec<_>>().join(", ")
}

fn refusal(reason: &Refusal) -> String {
    match reason {
        Refusal::Invariant(broken) => format!("it breaks {}", invariant(broken)),
        Refusal::WeakerPolicy => "the policy would be weaker than the one in force".to_owned(),
        Refusal::Unapproved => "no approval of this request lets it out".to_owned(),
        Refusal::ApprovalUsed => "its approval has already let an action out".to_owned(),
        Refusal::Declined => "the request was declined after it was approved".to_owned(),
        Refusal::AlreadyDispatched => "this action has already been let out".to_owned(),
    }
}

fn grant(grant: &Grant) -> String {
    let names = |items: Vec<String>| items.join(" ");
    let rights = grant
        .rights
        .iter()
        .map(|kind| kind_name(*kind).to_owned())
        .collect();
    let scope = grant
        .scope
        .iter()
        .map(|id| String::from_utf8_lossy(id.as_bytes()).into_owned())
        .collect();
    let narrows = grant
        .narrows
        .map_or(String::new(), |id| format!(", narrows grant {id}"));
    format!(
        "{} may write {} on {}{narrows}",
        short(&grant.holder),
        names(rights),
        names(scope)
    )
}

/// What an entry says, in words.
pub fn content(body: &Body) -> String {
    match body {
        Body::Policy(policy) => format!("invariants: {}", invariants(&policy.invariants)),
        Body::Grant(body) => grant(body),
        Body::Revoke(revoke) => format!("ends grant {}", revoke.grant),
        Body::Verse(verse) => {
            let evidence = match verse.evidence() {
                Some(Evidence::Decision) => "decision",
                Some(Evidence::Artefact) => "artefact",
                Some(Evidence::Measurement) => "measurement",
                Some(Evidence::ClosedQuestion) => "closed question",
                None => "no evidence named",
            };
            let refs = match verse.refs.len() {
                0 => String::new(),
                1 => format!(", ref {}", short(&verse.refs[0])),
                n => format!(", {n} refs"),
            };
            format!("[{evidence}] {:?}{refs}", verse.text)
        }
        Body::Preference(preference) => format!("{:?} = {:?}", preference.key, preference.value),
        Body::Verdict(verdict) => format!(
            "refused {} from {}: {}",
            kind_name(verdict.rejected.kind()),
            short(&verdict.rejected.writer),
            refusal(&verdict.reason)
        ),
        Body::Receipt(receipt) => format!(
            "{} {} at {}, {} commit(s) since {}",
            receipt.repo,
            receipt.branch,
            hex(&receipt.head),
            receipt.commits,
            receipt
                .previous
                .map_or("its first sighting".to_owned(), |previous| hex(&previous))
        ),
        Body::Access(access) => format!(
            "vessel {} {} {}: {} call(s), {} bytes in, {} out",
            hex(&access.vessel),
            access_name(access.what),
            if access.provider.is_empty() {
                "-"
            } else {
                access.provider.as_str()
            },
            access.calls,
            access.bytes_in,
            access.bytes_out
        ),
        Body::Approval(approval) => format!(
            "{} request {}",
            match approval.verdict {
                Answered::Approve => "approves",
                Answered::Decline => "declines",
            },
            hex(&approval.request)
        ),
        Body::Dispatch(dispatch) => format!(
            "lets out vessel {} op {} (request {}, approval {})",
            hex(&dispatch.vessel),
            dispatch.op,
            hex(&dispatch.request),
            dispatch.approval
        ),
        Body::Genesis(genesis) => format!("root {}", short(&genesis.root)),
        Body::Bootstrap(bootstrap) => bootstrap
            .grants
            .iter()
            .map(grant)
            .collect::<Vec<_>>()
            .join("; "),
    }
}

/// One entry, as Flare shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    /// The entry's position.
    pub seq: u64,
    /// When the appender ordered it, in microseconds since the epoch.
    pub at: u64,
    /// What kind of entry it is.
    pub kind: Kind,
    /// Its stream.
    pub stream: String,
    /// Who wrote it.
    pub writer: PublicKey,
    /// What it says.
    pub content: String,
}

impl Line {
    /// The line for one record.
    pub fn of(record: &Record) -> Self {
        let entry = &record.entry;
        let submission = &entry.submission;
        Self {
            seq: entry.seq,
            at: entry.at,
            kind: submission.kind(),
            stream: String::from_utf8_lossy(submission.stream.as_bytes()).into_owned(),
            writer: submission.writer,
            content: content(&submission.body),
        }
    }
}

impl fmt::Display for Line {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:>6}  {}  {:<10}  {:<10}  {}  {}",
            self.seq,
            time(self.at),
            kind_name(self.kind),
            self.stream,
            short(&self.writer),
            self.content
        )
    }
}

/// One stream in the overview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamSummary {
    /// Its name.
    pub name: String,
    /// How many entries it holds.
    pub entries: u64,
    /// Its newest entry.
    pub newest: Line,
}

/// What `see` shows without a stream named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Overview {
    /// How many entries the log holds.
    pub entries: u64,
    /// The newest entry's position and hash.
    pub head: Option<(u64, Hash)>,
    /// The root key the log was verified against.
    pub root: PublicKey,
    /// The policy in force and the entry that holds it.
    pub policy: Option<(u64, Policy)>,
    /// Every stream, by name.
    pub streams: Vec<StreamSummary>,
    /// How many submissions the gate has refused.
    pub verdicts: u64,
    /// How many times an override was asked for.
    pub override_requests: u64,
}

impl Overview {
    /// The overview of a verified log: its records, and the chain they
    /// replay to.
    pub fn of(records: &[Record], chain: &Chain) -> Self {
        let state = chain.state();
        let streams = state
            .streams
            .iter()
            .filter_map(|(id, stream)| {
                let newest = records.get(usize::try_from(stream.last).ok()?)?;
                Some(StreamSummary {
                    name: String::from_utf8_lossy(id.as_bytes()).into_owned(),
                    entries: stream.entries,
                    newest: Line::of(newest),
                })
            })
            .collect();
        let bodies = || records.iter().map(|record| &record.entry.submission.body);
        Self {
            entries: records.len() as u64,
            head: chain.head().map(|head| (head.seq, head.hash)),
            root: chain.root(),
            policy: (chain.policy_seq() > 0).then(|| (chain.policy_seq(), state.policy.clone())),
            streams,
            verdicts: bodies().filter(|body| matches!(body, Body::Verdict(_))).count() as u64,
            override_requests: bodies()
                .filter(|body| {
                    matches!(body, Body::Preference(preference) if preference.key == OVERRIDE_REQUEST)
                })
                .count() as u64,
        }
    }
}

impl fmt::Display for Overview {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.head {
            Some((seq, hash)) => writeln!(
                f,
                "{}, verified from genesis against root {}; head entry {seq}, {}",
                entries(self.entries),
                short(&self.root),
                short(&hash)
            )?,
            None => writeln!(f, "nothing pulled yet: the replica is empty")?,
        }
        match &self.policy {
            Some((seq, policy)) => writeln!(
                f,
                "policy    entry {seq}: {}",
                invariants(&policy.invariants)
            )?,
            None => writeln!(f, "policy    none yet")?,
        }
        let count =
            |n: u64, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        writeln!(
            f,
            "gate      {}, {}",
            count(self.verdicts, "verdict", "verdicts"),
            count(
                self.override_requests,
                "override request",
                "override requests"
            )
        )?;
        writeln!(f, "streams")?;
        for stream in &self.streams {
            writeln!(
                f,
                "  {:<10} {:>11}, newest:  {}  {}  {}",
                stream.name,
                entries(stream.entries),
                time(stream.newest.at),
                kind_name(stream.newest.kind),
                stream.newest.content
            )?;
        }
        Ok(())
    }
}

/// What `see` shows of one stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamView {
    /// The stream's name.
    pub name: String,
    /// How many entries it holds in all.
    pub total: u64,
    /// Its newest entries, oldest first.
    pub lines: Vec<Line>,
}

impl StreamView {
    /// The newest `entries` entries of the stream `name`.
    pub fn of(records: &[Record], name: &str, entries: usize) -> Self {
        let on_stream: Vec<&Record> = records
            .iter()
            .filter(|record| record.entry.submission.stream.as_bytes() == name.as_bytes())
            .collect();
        let skip = on_stream.len().saturating_sub(entries);
        Self {
            name: name.to_owned(),
            total: on_stream.len() as u64,
            lines: on_stream[skip..]
                .iter()
                .map(|record| Line::of(record))
                .collect(),
        }
    }
}

impl fmt::Display for StreamView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.total == 0 {
            return writeln!(f, "stream {}: no entries", self.name);
        }
        writeln!(
            f,
            "stream {}: {}, the newest {}",
            self.name,
            entries(self.total),
            self.lines.len()
        )?;
        for line in &self.lines {
            writeln!(f, "{line}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::time;

    #[test]
    fn times_print_as_utc_to_the_second() {
        assert_eq!(time(0), "1970-01-01T00:00:00Z");
        assert_eq!(time(1_794_312_000_999_999), "2026-11-10T12:00:00Z");
        assert_eq!(time(951_782_400_000_000), "2000-02-29T00:00:00Z");
        assert_eq!(time(4_102_444_800_000_000), "2100-01-01T00:00:00Z");
    }
}
