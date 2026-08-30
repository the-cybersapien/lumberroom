//! The two layers under the records: gzip, then age.
//!
//! Compression runs first because ciphertext does not compress. The order is also what lets
//! `age -d store.lumber | gunzip` work, which is the whole reason the layering stays plain.

use std::io::{Read, Write};

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use secrecy::SecretString;

use crate::{ArchiveError, Result};

/// The most an archive may decompress to before the reader gives up.
///
/// A decompression bomb reaches the process before any of this crate's parsing does, and the server
/// that inflates one is the server every tenant shares. Bounded while reading rather than checked
/// afterwards, because a check afterwards has already allocated the bytes.
pub const MAX_DECOMPRESSED: u64 = 2 * 1024 * 1024 * 1024;

/// The highest scrypt work factor this reader will spend time on.
///
/// The parameter travels in the age header and whoever wrote the file chose it, so a file asking
/// for 30 wants a terabyte of memory and minutes of a core before one record is parsed. `age`
/// compares the stanza's value against this ceiling inside `scrypt::Identity::unwrap_stanza`,
/// before it calls scrypt and before it verifies the header MAC, which is the ordering the guard
/// needs to be worth anything.
///
/// 22 sits four above the factor age targets for an interactive passphrase on current hardware.
///
/// TRAP: age picks the write-side factor by timing scrypt on the machine doing the sealing and
/// aiming at one second, so a fast enough machine could seal an archive this reader then refuses.
/// `a_sealed_archive_opens_with_the_same_passphrase` is the test that would catch it.
pub const MAX_WORK_FACTOR: u8 = 22;

pub enum Sealing {
    Passphrase(SecretString),
    /// No age layer. The caller has to ask for this by name, matching the hard stop
    /// `deploy/backup.sh` makes before it writes an unencrypted dump without
    /// `BACKUP_ALLOW_PLAINTEXT`.
    Plaintext,
}

pub fn seal(plain: &[u8], sealing: &Sealing) -> Result<Vec<u8>> {
    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    gz.write_all(plain)?;
    let compressed = gz.finish()?;

    match sealing {
        Sealing::Plaintext => Ok(compressed),
        Sealing::Passphrase(secret) => {
            let encryptor = age::Encryptor::with_user_passphrase(secret.clone());
            // Dropping this writer without calling `finish` leaves out the final chunk and writes
            // a file age refuses to decrypt. `finish` hands the buffer back.
            let mut writer = encryptor.wrap_output(Vec::new())?;
            writer.write_all(&compressed)?;
            let sealed = writer.finish()?;
            Ok(sealed)
        }
    }
}

pub fn open<'a>(bytes: &'a [u8], sealing: &Sealing, max_decompressed: u64) -> Result<Vec<u8>> {
    let compressed: Box<dyn Read + 'a> = match sealing {
        Sealing::Plaintext => Box::new(bytes),
        Sealing::Passphrase(secret) => {
            let decryptor = age::Decryptor::new_buffered(bytes).map_err(refusal)?;
            let mut identity = age::scrypt::Identity::new(secret.clone());
            // The default ceiling is whatever this machine happens to measure plus four, which
            // makes the refusal depend on the box rather than on a number anyone chose.
            identity.set_max_work_factor(MAX_WORK_FACTOR);
            let stream = decryptor
                .decrypt(std::iter::once(&identity as &dyn age::Identity))
                .map_err(refusal)?;
            Box::new(stream)
        }
    };

    // `take` bounds the read itself. One byte past the ceiling is enough to know, and reading that
    // one byte costs less than trusting a length nobody wrote.
    let mut reader = GzDecoder::new(compressed).take(max_decompressed.saturating_add(1));
    let mut plain = Vec::new();
    reader.read_to_end(&mut plain)?;
    if plain.len() as u64 > max_decompressed {
        return Err(ArchiveError::TooLarge { limit: max_decompressed });
    }
    Ok(plain)
}

