//! Bytes in, validated records out.
//!
//! Two refusals matter more than the rest. A file that decompresses past the ceiling is refused
//! before it is held, and a body that delivers fewer records than the header promised is refused
//! rather than returned short. The second is the one a person notices years later.

use crate::container::{open as open_container, Sealing};
use crate::record::*;
use crate::{ArchiveError, Result, FORMAT, VERSION};

#[derive(Debug)]
pub struct Archive {
    pub header: Header,
    pub memories: Vec<MemoryRecord>,
    pub registry: Vec<RegistryRecord>,
    pub aliases: Vec<AliasRecord>,
    pub registry_history: Vec<RegistryHistoryRecord>,
    pub sealed_items: Vec<SealedItemRecord>,
}

impl Archive {
    pub fn open(bytes: &[u8], sealing: &Sealing, max_decompressed: u64) -> Result<Self> {
        let plain = open_container(bytes, sealing, max_decompressed)?;
        let text = String::from_utf8_lossy(&plain);
        let mut lines = text.lines().enumerate();

        let (_, first) = lines
            .next()
            .ok_or_else(|| ArchiveError::NotAnArchive { found: "an empty file".into() })?;
        let header = match serde_json::from_str::<Record>(first) {
            Ok(Record::Header(h)) => h,
            Ok(other) => {
                return Err(ArchiveError::NotAnArchive {
                    found: format!("a {} record", section_name(&other)),
                })
            }
            Err(_) => {
                return Err(ArchiveError::NotAnArchive { found: "not a record at all".into() })
            }
        };
        if header.format != FORMAT {
            return Err(ArchiveError::NotAnArchive { found: header.format });
        }
        // Refused before a single body line is parsed. A version this build has never seen may have
        // moved a field's meaning rather than added one, and a partial import is worse than none.
        if header.version > VERSION {
            return Err(ArchiveError::TooNew { found: header.version });
        }

        let mut out = Archive {
            header,
            memories: Vec::new(),
            registry: Vec::new(),
            aliases: Vec::new(),
            registry_history: Vec::new(),
            sealed_items: Vec::new(),
        };
        for (n, line) in lines {
            if line.trim().is_empty() {
                continue;
            }
            let record = serde_json::from_str::<Record>(line)
                .map_err(|source| ArchiveError::Malformed { line: n + 1, source })?;
            match record {
                Record::Header(_) => {}
                Record::Memory(r) => out.memories.push(r),
                Record::Registry(r) => out.registry.push(r),
                Record::Alias(r) => out.aliases.push(r),
                Record::RegistryHistory(r) => out.registry_history.push(r),
                Record::SealedItem(r) => out.sealed_items.push(r),
            }
        }

        out.check_complete()?;
        Ok(out)
    }

    /// The header's promise against the body's delivery, section by section.
    ///
    /// age's STREAM framing already catches a cut file. This catches the other shape: a writer that
    /// stopped early for a reason of its own and sealed what it had, which age has no way to notice
    /// because the file it produced is intact.
    fn check_complete(&self) -> Result<()> {
        let c = &self.header.counts;
        let checks: [(&'static str, u64, usize); 5] = [
            ("memory", c.memory, self.memories.len()),
            ("registry", c.registry, self.registry.len()),
            ("alias", c.alias, self.aliases.len()),
            ("registry_history", c.registry_history, self.registry_history.len()),
            ("sealed_item", c.sealed_item, self.sealed_items.len()),
        ];
        for (section, expected, found) in checks {
            if expected != found as u64 {
                return Err(ArchiveError::Short { section, expected, found: found as u64 });
            }
        }
        Ok(())
    }
}

fn section_name(record: &Record) -> &'static str {
    match record {
        Record::Header(_) => "header",
        Record::Memory(_) => "memory",
        Record::Registry(_) => "registry",
        Record::Alias(_) => "alias",
        Record::RegistryHistory(_) => "registry_history",
        Record::SealedItem(_) => "sealed_item",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::{seal, MAX_DECOMPRESSED};
    use secrecy::SecretString;

    const AT: &str = "2026-08-30T11:04:00Z";

    fn sealing() -> Sealing {
        Sealing::Passphrase(SecretString::from("correct horse battery staple"))
    }

    fn line_of(record: &Record) -> String {
        serde_json::to_string(record).unwrap()
    }

    fn header_line(counts: Counts) -> String {
        line_of(&Record::Header(Header {
            format: FORMAT.to_string(),
            version: VERSION,
            created_at: AT.into(),
            source: Source { kind: "oss".into(), build: "0.1.0".into() },
            counts,
            includes_superseded: false,
            excluded: Excluded::default(),
        }))
    }

    fn an_alias() -> Record {
        Record::Alias(AliasRecord {
            namespace: "user:me".into(),
            alias: "home".into(),
            canonical: "home_address".into(),
            since: None,
            until: None,
            origin: "owner".into(),
            created_at: AT.into(),
        })
    }

    fn a_memory() -> Record {
        Record::Memory(MemoryRecord {
            id: "id-1".into(),
            namespace: "user:me".into(),
            content: "a fact".into(),
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
            created_at: AT.into(),
            embedding_model: None,
        })
    }

    fn archive_of(lines: &[String]) -> Vec<u8> {
        let body = format!("{}\n", lines.join("\n"));
        seal(body.as_bytes(), &sealing()).unwrap()
    }

    fn read(bytes: &[u8]) -> Result<Archive> {
        Archive::open(bytes, &sealing(), MAX_DECOMPRESSED)
    }

    #[test]
    fn a_body_with_more_records_than_the_header_promised_is_refused_too() {
        // The overshoot direction. A file claiming no aliases and carrying one has been edited, and
        // importing the extra row silently is how one bad archive becomes a bad store.
        let bytes = archive_of(&[header_line(Counts::default()), line_of(&an_alias())]);

        let err = read(&bytes).unwrap_err();
        assert!(matches!(err, ArchiveError::Short { section: "alias", expected: 0, found: 1 }));
    }

    #[test]
    fn a_blank_line_between_sections_is_not_a_missing_record() {
        let bytes = archive_of(&[header_line(Counts::default()), String::new()]);
        assert!(read(&bytes).is_ok());
    }

    #[test]
    fn a_first_line_that_is_a_memory_names_what_it_found() {
        let bytes = archive_of(&[line_of(&a_memory())]);

        match read(&bytes) {
            Err(ArchiveError::NotAnArchive { found }) => assert_eq!(found, "a memory record"),
            other => panic!("expected NotAnArchive, got {other:?}"),
        }
    }

    #[test]
    fn a_malformed_body_line_reports_its_line_number() {
        let bytes = archive_of(&[header_line(Counts::default()), "{ not json".into()]);

        match read(&bytes) {
            Err(ArchiveError::Malformed { line, .. }) => assert_eq!(line, 2),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }
}
