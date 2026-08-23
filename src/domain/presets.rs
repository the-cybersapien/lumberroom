//! Named grant shapes, so issuing a credential is a choice between four things rather than nine.
//!
//! The two-axis model is expressive and that is the problem at the moment somebody is adding a
//! client: read and write are lists of globs with a ceiling each, and five capability flags sit
//! beside them. Every one of those is a decision, and a form that asks all of them for every client
//! gets answered by copying whatever the last one had.
//!
//! These are the shapes actually handed out. They live in `domain` because a preset is policy and
//! touches no I/O, and because the console and the command line have to agree about what
//! "read only" means or the two surfaces issue different credentials under one name.
//!
//! **A preset is a starting point and never a ceiling on what the form can express.** The advanced
//! view edits the grant these produce. Anything a preset cannot say, the form still can.

use serde::{Deserialize, Serialize};

use crate::domain::policy::NamespaceGrant;
use crate::domain::types::Sensitivity;

/// What a client is for, in the words the owner would use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Preset {
    /// Reads everything, writes nothing.
    ReadOnly,
    /// Reads everything, writes at open. The shape a chat surface gets.
    ReadWrite,
    /// Fills the ingest queue and decides nothing.
    IngestBot,
    /// Everything except deletion.
    Full,
}

/// The grant a preset produces.
#[derive(Debug, Clone, PartialEq)]
pub struct Shape {
    pub read: Vec<NamespaceGrant>,
    pub write: Vec<NamespaceGrant>,
    pub registry_write: bool,
    pub sealed_capable: bool,
    pub may_delete: bool,
    pub may_ingest: bool,
    pub may_read_history: bool,
}

impl Preset {
    pub const ALL: [Preset; 4] =
        [Preset::ReadOnly, Preset::ReadWrite, Preset::IngestBot, Preset::Full];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::ReadWrite => "read-write",
            Self::IngestBot => "ingest-bot",
            Self::Full => "full",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == s)
    }

    /// The label on the form.
    pub fn title(self) -> &'static str {
        match self {
            Self::ReadOnly => "Read only",
            Self::ReadWrite => "Read and write",
            Self::IngestBot => "Ingest bot",
            Self::Full => "Full",
        }
    }

    /// What the owner is agreeing to, said plainly enough to choose by.
    pub fn detail(self) -> &'static str {
        match self {
            Self::ReadOnly => {
                "Reads every namespace at every level and writes nothing. A surface that answers \
                 questions and records none."
            }
            Self::ReadWrite => {
                "Reads every namespace at every level and writes at open. The shape a chat surface \
                 gets: it can remember something for you, and anything it writes that classifies \
                 private is refused rather than stored."
            }
            Self::IngestBot => {
                "Fills the proposal queue and decides nothing. No read, no write, and mayIngest \
                 alone. For a process that reads transcripts on your behalf."
            }
            Self::Full => {
                "Everything except deletion. Reads and writes at every level, seals, writes the \
                 registry, and reads history. Deletion stays off, because a client that can \
                 silently remove a memory is a worse failure than one that hoards them."
            }
        }
    }

    pub fn shape(self) -> Shape {
        match self {
            Self::ReadOnly => Shape {
                read: NamespaceGrant::everything(),
                // Explicitly empty, which the grant reader treats as no access at all. Absent
                // would mean unrestricted, and that is the opposite of this preset.
                write: vec![],
                registry_write: false,
                sealed_capable: false,
                may_delete: false,
                may_ingest: false,
                may_read_history: false,
            },
            Self::ReadWrite => Shape {
                read: NamespaceGrant::everything(),
                write: vec![NamespaceGrant::new("*", Sensitivity::Open)],
                registry_write: false,
                sealed_capable: false,
                may_delete: false,
                may_ingest: false,
                may_read_history: false,
            },
            Self::IngestBot => Shape {
                read: vec![],
                write: vec![],
                registry_write: false,
                sealed_capable: false,
                may_delete: false,
                may_ingest: true,
                may_read_history: false,
            },
            Self::Full => Shape {
                read: NamespaceGrant::everything(),
                write: NamespaceGrant::everything(),
                registry_write: true,
                sealed_capable: true,
                may_delete: false,
                may_ingest: true,
                may_read_history: true,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_round_trips_through_its_wire_name() {
        for p in Preset::ALL {
            assert_eq!(Preset::parse(p.as_str()), Some(p));
        }
        assert_eq!(Preset::parse("whatever-the-last-one-had"), None);
    }

    #[test]
    fn no_preset_grants_deletion() {
        // A client that can silently remove a memory is the one failure this system refuses to make
        // easy. Deletion is reachable through the advanced view and never by picking a shape.
        for p in Preset::ALL {
            assert!(!p.shape().may_delete, "{} grants deletion", p.as_str());
        }
    }

    #[test]
    fn read_only_writes_nothing_and_says_so_explicitly() {
        // An absent write list means unrestricted. An empty one means none. This preset needs the
        // second, and the difference is the whole of it.
        let s = Preset::ReadOnly.shape();
        assert!(s.write.is_empty());
        assert_eq!(s.read, NamespaceGrant::everything());
    }

    #[test]
    fn read_write_cannot_store_anything_private() {
        // The ceiling is open, so a write that classifies private is refused rather than stored at
        // a level the client was never granted.
        let s = Preset::ReadWrite.shape();
        assert_eq!(s.write.len(), 1);
        assert_eq!(s.write[0].max, Sensitivity::Open);
        assert!(!s.sealed_capable);
    }

    #[test]
    fn an_ingest_bot_reads_nothing_and_writes_nothing() {
        let s = Preset::IngestBot.shape();
        assert!(s.read.is_empty(), "an ingest bot has no reason to read the store");
        assert!(s.write.is_empty(), "it proposes, and approval is what writes");
        assert!(s.may_ingest);
    }

    #[test]
    fn full_is_everything_except_deletion() {
        let s = Preset::Full.shape();
        assert_eq!(s.read, NamespaceGrant::everything());
        assert_eq!(s.write, NamespaceGrant::everything());
        assert!(s.registry_write && s.sealed_capable && s.may_ingest && s.may_read_history);
        assert!(!s.may_delete);
    }

    #[test]
    fn every_preset_carries_a_title_and_a_reason_to_pick_it() {
        for p in Preset::ALL {
            assert!(!p.title().is_empty());
            assert!(p.detail().len() > 40, "{} has no explanation worth reading", p.as_str());
        }
    }
}
