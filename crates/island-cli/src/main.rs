//! Island command line (docs/spec.md, build steps 3, 5 and 6).
//!
//! - `keygen`, `public` and `init` start a log.
//! - `grant`, `revoke` and `policy` write root-signed entries to it.
//! - `verse` and `override` write as an ordinary key.
//! - `verify`, `replay`, `tail` and `grants` read one. They never change
//!   the file, and print nothing from a log they could not verify from
//!   genesis against the pinned root key.
//!
//! Until the broker exists (build step 7), the writing commands append to
//! the log file directly, holding its lock; they need the appender key.
//!
//! Exit codes: 0 done; 1 failed, which includes a write the gate refused
//! (its verdict is then in the log); 3 nothing written yet, sign the
//! printed hash and run the command again (see `signer`).

mod keys;
mod show;
mod signer;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use island_core::{
    Body, Evidence, Grant, GrantId, Hash, Invariant, Kind, Policy, Preference, PublicKey, Record,
    Revoke, SigningKey, StreamId, Submission, Verse, public_key,
};
use island_env::{FileStorage, OsRng, SystemClock};
use island_log::{Ack, Appender, Log, Rejected, genesis};

use keys::hex;
use signer::RootArgs;

/// The git commit this build was made from: what a `policy` entry records
/// as its `evaluator`. See `build.rs`.
const BUILD: &str = env!("ISLAND_BUILD_COMMIT");

/// The key of the `preference` entry that asks for an override.
pub const OVERRIDE_REQUEST: &str = "override-request";

/// The log as the writing commands hold it: open, locked and replayed.
type Open = Appender<FileStorage, SystemClock>;

/// Why a command stopped without doing its work.
pub enum Stop {
    /// It failed. Exit code 1.
    Failed(String),
    /// Nothing is wrong and nothing was written: a hash has to be signed
    /// by hand first. Exit code 3.
    SignFirst(String),
}

