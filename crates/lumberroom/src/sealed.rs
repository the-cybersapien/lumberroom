//! `seal` and `unseal`: the two commands where the key never leaves this machine.
//!
//! The server stores bytes it holds no key for. It cannot read a sealed item, cannot search one,
//! and cannot tell you what a namespace holds beyond a count. That property is worth nothing unless
//! this client and `bin/lumberroom.mjs` agree byte for byte, because an item sealed by one and
//! opened by the other is the whole point of having two clients.
//!
//! # The format, which is fixed by what is already stored
//!
//! Key: 32 random bytes, base64 with a trailing newline, at `LUMBERROOM_SEAL_KEY` or
//! `~/.config/lumberroom/seal-key`, mode 0600, generated on first use.
//!
//! Lookup name: `hex(HMAC-SHA256(key, namespace || 0x00 || name))`. The NUL separator is what stops
//! `"a" + "bc"` and `"ab" + "c"` colliding, and the HMAC is what stops the server enumerating a
//! namespace by guessing names.
//!
//! Blob: `base64(iv || ciphertext || tag)`, AES-256-GCM, 12-byte iv, 16-byte tag, no associated
//! data. AES-GCM's own output is standardised, so the only way the two clients can disagree is by
//! framing it differently, which is why the framing is spelled out here rather than inferred.
//!
//! # Why no AAD
//!
//! `bin/lumberroom.mjs` passes none, and binding the namespace or the key name as associated data
//! would be the better design on a blank sheet. Changing it now makes every item sealed by the
//! prototype unopenable, and this client exists to replace that prototype rather than to strand
//! what it wrote. A format change belongs with a version marker on the row, and `alg` is where it
//! would go.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use hmac::{Hmac, Mac};
use serde_json::json;
use sha2::Sha256;
use std::path::PathBuf;

use crate::args::Args;
use crate::client::{err, Client, Result};
use crate::config::{self, Env};
use crate::out;

const ALG: &str = "aes-256-gcm";
const IV_LEN: usize = 12;
const TAG_LEN: usize = 16;
const KEY_LEN: usize = 32;

/// Where the key lives. `LUMBERROOM_SEAL_KEY` names the file itself rather than a directory, which
/// is what `bin/lumberroom.mjs` does and what `scripts/policy-test.sh` relies on to hand one run a
/// key another run does not have.
pub fn seal_key_path(env: &dyn Env) -> PathBuf {
    if let Some(p) = env.get("LUMBERROOM_SEAL_KEY").filter(|s| !s.is_empty()) {
        return PathBuf::from(p);
    }
    let home = env.get("HOME").unwrap_or_default();
    PathBuf::from(home).join(".config").join("lumberroom").join("seal-key")
}

/// The key, generated on first use and never sent anywhere.
///
/// A short or malformed file is an error rather than a silent regeneration: overwriting a key
/// because it failed to parse would strand every item sealed under it, and a person who moved the
/// file wants to hear about it.
pub fn load_or_create_key(env: &dyn Env) -> Result<[u8; KEY_LEN]> {
    let path = seal_key_path(env);
    // Read, and only if that fails try to create. A second attempt to read follows a lost creation
    // race below, which is why this is a function rather than a match arm.
    if let Some(key) = read_key(&path)? {
        return Ok(key);
    }
    create_key(&path)
}

/// The key already on disk, or `None` when there is no file yet.
fn read_key(path: &std::path::Path) -> Result<Option<[u8; KEY_LEN]>> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let raw = B64.decode(text.trim()).map_err(|_| {
                err(format!(
                    "{} does not hold base64. Move it aside and a new key is written, but every \
                     item sealed under the old one stops opening.",
                    path.display()
                ))
            })?;
            let key: [u8; KEY_LEN] = raw.as_slice().try_into().map_err(|_| {
                err(format!(
                    "{} holds {} bytes and AES-256 takes {KEY_LEN}.",
                    path.display(),
                    raw.len()
                ))
            })?;
            Ok(Some(key))
        }
        Err(_) => Ok(None),
    }
}

/// A new key, created so that nothing else can read it and nothing else can lose to it silently.
///
/// Two properties, both learned the hard way in other people's key handling. The file is created
/// with mode 0600 rather than created and chmodded after, because between those two calls a 022
/// umask leaves a decryption key world readable. And it is created with `create_new`, so two `seal`
/// runs starting at once cannot each generate a key and have the loser's write replace the key the
/// winner already sealed a row under. That row would never open again, and nothing would report it.
fn create_key(path: &std::path::Path) -> Result<[u8; KEY_LEN]> {
    let mut key = [0u8; KEY_LEN];
    getrandom::fill(&mut key)
        .map_err(|e| err(format!("no randomness available for a new seal key: {e}")))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| err(format!("could not create {}: {e}", dir.display())))?;
    }

    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }

    match opts.open(path) {
        Ok(mut file) => {
            use std::io::Write;
            file.write_all(format!("{}\n", B64.encode(key)).as_bytes())
                .map_err(|e| err(format!("could not write {}: {e}", path.display())))?;
            // Windows has no mode on open, and a key written there is protected by the profile
            // directory rather than by this call.
            let _ = config::restrict(path);
            Ok(key)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // Another process created it between the read and here. Its key is the real one.
            read_key(path)?.ok_or_else(|| {
                err(format!("{} appeared and then could not be read", path.display()))
            })
        }
        Err(e) => Err(err(format!("could not create {}: {e}", path.display()))),
    }
}

