//! The credential tripwire. A textual pass over content on its way in, refusing anything that
//! looks like a live credential when the write would land at `open` (Phase 3 spec §2).
//!
//! Inference by namespace default gets classification right almost always, and wrong in one
//! expensive direction: a pasted API key in `user:me` becomes a plaintext, lexically indexed,
//! world-readable row. This module is the backstop for that case. It catches obvious shapes and
//! misses prose that happens to be sensitive, which is what the namespace defaults are for.
//!
//! Textual only. No extraction, no model, nothing that talks to a network, because the standing
//! constraint is that there is no model in the write path.
//!
//! **False positives are the risk that matters.** This store legitimately holds UUIDs (every
//! memory id is one), git SHAs, content hashes, base64 of images, file paths and prose. A tripwire
//! that fires on a memory id is a tripwire the owner switches off, which is strictly worse than
//! not having one. Every rule here is therefore tuned to be quiet: the known-prefix rules demand
//! the full shape of a live key rather than the prefix alone, and the generic entropy rule demands
//! length, charset, mixed case, per-character entropy above what hex can reach, and absence from a
//! pasted binary blob, all at once.
//!
//! That last demand left one hole, and it was this system's own. Every secret lumberroom mints is
//! `openssl rand -hex`: the client tokens, the Postgres password, the KEK. Hex has no upper case,
//! so the entropy rule cannot fire on any of them by construction, and a client token pasted into
//! `user:me` stored at open. The hex rule below anchors on a credential word beside the run
//! rather than on the run itself, which is what lets a git SHA and a `sha256` line stay quiet.

use crate::domain::errors::DomainError;

/// What fired, and enough detail for the caller to fix the write without guessing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Stable short name of the rule, safe to log and to put in an error message.
    pub rule: &'static str,
    /// Human-readable explanation. MUST NOT contain the matched secret itself.
    pub detail: String,
}

impl Finding {
    fn new(rule: &'static str, detail: String) -> Self {
        Self { rule, detail }
    }

    /// The refusal a write at `open` gets. `Validation` rather than `Forbidden` because the caller
    /// can fix it by sending the same content at `sealed`, and the message says so.
    ///
    /// Composed here rather than at the call site so the "names the rule, suggests `sealed`, never
    /// echoes the secret" requirement is testable in the module that owns it.
    pub fn refusal(&self) -> DomainError {
        DomainError::validation(format!(
            "refusing to store this at 'open': {} (rule {}). Store it at 'sealed' instead, or \
             remove the credential and write the rest.",
            self.detail, self.rule
        ))
    }
}

/// None when the content carries nothing credential-shaped.
///
/// Rules run in a fixed priority order and the first hit wins: private key header, connection
/// string, known prefix, JWT, hex beside a credential word, generic entropy. Content carrying two
/// shapes reports one Finding by design, because the caller's next move is the same either way
/// and a list invites a caller to fix the first item and retry.
pub fn scan(content: &str) -> Option<Finding> {
    let bytes = content.as_bytes();
    scan_private_key_header(bytes)
        .or_else(|| scan_connection_string(bytes))
        .or_else(|| scan_tokens(bytes))
}

// ---------------------------------------------------------------------------------------------
// Private key headers
// ---------------------------------------------------------------------------------------------

/// The header line alone is the signal. Matching on `PRIVATE KEY` inside a `-----BEGIN` line
/// covers RSA, EC, DSA, OPENSSH, PGP, PKCS#8 and anything a future tool invents, while leaving
/// `-----BEGIN CERTIFICATE-----` and `-----BEGIN PUBLIC KEY-----` alone. Both of those are
/// publishable and both plausibly live in this store.
fn scan_private_key_header(bytes: &[u8]) -> Option<Finding> {
    const BEGIN: &[u8] = b"-----BEGIN ";
    let mut at = 0;
    while let Some(offset) = find(&bytes[at..], BEGIN) {
        let start = at + offset;
        let line_end =
            bytes[start..].iter().position(|&b| b == b'\n').map_or(bytes.len(), |p| start + p);
        let line = &bytes[start..line_end];
        if find(line, b"PRIVATE KEY").is_some() {
            let what = if find(line, b"RSA ").is_some() {
                "an RSA private key header"
            } else if find(line, b"OPENSSH ").is_some() {
                "an OpenSSH private key header"
            } else if find(line, b"EC ").is_some() {
                "an EC private key header"
            } else if find(line, b"DSA ").is_some() {
                "a DSA private key header"
            } else if find(line, b"PGP ").is_some() {
                "a PGP private key header"
            } else {
                "a private key header"
            };
            return Some(Finding::new("pem_private_key", format!("{what} at byte {start}")));
        }
        at = start + BEGIN.len();
    }
    None
}

// ---------------------------------------------------------------------------------------------
// Connection strings carrying an inline password
// ---------------------------------------------------------------------------------------------

/// Schemes whose URL form puts a live password in the userinfo. A connection string is the way a
/// credential reaches this store without looking like a key, so it gets its own rule.
const URL_SCHEMES: &[&str] = &[
    "postgresql://",
    "postgres://",
    "mongodb+srv://",
    "mongodb://",
    "mysql://",
    "rediss://",
    "redis://",
    "amqps://",
    "amqp://",
];

