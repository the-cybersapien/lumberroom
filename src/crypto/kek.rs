//! Where the key-encryption key comes from, and how one KEK is told apart from another.
//!
//! Two local providers ship: a file and an environment variable. Both are honest about defending a
//! stolen dump rather than a stolen disk; the module docs in `super` state the limit in full. A KMS
//! implementation slots in behind the same trait without touching a caller.
//!
//! Every failure here is `Internal` rather than `Validation`, which departs from the convention in
//! `config.rs`. A KEK path, a file mode and an environment variable name describe the server's key
//! storage, and no client should learn any of it from an error. `Internal` keeps the detail in the
//! log and in `Display`, where the operator reads it at boot, and shows the client nothing.

use async_trait::async_trait;
use base64::Engine as _;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::domain::errors::{DomainError, Result};

/// 32 bytes, zeroized on drop. Never logged, never serialised, never written to disk by this
/// process.
pub type Kek = Zeroizing<[u8; 32]>;

const KEK_LEN: usize = 32;
/// Hex characters in an encoded 256-bit key.
const HEX_LEN: usize = 64;
/// Standard padded base64 characters in an encoded 256-bit key.
const B64_LEN: usize = 44;

/// The label [`fingerprint`] runs the HMAC over. Frozen: the fingerprint of a live key is stored in
/// `kek_state` and compared at every boot, so changing this label would make every deployment look
/// like it had rotated its key. It still carries the product's old name for that reason: the rename
/// to lumberroom swept this constant once and the next boot refused every private write.
const FINGERPRINT_LABEL: &[u8] = b"sutr/kek-fingerprint/v1";

#[async_trait]
pub trait KeyProvider: Send + Sync {
    /// Stable name recorded on every encrypted row, so a rotation is distinguishable from
    /// data loss.
    fn kek_id(&self) -> String;
    fn provider(&self) -> &'static str;
    async fn kek(&self) -> Result<Kek>;
}

/// KEK read from a file holding 64 hex characters or 44 base64 characters. Refuses a file that
/// is group or world readable, because a key with the wrong mode is a key that is already
/// somewhere else.
pub struct FileKeyProvider {
    path: String,
    kek_id: String,
}

impl FileKeyProvider {
    pub fn new(path: impl Into<String>, kek_id: impl Into<String>) -> Self {
        Self { path: path.into(), kek_id: kek_id.into() }
    }
}

#[async_trait]
impl KeyProvider for FileKeyProvider {
    fn kek_id(&self) -> String {
        self.kek_id.clone()
    }

    fn provider(&self) -> &'static str {
        "file"
    }

    async fn kek(&self) -> Result<Kek> {
        refuse_loose_permissions(&self.path)?;

        // Blocking I/O in an async fn, on purpose. This reads a 64-byte file once at boot, and
        // moving that to a blocking pool would cost more than the read does.
        let raw = Zeroizing::new(std::fs::read_to_string(&self.path).map_err(|e| {
            DomainError::internal(format!("cannot read KEK file {}: {e}", self.path))
        })?);
        decode_key(&raw)
    }
}

/// KEK from an environment variable. Convenient for compose, and weaker: the environment of a
/// container is readable by anything that can inspect the container.
pub struct EnvKeyProvider {
    var: String,
    kek_id: String,
}

impl EnvKeyProvider {
    pub fn new(var: impl Into<String>, kek_id: impl Into<String>) -> Self {
        Self { var: var.into(), kek_id: kek_id.into() }
    }
}

#[async_trait]
impl KeyProvider for EnvKeyProvider {
    fn kek_id(&self) -> String {
        self.kek_id.clone()
    }

    fn provider(&self) -> &'static str {
        "env"
    }

    async fn kek(&self) -> Result<Kek> {
        let raw = Zeroizing::new(
            std::env::var(&self.var)
                .map_err(|_| DomainError::internal(format!("{} is not set", self.var)))?,
        );
        decode_key(&raw)
    }
}

/// Identifies a key without helping recover it: HMAC-SHA256 of a fixed label under the KEK,
/// hex, truncated to 32 characters.
pub fn fingerprint(kek: &Kek) -> String {
    // HMAC accepts a key of any length, so this construction cannot fail for a 32-byte key.
    let mut mac =
        Hmac::<Sha256>::new_from_slice(&kek[..]).expect("HMAC-SHA256 takes a key of any length");
    mac.update(FINGERPRINT_LABEL);
    // 128 bits of the tag. Enough to notice a swapped key, and truncation costs nothing here
    // because the value is compared for equality and never used as a secret.
    hex::encode(mac.finalize().into_bytes())[..32].to_string()
}