/// The name the server files this item under, which tells it nothing about the name you used.
pub fn key_hmac(key: &[u8; KEY_LEN], namespace: &str, name: &str) -> String {
    let mut mac =
        <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC takes a key of any length");
    mac.update(namespace.as_bytes());
    mac.update(&[0u8]);
    mac.update(name.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

fn encrypt(key: &[u8; KEY_LEN], plaintext: &[u8]) -> Result<String> {
    let mut iv = [0u8; IV_LEN];
    getrandom::fill(&mut iv)
        .map_err(|e| err(format!("no randomness available for a nonce: {e}")))?;
    let cipher = Aes256Gcm::new(key.into());
    let ct = cipher
        .encrypt(&Nonce::from(iv), Payload { msg: plaintext, aad: b"" })
        .map_err(|_| err("the value could not be encrypted"))?;
    let mut blob = Vec::with_capacity(IV_LEN + ct.len());
    blob.extend_from_slice(&iv);
    blob.extend_from_slice(&ct);
    Ok(B64.encode(blob))
}

/// `aes-gcm` returns ciphertext with the tag already appended, which is the same layout the node
/// client builds by hand from `getAuthTag()`. Splitting the iv off the front is all that is left.
fn decrypt(key: &[u8; KEY_LEN], blob: &str) -> Result<String> {
    let raw = B64
        .decode(blob.trim())
        .map_err(|_| err("the stored ciphertext is not base64, so this row was not written by a lumberroom client"))?;
    if raw.len() < IV_LEN + TAG_LEN {
        return Err(err(format!(
            "the stored ciphertext is {} bytes, too short to hold a {IV_LEN} byte nonce and a \
             {TAG_LEN} byte tag",
            raw.len()
        )));
    }
    let (iv, rest) = raw.split_at(IV_LEN);
    let iv: [u8; IV_LEN] = iv.try_into().expect("split_at gave exactly IV_LEN bytes");
    let cipher = Aes256Gcm::new(key.into());
    let plain =
        cipher.decrypt(&Nonce::from(iv), Payload { msg: rest, aad: b"" }).map_err(|_| {
            err("this key does not open that item. A sealed item is bound to the key that wrote \
                 it, so a different machine needs the same seal-key file.")
        })?;
    String::from_utf8(plain).map_err(|_| {
        err("the item opened but does not hold text, and this command only prints text")
    })
}

fn namespace_of(args: &Args) -> Result<String> {
    args.value("namespace")
        .or_else(|| args.value("ns"))
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| err("--namespace is required"))
}

pub async fn seal(c: &Client, args: &Args, env: &dyn Env) -> Result<()> {
    let Some(name) = args.positional_at(1) else {
        return Err(err(
            "usage: lumberroom seal <key> --namespace ns [--value \"...\"] (reads stdin if --value is absent)",
        ));
    };
    let namespace = namespace_of(args)?;

    let value = match args.value("value") {
        Some(v) => v.to_string(),
        None => {
            let mut buf = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)
                .map_err(|e| err(format!("could not read stdin: {e}")))?;
            if buf.trim().is_empty() {
                return Err(err("no value on stdin and no --value given"));
            }
            buf
        }
    };

    let key = load_or_create_key(env)?;
    let body = json!({
        "namespace": namespace,
        "key_hmac": key_hmac(&key, &namespace, name),
        "ciphertext": encrypt(&key, value.as_bytes())?,
        "alg": ALG,
        "source_client": c.cfg.invocation.as_str(),
    });

    let (status, response) =
        c.http_request(reqwest::Method::PUT, "/admin/sealed", Some(body)).await?;
    if status != 200 {
        return Err(err(format!("seal failed ({status}): {response}")));
    }
    out(&format!("sealed {name} in {namespace} (the server never saw the plaintext)"));
    Ok(())
}

