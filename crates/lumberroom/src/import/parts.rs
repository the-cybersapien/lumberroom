//! Checking an export directory against the manifest that describes it.
//!
//! # Why nothing here reaches the network
//!
//! The obvious version of this module fetched each `export_url` and saved the archive. It cannot
//! work, and the reason belongs here so nobody rebuilds it.
//!
//! Measured on 25 August 2026. A GET of an export URL from outside a browser is answered with the
//! claude.ai single page application: 101,695 bytes of HTML holding no file URL, no signed storage
//! URL, and no reference to any storage host. The archive is fetched afterwards by that
//! application's own JavaScript, carrying the account's session cookie, and without the cookie the
//! server answers with the shell rather than the file. What comes back also varies by client: curl
//! was answered HTTP 403 with a Cloudflare interstitial while reqwest was answered HTTP 200 with
//! the shell, one minute apart against the same link.
//!
//! Carrying the cookie would mean asking the owner to paste a session credential into a CLI, for the
//! one workflow whose point is being easy, and it would break each time that cookie rotates. So the
//! browser downloads the files and this module does the part a browser is bad at: proving that what
//! landed is complete, is what the manifest named, and is an archive rather than a saved error page.
//!
//! The manifest's own constraint survives rather than being worked around. A link is spent when the
//! browser opens it once, and a survey that requests nothing runs as often as the owner likes.

use std::path::{Path, PathBuf};

use crate::import::manifest::{looks_like_zip, Manifest, Part};

/// What the directory holds for one part the manifest named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Present {
        path: PathBuf,
        bytes: u64,
    },
    /// The filename is there and the bytes are not an archive. A browser that was signed out saves
    /// the HTML shell under the name it was given, so in `ls` this case is indistinguishable from
    /// success, and it stays that way until something reads the first four bytes.
    NotAnArchive {
        path: PathBuf,
        bytes: u64,
    },
    Missing,
}

impl State {
    pub fn is_ready(&self) -> bool {
        matches!(self, State::Present { .. })
    }
}

pub fn survey(m: &Manifest, dir: &Path) -> Vec<(Part, State)> {
    m.data_files
        .iter()
        .map(|part| {
            let path = dir.join(&part.filename);
            let state = match std::fs::read(&path) {
                Err(_) => State::Missing,
                Ok(bytes) if looks_like_zip(&bytes) => {
                    State::Present { path, bytes: bytes.len() as u64 }
                }
                Ok(bytes) => State::NotAnArchive { path, bytes: bytes.len() as u64 },
            };
            (part.clone(), state)
        })
        .collect()
}

/// The manual step, listing only what is outstanding.
pub fn download_instructions(survey: &[(Part, State)], dir: &Path, manifest: &Path) -> String {
    let mut s = String::from(
        "An export link is served to the claude.ai app rather than to a plain download, so it \
         needs the session your browser already holds.\n\n\
         Open each of these in the browser you are signed in to claude.ai with. \
         Each link works once:\n\n",
    );
    for (part, state) in survey.iter().filter(|(_, s)| !s.is_ready()) {
        s.push_str(&format!("  {}\n    {}\n", part.filename, part.export_url));
        if let State::NotAnArchive { path, bytes } = state {
            s.push_str(&format!(
                "    replace {}: the {bytes} bytes there now are not an archive\n",
                path.display()
            ));
        }
    }
    s.push_str(&format!(
        "\nSave them into {}, keeping the filenames, then run the same command again:\n\n  \
         lumberroom import claude --manifest {} --dir {}\n",
        dir.display(),
        manifest.display(),
        dir.display()
    ));
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::manifest::ZIP_MAGIC;

    fn manifest_of(names: &[&str]) -> Manifest {
        let files: Vec<String> = names
            .iter()
            .map(|n| {
                format!(
                    r#"{{"export_url":"https://claude.ai/export/x/download/{n}",
                        "category":"{n}","filename":"{n}.zip"}}"#
                )
            })
            .collect();
        serde_json::from_str(&format!(r#"{{"data_files":[{}]}}"#, files.join(","))).unwrap()
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lr-parts-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_missing_file_is_missing_and_a_real_archive_is_ready() {
        let dir = scratch("mixed");
        let m = manifest_of(&["memories", "conversations"]);
        let mut zip = ZIP_MAGIC.to_vec();
        zip.extend_from_slice(b"contents");
        std::fs::write(dir.join("memories.zip"), &zip).unwrap();

        let s = survey(&m, &dir);
        assert!(s[0].1.is_ready(), "memories should be ready: {:?}", s[0].1);
        assert_eq!(s[1].1, State::Missing);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The case `ls` cannot see. A signed-out browser saves the app shell under the right filename,
    /// and every later stage would read it as an export and find nothing in it.
    #[test]
    fn a_saved_html_page_under_the_right_name_is_not_accepted() {
        let dir = scratch("html");
        let m = manifest_of(&["memories"]);
        std::fs::write(dir.join("memories.zip"), b"<!DOCTYPE html><html><body>Claude</body>")
            .unwrap();

        let s = survey(&m, &dir);
        assert!(!s[0].1.is_ready());
        assert!(matches!(s[0].1, State::NotAnArchive { .. }), "was {:?}", s[0].1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_instructions_name_only_what_is_outstanding_and_a_command_that_exists() {
        let dir = scratch("instr");
        let m = manifest_of(&["memories", "conversations"]);
        let mut zip = ZIP_MAGIC.to_vec();
        zip.extend_from_slice(b"contents");
        std::fs::write(dir.join("memories.zip"), &zip).unwrap();

        let s = survey(&m, &dir);
        let text = download_instructions(&s, &dir, Path::new("/tmp/manifest.json"));
        assert!(text.contains("conversations.zip"), "outstanding part missing from: {text}");
        assert!(!text.contains("memories.zip"), "a ready part was listed: {text}");
        assert!(text.contains("--manifest /tmp/manifest.json"), "retry command wrong: {text}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_file_that_is_not_an_archive_is_named_for_replacement() {
        let dir = scratch("replace");
        let m = manifest_of(&["memories"]);
        std::fs::write(dir.join("memories.zip"), b"<!DOCTYPE html>").unwrap();

        let s = survey(&m, &dir);
        let text = download_instructions(&s, &dir, Path::new("/tmp/m.json"));
        assert!(text.contains("are not an archive"), "no replace hint in: {text}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