impl From<String> for Stop {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

impl From<&str> for Stop {
    fn from(message: &str) -> Self {
        Self::Failed(message.to_owned())
    }
}

#[derive(Parser)]
#[command(
    name = "island-cli",
    version,
    about = "Start, verify, read and administer an Island log"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Write a new ed25519 key file (for development, and for the appender).
    Keygen {
        /// Where to write it. Never replaces a file.
        file: PathBuf,
    },
    /// Print the public key of a key file.
    Public {
        /// The key file.
        file: PathBuf,
    },
    /// Start a new log: entry 0 (genesis), entry 1 (bootstrap) and entry 2
    /// (the first policy).
    Init {
        /// The log file to create. Must not exist.
        #[arg(long)]
        log: PathBuf,
        /// The appender key file. Orders and seals every entry.
        #[arg(long)]
        appender_key: PathBuf,
        #[command(flatten)]
        root: RootArgs,
    },
    /// Grant a key the right to write some kinds to some streams, or
    /// narrow a grant it holds.
    Grant {
        /// The log file.
        #[arg(long)]
        log: PathBuf,
        /// The appender key file.
        #[arg(long)]
        appender_key: PathBuf,
        /// The public key that receives the grant, in hex.
        #[arg(long, value_parser = keys::parse_public_key)]
        holder: PublicKey,
        /// The streams it covers, separated by commas.
        #[arg(long, value_delimiter = ',')]
        scope: Vec<String>,
        /// The kinds it may write there, separated by commas: verse,
        /// preference, policy.
        #[arg(long, value_delimiter = ',', value_parser = parse_kind)]
        rights: Vec<Kind>,
        /// The grant this one replaces, as entry.index. Scope and rights
        /// must then be subsets of it, for the same holder.
        #[arg(long)]
        narrows: Option<GrantId>,
        #[command(flatten)]
        root: RootArgs,
    },
    /// End a grant. It stops covering writes from the next entry on.
    Revoke {
        /// The log file.
        #[arg(long)]
        log: PathBuf,
        /// The appender key file.
        #[arg(long)]
        appender_key: PathBuf,
        /// The grant to end, as entry.index.
        #[arg(long)]
        grant: GrantId,
        #[command(flatten)]
        root: RootArgs,
    },
    /// Tighten the policy in force. The new policy is the one in force
    /// with the changes given here, judged by this build. One that would
    /// be weaker is refused by the gate, and the refusal is recorded.
    Policy {
        /// The log file.
        #[arg(long)]
        log: PathBuf,
        /// The appender key file.
        #[arg(long)]
        appender_key: PathBuf,
        /// Set the longest verse text allowed, in bytes.
        #[arg(long, value_name = "BYTES")]
        verse_text_max: Option<u32>,
        /// Require every verse to name its kind of evidence.
        #[arg(long)]
        verse_names_evidence: bool,
        /// Require every verse with artefact evidence to carry a ref.
        #[arg(long)]
        artefact_has_ref: bool,
        #[command(flatten)]
        root: RootArgs,
    },
    /// Write a verse. A tool for trying the log out until the broker and
    /// its client exist (build step 7).
    Verse {
        #[command(flatten)]
        writer: WriterArgs,
        /// The stream: a project.
        #[arg(long)]
        stream: String,
        /// The kind of evidence: decision, artefact, measurement,
        /// closed-question, or none (which the first policy refuses).
        #[arg(long, value_parser = parse_evidence)]
        evidence: u8,
        /// The text.
        #[arg(long)]
        text: String,
        /// The hash of an artefact, in hex. May be given more than once.
        #[arg(long = "ref", value_name = "HASH", value_parser = parse_hash)]
        refs: Vec<Hash>,
    },
    /// Ask for a refused submission to be let through after all. Nothing
    /// is let through: the request is recorded on the `plan` stream, and
    /// requests are counted.
    Override {
        #[command(flatten)]
        writer: WriterArgs,
        /// The refused submission: its hash in hex, or the first eight or
        /// more digits of it, as `tail` shows them.
        submission: String,
        /// Why it should have been accepted.
        why: String,
    },
    /// List every grant a verified log has held, with its id.
    Grants {
        /// The log file.
        #[arg(long)]
        log: PathBuf,
        /// The pinned root public key, in hex.
        #[arg(long, value_parser = keys::parse_public_key)]
        root: PublicKey,
    },
    /// Check a log from genesis: chain, hashes, signatures, counters,
    /// grants, the gate, and every verdict judged again.
    Verify {
        /// The log file.
        #[arg(long)]
        log: PathBuf,
        /// The pinned root public key, in hex.
        #[arg(long, value_parser = keys::parse_public_key)]
        root: PublicKey,
    },
    /// Replay a log from genesis and print the state it arrives at.
    Replay {
        /// The log file.
        #[arg(long)]
        log: PathBuf,
        /// The pinned root public key, in hex.
        #[arg(long, value_parser = keys::parse_public_key)]
        root: PublicKey,
    },
    /// Print the newest entries of a verified log.
    Tail {
        /// The log file.
        #[arg(long)]
        log: PathBuf,
        /// The pinned root public key, in hex.
        #[arg(long, value_parser = keys::parse_public_key)]
        root: PublicKey,
        /// How many entries.
        #[arg(short = 'n', long, default_value_t = 10)]
        entries: usize,
    },
}

/// The log and the key of an ordinary writer.
#[derive(clap::Args)]
struct WriterArgs {
    /// The log file.
    #[arg(long)]
    log: PathBuf,
    /// The appender key file.
    #[arg(long)]
    appender_key: PathBuf,
    /// The pinned root public key, in hex.
    #[arg(long, value_parser = keys::parse_public_key)]
    root: PublicKey,
    /// The key file of the writer.
    #[arg(long)]
    key: PathBuf,
}

/// A kind of evidence by its name, as `--evidence` takes it. `none` is a
/// byte that names no kind.
fn parse_evidence(name: &str) -> Result<u8, String> {
    Ok(match name {
        "decision" => Evidence::Decision.as_u8(),
        "artefact" => Evidence::Artefact.as_u8(),
        "measurement" => Evidence::Measurement.as_u8(),
        "closed-question" => Evidence::ClosedQuestion.as_u8(),
        "none" => u8::MAX,
        _ => {
            return Err(format!(
                "`{name}` is not a kind of evidence (decision, artefact, measurement, \
                 closed-question, none)"
            ));
        }
    })
}