fn scan_connection_string(bytes: &[u8]) -> Option<Finding> {
    for i in 0..bytes.len() {
        // Only at a boundary, which keeps the scheme comparison off five bytes in six and means
        // `notpostgres://` cannot match.
        if i > 0 && is_word_byte(bytes[i - 1]) {
            continue;
        }
        let Some(scheme) = URL_SCHEMES.iter().find(|s| bytes[i..].starts_with(s.as_bytes())) else {
            continue;
        };
        let authority_start = i + scheme.len();
        let authority_end = bytes[authority_start..]
            .iter()
            .position(|&b| {
                matches!(
                    b,
                    b'/' | b'?' | b'#' | b',' | b'"' | b'\'' | b'`' | b'<' | b'>' | b')' | b']'
                ) || b.is_ascii_whitespace()
            })
            .map_or(bytes.len(), |p| authority_start + p);
        let authority = &bytes[authority_start..authority_end];

        // Userinfo ends at the last `@`: an unencoded `@` inside a password is legal and common,
        // and splitting on the first one would read half a password as the username.
        let Some(at) = authority.iter().rposition(|&b| b == b'@') else {
            continue;
        };
        // Password starts at the first `:`: it may itself contain `:`.
        let userinfo = &authority[..at];
        let Some(colon) = userinfo.iter().position(|&b| b == b':') else {
            continue;
        };
        let password = &userinfo[colon + 1..];
        if is_placeholder(password) {
            continue;
        }
        return Some(Finding::new(
            "connection_string_password",
            format!("a {scheme} URL with an inline password at byte {i}"),
        ));
    }
    None
}

/// Structural placeholders only. Deliberately not a word list: "password" as an actual password is
/// a thing people do, and every entry in such a list is a false negative someone has to live with.
/// What is recognisable without opinion is emptiness, shell or template interpolation, a
/// `<bracketed>` doc placeholder, and a redaction run.
///
/// `%` stays out of the interpolation set. It reads as a Windows-style placeholder about as often
/// as it reads as percent-encoding in a password that is entirely real.
fn is_placeholder(password: &[u8]) -> bool {
    if password.is_empty() {
        return true;
    }
    if password.iter().any(|&b| matches!(b, b'$' | b'{' | b'}')) {
        return true;
    }
    if password.first() == Some(&b'<') && password.last() == Some(&b'>') {
        return true;
    }
    password.iter().all(|&b| b == b'*')
}

// ---------------------------------------------------------------------------------------------
// Token rules: known prefixes, JWTs, generic entropy
// ---------------------------------------------------------------------------------------------

/// How much has to follow a prefix before it counts as a live key rather than a mention of one.
enum Tail {
    /// At least this many token bytes. Length is what separates `sk-ant-api03-...` from prose
    /// saying "keys start with sk-".
    AtLeast(usize),
    /// Exactly this many token bytes, then a boundary. For fixed-width ids, where exactness buys
    /// a large drop in false positives.
    Exactly(usize),
    /// Exactly this many uppercase alphanumerics, then a boundary. `ASIA` and `AKIA` are also
    /// English, so the AWS rule leans entirely on the fixed 16-character uppercase body.
    ExactlyUpper(usize),
    /// `.<22 token bytes>.<16 or more token bytes>`, the SendGrid shape. `SG.` on its own is two
    /// letters and a full stop, so nothing less specific is usable.
    SendGrid,
}

struct PrefixRule {
    prefix: &'static str,
    rule: &'static str,
    what: &'static str,
    tail: Tail,
}

/// Ordered longest-first where prefixes nest, so `sk-ant-` is reported as an Anthropic key rather
/// than an OpenAI one.
const PREFIX_RULES: &[PrefixRule] = &[
    PrefixRule {
        prefix: "sk-ant-",
        rule: "anthropic_api_key",
        what: "an Anthropic API key",
        tail: Tail::AtLeast(24),
    },
    PrefixRule {
        prefix: "sk-",
        rule: "openai_api_key",
        what: "an OpenAI API key",
        tail: Tail::AtLeast(20),
    },
    PrefixRule {
        prefix: "sk_live_",
        rule: "stripe_secret_key",
        what: "a Stripe live secret key",
        tail: Tail::AtLeast(16),
    },
    PrefixRule {
        prefix: "rk_live_",
        rule: "stripe_restricted_key",
        what: "a Stripe live restricted key",
        tail: Tail::AtLeast(16),
    },
    PrefixRule {
        prefix: "github_pat_",
        rule: "github_token",
        what: "a GitHub fine-grained personal access token",
        tail: Tail::AtLeast(40),
    },
    PrefixRule {
        prefix: "ghp_",
        rule: "github_token",
        what: "a GitHub personal access token",
        tail: Tail::AtLeast(30),
    },
    PrefixRule {
        prefix: "gho_",
        rule: "github_token",
        what: "a GitHub OAuth token",
        tail: Tail::AtLeast(30),
    },
    PrefixRule {
        prefix: "ghs_",
        rule: "github_token",
        what: "a GitHub app server token",
        tail: Tail::AtLeast(30),
    },
    PrefixRule {
        prefix: "ghu_",
        rule: "github_token",
        what: "a GitHub app user token",
        tail: Tail::AtLeast(30),
    },
    PrefixRule {
        prefix: "ghr_",
        rule: "github_token",
        what: "a GitHub refresh token",
        tail: Tail::AtLeast(30),
    },
    PrefixRule {
        prefix: "glpat-",
        rule: "gitlab_token",
        what: "a GitLab personal access token",
        tail: Tail::AtLeast(20),
    },
    PrefixRule {
        prefix: "xoxb-",
        rule: "slack_token",
        what: "a Slack bot token",
        tail: Tail::AtLeast(20),
    },
    PrefixRule {
        prefix: "xoxp-",
        rule: "slack_token",
        what: "a Slack user token",
        tail: Tail::AtLeast(20),
    },
    PrefixRule {
        prefix: "xoxa-",
        rule: "slack_token",
        what: "a Slack app token",
        tail: Tail::AtLeast(20),
    },
    PrefixRule {
        prefix: "AKIA",
        rule: "aws_access_key_id",
        what: "an AWS access key id",
        tail: Tail::ExactlyUpper(16),
    },
    PrefixRule {
        prefix: "ASIA",
        rule: "aws_access_key_id",
        what: "an AWS temporary access key id",
        tail: Tail::ExactlyUpper(16),
    },
    PrefixRule {
        prefix: "AIza",
        rule: "google_api_key",
        what: "a Google API key",
        tail: Tail::Exactly(35),
    },
    PrefixRule {
        prefix: "hf_",
        rule: "huggingface_token",
        what: "a Hugging Face access token",
        tail: Tail::AtLeast(30),
    },
    PrefixRule {
        prefix: "npm_",
        rule: "npm_token",
        what: "an npm access token",
        tail: Tail::AtLeast(30),
    },
    PrefixRule {
        prefix: "dop_v1_",
        rule: "digitalocean_token",
        what: "a DigitalOcean personal access token",
        tail: Tail::AtLeast(60),
    },
    PrefixRule {
        prefix: "shpat_",
        rule: "shopify_access_token",
        what: "a Shopify access token",
        tail: Tail::AtLeast(28),
    },
    PrefixRule {
        prefix: "SG",
        rule: "sendgrid_api_key",
        what: "a SendGrid API key",
        tail: Tail::SendGrid,
    },
];