/// Generate a fresh KEK for an operator to store. Returns hex.
pub fn generate_kek_hex() -> Result<String> {
    let mut bytes = Zeroizing::new([0u8; KEK_LEN]);
    super::fill_random(&mut bytes[..])?;
    // The returned String is the key. The caller prints it once for the operator to store and does
    // not log it, put it in an error, or write it anywhere this process controls.
    Ok(hex::encode(&bytes[..]))
}

/// Accepts exactly the two encodings [`FileKeyProvider`] promises and nothing else.
///
/// The encoded form is the key, so the decoded buffer is zeroized whichever way this returns. The
/// caller owns the zeroizing of the input it read.
fn decode_key(raw: &str) -> Result<Kek> {
    let trimmed = raw.trim();
    let decoded: Zeroizing<Vec<u8>> = match trimmed.len() {
        HEX_LEN => Zeroizing::new(hex::decode(trimmed).map_err(|_| unusable_key_material())?),
        B64_LEN => Zeroizing::new(
            base64::engine::general_purpose::STANDARD
                .decode(trimmed)
                .map_err(|_| unusable_key_material())?,
        ),
        _ => return Err(unusable_key_material()),
    };

    // Reachable: 44 unpadded base64 characters decode to 33 bytes, so length of the input does not
    // imply length of the key.
    if decoded.len() != KEK_LEN {
        return Err(unusable_key_material());
    }

    let mut kek: Kek = Zeroizing::new([0u8; KEK_LEN]);
    kek.copy_from_slice(&decoded);
    Ok(kek)
}

/// Names the accepted shapes and never echoes the input, which is key material even when it is the
/// wrong length.
fn unusable_key_material() -> DomainError {
    DomainError::internal(
        "KEK is not a 256-bit key. Expected 64 hex characters or 44 base64 characters. \
         Generate one with `lumberroom keygen`.",
    )
}

/// A key file readable by group or other is a key that is already somewhere else. This refuses
/// rather than warns: the fix is one `chmod`, and a warning in a log nobody reads is how a bad mode
/// survives to the next incident.
#[cfg(unix)]
fn refuse_loose_permissions(path: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let meta = std::fs::metadata(path)
        .map_err(|e| DomainError::internal(format!("cannot stat KEK file {path}: {e}")))?;
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(DomainError::internal(format!(
            "KEK file {path} has mode {mode:04o} and must not be readable by group or other. \
             Run: chmod 600 {path}"
        )));
    }
    Ok(())
}