/// A hash given on the command line.
fn parse_hash(text: &str) -> Result<Hash, String> {
    keys::unhex(text).ok_or_else(|| "a hash is 64 hex digits".to_owned())
}

/// A kind by its name, as `--rights` takes it.
fn parse_kind(name: &str) -> Result<Kind, String> {
    Ok(match name {
        "policy" => Kind::Policy,
        "grant" => Kind::Grant,
        "revoke" => Kind::Revoke,
        "verse" => Kind::Verse,
        "preference" => Kind::Preference,
        "verdict" => Kind::Verdict,
        "genesis" => Kind::Genesis,
        "bootstrap" => Kind::Bootstrap,
        _ => {
            return Err(format!(
                "`{name}` is not a kind (verse, preference, policy)"
            ));
        }
    })
}

fn main() -> ExitCode {
    match run(Cli::parse().command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(Stop::Failed(error)) => {
            eprintln!("island-cli: {error}");
            ExitCode::FAILURE
        }
        Err(Stop::SignFirst(instructions)) => {
            print!("{instructions}");
            ExitCode::from(3)
        }
    }
}

fn run(command: Command) -> Result<(), Stop> {
    match command {
        Command::Keygen { file } => {
            let key = keys::generate(&file, &mut OsRng)?;
            println!("wrote   {}", file.display());
            println!("public  {}", hex(&public_key(&key)));
            Ok(())
        }
        Command::Public { file } => {
            println!("{}", hex(&public_key(&keys::load(&file)?)));
            Ok(())
        }
        Command::Init {
            log,
            appender_key,
            root,
        } => init(&log, &appender_key, &root),
        Command::Grant {
            log,
            appender_key,
            holder,
            scope,
            rights,
            narrows,
            root,
        } => {
            let grant = Grant {
                holder,
                scope: scope
                    .iter()
                    .map(|name| StreamId::from(name.as_str()))
                    .collect(),
                rights: BTreeSet::from_iter(rights),
                narrows,
            };
            let ack = write_as_root(&log, &appender_key, &root, |_| Ok(Body::Grant(grant)))?;
            println!("entry {}  grant {}", ack.seq, GrantId::new(ack.seq, 0));
            Ok(())
        }
        Command::Revoke {
            log,
            appender_key,
            grant,
            root,
        } => {
            let revoke = Body::Revoke(Revoke { grant });
            let ack = write_as_root(&log, &appender_key, &root, |_| Ok(revoke))?;
            println!("entry {}  revoked grant {grant}", ack.seq);
            Ok(())
        }
        Command::Policy {
            log,
            appender_key,
            verse_text_max,
            verse_names_evidence,
            artefact_has_ref,
            root,
        } => {
            let mut shown = String::new();
            let ack = write_as_root(&log, &appender_key, &root, |open| {
                // The policy in force, with what was asked for.
                let mut invariants = open.chain().state().policy.invariants.clone();
                if let Some(bytes) = verse_text_max {
                    invariants.retain(|held| !matches!(held, Invariant::VerseTextMax { .. }));
                    invariants.push(Invariant::VerseTextMax { bytes });
                }
                let more = [
                    (verse_names_evidence, Invariant::VerseNamesEvidence),
                    (artefact_has_ref, Invariant::ArtefactHasRef),
                ];
                for (asked, invariant) in more {
                    if asked && !invariants.contains(&invariant) {
                        invariants.push(invariant);
                    }
                }
                invariants.sort_by_key(Invariant::tag);
                shown = show::invariants(&invariants);
                Ok(Body::Policy(Policy {
                    evaluator: BUILD.to_owned(),
                    invariants,
                }))
            })?;
            println!("entry {}  policy in force: {shown}", ack.seq);
            println!("evaluator  {BUILD}");
            Ok(())
        }
        Command::Verse {
            writer,
            stream,
            evidence,
            text,
            refs,
        } => {
            let body = Body::Verse(Verse {
                evidence,
                text,
                refs,
            });
            let ack = write_as_key(&writer, StreamId::from(stream.as_str()), body)?;
            println!("entry {}  {}", ack.seq, hex(&ack.hash));
            Ok(())
        }
        Command::Override {
            writer,
            submission,
            why,
        } => {
            // Verdicts never leave a log, so the one found here is still
            // there when the request is written.
            let verified = read(&writer.log, writer.root)?;
            let named = hex(&refused_submission(&verified.records, &submission)?);
            let request = Body::Preference(Preference {
                key: OVERRIDE_REQUEST.to_owned(),
                value: format!("{named} {why}"),
            });
            let ack = write_as_key(&writer, StreamId::from(StreamId::PLAN), request)?;
            println!(
                "entry {}  override requested for submission {named}",
                ack.seq
            );
            println!("nothing is let through; the request is counted (see `replay`)");
            Ok(())
        }
        Command::Grants { log, root } => {
            print!("{}", show::grants(&read(&log, root)?.chain));
            Ok(())
        }
        Command::Verify { log, root } => {
            let verified = read(&log, root)?;
            let Some(head) = verified.chain.head() else {
                println!("ok: an empty log");
                return Ok(());
            };
            println!(
                "ok: {} entries verified from genesis",
                verified.records.len()
            );
            println!("head        entry {}, {}", head.seq, hex(&head.hash));
            println!("state hash  {}", hex(&verified.chain.state_hash()));
            match verified.chain.appender() {
                Some(appender) => println!("appender    {}", hex(&appender)),
                None => println!("appender    not declared yet: the log ends before entry 1"),
            }
            Ok(())
        }
        Command::Replay { log, root } => {
            let verified = read(&log, root)?;
            println!("{} entries replayed from genesis", verified.records.len());
            print!("{}", show::state(&verified.chain));
            print!("{}", show::counts(&verified.records));
            Ok(())
        }
        Command::Tail { log, root, entries } => {
            let verified = read(&log, root)?;
            let skip = verified.records.len().saturating_sub(entries);
            for record in &verified.records[skip..] {
                println!("{}", show::entry(record));
            }
            Ok(())
        }
    }
}