/// Prefixes that mark a value as publishable. Entropy cannot tell a Stripe publishable key from a
/// secret one: both are long, mixed-case and random-looking. Firing on a value the owner is
/// expected to paste into a web page is the fastest way to lose their trust in the whole feature.
///
/// SSH public keys need no entry here. The base64 body of every one of them starts with a
/// four-byte length prefix whose high bytes are zero, so it starts `AAAA` and the repeated-run test
/// below drops it.
const PUBLISHABLE_PREFIXES: &[&str] = &["pk_live_", "pk_test_", "pub_"];

/// Minimum length for the generic rule. Below this, entropy over so few characters is noise.
const MIN_TOKEN_LEN: usize = 32;
/// Maximum length for the generic rule. Longer contiguous runs are pasted data, not credentials:
/// the long real credentials all have a rule of their own above.
const MAX_TOKEN_LEN: usize = 128;
/// Bits per character. Hex cannot exceed about 3.97 even at 64 characters, so this threshold
/// excludes every UUID, git SHA and content hash arithmetically rather than by pattern.
const MIN_ENTROPY_BITS: f64 = 4.2;
/// Four identical characters in a row. Random base62 does that about once in four thousand
/// tokens; base64 of binary with runs of zero bytes does it constantly.
const MAX_REPEAT_RUN: usize = 4;
/// Each case must hold at least a fifth of the token, expressed as a reciprocal.
///
/// Entropy alone cannot separate a long camelCase identifier from a secret: names like
/// `resolveSensitivityForWriteWithNamespaceDefault` measure 4.2 to 4.6 bits per character, right
/// on top of the threshold. What separates them is case balance. An identifier carries one capital
/// per word, 12 to 18 per cent, and a SCREAMING_CASE constant carries none in lower case, while a
/// random base62 token is about 42 per cent each. Measured over 20000 samples, this test drops
/// every identifier tried and keeps 99.7 per cent of 40-character random tokens.
const MIN_CASE_SHARE: usize = 5;

/// Shortest hex run the hex rule considers. `openssl rand -hex 24` is 48 characters and the
/// shortest secret any lumberroom script mints; the tokens and the KEK are 64. A git SHA is 40
/// and sits under this whatever word stands beside it.
const MIN_HEX_CREDENTIAL_LEN: usize = 48;
/// How far either side of a hex run a credential word is looked for. Forty bytes holds
/// `POSTGRES_PASSWORD=` and `the claude-code-mac token is ` with room to spare, and does not
/// reach a `token` two sentences away.
const HEX_ANCHOR_WINDOW: usize = 40;
/// The words that make a long hex run a credential. Compared case-folded as substrings, so
/// `AUTH_TOKENS`, `tokens` and `POSTGRES_PASSWORD` all match through `token` and `password`.
/// `key` on its own is absent: it is half the vocabulary of a hashing discussion.
const HEX_ANCHOR_WORDS: &[&str] = &[
    "token",
    "bearer",
    "secret",
    "password",
    "passwd",
    "credential",
    "kek",
    "api_key",
    "api-key",
    "apikey",
];
/// Words that say the hex is a digest. One of these in the window keeps the rule quiet even
/// beside a credential word, so "the token's sha256 is ..." is read as the checksum it is. A
/// credential written as "password hash: <hex>" is missed by this; a hash of a password is not
/// the password, and hashing one into hex is not a thing anyone does.
const HEX_DIGEST_WORDS: &[&str] =
    &["sha256", "sha-256", "sha1", "sha-1", "sha512", "digest", "checksum", "commit", "hash"];

/// One pass over the maximal runs of token bytes, collecting the first hit for each rule and
/// resolving priority at the end. Walking the content once matters because this runs on every
/// write.
fn scan_tokens(bytes: &[u8]) -> Option<Finding> {
    let mut jwt_hit: Option<Finding> = None;
    let mut hex_hit: Option<Finding> = None;
    let mut entropy_hit: Option<Finding> = None;
    // Computed only when a token survives every cheaper check, which is almost never.
    let mut blobs: Option<Vec<(usize, usize)>> = None;

    let mut i = 0;
    while i < bytes.len() {
        if !is_word_byte(bytes[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && is_word_byte(bytes[i]) {
            i += 1;
        }
        let end = i;

        // Highest priority of the token rules, so a hit ends the walk.
        if let Some(hit) = match_prefix_rule(bytes, start) {
            return Some(hit);
        }
        if jwt_hit.is_none() {
            jwt_hit = match_jwt(bytes, start, end);
        }
        if hex_hit.is_none() {
            hex_hit = match_hex_credential(bytes, start, end);
        }
        if entropy_hit.is_none() {
            entropy_hit = match_high_entropy(bytes, start, end, &mut blobs);
        }
    }

    jwt_hit.or(hex_hit).or(entropy_hit)
}

/// A uniform hex run of credential length with a credential word beside it and no digest word.
///
/// Uniform means one case throughout: `openssl rand -hex` is lowercase and a tool that upper-cases
/// it upper-cases all of it, while mixed-case hex is somebody's identifier. The anchor is the
/// neighbouring word and never the run, because a 64-character hex string with nothing said about
/// it is a content hash far more often than a key.
fn match_hex_credential(bytes: &[u8], start: usize, end: usize) -> Option<Finding> {
    let token = &bytes[start..end];
    if token.len() < MIN_HEX_CREDENTIAL_LEN || !is_uniform_hex(token) {
        return None;
    }
    let before = &bytes[start.saturating_sub(HEX_ANCHOR_WINDOW)..start];
    let after = &bytes[end..(end + HEX_ANCHOR_WINDOW).min(bytes.len())];
    let window: Vec<u8> = before.iter().chain(after.iter()).map(u8::to_ascii_lowercase).collect();
    if HEX_DIGEST_WORDS.iter().any(|w| find(&window, w.as_bytes()).is_some()) {
        return None;
    }
    let word = HEX_ANCHOR_WORDS.iter().find(|w| find(&window, w.as_bytes()).is_some())?;
    Some(Finding::new(
        "hex_credential",
        format!("a {}-character hex token beside the word {word:?} at byte {start}", token.len()),
    ))
}

/// Hex digits and letters of one case only. Digits alone count: a run of 48 digits beside the
/// word `password` is a credential the owner would want caught, and it is not an identifier.
fn is_uniform_hex(token: &[u8]) -> bool {
    let lower = token.iter().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b));
    let upper = token.iter().all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(b));
    lower || upper
}