/// Windows has no mode bits to inspect and is not a deployment target. The file provider still
/// works there, with one guarantee fewer.
#[cfg(not(unix))]
fn refuse_loose_permissions(_path: &str) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX_KEY: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

    fn temp_path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("lumberroom-kek-{}.key", uuid::Uuid::new_v4()))
    }

    /// The mode is explicit because the default umask writes 644, which this provider refuses. A
    /// test that expects a successful read has to ask for 600.
    fn write_key_file(contents: &str, mode: u32) -> std::path::PathBuf {
        let path = temp_path();
        std::fs::write(&path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        let _ = mode;
        path
    }

    #[test]
    fn a_hex_encoded_key_decodes_to_thirty_two_bytes() {
        let kek = decode_key(HEX_KEY).unwrap();
        assert_eq!(kek[0], 0x00);
        assert_eq!(kek[31], 0x1f);
    }

    #[test]
    fn a_base64_encoded_key_decodes_to_the_same_bytes() {
        let bytes = hex::decode(HEX_KEY).unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        assert_eq!(b64.len(), B64_LEN, "a 256-bit key is 44 padded base64 characters");
        assert_eq!(decode_key(&b64).unwrap()[..], bytes[..]);
    }

    #[test]
    fn surrounding_whitespace_from_an_echo_is_ignored() {
        let padded = decode_key(&format!("  {HEX_KEY}\n")).unwrap();
        assert_eq!(padded[..], decode_key(HEX_KEY).unwrap()[..]);
    }

    #[test]
    fn a_key_of_the_wrong_length_is_refused() {
        // 128 bits of hex, 33 bytes of unpadded base64, empty, and a passphrase somebody typed.
        for bad in [
            "000102030405060708090a0b0c0d0e0f",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "",
            "correct horse battery staple",
        ] {
            assert!(decode_key(bad).is_err(), "should have refused {bad:?}");
        }
    }

    #[test]
    fn a_key_of_the_right_length_that_is_not_hex_is_refused() {
        assert!(decode_key(&"z".repeat(HEX_LEN)).is_err());
    }

    #[test]
    fn an_error_about_a_key_never_contains_the_key() {
        let err = decode_key(&format!("{HEX_KEY}extra")).unwrap_err();
        assert!(!err.log_message().contains("000102"), "the message must not echo key material");
    }

    #[test]
    fn a_fingerprint_is_stable_for_one_key() {
        let a = decode_key(HEX_KEY).unwrap();
        let b = decode_key(HEX_KEY).unwrap();
        assert_eq!(fingerprint(&a), fingerprint(&b));
        assert_eq!(fingerprint(&a).len(), 32);
    }

    #[test]
    fn a_fingerprint_differs_across_keys() {
        let a: Kek = Zeroizing::new([1u8; KEK_LEN]);
        let mut second = [1u8; KEK_LEN];
        second[31] = 2;
        let b: Kek = Zeroizing::new(second);
        assert_ne!(fingerprint(&a), fingerprint(&b), "one flipped bit must change the fingerprint");
    }

    #[test]
    fn a_generated_key_is_hex_and_decodes_back() {
        let hex_key = generate_kek_hex().unwrap();
        assert_eq!(hex_key.len(), HEX_LEN);
        assert!(decode_key(&hex_key).is_ok());
        assert_ne!(hex_key, generate_kek_hex().unwrap(), "each call is a fresh key");
    }

    #[tokio::test]
    async fn the_file_provider_reads_a_locked_down_key_file() {
        let path = write_key_file(HEX_KEY, 0o600);
        let provider = FileKeyProvider::new(path.to_string_lossy().to_string(), "kek-1");

        assert_eq!(provider.provider(), "file");
        assert_eq!(provider.kek_id(), "kek-1");
        assert_eq!(provider.kek().await.unwrap()[..], decode_key(HEX_KEY).unwrap()[..]);

        std::fs::remove_file(&path).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_file_provider_refuses_a_group_readable_key_file() {
        for mode in [0o640, 0o644, 0o604] {
            let path = write_key_file(HEX_KEY, mode);
            let provider = FileKeyProvider::new(path.to_string_lossy().to_string(), "kek-1");
            let err = provider.kek().await.unwrap_err();
            assert!(
                err.log_message().contains("chmod"),
                "mode {mode:o} should have been refused with the fix"
            );
            std::fs::remove_file(&path).unwrap();
        }
    }

    #[tokio::test]
    async fn a_missing_key_file_is_an_error_and_not_an_empty_key() {
        let provider = FileKeyProvider::new(temp_path().to_string_lossy().to_string(), "kek-1");
        assert!(provider.kek().await.is_err());
    }

    #[tokio::test]
    async fn the_env_provider_reads_its_variable() {
        // A unique name per test, because the environment is process-global and the suite is
        // threaded.
        let var = format!("LUMBERROOM_TEST_KEK_{}", uuid::Uuid::new_v4().simple());
        std::env::set_var(&var, HEX_KEY);

        let provider = EnvKeyProvider::new(var.clone(), "kek-env");
        assert_eq!(provider.provider(), "env");
        assert_eq!(provider.kek().await.unwrap()[..], decode_key(HEX_KEY).unwrap()[..]);

        std::env::remove_var(&var);
        assert!(provider.kek().await.is_err(), "an unset variable is a failure, not a zero key");
    }

    #[tokio::test]
    async fn a_provider_error_never_names_the_key_it_failed_to_read() {
        let var = format!("LUMBERROOM_TEST_KEK_{}", uuid::Uuid::new_v4().simple());
        std::env::set_var(&var, "not-a-key");
        let err = EnvKeyProvider::new(var.clone(), "kek-env").kek().await.unwrap_err();
        std::env::remove_var(&var);
        assert!(!err.log_message().contains("not-a-key"));
    }
}
