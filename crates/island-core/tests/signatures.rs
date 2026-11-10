//! Signing and verification: who wrote, and who ordered.

use island_core::{
    BadSignature, Body, Entry, Evidence, Record, SigningKey, StreamId, Submission, VerifyError,
    Verse, public_key, sign_hash, verify_hash,
};

fn key(byte: u8) -> SigningKey {
    SigningKey::from_bytes(&[byte; 32])
}

fn sealed() -> (Record, [u8; 32]) {
    let writer = key(1);
    let appender = key(2);
    let submission = Submission {
        stream: StreamId::from("island"),
        writer: public_key(&writer),
        counter: 1,
        body: Body::Verse(Verse::new(
            Evidence::Decision,
            "sign what you submit",
            vec![],
        )),
    };
    let sig_writer = submission.sign(&writer).expect("the writer's own key");
    let entry = Entry {
        seq: 3,
        at: 10,
        policy: 2,
        prev: [0; 32],
        submission,
        sig_writer,
    };
    (Record::seal(entry, &appender), public_key(&appender))
}

/// RFC 8032, section 7.1, test 1: the empty message.
#[test]
fn ed25519_matches_rfc_8032() {
    fn unhex<const N: usize>(text: &str) -> [u8; N] {
        let bytes: Vec<u8> = (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex digit"))
            .collect();
        bytes.try_into().expect("length")
    }
    let secret: [u8; 32] =
        unhex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60");
    let public: [u8; 32] =
        unhex("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a");
    let signature: [u8; 64] = unhex(concat!(
        "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e06522490155",
        "5fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b"
    ));
    use ed25519_dalek::Signer;
    let key = SigningKey::from_bytes(&secret);
    assert_eq!(public_key(&key), public);
    assert_eq!(key.sign(b"").to_bytes(), signature);
}

#[test]
fn a_sealed_record_verifies() {
    let (record, appender) = sealed();
    assert_eq!(record.verify(&appender), Ok(()));
    assert_eq!(record.entry.verify_writer(), Ok(()));
}

#[test]
fn only_the_named_writer_can_sign() {
    let (record, _) = sealed();
    assert_eq!(record.entry.submission.sign(&key(9)), None);
}

#[test]
fn signing_is_deterministic() {
    let hash = [5; 32];
    assert_eq!(sign_hash(&key(1), &hash), sign_hash(&key(1), &hash));
    assert_eq!(
        verify_hash(&public_key(&key(1)), &hash, &sign_hash(&key(1), &hash)),
        Ok(())
    );
    assert_eq!(
        verify_hash(&public_key(&key(2)), &hash, &sign_hash(&key(1), &hash)),
        Err(BadSignature)
    );
}

#[test]
fn a_changed_submission_fails_the_hash_first() {
    let (mut record, appender) = sealed();
    record.entry.submission.counter = 2;
    assert_eq!(record.verify(&appender), Err(VerifyError::HashMismatch));
}

#[test]
fn a_changed_position_fails_the_hash() {
    let (mut record, appender) = sealed();
    record.entry.seq = 4;
    assert_eq!(record.verify(&appender), Err(VerifyError::HashMismatch));
}

#[test]
fn a_forged_writer_signature_is_caught_even_when_the_appender_signs() {
    // An appender that seals a submission the writer never signed.
    let (record, appender) = sealed();
    let mut entry = record.entry;
    entry.submission.counter = 2;
    let forged = Record::seal(entry, &key(2));
    assert_eq!(
        forged.verify(&appender),
        Err(VerifyError::BadWriterSignature)
    );
}

#[test]
fn another_appender_is_refused() {
    let (record, _) = sealed();
    let resealed = Record::seal(record.entry, &key(3));
    assert_eq!(
        resealed.verify(&public_key(&key(2))),
        Err(VerifyError::BadAppenderSignature)
    );
}

#[test]
fn a_key_that_is_not_a_curve_point_is_a_bad_signature() {
    // These 32 bytes (y = 2) are not a point on the curve, so not a key.
    let mut not_a_key = [0u8; 32];
    not_a_key[0] = 2;
    assert_eq!(
        verify_hash(&not_a_key, &[0; 32], &[0; 64]),
        Err(BadSignature)
    );
}

#[test]
fn the_two_hash_domains_differ() {
    // The same bytes under the two domains must not collide.
    let bytes = b"same bytes";
    assert_ne!(
        blake3::derive_key(island_core::SUBMISSION_DOMAIN, bytes),
        blake3::derive_key(island_core::ENTRY_DOMAIN, bytes)
    );
}