/// `start` is always the first byte of a token run, so the preceding byte is a boundary already
/// and `foobarsk-...` cannot match.
fn match_prefix_rule(bytes: &[u8], start: usize) -> Option<Finding> {
    for rule in PREFIX_RULES {
        if !bytes[start..].starts_with(rule.prefix.as_bytes()) {
            continue;
        }
        let tail = start + rule.prefix.len();
        let matched = match rule.tail {
            Tail::AtLeast(n) => run_len(bytes, tail, is_word_byte) >= n,
            Tail::Exactly(n) => run_len(bytes, tail, is_word_byte) == n,
            Tail::ExactlyUpper(n) => {
                run_len(bytes, tail, is_upper_alnum_byte) == n
                    && run_len(bytes, tail, is_word_byte) == n
            }
            Tail::SendGrid => matches_sendgrid(bytes, tail),
        };
        if matched {
            return Some(Finding::new(rule.rule, format!("{} at byte {}", rule.what, start)));
        }
    }
    None
}

/// `SG.<22>.<16 or more>`. The dots are part of the shape, which is why this cannot be expressed
/// as a prefix plus a tail length.
fn matches_sendgrid(bytes: &[u8], tail: usize) -> bool {
    if bytes.get(tail) != Some(&b'.') {
        return false;
    }
    let first = tail + 1;
    if run_len(bytes, first, is_word_byte) != 22 {
        return false;
    }
    let dot = first + 22;
    if bytes.get(dot) != Some(&b'.') {
        return false;
    }
    run_len(bytes, dot + 1, is_word_byte) >= 16
}

/// Three base64url segments where the first decodes to a JSON object carrying `alg`.
///
/// Anchoring on `eyJ` costs nothing and means the decode runs on a handful of candidates rather
/// than on every token: a JSON header starts `{"`, which is always `eyJ` in base64url. Decoding
/// rather than pattern-matching is what keeps this rule quiet, since a dotted triple of
/// base64-ish words is otherwise an ordinary thing for text to contain.
fn match_jwt(bytes: &[u8], start: usize, end: usize) -> Option<Finding> {
    if !bytes[start..].starts_with(b"eyJ") {
        return None;
    }
    if bytes.get(end) != Some(&b'.') {
        return None;
    }
    let payload = end + 1;
    let payload_len = run_len(bytes, payload, is_word_byte);
    if payload_len < 8 {
        return None;
    }
    let dot = payload + payload_len;
    if bytes.get(dot) != Some(&b'.') {
        return None;
    }
    // A real signature is 43 bytes for HS256 and longer for RS256. Requiring 20 keeps the rule
    // off dotted prose without needing to know the algorithm.
    let signature_len = run_len(bytes, dot + 1, is_word_byte);
    if signature_len < 20 {
        return None;
    }
    let header = decode_base64url(&bytes[start..end])?;
    let text = std::str::from_utf8(&header).ok()?.trim();
    if !(text.starts_with('{') && text.ends_with('}') && text.contains("\"alg\"")) {
        return None;
    }
    let total = dot + 1 + signature_len - start;
    Some(Finding::new(
        "jwt",
        format!("a JWT with a decodable JSON header at byte {start}, {total} characters"),
    ))
}

fn match_high_entropy(
    bytes: &[u8],
    start: usize,
    end: usize,
    blobs: &mut Option<Vec<(usize, usize)>>,
) -> Option<Finding> {
    let token = &bytes[start..end];
    let len = token.len();
    if !(MIN_TOKEN_LEN..=MAX_TOKEN_LEN).contains(&len) {
        return None;
    }
    // A token flanked by base64 punctuation is a slice of a larger encoded blob rather than a
    // value someone pasted. Standard base64 of binary hits `+` or `/` every dozen characters, so
    // an image or a certificate body shatters into pieces that all fail this test.
    let flanked = |b: u8| matches!(b, b'+' | b'/' | b'=');
    if start > 0 && flanked(bytes[start - 1]) {
        return None;
    }
    if end < bytes.len() && flanked(bytes[end]) {
        return None;
    }
    if PUBLISHABLE_PREFIXES.iter().any(|p| token.starts_with(p.as_bytes())) {
        return None;
    }
    // Mixed case with a digit, and neither case a minority. Hex, UUIDs, git SHAs, base32 and
    // lowercase slugs fail the first half; identifiers and prose-shaped tokens fail the second.
    let lower = token.iter().filter(|b| b.is_ascii_lowercase()).count();
    let upper = token.iter().filter(|b| b.is_ascii_uppercase()).count();
    if !token.iter().any(|b| b.is_ascii_digit()) {
        return None;
    }
    if lower * MIN_CASE_SHARE < len || upper * MIN_CASE_SHARE < len {
        return None;
    }
    if longest_repeat(token) >= MAX_REPEAT_RUN {
        return None;
    }
    let bits = shannon_bits_per_char(token);
    if bits < MIN_ENTROPY_BITS {
        return None;
    }
    if blobs.is_none() {
        *blobs = Some(blob_spans(bytes));
    }
    if blobs.as_ref().is_some_and(|spans| spans.iter().any(|&(a, b)| start >= a && start < b)) {
        return None;
    }
    Some(Finding::new(
        "high_entropy_token",
        format!(
            "a {len}-character high-entropy token at byte {start} ({bits:.1} bits per character)"
        ),
    ))
}

