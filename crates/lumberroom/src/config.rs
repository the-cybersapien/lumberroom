//! Credential resolution, and the config file both clients share.
//!
//! `~/.config/lumberroom/config.json` is written by `bin/lumberroom.mjs` and by `wire-mac.sh`, and this client
//! reads and writes the same file. Node's `saveConfig` is a shallow merge over whatever it parsed,
//! so it preserves keys it knows nothing about. A typed struct that serialised only its own fields
//! would delete the other client's data on the first `login`, so the file is carried as a
//! `serde_json::Value` and patched at the top level.

use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const DEFAULT_URL: &str = "http://127.0.0.1:8787";
pub const DEFAULT_TIMEOUT_MS: u64 = 15_000;

/// Environment lookup, injected rather than read from the process.
///
/// Tests set precedence cases in parallel and `std::env::set_var` is process-wide, so a test that
/// mutated it would race every other test in the binary.
pub trait Env {
    fn get(&self, key: &str) -> Option<String>;
}

pub struct ProcessEnv;

impl Env for ProcessEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

impl Env for HashMap<String, String> {
    fn get(&self, key: &str) -> Option<String> {
        HashMap::get(self, key).cloned()
    }
}

/// The file, kept whole. `value` is whatever parsed, or an empty object when the file is absent or
/// unreadable: node swallows both cases the same way, and a missing config is the normal first run.
#[derive(Debug, Clone)]
pub struct FileConfig {
    pub path: PathBuf,
    pub value: Value,
}

impl FileConfig {
    pub fn load(path: PathBuf) -> Self {
        let value = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}));
        Self { path, value }
    }

    pub fn empty(path: PathBuf) -> Self {
        Self { path, value: json!({}) }
    }

    pub fn str_field(&self, key: &str) -> Option<&str> {
        self.value.get(key)?.as_str().filter(|s| !s.is_empty())
    }

    pub fn oauth(&self, key: &str) -> Option<&str> {
        self.value.get("oauth")?.get(key)?.as_str().filter(|s| !s.is_empty())
    }

    pub fn has_oauth_access_token(&self) -> bool {
        self.oauth("access_token").is_some()
    }

    /// Merge a top-level patch and write the file back at 0600.
    ///
    /// Two chmods' worth of care, for the reason node states: the create-mode argument is ignored
    /// for a file that already exists, so an existing config keeps whatever bits it had unless the
    /// permissions are set again after the write.
    pub fn save(&mut self, patch: Map<String, Value>) -> std::io::Result<()> {
        let obj = self.value.as_object_mut().expect("config value is an object by construction");
        for (k, v) in patch {
            obj.insert(k, v);
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = format!("{}\n", serde_json::to_string_pretty(&self.value)?);
        std::fs::write(&self.path, body)?;
        restrict(&self.path)
    }
}

