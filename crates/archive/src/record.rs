//! The records an archive is made of.
//!
//! One JSON object per line, each tagged by `section`. Sections appear in a fixed order and the
//! header declares how many of each to expect, which is what turns a truncated file into a loud
//! failure instead of a short store.

use serde::{Deserialize, Serialize};

/// Timestamps travel as RFC 3339 strings rather than as `DateTime<Utc>`.
///
/// A reader that rejects a whole archive because one row carries an instant `chrono` will not parse
/// is worse than one that reports that row. The service layer parses and reports per row.
pub type Timestamp = String;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "section", rename_all = "snake_case")]
pub enum Record {
    Header(Header),
    Memory(MemoryRecord),
    Registry(RegistryRecord),
    Alias(AliasRecord),
    RegistryHistory(RegistryHistoryRecord),
    SealedItem(SealedItemRecord),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    pub format: String,
    pub version: u32,
    pub created_at: Timestamp,
    pub source: Source,
    pub counts: Counts,
    /// Whether retired rows are present. A reader uses it to explain an import that produced fewer
    /// live rows than the file's memory count.
    pub includes_superseded: bool,
    pub excluded: Excluded,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    /// `oss` or `cloud`. Provenance for a human reading the file, never a branch in the reader.
    pub kind: String,
    pub build: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    pub memory: u64,
    pub registry: u64,
    pub alias: u64,
    pub registry_history: u64,
    pub sealed_item: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Excluded {
    pub total: u64,
    pub reasons: Vec<String>,
}

/// One memory row.
///
/// No `content_ct`, no `dek_wrapped`, no `kek_id`, and adding one is a bug rather than a feature.
/// `envelope::seal` authenticates the row id as associated data, so ciphertext moved to another
/// id or another install fails its tag check. Private content travels as plaintext inside the age
/// layer and the destination reseals it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub id: String,
    pub namespace: String,
    pub content: String,
    pub tags: Vec<String>,
    pub source_client: String,
    pub sensitivity: String,
    pub supersedes: Option<String>,
    pub superseded_by: Option<String>,
    pub superseded_at: Option<Timestamp>,
    pub occurred_at: Option<Timestamp>,
    pub occurred_until: Option<Timestamp>,
    pub access_count: i32,
    pub last_accessed_at: Option<Timestamp>,
    pub last_confirmed_at: Option<Timestamp>,
    pub created_at: Timestamp,
    /// Provenance. The destination embeds every row it accepts with its own model, because a vector
    /// from one model against documents from another returns confident nonsense rather than an
    /// error.
    pub embedding_model: Option<String>,
}

/// One registry entry.
///
/// The registry is keyed on `(namespace, kind, key)`, so a record without `kind` names no row and an
/// importer can only refuse it. No rule derives the kind from the key: `machines.desktop.os` is a
/// `host` because a writer said so, which is why it travels in the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryRecord {
    pub namespace: String,
    pub kind: String,
    pub key: String,
    pub value: String,
    pub sensitivity: String,
    pub source_client: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AliasRecord {
    pub namespace: String,
    pub alias: String,
    pub canonical: String,
    pub since: Option<Timestamp>,
    pub until: Option<Timestamp>,
    pub origin: String,
    pub created_at: Timestamp,
}

/// One replaced registry value, carrying the same three-part key as the entry it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryHistoryRecord {
    pub namespace: String,
    pub kind: String,
    pub key: String,
    pub value: String,
    pub sensitivity: String,
    pub recorded_at: Timestamp,
}

/// One row of `sealed_item`, copied and never read.
///
/// The columns are exactly what `migrations/20260819000008_encryption.sql` declares: no row id and
/// no wrapped DEK, because that table has neither. The client holds the key and the server binds no
/// associated data, which is why this is the one blob a copy preserves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedItemRecord {
    pub namespace: String,
    pub key_hmac: String,
    /// Base64, because NDJSON carries no bytes.
    pub ciphertext: String,
    pub alg: String,
    pub source_client: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_memory_record_round_trips_through_json() {
        let record = Record::Memory(MemoryRecord {
            id: "0195c0de-0000-7000-8000-000000000001".into(),
            namespace: "user:me".into(),
            content: "a fact".into(),
            tags: vec!["preference".into()],
            source_client: "claude-code".into(),
            sensitivity: "private".into(),
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
        });
        let line = serde_json::to_string(&record).unwrap();
        assert!(line.contains(r#""section":"memory""#));
        let back: Record = serde_json::from_str(&line).unwrap();
        assert_eq!(record, back);
    }

    #[test]
    fn a_registry_record_carries_the_kind_its_key_needs() {
        // Drop this field and every registry row is refused on import, while the restore still
        // reports success over a store with no registry in it.
        let record = Record::Registry(RegistryRecord {
            namespace: "global".into(),
            kind: "host".into(),
            key: "machines.desktop.os".into(),
            value: "darwin".into(),
            sensitivity: "open".into(),
            source_client: "lumberroom-cli".into(),
            created_at: "2026-08-30T11:04:00Z".into(),
            updated_at: "2026-08-30T11:04:00Z".into(),
        });
        let line = serde_json::to_string(&record).unwrap();
        assert!(line.contains(r#""kind":"host""#));
        let back: Record = serde_json::from_str(&line).unwrap();
        assert_eq!(record, back);
    }

    #[test]
    fn a_sealed_item_carries_no_row_id_and_no_wrapped_dek() {
        // sealed_item has neither column. A record that grew them would be describing a private
        // memory row, which is the mistake revision 1 of the spec made.
        let line = serde_json::to_string(&Record::SealedItem(SealedItemRecord {
            namespace: "credentials:aws".into(),
            key_hmac: "abc".into(),
            ciphertext: "YmFzZTY0".into(),
            alg: "xchacha20poly1305".into(),
            source_client: "lumberroom-cli".into(),
            created_at: "2026-08-30T11:04:00Z".into(),
            updated_at: "2026-08-30T11:04:00Z".into(),
        }))
        .unwrap();
        assert!(!line.contains("dek_wrapped"));
        assert!(!line.contains(r#""id""#));
    }
}
