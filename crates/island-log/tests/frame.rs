//! Framing: what a log file is, and what a crash can leave at its end.

use island_log::frame::{self, FrameError, HEADER_LEN, MAGIC, MAX_RECORD_LEN};

fn file(records: &[&[u8]]) -> Vec<u8> {
    let mut bytes = frame::header();
    for record in records {
        bytes.extend(frame::frame(record).expect("small enough"));
    }
    bytes
}

#[test]
fn a_file_scans_back_to_its_records() {
    let bytes = file(&[b"one", b"", b"three"]);
    let scan = frame::scan(&bytes).expect("a log");
    assert_eq!(scan.records, [&b"one"[..], &b""[..], &b"three"[..]]);
    assert_eq!(scan.valid_len, bytes.len());
    assert_eq!(scan.torn, 0);
}

#[test]
fn every_cut_leaves_the_complete_records_and_a_torn_tail() {
    let records: [&[u8]; 3] = [b"first record", b"second", b"the third record"];
    let bytes = file(&records);

    // Where each complete prefix of the file ends.
    let mut ends = vec![MAGIC.len()];
    for record in &records {
        ends.push(ends[ends.len() - 1] + HEADER_LEN + record.len());
    }

    for cut in 0..=bytes.len() {
        let scan = frame::scan(&bytes[..cut]).expect("a cut is never damage");
        let complete = ends.iter().filter(|end| **end <= cut).count();
        let expected = complete.saturating_sub(1);
        assert_eq!(scan.records.len(), expected, "cut at {cut}");
        assert_eq!(scan.records, &records[..expected], "cut at {cut}");
        let valid_len = if complete == 0 { 0 } else { ends[complete - 1] };
        assert_eq!(scan.valid_len, valid_len, "cut at {cut}");
        assert_eq!(scan.torn, cut - valid_len, "cut at {cut}");
    }
}

#[test]
fn a_file_that_is_not_a_log_is_refused() {
    assert_eq!(frame::scan(b"not a log at all"), Err(FrameError::NotALog));
    assert_eq!(frame::scan(b"isle?"), Err(FrameError::NotALog));
}

#[test]
fn a_damaged_length_is_damage_not_a_torn_tail() {
    let bytes = file(&[b"one", b"two", b"three"]);
    let second = MAGIC.len() + HEADER_LEN + 3;

    // Every single-bit change to the second frame's header is caught. In
    // particular a length that now points past the end of the file is not
    // taken for an unfinished record.
    for byte in 0..HEADER_LEN {
        for bit in 0..8 {
            let mut damaged = bytes.clone();
            damaged[second + byte] ^= 1 << bit;
            assert_eq!(
                frame::scan(&damaged),
                Err(FrameError::BadHeader {
                    index: 1,
                    offset: second as u64
                }),
                "byte {byte}, bit {bit}"
            );
        }
    }
}

#[test]
fn a_zero_filled_tail_is_damage() {
    let mut bytes = file(&[b"one"]);
    let end = bytes.len() as u64;
    bytes.extend([0u8; 64]);
    assert_eq!(
        frame::scan(&bytes),
        Err(FrameError::BadHeader {
            index: 1,
            offset: end
        })
    );
}

#[test]
fn an_oversized_record_is_not_framed_or_read() {
    assert!(frame::frame(&vec![0u8; MAX_RECORD_LEN + 1]).is_none());
    assert!(frame::frame(&vec![0u8; MAX_RECORD_LEN]).is_some());

    let len = (MAX_RECORD_LEN + 1) as u32;
    let mut bytes = frame::header();
    bytes.extend(len.to_le_bytes());
    bytes.extend((!len).to_le_bytes());
    assert_eq!(
        frame::scan(&bytes),
        Err(FrameError::BadHeader {
            index: 0,
            offset: MAGIC.len() as u64
        })
    );
}