/// Owner-only, on a file holding a bearer token.
#[cfg(unix)]
pub fn restrict(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
pub fn restrict(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

fn nonempty(v: Option<String>) -> Option<String> {
    v.filter(|s| !s.is_empty())
}

pub fn config_path(env: &dyn Env) -> PathBuf {
    if let Some(p) = env.get("LUMBERROOM_CONFIG").filter(|s| !s.is_empty()) {
        return PathBuf::from(p);
    }
    let home = env.get("HOME").unwrap_or_default();
    PathBuf::from(home).join(".config").join("lumberroom").join("config.json")
}

/// Which credential a run is using, for the line `doctor` prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Credential {
    Static,
    Oauth,
    None,
}

#[derive(Debug, Clone)]
pub struct Resolved {
    /// Trailing slashes stripped, exactly as node does before deriving the other two.
    pub base_url: String,
    /// Where the MCP transport is mounted.
    pub mcp_url: String,
    /// Everything else hangs off here: /healthz, /admin/*, /oauth/*.
    pub http_base: String,
    pub token: String,
    pub credential: Credential,
    pub invocation: String,
    pub timeout_ms: u64,
}

/// Flags beat environment beats file, and a static token beats an OAuth access token.
///
/// The static-over-oauth order is not a preference, it is what keeps a mode switch honest:
/// `wire-mac.sh --token-mode` writes `token` and never touches `oauth`, `--oauth-mode` does the
/// reverse, and a leftover `token` from an earlier mode would otherwise defeat a credential
/// `login` had just minted while looking like a server fault.
pub fn resolve(
    env: &dyn Env,
    file: &FileConfig,
    url_flag: Option<&str>,
    token_flag: Option<&str>,
    invocation_flag: Option<&str>,
    hook: bool,
    timeout_flag: Option<&str>,
) -> Resolved {
    // An empty value counts as unset at every step, which is the one place this client does not
    // copy node. Node's `??` chain stops on an empty string, so `LUMBERROOM_TOKEN=` there yields no
    // credential at all and `LUMBERROOM_URL=` yields the endpoint `/mcp`. An operator who leaves a blank
    // variable in a compose file means "not set", and reading it as "set to nothing" turns a
    // configuration slip into a request against a URL that cannot resolve.
    let base_url = nonempty(url_flag.map(str::to_string))
        .or_else(|| nonempty(env.get("LUMBERROOM_URL")))
        .or_else(|| file.str_field("url").map(str::to_string))
        .unwrap_or_else(|| DEFAULT_URL.to_string());
    let base_url = base_url.trim_end_matches('/').to_string();

    let mut credential = Credential::Static;
    let token = nonempty(token_flag.map(str::to_string))
        .or_else(|| nonempty(env.get("LUMBERROOM_TOKEN")))
        .or_else(|| file.str_field("token").map(str::to_string))
        .or_else(|| {
            credential = Credential::Oauth;
            file.oauth("access_token").map(str::to_string)
        })
        .unwrap_or_else(|| {
            credential = Credential::None;
            String::new()
        });

    let mcp_url = if base_url.ends_with("/mcp") {
        base_url.clone()
    } else {
        format!("{base_url}/mcp")
    };
    let http_base = base_url.strip_suffix("/mcp").unwrap_or(&base_url).to_string();

    let invocation = invocation_flag
        .map(str::to_string)
        .unwrap_or_else(|| if hook { "hook".to_string() } else { "cli".to_string() });

    let timeout_ms = nonempty(timeout_flag.map(str::to_string))
        .or_else(|| nonempty(env.get("LUMBERROOM_TIMEOUT_MS")))
        .and_then(|v| crate::args::parse_int_prefix(&v))
        .filter(|n| *n > 0)
        .map(|n| n as u64)
        .unwrap_or(DEFAULT_TIMEOUT_MS);

    Resolved { base_url, mcp_url, http_base, token, credential, invocation, timeout_ms }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn file(value: Value) -> FileConfig {
        FileConfig { path: PathBuf::from("/dev/null"), value }
    }

    #[test]
    fn flag_beats_env_beats_file() {
        let e = env(&[("LUMBERROOM_URL", "https://env.example"), ("LUMBERROOM_TOKEN", "env-token")]);
        let f = file(json!({ "url": "https://file.example", "token": "file-token" }));

        let r = resolve(&e, &f, Some("https://flag.example"), Some("flag-token"), None, false, None);
        assert_eq!(r.base_url, "https://flag.example");
        assert_eq!(r.token, "flag-token");

        let r = resolve(&e, &f, None, None, None, false, None);
        assert_eq!(r.base_url, "https://env.example");
        assert_eq!(r.token, "env-token");

        let r = resolve(&HashMap::new(), &f, None, None, None, false, None);
        assert_eq!(r.base_url, "https://file.example");
        assert_eq!(r.token, "file-token");

        let r = resolve(&HashMap::new(), &file(json!({})), None, None, None, false, None);
        assert_eq!(r.base_url, DEFAULT_URL);
        assert_eq!(r.token, "");
        assert_eq!(r.credential, Credential::None);
    }

    #[test]
    fn a_static_token_beats_an_oauth_access_token() {
        let f = file(json!({ "token": "static", "oauth": { "access_token": "oauth" } }));
        let r = resolve(&HashMap::new(), &f, None, None, None, false, None);
        assert_eq!(r.token, "static");
        assert_eq!(r.credential, Credential::Static);
    }

    #[test]
    fn oauth_is_used_when_no_static_token_exists() {
        let f = file(json!({ "oauth": { "access_token": "oauth" } }));
        let r = resolve(&HashMap::new(), &f, None, None, None, false, None);
        assert_eq!(r.token, "oauth");
        assert_eq!(r.credential, Credential::Oauth);
    }

    #[test]
    fn trailing_slashes_go_and_mcp_is_not_doubled() {
        let r = resolve(&HashMap::new(), &file(json!({})), Some("https://s.example///"), None, None, false, None);
        assert_eq!(r.base_url, "https://s.example");
        assert_eq!(r.mcp_url, "https://s.example/mcp");
        assert_eq!(r.http_base, "https://s.example");

        let r = resolve(&HashMap::new(), &file(json!({})), Some("https://s.example/mcp"), None, None, false, None);
        assert_eq!(r.mcp_url, "https://s.example/mcp");
        assert_eq!(r.http_base, "https://s.example");
    }

    #[test]
    fn an_empty_env_value_does_not_shadow_the_file() {
        let e = env(&[("LUMBERROOM_TOKEN", "")]);
        let f = file(json!({ "token": "file-token" }));
        assert_eq!(resolve(&e, &f, None, None, None, false, None).token, "file-token");
    }

    #[test]
    fn invocation_marks_hook_runs() {
        let f = file(json!({}));
        assert_eq!(resolve(&HashMap::new(), &f, None, None, None, false, None).invocation, "cli");
        assert_eq!(resolve(&HashMap::new(), &f, None, None, None, true, None).invocation, "hook");
        assert_eq!(
            resolve(&HashMap::new(), &f, None, None, Some("openwebui"), true, None).invocation,
            "openwebui"
        );
    }

    #[test]
    fn timeout_comes_from_the_flag_then_the_environment() {
        let e = env(&[("LUMBERROOM_TIMEOUT_MS", "5000")]);
        let f = file(json!({}));
        assert_eq!(resolve(&e, &f, None, None, None, false, None).timeout_ms, 5000);
        assert_eq!(resolve(&e, &f, None, None, None, false, Some("900")).timeout_ms, 900);
        assert_eq!(resolve(&HashMap::new(), &f, None, None, None, false, None).timeout_ms, DEFAULT_TIMEOUT_MS);
    }

    #[test]
    fn config_path_honours_lumberroom_config_then_home() {
        let e = env(&[("LUMBERROOM_CONFIG", "/tmp/x.json"), ("HOME", "/home/o")]);
        assert_eq!(config_path(&e), PathBuf::from("/tmp/x.json"));
        let e = env(&[("HOME", "/home/o")]);
        assert_eq!(config_path(&e), PathBuf::from("/home/o/.config/lumberroom/config.json"));
    }

    #[test]
    fn save_keeps_keys_this_client_does_not_know() {
        let dir = std::env::temp_dir().join(format!("lumberroom-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"url":"https://s.example","somethingElse":{"a":1}}"#).unwrap();

        let mut cfg = FileConfig::load(path.clone());
        let mut patch = Map::new();
        patch.insert("oauth".into(), json!({ "access_token": "t" }));
        cfg.save(patch).unwrap();

        let back: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(back["somethingElse"]["a"], json!(1));
        assert_eq!(back["url"], json!("https://s.example"));
        assert_eq!(back["oauth"]["access_token"], json!("t"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_corrupt_config_file_reads_as_empty() {
        let dir = std::env::temp_dir().join(format!("lumberroom-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, "not json at all").unwrap();
        let cfg = FileConfig::load(path);
        assert_eq!(cfg.value, json!({}));
        std::fs::remove_dir_all(&dir).ok();
    }
}
