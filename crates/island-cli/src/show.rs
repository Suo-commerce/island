//! How entries and state are printed.

use island_core::{Answer, Body, Evidence, Grant, Invariant, Record, Role, StreamId};
use island_log::Chain;

use crate::keys::hex;

/// A stream's name; bytes that are not UTF-8 show as replacement marks.
pub fn stream(id: &StreamId) -> String {
    String::from_utf8_lossy(id.as_bytes()).into_owned()
}

/// The first eight hex digits of a key or hash: enough to tell them apart
/// by eye.
pub fn short(bytes: &[u8]) -> String {
    hex(&bytes[..bytes.len().min(4)])
}

/// Microseconds since the epoch as `2026-11-10T12:00:00.000000Z`.
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
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:06}Z",
        of_day / 3_600,
        of_day % 3_600 / 60,
        of_day % 60,
        micros % 1_000_000
    )
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

/// What a grant allows: `verse preference on island flare`.
fn allows(grant: &Grant) -> String {
    let list = |items: Vec<String>| {
        if items.is_empty() {
            "nothing".to_owned()
        } else {
            items.join(" ")
        }
    };
    let rights = grant
        .rights
        .iter()
        .map(|kind| format!("{kind:?}").to_lowercase())
        .collect();
    let scope = grant.scope.iter().map(stream).collect();
    format!("{} on {}", list(rights), list(scope))
}

fn grant(grant: &Grant) -> String {
    let narrows = grant
        .narrows
        .map_or(String::new(), |id| format!(", narrows grant {id}"));
    format!(
        "{} may write {}{narrows}",
        short(&grant.holder),
        allows(grant)
    )
}

/// Every grant the log has held, one per line, with its id.
pub fn grants(chain: &Chain) -> String {
    let mut out = String::new();
    for (id, held) in &chain.state().grants {
        let status = match held.ended {
            None => "in force".to_owned(),
            Some(ended) => match ended.by {
                Some(by) => format!("replaced by {by} at entry {}", ended.at),
                None => format!("revoked at entry {}", ended.at),
            },
        };
        let role = match held.role {
            Role::Ordinary => "",
            Role::Root => "  [root capability]",
            Role::Appender => "  [appender]",
        };
        out.push_str(&format!(
            "{:<6}  {}  {}  ({status}){role}\n",
            id.to_string(),
            hex(&held.grant.holder),
            allows(&held.grant),
        ));
    }
    out
}

fn body(body: &Body) -> String {
    match body {
        Body::Policy(policy) => format!(
            "evaluator {:?}, invariants: {}",
            policy.evaluator,
            invariants(&policy.invariants)
        ),
        Body::Grant(body) => grant(body),
        Body::Revoke(revoke) => format!("grant {}", revoke.grant),
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
            "refused {:?} from {}: {:?}",
            verdict.rejected.kind(),
            short(&verdict.rejected.writer),
            verdict.reason
        ),
        Body::Genesis(genesis) => format!("root {}", hex(&genesis.root)),
        Body::Bootstrap(bootstrap) => bootstrap
            .grants
            .iter()
            .map(grant)
            .collect::<Vec<_>>()
            .join("; "),
    }
}

/// One entry on one line.
pub fn entry(record: &Record) -> String {
    let entry = &record.entry;
    let submission = &entry.submission;
    format!(
        "{:>6}  {}  {:<10}  {:<12}  {}  {}",
        entry.seq,
        time(entry.at),
        format!("{:?}", submission.kind()).to_lowercase(),
        stream(&submission.stream),
        short(&submission.writer),
        body(&submission.body)
    )
}

/// The state a log replays to.
pub fn state(chain: &Chain) -> String {
    let state = chain.state();
    let mut out = format!("state hash  {}\n", hex(&chain.state_hash()));

    let policy = match chain.policy_seq() {
        0 => "none yet".to_owned(),
        seq => format!(
            "entry {seq}, evaluator {:?}, invariants: {}",
            state.policy.evaluator,
            invariants(&state.policy.invariants)
        ),
    };
    out.push_str(&format!("policy      {policy}\n"));

    out.push_str("streams\n");
    for (id, stream_state) in &state.streams {
        out.push_str(&format!(
            "  {:<12}  {} entries, newest {}\n",
            stream(id),
            stream_state.entries,
            stream_state.last
        ));
    }

    out.push_str("writers\n");
    for (key, last) in &state.writers {
        let answer = match last.answer {
            Answer::Accepted { seq, .. } => format!("accepted as entry {seq}"),
            Answer::Refused { seq, .. } => format!("refused, verdict at entry {seq}"),
        };
        out.push_str(&format!(
            "  {}  counter {}, {answer}\n",
            hex(key),
            last.counter
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::time;

    #[test]
    fn times_print_as_utc() {
        assert_eq!(time(0), "1970-01-01T00:00:00.000000Z");
        assert_eq!(time(1_794_312_000_000_000), "2026-11-10T12:00:00.000000Z");
        assert_eq!(time(951_782_399_999_999), "2000-02-28T23:59:59.999999Z");
        assert_eq!(time(951_782_400_000_000), "2000-02-29T00:00:00.000000Z");
        assert_eq!(time(1_709_251_199_000_001), "2024-02-29T23:59:59.000001Z");
        assert_eq!(time(4_102_444_800_000_000), "2100-01-01T00:00:00.000000Z");
    }
}
