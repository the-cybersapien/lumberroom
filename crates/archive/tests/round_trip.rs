//! The writer and the reader against each other, through the real container.
//!
//! Every assertion here is about the property the format exists for: what went in comes back out,
//! or the reader says why it did not. Nothing here decrypts a sealed item, because nothing in the
//! server can.

use lumberroom_archive::*;
use secrecy::SecretString;

fn sealing() -> container::Sealing {
    container::Sealing::Passphrase(SecretString::from("correct horse battery staple"))
}

fn a_memory(id: &str, sensitivity: &str) -> MemoryRecord {
    MemoryRecord {
        id: id.into(),
        namespace: "user:me".into(),
        content: format!("fact {id}"),
        tags: vec!["preference".into()],
        source_client: "claude-code".into(),
        sensitivity: sensitivity.into(),
        supersedes: None,
        superseded_by: None,
        superseded_at: None,
        occurred_at: None,
        occurred_until: None,
        access_count: 4,
        last_accessed_at: None,
        last_confirmed_at: None,
        created_at: "2026-08-30T11:04:00Z".into(),
        embedding_model: Some("bge-small-en-v1.5".into()),
    }
}

fn a_sealed_item() -> SealedItemRecord {
    SealedItemRecord {
        namespace: "credentials:aws".into(),
        key_hmac: "abc".into(),
        ciphertext: "YmFzZTY0".into(),
        alg: "xchacha20poly1305".into(),
        source_client: "lumberroom-cli".into(),
        created_at: "2026-08-30T11:04:00Z".into(),
        updated_at: "2026-08-30T11:04:00Z".into(),
    }
}

fn build() -> Vec<u8> {
    let mut w = writer::ArchiveWriter::new(Source { kind: "oss".into(), build: "0.1.0".into() });
    w.push(Record::Memory(a_memory("id-1", "open")));
    w.push(Record::Memory(a_memory("id-2", "private")));
    w.push(Record::SealedItem(a_sealed_item()));
    w.finish("2026-08-30T11:04:00Z".into(), true, Excluded::default(), &sealing()).unwrap()
}

#[test]
fn every_field_survives_a_round_trip() {
    let archive = reader::Archive::open(&build(), &sealing(), container::MAX_DECOMPRESSED).unwrap();
    assert_eq!(archive.header.format, FORMAT);
    assert_eq!(archive.header.version, VERSION);
    assert_eq!(archive.header.counts.memory, 2);
    assert_eq!(archive.header.counts.sealed_item, 1);
    assert_eq!(archive.memories[0], a_memory("id-1", "open"));
    assert_eq!(archive.memories[1], a_memory("id-2", "private"));
    // The test never decrypts it, which is the point of the level.
    assert_eq!(archive.sealed_items[0], a_sealed_item());
}

#[test]
fn a_header_promising_more_than_arrived_is_refused() {
    // The check that catches a writer stopping early, independent of age's framing. Both fire on a
    // real truncation and a regression in one must not hide behind the other.
    let plain = container::open(&build(), &sealing(), container::MAX_DECOMPRESSED).unwrap();
    let text = String::from_utf8(plain).unwrap();
    let kept: Vec<&str> = text.lines().take(2).collect(); // header plus one memory
    let short = container::seal(kept.join("\n").as_bytes(), &sealing()).unwrap();

    let err = reader::Archive::open(&short, &sealing(), container::MAX_DECOMPRESSED).unwrap_err();
    assert!(matches!(err, ArchiveError::Short { section: "memory", expected: 2, found: 1 }));
}

#[test]
fn a_file_that_is_not_an_archive_says_so() {
    let junk = container::seal(b"{\"hello\":\"world\"}\n", &sealing()).unwrap();
    assert!(matches!(
        reader::Archive::open(&junk, &sealing(), container::MAX_DECOMPRESSED),
        Err(ArchiveError::NotAnArchive { .. })
    ));
}

#[test]
fn a_newer_version_is_refused_by_name() {
    let plain = container::open(&build(), &sealing(), container::MAX_DECOMPRESSED).unwrap();
    let bumped = String::from_utf8(plain).unwrap().replacen("\"version\":1", "\"version\":99", 1);
    let sealed = container::seal(bumped.as_bytes(), &sealing()).unwrap();
    assert!(matches!(
        reader::Archive::open(&sealed, &sealing(), container::MAX_DECOMPRESSED),
        Err(ArchiveError::TooNew { found: 99 })
    ));
}

#[test]
fn the_header_is_line_one_and_the_sections_follow_in_order() {
    // A person opening their own export with `age -d ... | gunzip | head` is the reader this order
    // serves. Pinning it here stops a later section landing wherever the writer happened to put it.
    let plain = container::open(&build(), &sealing(), container::MAX_DECOMPRESSED).unwrap();
    let text = String::from_utf8(plain).unwrap();
    let sections: Vec<String> = text.lines().map(section_of).collect();
    assert_eq!(sections, ["header", "memory", "memory", "sealed_item"]);
}

fn section_of(line: &str) -> String {
    let value: serde_json::Value = serde_json::from_str(line).unwrap();
    value["section"].as_str().unwrap().to_string()
}

#[test]
fn an_archive_with_nothing_in_it_still_reads_back() {
    // An owner with an empty store gets a file that opens, rather than one the reader rejects for
    // looking truncated. Refusing an empty export is the service's call, not the format's.
    let w = writer::ArchiveWriter::new(Source { kind: "oss".into(), build: "0.1.0".into() });
    let at = "2026-08-30T11:04:00Z".to_string();
    let bytes = w.finish(at, false, Excluded::default(), &sealing()).unwrap();

    let archive = reader::Archive::open(&bytes, &sealing(), container::MAX_DECOMPRESSED).unwrap();
    assert_eq!(archive.header.counts, Counts::default());
    assert!(archive.memories.is_empty());
}
