//! The lumberroom archive format.
//!
//! A `.lumber` file is `.jsonl.gz.age` and the extension is an alias rather than a container of its
//! own. `age -d store.lumber | gunzip | jq` reads one without this crate, which is the property
//! that matters most: an export of a person's memory should never need a vendor's tool to open.
//!
//! This crate holds no database handle, opens no socket and reads no environment. It turns records
//! into bytes and bytes back into records.

pub mod container;
pub mod reader;
pub mod record;
pub mod writer;

pub use record::*;

/// The `format` value every archive carries. A file without it is not one of ours.
pub const FORMAT: &str = "lumberroom.archive";

/// The version this crate writes. A reader accepts this and below.
pub const VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("this is not a lumberroom archive: the first record is {found}")]
    NotAnArchive { found: String },
    #[error("archive version {found} is newer than this build understands ({VERSION})")]
    TooNew { found: u32 },
    /// The body and the header disagree in either direction. age's own framing catches a cut file;
    /// this catches a writer that stopped early for a reason of its own, and an edited file that
    /// carries rows the header never counted.
    #[error(
        "this archive does not match its header: it promised {expected} {section} records and {found} arrived"
    )]
    Short { section: &'static str, expected: u64, found: u64 },
    #[error("record {line} is not valid JSON: {source}")]
    Malformed { line: usize, source: serde_json::Error },
    #[error("the archive decompresses past the {limit} byte ceiling, so it was refused unread")]
    TooLarge { limit: u64 },
    #[error("the passphrase does not open this archive")]
    BadPassphrase,
    /// Refused before the stretch runs, not after. The uploader chooses this parameter.
    #[error("this archive asks for scrypt work factor {found}, and this reader accepts {limit}")]
    WorkFactorTooHigh { found: u8, limit: u8 },
    #[error("age refused the file: {0}")]
    Age(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, ArchiveError>;