/// Read and verify a log file. A torn tail is reported on stderr.
fn read(path: &Path, root: PublicKey) -> Result<Log, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let log =
        island_log::read(&bytes, root).map_err(|error| format!("{}: {error}", path.display()))?;
    if log.torn > 0 {
        eprintln!(
            "island-cli: note: {} bytes of an unfinished last record are not part of the log",
            log.torn
        );
    }
    Ok(log)
}

fn init(log: &Path, appender_key: &Path, root: &RootArgs) -> Result<(), Stop> {
    let signer = root.signer()?;
    let appender = keys::load(appender_key)?;
    let (pin, appender_public) = (signer.public(), public_key(&appender));
    if pin == appender_public {
        return Err("the appender key must not be the root key".into());
    }

    // The entries are fixed by the two keys and this build, so they can
    // be signed before the log exists. Nothing is created until they are.
    let entries = [
        ("genesis", genesis::genesis(pin)),
        ("bootstrap", genesis::bootstrap(pin, appender_public)),
        ("policy", genesis::first_policy(pin, BUILD)),
    ];
    let submissions: Vec<Submission> = entries.iter().map(|(_, entry)| entry.clone()).collect();
    let signatures = signer.sign(&submissions)?;

    let storage =
        FileStorage::create(log).map_err(|error| format!("{}: {error}", log.display()))?;
    let (mut appender, _) = Appender::open(storage, SystemClock, appender, pin)
        .map_err(|error| format!("{}: {error}", log.display()))?;

    println!("log       {}", log.display());
    println!("root      {}   pin this in every replica", hex(&pin));
    println!("appender  {}", hex(&appender_public));
    println!("evaluator {BUILD}");
    for ((name, submission), signature) in entries.into_iter().zip(signatures) {
        let ack = appender
            .submit(submission, signature)
            .map_err(|error| format!("{name}: {error}"))?;
        println!("entry {}   {name:<9} {}", ack.seq, hex(&ack.hash));
    }
    Ok(())
}

