//! Records in, `.lumber` bytes out.
//!
//! The writer owns two things nothing else may own: the order sections appear in, and the counts
//! the header declares. Both exist so a reader can tell a complete archive from a short one.

use crate::container::{seal, Sealing};
use crate::record::*;
use crate::{Result, FORMAT, VERSION};

pub struct ArchiveWriter {
    source: Source,
    counts: Counts,
    memories: Vec<MemoryRecord>,
    registry: Vec<RegistryRecord>,
    aliases: Vec<AliasRecord>,
    registry_history: Vec<RegistryHistoryRecord>,
    sealed_items: Vec<SealedItemRecord>,
}

impl ArchiveWriter {
    pub fn new(source: Source) -> Self {
        Self {
            source,
            counts: Counts::default(),
            memories: Vec::new(),
            registry: Vec::new(),
            aliases: Vec::new(),
            registry_history: Vec::new(),
            sealed_items: Vec::new(),
        }
    }

    /// A `Record::Header` handed here is dropped. The header is built by `finish` from the counts
    /// this writer accumulated, because a caller-supplied count that disagreed with the body would
    /// produce exactly the quietly-short archive the counts exist to catch.
    pub fn push(&mut self, record: Record) {
        match record {
            Record::Header(_) => {}
            Record::Memory(r) => {
                self.counts.memory += 1;
                self.memories.push(r);
            }
            Record::Registry(r) => {
                self.counts.registry += 1;
                self.registry.push(r);
            }
            Record::Alias(r) => {
                self.counts.alias += 1;
                self.aliases.push(r);
            }
            Record::RegistryHistory(r) => {
                self.counts.registry_history += 1;
                self.registry_history.push(r);
            }
            Record::SealedItem(r) => {
                self.counts.sealed_item += 1;
                self.sealed_items.push(r);
            }
        }
    }

    /// What the header will say, before it says it.
    ///
    /// The export service reports progress and refuses an empty archive, and neither should have to
    /// count the rows it just handed over a second time.
    pub fn counts(&self) -> Counts {
        self.counts
    }

    pub fn finish(
        self,
        created_at: Timestamp,
        includes_superseded: bool,
        excluded: Excluded,
        sealing: &Sealing,
    ) -> Result<Vec<u8>> {
        let header = Record::Header(Header {
            format: FORMAT.to_string(),
            version: VERSION,
            created_at,
            source: self.source,
            counts: self.counts,
            includes_superseded,
            excluded,
        });

        // Sections in the order the format declares: memory, registry, alias, registry_history,
        // sealed_item. A reader takes them in any order because each line carries its own tag, so
        // the order serves the person running `age -d store.lumber | gunzip | head`, who should
        // meet their own facts before the bookkeeping.
        let mut lines = String::new();
        append(&mut lines, &header);
        for r in self.memories {
            append(&mut lines, &Record::Memory(r));
        }
        for r in self.registry {
            append(&mut lines, &Record::Registry(r));
        }
        for r in self.aliases {
            append(&mut lines, &Record::Alias(r));
        }
        for r in self.registry_history {
            append(&mut lines, &Record::RegistryHistory(r));
        }
        for r in self.sealed_items {
            append(&mut lines, &Record::SealedItem(r));
        }

        seal(lines.as_bytes(), sealing)
    }
}

/// `serde_json` fails on non-string map keys and on a `Serialize` that returns an error. Every
/// record is a flat struct of strings, integers and `Vec<String>`, so neither is reachable, and a
/// fallible signature here would push a `?` onto callers for a branch that cannot be taken.
fn append(out: &mut String, record: &Record) {
    out.push_str(&serde_json::to_string(record).expect("a record has no unserialisable field"));
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::{open as open_container, MAX_DECOMPRESSED};
    use secrecy::SecretString;

    fn sealing() -> Sealing {
        Sealing::Passphrase(SecretString::from("correct horse battery staple"))
    }

    fn source() -> Source {
        Source { kind: "oss".into(), build: "0.1.0".into() }
    }

    fn a_memory(id: &str) -> MemoryRecord {
        MemoryRecord {
            id: id.into(),
            namespace: "user:me".into(),
            content: format!("fact {id}"),
            tags: Vec::new(),
            source_client: "claude-code".into(),
            sensitivity: "open".into(),
            supersedes: None,
            superseded_by: None,
            superseded_at: None,
            occurred_at: None,
            occurred_until: None,
            access_count: 0,
            last_accessed_at: None,
            last_confirmed_at: None,
            created_at: "2026-08-30T11:04:00Z".into(),
            embedding_model: None,
        }
    }

    /// The written bytes back as NDJSON, so a test asserts on the file rather than on the writer.
    fn body(w: ArchiveWriter) -> String {
        let at = "2026-08-30T11:04:00Z".to_string();
        let bytes = w.finish(at, false, Excluded::default(), &sealing()).unwrap();
        let plain = open_container(&bytes, &sealing(), MAX_DECOMPRESSED).unwrap();
        String::from_utf8(plain).unwrap()
    }

    #[test]
    fn the_header_counts_come_from_what_was_pushed() {
        let mut w = ArchiveWriter::new(source());
        w.push(Record::Memory(a_memory("id-1")));
        w.push(Record::Memory(a_memory("id-2")));
        assert_eq!(w.counts().memory, 2);

        let text = body(w);
        assert!(text.lines().next().unwrap().contains(r#""memory":2"#));
    }

    #[test]
    fn a_caller_supplied_header_is_dropped_rather_than_written() {
        let mut w = ArchiveWriter::new(source());
        w.push(Record::Header(Header {
            format: "not-ours".into(),
            version: 99,
            created_at: "1999-01-01T00:00:00Z".into(),
            source: Source { kind: "forged".into(), build: "x".into() },
            counts: Counts { memory: 4812, ..Counts::default() },
            includes_superseded: true,
            excluded: Excluded::default(),
        }));
        w.push(Record::Memory(a_memory("id-1")));

        let text = body(w);
        assert_eq!(text.lines().count(), 2, "one header and one memory");
        assert!(!text.contains("forged"));
        assert!(text.lines().next().unwrap().contains(r#""memory":1"#));
    }

    #[test]
    fn sections_appear_in_the_order_the_format_declares() {
        let mut w = ArchiveWriter::new(source());
        w.push(Record::SealedItem(SealedItemRecord {
            namespace: "credentials:aws".into(),
            key_hmac: "abc".into(),
            ciphertext: "YmFzZTY0".into(),
            alg: "xchacha20poly1305".into(),
            source_client: "lumberroom-cli".into(),
            created_at: "2026-08-30T11:04:00Z".into(),
            updated_at: "2026-08-30T11:04:00Z".into(),
        }));
        w.push(Record::Alias(AliasRecord {
            namespace: "user:me".into(),
            alias: "home".into(),
            canonical: "home_address".into(),
            since: None,
            until: None,
            origin: "owner".into(),
            created_at: "2026-08-30T11:04:00Z".into(),
        }));
        w.push(Record::Memory(a_memory("id-1")));

        let text = body(w);
        let sections: Vec<String> = text.lines().map(section_of).collect();
        assert_eq!(sections, ["header", "memory", "alias", "sealed_item"]);
    }

    fn section_of(line: &str) -> String {
        let value: serde_json::Value = serde_json::from_str(line).unwrap();
        value["section"].as_str().unwrap().to_string()
    }
}