/// Byte ranges covering pasted blocks of encoded data.
///
/// Wrapped base64 is the one shape the flanking test cannot see: a line break is not base64
/// punctuation, so a certificate body wrapped at 64 columns offers a fresh 64-character candidate
/// on every line, and roughly one line in sixty carries no `+` or `/` to disqualify it. Measured on
/// random binary, wrapped bodies produced a hit on 196 of 200 samples without this test.
///
/// A block is a run of consecutive lines that are entirely base64 characters, and it counts as
/// pasted data when three or more of those lines are 60 characters or longer. Membership rather
/// than width is what puts the short final line of a wrapped body inside the span; keying on width
/// alone left that line exposed, and it is the line most likely to be the one that fires.
///
/// The 60-character floor is what keeps this from swallowing real secrets written one per line,
/// since a base64url secret is 43 characters. A secret sharing a block with a wrapped body, on the
/// line directly below it with no blank line between, is absorbed and missed. That is the trade.
fn blob_spans(bytes: &[u8]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    // Start, end, and how many lines so far are wide enough to count.
    let mut block: Option<(usize, usize, usize)> = None;
    let mut line_start = 0usize;
    loop {
        let line_end = bytes[line_start..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(bytes.len(), |p| line_start + p);
        let line = trim_ascii(&bytes[line_start..line_end]);
        let encoded = !line.is_empty() && line.iter().all(|&b| is_base64ish_byte(b));
        if encoded {
            let wide = usize::from(line.len() >= 60);
            block = Some(match block {
                Some((start, _, wide_lines)) => (start, line_end, wide_lines + wide),
                None => (line_start, line_end, wide),
            });
        } else if let Some((start, end, wide_lines)) = block.take() {
            if wide_lines >= 3 {
                spans.push((start, end));
            }
        }
        if line_end == bytes.len() {
            break;
        }
        line_start = line_end + 1;
    }
    if let Some((start, end, wide_lines)) = block {
        if wide_lines >= 3 {
            spans.push((start, end));
        }
    }
    spans
}

// ---------------------------------------------------------------------------------------------
// Small helpers. Byte oriented throughout: every character class here is ASCII, and slicing a
// `&str` at an offset found by scanning would panic the moment surrounding prose held a multibyte
// character.
// ---------------------------------------------------------------------------------------------

/// The token charset. Base64url and base62 secrets live in it; `+`, `/`, `=`, `.` and `/` do not,
/// so paths and base64 of binary break into pieces rather than forming one long candidate.
fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

fn is_upper_alnum_byte(b: u8) -> bool {
    b.is_ascii_uppercase() || b.is_ascii_digit()
}

fn is_base64ish_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'_' | b'-')
}

fn run_len(bytes: &[u8], from: usize, accept: fn(u8) -> bool) -> usize {
    bytes[from.min(bytes.len())..].iter().take_while(|&&b| accept(b)).count()
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn trim_ascii(mut s: &[u8]) -> &[u8] {
    while let Some((first, rest)) = s.split_first() {
        if first.is_ascii_whitespace() {
            s = rest;
        } else {
            break;
        }
    }
    while let Some((last, rest)) = s.split_last() {
        if last.is_ascii_whitespace() {
            s = rest;
        } else {
            break;
        }
    }
    s
}

fn longest_repeat(token: &[u8]) -> usize {
    let mut best = 1;
    let mut current = 1;
    for pair in token.windows(2) {
        current = if pair[0] == pair[1] { current + 1 } else { 1 };
        best = best.max(current);
    }
    best
}

/// Shannon entropy per character over the token's own symbol distribution.
fn shannon_bits_per_char(token: &[u8]) -> f64 {
    let mut counts = [0u32; 128];
    for &b in token {
        counts[(b & 0x7f) as usize] += 1;
    }
    let n = token.len() as f64;
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = f64::from(c) / n;
            -p * p.log2()
        })
        .sum()
}