pub async fn unseal(c: &Client, args: &Args, env: &dyn Env) -> Result<()> {
    let Some(name) = args.positional_at(1) else {
        return Err(err("usage: lumberroom unseal <key> --namespace ns"));
    };
    let namespace = namespace_of(args)?;

    let key = load_or_create_key(env)?;
    let path = format!(
        "/admin/sealed?namespace={}&key_hmac={}",
        crate::commands::urlencode(&namespace),
        key_hmac(&key, &namespace, name)
    );
    let (status, body) = c.http_get(&path).await?;
    if status == 404 {
        return Err(err(format!("nothing sealed at {namespace}/{name}")));
    }
    if status != 200 {
        return Err(err(format!("unseal lookup failed ({status}): {body}")));
    }

    let alg = body.get("alg").and_then(|v| v.as_str()).unwrap_or_default();
    if alg != ALG {
        return Err(err(format!("unsupported alg {alg}; this client only unseals {ALG}")));
    }
    let blob = body
        .get("ciphertext")
        .and_then(|v| v.as_str())
        .ok_or_else(|| err("the server returned a row with no ciphertext"))?;

    out(&decrypt(&key, blob)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeEnv(Vec<(String, String)>);

    impl Env for FakeEnv {
        fn get(&self, key: &str) -> Option<String> {
            self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
        }
    }

    fn env(pairs: &[(&str, &str)]) -> FakeEnv {
        FakeEnv(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())
    }

    /// The vector that pins the wire format.
    ///
    /// Produced by `bin/lumberroom.mjs`'s own primitives: an all-zero key, so the value is
    /// reproducible by anyone reading this test, and the name and namespace the policy gate uses.
    /// If this assertion ever fails, every item sealed by the node client has stopped being
    /// findable, whatever else still passes.
    #[test]
    fn the_lookup_name_matches_the_node_client_byte_for_byte() {
        let key = [0u8; KEY_LEN];
        assert_eq!(
            key_hmac(&key, "credentials:aws", "deploy-key"),
            // Computed with the node client's own primitive, NUL being a zero byte:
            //   createHmac('sha256', Buffer.alloc(32))
            //     .update('credentials:aws' + String.fromCharCode(0) + 'deploy-key')
            //     .digest('hex')
            "680f25ebbcfb03b886c492ec47d91b74281f4b74a40842bf4fc9fcf6d0516ded"
        );
    }

    #[test]
    fn the_nul_separator_keeps_two_names_from_colliding() {
        let key = [7u8; KEY_LEN];
        assert_ne!(key_hmac(&key, "a", "bc"), key_hmac(&key, "ab", "c"));
    }

    #[test]
    fn a_sealed_value_comes_back_out() {
        let key = [3u8; KEY_LEN];
        let blob = encrypt(&key, b"the deploy key").unwrap();
        assert_eq!(decrypt(&key, &blob).unwrap(), "the deploy key");
    }

    /// The framing, pinned against the other client rather than against itself.
    ///
    /// A round trip through one implementation proves nothing about interoperability: this client
    /// and `bin/lumberroom.mjs` could each be internally consistent and still disagree about where
    /// the nonce ends and the tag begins, and the owner would find out when an item sealed on one
    /// machine refused to open on another. This blob was produced by the node client's own
    /// primitives, with a key of thirty-two 0x03 bytes:
    ///
    ///   const c = createCipheriv('aes-256-gcm', Buffer.alloc(32, 3), iv);
    ///   Buffer.concat([iv, c.update('the deploy key'), c.final(), c.getAuthTag()]).toString('base64')
    #[test]
    fn a_blob_sealed_by_the_node_client_opens_here() {
        let blob = "qox64ildai2p/rjMRD/kMsyGeGpKI+hkR+PWE+VZ+oviAeDiNNPmR7sy";
        assert_eq!(decrypt(&[3u8; KEY_LEN], blob).unwrap(), "the deploy key");
    }

    #[test]
    fn every_seal_uses_a_fresh_nonce() {
        let key = [3u8; KEY_LEN];
        assert_ne!(encrypt(&key, b"same").unwrap(), encrypt(&key, b"same").unwrap());
    }

    #[test]
    fn another_key_does_not_open_it() {
        let blob = encrypt(&[3u8; KEY_LEN], b"the deploy key").unwrap();
        let opened = decrypt(&[4u8; KEY_LEN], &blob);
        assert!(opened.is_err(), "a different key opened a sealed item");
    }

    #[test]
    fn a_truncated_blob_is_refused_rather_than_panicking() {
        let key = [3u8; KEY_LEN];
        assert!(decrypt(&key, &B64.encode([0u8; 8])).is_err());
    }

    #[test]
    fn a_new_key_file_is_owner_only_from_the_moment_it_exists() {
        let dir = std::env::temp_dir().join(format!("lumberroom-seal-{}", std::process::id()));
        let path = dir.join("seal-key");
        let _ = std::fs::remove_dir_all(&dir);
        let e = env(&[("LUMBERROOM_SEAL_KEY", path.to_str().unwrap())]);

        let first = load_or_create_key(&e).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "a decryption key was readable by other accounts");
        }

        // A second call reads rather than regenerates. Regenerating would strand every row the
        // first key sealed.
        let second = load_or_create_key(&e).unwrap();
        assert_eq!(first, second);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_env_names_the_key_file_itself() {
        let e = env(&[("LUMBERROOM_SEAL_KEY", "/tmp/k"), ("HOME", "/home/o")]);
        assert_eq!(seal_key_path(&e), PathBuf::from("/tmp/k"));
        let e = env(&[("HOME", "/home/o")]);
        assert_eq!(seal_key_path(&e), PathBuf::from("/home/o/.config/lumberroom/seal-key"));
    }
}
