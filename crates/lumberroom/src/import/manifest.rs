//! The claude.ai export manifest, and fetching the parts it names.
//!
//! An export arrives as a manifest naming four zips, and it states the constraint that shapes
//! everything downstream: **"Each export URL can only be used once."** The owner opens each link in
//! a browser, which spends it, and every later run of `import` reads the directory rather than the
//! network so a second look costs nothing.
//!
//! Reading it is all this module does. Fetching the parts it names is not possible from here, and
//! `parts` carries the measurement that settled that.

use std::path::Path;

use serde::Deserialize;

use crate::client::{err, Result};

/// One row of `data_files`. Fields beyond these exist (`batch_index`, `part`) and are not read:
/// the filename already orders and names the part, and a field this code does not use is a field
/// that cannot go stale.
#[derive(Debug, Clone, Deserialize)]
pub struct Part {
    pub filename: String,
    pub export_url: String,
    pub category: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub data_files: Vec<Part>,
}

impl Manifest {
    /// Read and validate. A manifest with no parts is refused here rather than reported as a run
    /// that fetched nothing and succeeded.
    pub fn read(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| err(format!("cannot read manifest {}: {e}", path.display())))?;
        let m: Manifest = serde_json::from_str(&raw).map_err(|e| {
            err(format!(
                "{} is not a claude.ai export manifest: {e}. \
                 Expected the JSON that arrives with the export mail, holding `data_files`.",
                path.display()
            ))
        })?;
        if m.data_files.is_empty() {
            return Err(err(format!("{} names no data_files", path.display())));
        }
        Ok(m)
    }
}

/// The first four bytes of any zip, `PK\x03\x04`. Content-type is not enough on its own, and a
/// local magic check costs nothing and cannot be talked out of by a header.
pub const ZIP_MAGIC: [u8; 4] = [0x50, 0x4B, 0x03, 0x04];

pub fn looks_like_zip(bytes: &[u8]) -> bool {
    bytes.len() > 4 && bytes[..4] == ZIP_MAGIC
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The manifest this was built against, trimmed to two parts. Field names come from the real
    /// file so a rename upstream fails here rather than at the owner's first import.
    const REAL_SHAPE: &str = r#"{
      "instructions": "Download each file using the export_url. Note: Each export URL can only be used once.",
      "created_at": "2026-08-25T05:13:31.674382+00:00",
      "total_files": 4,
      "data_files": [
        {"batch_index": 2, "export_url": "https://claude.ai/export/x/download/aaa",
         "category": "memories", "part": 0, "filename": "memories-000.zip"},
        {"batch_index": 3, "export_url": "https://claude.ai/export/x/download/bbb",
         "category": "conversations", "part": 0, "filename": "conversations-000.zip"}
      ],
      "version": "1.0"
    }"#;

    /// The first bytes of the Cloudflare interstitial all four URLs answered with on 25 August
    /// 2026. Kept verbatim: a paraphrase would test the paraphrase.
    const CHALLENGE: &[u8] = br#"<!DOCTYPE html><html lang="en-US"><head><title>Just a moment...</title><meta http-equiv="Content-Type" content="text/html; charset=UTF-8">"#;

    fn write(dir: &std::path::Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn the_real_manifest_shape_parses() {
        let dir = std::env::temp_dir().join(format!("lr-man-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = write(&dir, "manifest.json", REAL_SHAPE.as_bytes());
        let m = Manifest::read(&p).unwrap();
        assert_eq!(m.data_files.len(), 2);
        assert_eq!(m.data_files[0].category, "memories");
        assert_eq!(m.data_files[1].filename, "conversations-000.zip");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_manifest_with_no_parts_is_refused_rather_than_succeeding_quietly() {
        let dir = std::env::temp_dir().join(format!("lr-man-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = write(&dir, "m.json", br#"{"data_files": []}"#);
        assert!(Manifest::read(&p).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn something_that_is_not_a_manifest_says_so() {
        let dir = std::env::temp_dir().join(format!("lr-man-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = write(&dir, "m.json", b"{\"conversations\": []}");
        let e = Manifest::read(&p).unwrap_err();
        assert!(e.message.contains("export manifest"), "message was: {}", e.message);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The one thing this module still decides. Whatever a browser saved under an export
    /// filename must not read as an archive unless it is one.
    #[test]
    fn nothing_that_is_not_an_archive_reads_as_one() {
        let app_shell = b"<!DOCTYPE html><html><head><title>Claude</title></head><body>";
        for body in
            [CHALLENGE, &app_shell[..], b"{\"error\":\"expired\"}", b"", b"PK", &ZIP_MAGIC[..]]
        {
            assert!(
                !looks_like_zip(body),
                "{:?} must not read as an archive",
                &body[..body.len().min(24)]
            );
        }
    }

    #[test]
    fn an_archive_is_recognised() {
        let mut zip = ZIP_MAGIC.to_vec();
        zip.extend_from_slice(b"the rest of some archive");
        assert!(looks_like_zip(&zip));
    }
}