/// Unpadded base64url, hand rolled because a JWT header is 30-odd bytes and pulling a decoder in
/// for it would put a dependency in the write path for no gain.
fn decode_base64url(input: &[u8]) -> Option<Vec<u8>> {
    let value = |b: u8| -> Option<u32> {
        Some(match b {
            b'A'..=b'Z' => u32::from(b - b'A'),
            b'a'..=b'z' => u32::from(b - b'a') + 26,
            b'0'..=b'9' => u32::from(b - b'0') + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        })
    };
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for &b in input {
        acc = (acc << 6) | value(b)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CERT: &str = "-----BEGIN CERTIFICATE-----\n\
        MIIDdzCCAl+gAwIBAgIEAgAAuTANBgkqhkiG9w0BAQUFADBaMQswCQYDVQQGEwJJ\n\
        RTESMBAGA1UEChMJQmFsdGltb3JlMRMwEQYDVQQLEwpDeWJlclRydXN0MSIwIAYD\n\
        VQQDExlCYWx0aW1vcmUgQ3liZXJUcnVzdCBSb290MB4XDTAwMDUxMjE4NDYwMFoX\n\
        DTI1MDUxMjIzNTkwMFowWjELMAkGA1UEBhMCSUUxEjAQBgNVBAoTCUJhbHRpbW9y\n\
        ZTETMBEGA1UECxMKQ3liZXJUcnVzdDEiMCAGA1UEAxMZQmFsdGltb3JlIEN5YmVy\n\
        VHJ1c3QgUm9vdIIBAQDKDpMIQtGGa1D8L\n\
        -----END CERTIFICATE-----";

    /// Everything this store legitimately contains. A hit on any of these is the failure that
    /// makes the owner switch the tripwire off, so this table is the important half of the suite.
    const NEGATIVES: &[(&str, &str)] = &[
        ("a memory id", "superseded by 9f2a4c1e-7b3d-4f8a-9c2e-1d5b6a7c8e9f"),
        ("a uuid with the dashes stripped", "9f2a4c1e7b3d4f8a9c2e1d5b6a7c8e9f"),
        ("a git sha", "fixed in e3b0c44298fc1c149afbf4c8996fb92427ae41e4"),
        (
            "a content hash",
            "sha256 a94a8fe5ccb19ba61c4c0873d391e987982fbbd3b1b0d1e2c3f4a5b6c7d8e9f0",
        ),
        (
            "a bare sixty-four hex string with nothing said about it",
            "a94a8fe5ccb19ba61c4c0873d391e987982fbbd3b1b0d1e2c3f4a5b6c7d8e9f0",
        ),
        (
            "a sha256 git commit",
            "commit 6f2a4c1e7b3d4f8a9c2e1d5b6a7c8e9f0a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d",
        ),
        (
            "a digest of a token, named as a digest",
            "the token's sha256 is a94a8fe5ccb19ba61c4c0873d391e987982fbbd3b1b0d1e2c3f4a5b6c7d8e9f0",
        ),
        (
            "a checksum on a download line",
            "checksum for the tarball: 6f2a4c1e7b3d4f8a9c2e1d5b6a7c8e9f0a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d",
        ),
        (
            "a forty-character sha beside the word token",
            "the token commit was e3b0c44298fc1c149afbf4c8996fb92427ae41e4",
        ),
        (
            "a credential word two sentences away from a hash",
            // No digest word anywhere in this string, so only the distance keeps the rule quiet.
            "Rotated the deploy token today. Unrelated: the image tag moved on to the next build. \
             Build id a94a8fe5ccb19ba61c4c0873d391e987982fbbd3b1b0d1e2c3f4a5b6c7d8e9f0",
        ),
        (
            "mixed-case hex, which is an identifier and not a minted secret",
            "token id A94a8FE5ccb19BA61c4c0873D391e987982fbbd3b1b0D1E2c3f4a5b6c7d8e9f0",
        ),
        ("prose about a password", "the postgres password lives in 1Password, not in here"),
        ("a prefix mentioned in passing", "OpenAI keys start with sk- and Anthropic's with sk-ant-"),
        ("a prefix inside a word", "the mysk-ant-handler function is unrelated"),
        (
            "a base64 png fragment",
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFAAH/q842iQAAAABJRU5ErkJggg==",
        ),
        (
            "a base64 jpeg fragment",
            "/9j/4AAQSkZJRgABAQEAYABgAAD/2wBDAAgGBgcGBQgHBwcJCQgKDBQNDAsLDBkSEw8UHRofHh0aHBwc",
        ),
        (
            "an ssh public key",
            "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABgQC7vbqajDw4o4+diGYlKwSMcMBZ1kk4bY8jU8x9ZDkMlQ user@host",
        ),
        (
            "a file path",
            "/Users/example/work/acme/memoryEngine/src/domain/tripwire.rs",
        ),
        (
            "a long camelCase identifier",
            "resolveSensitivityForWriteWithNamespaceDefault2ndAttempt",
        ),
        (
            "a long screaming case constant",
            "RESOLVE_SENSITIVITY_FOR_WRITE_WITH_NAMESPACE_DEFAULT_2",
        ),
        ("a long snake case test name", "a_write_at_open_carrying_a_credential_is_refused_2"),
        (
            "a long type name",
            "ContextBootstrapServiceWithNamespaceCeilingsV2Handler",
        ),
        (
            "a stripe publishable key",
            "pk_live_51H8xQ2eZvKYlo2CabcDEfGhIjKlMnOpQrStUvWxYz",
        ),
        (
            "a stripe test publishable key",
            "pk_test_51H8xQ2eZvKYlo2CabcDEfGhIjKlMnOpQrStUvWxYz",
        ),
        ("a connection string with no password", "postgres://lumberroom@db.internal:5432/lumberroom"),
        (
            "a connection string with an env placeholder",
            "postgres://lumberroom:${DB_PASSWORD}@db.internal:5432/lumberroom",
        ),
        (
            "a connection string with a doc placeholder",
            "mysql://root:<password>@127.0.0.1:3306/app",
        ),
        ("a connection string with a redacted password", "amqp://app:****@rabbit:5672/"),
        ("a redis url with a port and no credentials", "redis://cache.internal:6379/0"),
        ("a public key header", "-----BEGIN PUBLIC KEY-----"),
        ("a certificate header", "-----BEGIN CERTIFICATE-----"),
        ("a wrapped certificate body, short final line and all", CERT),
        ("prose about jwts", "a jwt has three segments: header, payload and signature"),
        ("empty content", ""),
        ("ordinary prose", "Renewed the domain. Registrar is Cloudflare, expires in March."),
    ];

    #[test]
    fn nothing_the_store_legitimately_holds_fires() {
        for (what, content) in NEGATIVES {
            assert_eq!(
                scan(content),
                None,
                "false positive on {what}: {:?}",
                scan(content).map(|f| f.rule)
            );
        }
    }

    fn fires_as(content: &str, expected_rule: &str) -> Finding {
        let finding = scan(content)
            .unwrap_or_else(|| panic!("expected {expected_rule} to fire on {content:?}"));
        assert_eq!(finding.rule, expected_rule);
        finding
    }

    #[test]
    fn every_private_key_header_shape_fires() {
        for header in [
            "-----BEGIN RSA PRIVATE KEY-----",
            "-----BEGIN EC PRIVATE KEY-----",
            "-----BEGIN OPENSSH PRIVATE KEY-----",
            "-----BEGIN PRIVATE KEY-----",
            "-----BEGIN ENCRYPTED PRIVATE KEY-----",
            "-----BEGIN PGP PRIVATE KEY BLOCK-----",
        ] {
            let content = format!("deploy key for the box\n{header}\nMIIEow==\n");
            fires_as(&content, "pem_private_key");
        }
    }

    #[test]
    fn the_private_key_rule_names_the_flavour_it_found() {
        let f = fires_as("-----BEGIN OPENSSH PRIVATE KEY-----", "pem_private_key");
        assert!(f.detail.contains("OpenSSH"), "{}", f.detail);
        assert!(f.detail.contains("byte 0"), "{}", f.detail);
    }

    #[test]
    fn known_live_credential_prefixes_fire() {
        let cases: &[(&str, &str)] = &[
            ("sk-ant-api03-Ab3dEf7gH9iJkLmNoPqRsTuVwXyZ01234567", "anthropic_api_key"),
            ("sk-proj-Ab3dEf7gH9iJkLmNoPqRsTuVwXyZ0123", "openai_api_key"),
            ("ghp_16C7e42F292c6912E7710c838347Ae178B4a", "github_token"),
            ("gho_16C7e42F292c6912E7710c838347Ae178B4a", "github_token"),
            ("ghs_16C7e42F292c6912E7710c838347Ae178B4a", "github_token"),
            (
                "github_pat_11ABCDEFG0aBcDeFgHiJkL_9zYxWvUtSrQpOnMlKjIhGfEdCbA1234567890abcd",
                "github_token",
            ),
            ("glpat-Ab3dEf7gH9iJkLmNoPqR", "gitlab_token"),
            ("xoxb-123456789012-1234567890123-AbCdEfGhIjKlMnOpQrStUvWx", "slack_token"),
            ("xoxp-123456789012-1234567890123-AbCdEfGhIjKlMnOpQrStUvWx", "slack_token"),
            ("xoxa-123456789012-1234567890123-AbCdEfGhIjKlMnOpQrStUvWx", "slack_token"),
            ("AKIAIOSFODNN7EXAMPLE", "aws_access_key_id"),
            ("ASIAY34FZKBOKMUTVV7A", "aws_access_key_id"),
            ("AIzaSyD-9tSrke72PouQMnMX-a7eZSW0jkFMBWY", "google_api_key"),
            ("hf_QwErTyUiOpAsDfGhJkLzXcVbNm1234567890", "huggingface_token"),
            ("npm_QwErTyUiOpAsDfGhJkLzXcVbNm1234567890", "npm_token"),
            (
                "dop_v1_9f2a4c1e7b3d4f8a9c2e1d5b6a7c8e9f0a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5",
                "digitalocean_token",
            ),
            ("sk_live_51H8xQ2eZvKYlo2CAbCdEfGhIj", "stripe_secret_key"),
            ("rk_live_51H8xQ2eZvKYlo2CAbCdEfGhIj", "stripe_restricted_key"),
            (
                "SG.AbCdEfGhIjKlMnOpQrStUv.WxYz0123456789AbCdEfGhIjKlMnOpQrStUvWxYz012",
                "sendgrid_api_key",
            ),
            ("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", "shopify_access_token"),
        ];
        for (secret, rule) in cases {
            fires_as(secret, rule);
            fires_as(&format!("the key is {secret} and it is live"), rule);
        }
    }

    #[test]
    fn an_anthropic_key_is_not_reported_as_an_openai_one() {
        let f = fires_as("sk-ant-api03-Ab3dEf7gH9iJkLmNoPqRsTuVwXyZ01234567", "anthropic_api_key");
        assert!(f.detail.contains("Anthropic"), "{}", f.detail);
    }

    #[test]
    fn an_aws_key_id_needs_the_full_fixed_width_body() {
        assert_eq!(scan("we run in ASIA and in EU"), None);
        assert_eq!(scan("AKIASHORT"), None, "a truncated body is not a key id");
        assert_eq!(
            scan("AKIAIOSFODNN7EXAMPLEEXTRA"),
            None,
            "an over-long body is something else that happens to start with AKIA"
        );
    }

    /// An over-long body is not a Google key. It may still be a secret by the generic rule, which
    /// is fine: what must not happen is the tripwire asserting a provider it has not established.
    #[test]
    fn a_google_key_needs_its_exact_length() {
        assert_eq!(scan("AIzaShort"), None);
        assert_ne!(
            scan("AIzaSyD-9tSrke72PouQMnMX-a7eZSW0jkFMBWYTOOLONG").map(|f| f.rule),
            Some("google_api_key")
        );
    }

    #[test]
    fn a_jwt_fires_only_when_its_header_actually_decodes() {
        let jwt = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.\
            eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIyfQ.\
            SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        fires_as(jwt, "jwt");
        fires_as(&format!("session token: {jwt}"), "jwt");

        assert_ne!(
            scan("eyJnb3RjaGEiOiJubyBhbGcga2V5IGhlcmUgYXQgYWxsIn0.cGF5bG9hZA.c2lnbmF0dXJlc2lnbmF0dXJlMTIz")
                .map(|f| f.rule),
            Some("jwt"),
            "base64 of JSON without an alg claim is not a JWT"
        );
        assert_eq!(scan("eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.short"), None, "no real signature");
    }

    #[test]
    fn connection_strings_with_an_inline_password_fire() {
        for url in [
            "postgres://lumberroom:s3cr3tPassw0rd@db.internal:5432/lumberroom",
            "postgresql://lumberroom:s3cr3tPassw0rd@db.internal:5432/lumberroom",
            "mysql://root:s3cr3tPassw0rd@127.0.0.1:3306/app",
            "mongodb://admin:s3cr3tPassw0rd@cluster0.example.net/admin",
            "mongodb+srv://admin:s3cr3tPassw0rd@cluster0.example.net/admin",
            "redis://:s3cr3tPassw0rd@cache.internal:6379/0",
            "amqp://app:s3cr3tPassw0rd@rabbit:5672/",
        ] {
            let f = fires_as(url, "connection_string_password");
            assert!(!f.detail.contains("s3cr3tPassw0rd"), "{}", f.detail);
            assert!(!f.detail.contains("db.internal"), "the host stays out of it too");
        }
    }

    #[test]
    fn a_password_containing_an_at_sign_is_still_found() {
        fires_as(
            "postgres://lumberroom:p@ssw0rdWithAt@db.internal:5432/lumberroom",
            "connection_string_password",
        );
    }

    #[test]
    fn a_percent_encoded_password_is_a_password() {
        fires_as(
            "postgres://lumberroom:p%40ssw0rdEncoded@db.internal:5432/lumberroom",
            "connection_string_password",
        );
    }

    const HEX64: &str = "6f2a4c1e7b3d4f8a9c2e1d5b6a7c8e9f0a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d";
    const HEX48: &str = "6f2a4c1e7b3d4f8a9c2e1d5b6a7c8e9f0a1b2c3d4e5f6a7b";

    /// The shapes this system mints itself: the install script's tokens, its Postgres password
    /// and its KEK, in the sentences they get pasted in.
    #[test]
    fn lumberrooms_own_hex_credentials_fire_beside_a_credential_word() {
        let cases = [
            format!("the claude-code-mac token is {HEX64}"),
            format!("AUTH_TOKENS=claude-code-mac:{HEX64}"),
            format!("POSTGRES_PASSWORD={HEX48}"),
            format!("KEK: {HEX64}"),
            format!("./client/wire-mac.sh --url https://memory.example.com --token {HEX64}"),
            format!("Authorization: Bearer {HEX64}"),
            format!("{HEX64} is the secret for the bot"),
            format!("the api_key is {}", HEX64.to_ascii_uppercase()),
        ];
        for content in &cases {
            let f = fires_as(content, "hex_credential");
            assert!(!f.detail.contains("6f2a4c1e"), "{}", f.detail);
            assert!(f.detail.contains("byte"), "{}", f.detail);
        }
    }

    #[test]
    fn the_hex_rule_needs_credential_length_and_a_word_beside_the_run() {
        // Forty characters is a git SHA whatever stands beside it.
        assert_eq!(scan("token e3b0c44298fc1c149afbf4c8996fb92427ae41e4"), None);
        // Sixty-four characters with no word beside it is a hash.
        assert_eq!(scan(HEX64), None);
        // The word has to be inside the window.
        let far = format!(
            "the token rotates monthly and is stored in the vault under infra/prod. {HEX64}"
        );
        assert_eq!(scan(&far), None, "a word outside the window is not an anchor");
    }

    #[test]
    fn a_long_high_entropy_token_fires_with_no_prefix_to_go_on() {
        let f = fires_as(
            "the deploy token is Xk7pQ2vNbR4tYw9zAe3LcH6mJf1sDgU8oIkPnQrZtVy and it rotates monthly",
            "high_entropy_token",
        );
        assert!(f.detail.contains("bits per character"), "{}", f.detail);
    }

    #[test]
    fn secrets_written_one_per_line_are_not_mistaken_for_a_pasted_blob() {
        let content = "Xk7pQ2vNbR4tYw9zAe3LcH6mJf1sDgU8oIkPnQrZtVy\n\
                       Zt4mHq8wCx2vBn6kLp3fRd9sGa5jEu7yTiOoWzXcVbN\n\
                       Qp9zLm3xKv7bNr2tYw8sAe4jHc6fDg1uIoPkZtVyXcB";
        assert!(scan(content).is_some(), "three secrets on three lines is not a certificate");
    }

    #[test]
    fn the_detail_never_echoes_what_it_matched() {
        let secret = "sk-ant-api03-Ab3dEf7gH9iJkLmNoPqRsTuVwXyZ01234567";
        let f = fires_as(secret, "anthropic_api_key");
        assert!(!f.detail.contains("Ab3dEf7gH9iJkLmNoPqRsTuVwXyZ"), "{}", f.detail);
        assert!(f.detail.contains("byte"), "position, not content: {}", f.detail);
    }

    #[test]
    fn the_refusal_names_the_rule_and_points_at_sealed_without_leaking_the_secret() {
        let secret = "ghp_16C7e42F292c6912E7710c838347Ae178B4a";
        let f = fires_as(secret, "github_token");
        let message = f.refusal().client_message().to_string();
        assert!(message.contains("github_token"), "{message}");
        assert!(message.contains("'sealed'"), "{message}");
        assert!(message.contains("'open'"), "{message}");
        assert!(!message.contains("16C7e42F292c6912E7710c838347Ae178B4a"), "{message}");
    }

    #[test]
    fn a_multibyte_character_next_to_a_candidate_does_not_panic() {
        assert!(scan("clé: sk-ant-api03-Ab3dEf7gH9iJkLmNoPqRsTuVwXyZ01234567 ✅").is_some());
        assert_eq!(scan("naïve прозе 日本語 · nothing here"), None);
    }

    #[test]
    fn the_first_rule_in_priority_order_is_the_one_reported() {
        let content = "-----BEGIN RSA PRIVATE KEY-----\n\
                       postgres://lumberroom:s3cr3tPassw0rd@db.internal:5432/lumberroom\n\
                       ghp_16C7e42F292c6912E7710c838347Ae178B4a";
        assert_eq!(scan(content).unwrap().rule, "pem_private_key");
    }

    #[test]
    fn hex_can_never_reach_the_entropy_threshold() {
        let hex: String = "0123456789abcdef".repeat(8);
        assert!(
            shannon_bits_per_char(hex.as_bytes()) < MIN_ENTROPY_BITS,
            "a perfectly uniform 128-character hex string is the ceiling for hex and it must \
             still sit below the threshold, which is what keeps every id and hash in the store safe"
        );
    }
}