/// Open a log for writing: locked, replayed from genesis against `pin`.
fn open(log: &Path, appender_key: &Path, pin: PublicKey) -> Result<Open, String> {
    let appender = keys::load(appender_key)?;
    let storage = FileStorage::open(log).map_err(|error| format!("{}: {error}", log.display()))?;
    let (appender, recovery) = Appender::open(storage, SystemClock, appender, pin)
        .map_err(|error| format!("{}: {error}", log.display()))?;
    if recovery.dropped > 0 {
        eprintln!(
            "island-cli: note: dropped {} bytes of an unfinished last record",
            recovery.dropped
        );
    }
    Ok(appender)
}

/// What a writer is told when its submission did not become an entry. A
/// gate refusal names the verdict, and the hash an override request needs.
fn rejected(submission: &Submission, error: &Rejected) -> String {
    match error {
        Rejected::Refused { seq, reason, .. } => format!(
            "refused by the gate: {}\n\
             verdict at entry {seq}; the refused submission is {}",
            show::refusal(reason),
            hex(&submission.hash())
        ),
        other => other.to_string(),
    }
}

/// The hash of the refused submission `name` stands for: the whole hash,
/// or the first digits of it. A verdict in the log must hold it.
fn refused_submission(records: &[Record], name: &str) -> Result<Hash, String> {
    let name = name.to_lowercase();
    if name.len() < 8 || !name.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("name the refused submission by at least eight hex digits of its hash".into());
    }
    let refused: BTreeSet<Hash> = records
        .iter()
        .filter_map(|record| match &record.entry.submission.body {
            Body::Verdict(verdict) => Some(verdict.rejected.hash()),
            _ => None,
        })
        .filter(|hash| hex(hash).starts_with(&name))
        .collect();
    let mut refused = refused.into_iter();
    match (refused.next(), refused.next()) {
        (Some(hash), None) => Ok(hash),
        (None, _) => Err(format!(
            "no verdict in the log holds a refused submission whose hash starts {name}"
        )),
        _ => Err(format!(
            "more than one refused submission starts {name}; give more digits"
        )),
    }
}

/// Append one entry written by the root key, on the stream its kind
/// belongs to.
///
/// The log is opened and locked first, so `body` can be made from the
/// state it meets, and the root's counter in the submission is the one
/// the log expects. If the hash has yet to be signed by hand, this stops
/// before writing anything.
fn write_as_root(
    log: &Path,
    appender_key: &Path,
    root: &RootArgs,
    body: impl FnOnce(&Open) -> Result<Body, String>,
) -> Result<Ack, Stop> {
    let signer = root.signer()?;
    let pin = signer.public();
    let mut appender = open(log, appender_key, pin)?;

    let counter = appender
        .chain()
        .next_counter(&pin)
        .ok_or("the root key has no counter left")?;
    let body = body(&appender)?;
    let stream = match body {
        Body::Policy(_) => StreamId::POLICY,
        _ => StreamId::AUTHORITY,
    };
    let submission = Submission {
        stream: StreamId::from(stream),
        writer: pin,
        counter,
        body,
    };
    let signature = signer
        .sign(std::slice::from_ref(&submission))?
        .pop()
        .ok_or("no signature")?;
    Ok(appender
        .submit(submission.clone(), signature)
        .map_err(|error| rejected(&submission, &error))?)
}

/// Append one entry written by an ordinary key, signed with its key file.
fn write_as_key(writer: &WriterArgs, stream: StreamId, body: Body) -> Result<Ack, Stop> {
    let key: SigningKey = keys::load(&writer.key)?;
    let public = public_key(&key);
    let mut appender = open(&writer.log, &writer.appender_key, writer.root)?;

    let counter = appender
        .chain()
        .next_counter(&public)
        .ok_or("the key has no counter left")?;
    let submission = Submission {
        stream,
        writer: public,
        counter,
        body,
    };
    let signature = submission.sign(&key).ok_or("the key does not sign")?;
    Ok(appender
        .submit(submission.clone(), signature)
        .map_err(|error| rejected(&submission, &error))?)
}