/// age tells a passphrase that decrypted nothing apart from a file carrying no stanza this identity
/// recognises. A caller holding a passphrase gets the same answer either way.
fn refusal(e: age::DecryptError) -> ArchiveError {
    match e {
        age::DecryptError::ExcessiveWork { required, .. } => {
            ArchiveError::WorkFactorTooHigh { found: required, limit: MAX_WORK_FACTOR }
        }
        age::DecryptError::DecryptionFailed | age::DecryptError::NoMatchingKeys => {
            ArchiveError::BadPassphrase
        }
        other => ArchiveError::Age(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phrase() -> Sealing {
        Sealing::Passphrase(SecretString::from("correct horse battery staple"))
    }

    #[test]
    fn a_sealed_archive_opens_with_the_same_passphrase() {
        let sealed = seal(b"one\ntwo\n", &phrase()).unwrap();
        assert_eq!(open(&sealed, &phrase(), MAX_DECOMPRESSED).unwrap(), b"one\ntwo\n");
    }

    #[test]
    fn the_wrong_passphrase_is_refused_rather_than_returning_rubbish() {
        let sealed = seal(b"one\n", &phrase()).unwrap();
        let wrong = Sealing::Passphrase(SecretString::from("hunter2"));
        assert!(matches!(
            open(&sealed, &wrong, MAX_DECOMPRESSED),
            Err(ArchiveError::BadPassphrase)
        ));
    }

    #[test]
    fn a_truncated_archive_fails_rather_than_returning_a_shorter_one() {
        // The property age's STREAM construction buys and hand-rolled chunk framing usually
        // misses. Every chunk still authenticates; only the final-chunk marker notices the cut.
        let sealed = seal(b"one\ntwo\nthree\n", &phrase()).unwrap();
        assert!(open(&sealed[..sealed.len() - 8], &phrase(), MAX_DECOMPRESSED).is_err());
        // A second cut further back, so a pass here cannot be the header parser refusing a file
        // that is merely short. Both land inside the payload: the header and the nonce run to
        // about 166 bytes and the sealed payload adds another 50.
        assert!(open(&sealed[..sealed.len() - 20], &phrase(), MAX_DECOMPRESSED).is_err());
    }

    #[test]
    fn a_bomb_is_refused_before_it_is_held_in_memory() {
        // One upload should never be an outage for every tenant on the box.
        let sealed = seal(&vec![b'a'; 4096], &phrase()).unwrap();
        assert!(matches!(
            open(&sealed, &phrase(), 1024),
            Err(ArchiveError::TooLarge { limit: 1024 })
        ));
    }

    #[test]
    fn an_inflated_work_factor_is_refused_before_any_key_stretching_runs() {
        // scrypt parameters travel in the age header and an uploader chooses them. A file asking
        // for work factor 30 pins a core for minutes, and the dry run inflates uploads inside the
        // one process every tenant shares.
        let hostile = header_with_work_factor(&seal(b"one\n", &phrase()).unwrap(), 30);
        assert!(matches!(
            open(&hostile, &phrase(), MAX_DECOMPRESSED),
            Err(ArchiveError::WorkFactorTooHigh { found: 30, limit: MAX_WORK_FACTOR })
        ));
    }

    #[test]
    fn plaintext_sealing_round_trips_and_is_still_gzipped() {
        let out = seal(b"one\n", &Sealing::Plaintext).unwrap();
        assert_eq!(&out[..2], &[0x1f, 0x8b], "gzip magic");
        assert_eq!(open(&out, &Sealing::Plaintext, MAX_DECOMPRESSED).unwrap(), b"one\n");
    }

    /// Rewrites the work factor in the scrypt stanza and leaves the header MAC stale.
    ///
    /// Asking age's own encryptor for work factor 30 would run the stretch this guard exists to
    /// refuse, so the hostile file is forged rather than generated. age reads the stanza argument
    /// and compares it against the ceiling before it verifies the MAC, so the stale MAC does not
    /// hide the refusal behind a different error.
    fn header_with_work_factor(sealed: &[u8], log_n: u8) -> Vec<u8> {
        const STANZA: &[u8] = b"-> scrypt ";

        let start = sealed
            .windows(STANZA.len())
            .position(|window| window == STANZA)
            .expect("a passphrase archive carries one scrypt stanza");
        let end = start
            + sealed[start..]
                .iter()
                .position(|byte| *byte == b'\n')
                .expect("the stanza line ends in a newline");
        let line = std::str::from_utf8(&sealed[start..end]).expect("the age header is ASCII");
        let salt = line.rsplit(' ').nth(1).expect("the stanza carries a salt and a work factor");

        let mut forged = Vec::with_capacity(sealed.len());
        forged.extend_from_slice(&sealed[..start]);
        forged.extend_from_slice(format!("-> scrypt {salt} {log_n}").as_bytes());
        forged.extend_from_slice(&sealed[end..]);
        forged
    }
}
