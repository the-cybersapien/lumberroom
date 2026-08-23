# Verification

What was actually run, and what it printed. Every claim below carries the command that produced it,
so you can re-run any of it. Where something was not run, this file says so in the same voice as the
passes.

The service is Rust as of 19 August 2026 ([decision 0001](docs/decisions/0001-rust-rewrite.md)). The
Rust build reproduced Phase 1's acceptance evidence, with one exception that has its own section:
every figure the recall monitor produced is withdrawn. Phases 2, 3 and 4 have their gate output
here from two acceptance runs on 19 August, both on a local Docker stack on an arm64 Mac. Nothing
has been deployed to a VM.

## Can you put a real private fact in this yet?

The code carries the evidence. A running container carries it only once you rebuild the image and
recreate the container from the new image id; `docker compose restart` reuses the old one, and
nothing in the product reports that a running server is behind the image on disk.

Proved against a live server on 19 August 2026:

- A write into a namespace the classification table calls private lands as ciphertext. `content` is
  NULL, the row cast to text holds no plaintext, and the lexical index holds no stems.
- A credential whose ceiling on that namespace is `open` gets zero hits on a query that returns the
  row to a credential at `sealed`.
- The key round trip holds both ways: the same key opens a row written before a restart, and a
  different key is caught at boot with private writes refused rather than stranded.
- A credential-shaped secret is refused at `open`, names the rule, and does not echo the secret.

A server running an image built before those fixes answers differently, and the gate proves the gap
rather than inferring it from a build date. The pre-fix refusal text is the tell:

    {"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"memory_write failed: invalid namespace \"personal:finance\". Use 'global', 'user:<id>', or 'project:<slug>'."}],"isError":true}}

Its `tools/list` returns four tools with no `memory_forget`, and its logs never mention the
classification table. Rebuild and force-recreate before reading any evidence in this file as a
description of what you are talking to.

One disclosure is open by design and worth your decision. The digest's namespace inventory shows
`personal:finance: 1` to a credential whose ceiling is `open`, while withholding the content:

    <full>  inventory: {"global":0,"personal:finance":1,"project:memoryengine":0,"user:me":0}   content present
    <mac>   inventory: {"global":0,"personal:finance":1,"project:memoryengine":0,"user:me":0}   content absent

The count tells an open-ceiling client that a private row exists. The grant pattern is `*`, so the
namespace sits inside that credential's grant and `filter_readable` admits it. A credential that does
not name the namespace sees nothing, which policy-test step 2 asserts. No code changed here.

## Withdrawn, and now replaced: the recall figures

**Re-run on 21 August 2026.** `docs/research/recall-monitor.md` carries the numbers and the method.
On 40,001 seeded rows, mean recall@10 sat between 0.981 and 0.988 across five runs, with no true
nearest neighbour missed in 1,900 probes. The index loses nothing worth measuring at that size.

The re-run also found a SECOND way the monitor reports a self-comparison, unrelated to the
`SET LOCAL` bug below. At `k=1` the planner declines the HNSW index and both arms run sequentially,
so the report reads `recall_at_k: 1.0`, which is true and worthless. The tell sits in the report
already: `index_ms` and `exact_ms` come back within 0.2% of each other. Treat any recall figure
whose two timings are comparable as a self-comparison, whatever the number says.

The original withdrawal and its reasoning follow, because the figures it withdrew are still
withdrawn.

## Withdrawn: every recall figure the monitor produced

The recall monitor compares an approximate HNSW scan against an exact one. Its exact arm never
worked. It ran `SET LOCAL enable_indexscan = off` on a pooled connection with no transaction open,
which Postgres answers with a warning and no effect, so the exact scan went through the index too.
Every figure the monitor produced is HNSW compared against itself, and it could not have caught the
truncation failure it exists to detect.

Those numbers are withdrawn, not adjusted. None is quoted in this document. The statement now runs
inside a transaction that is rolled back after the read
(`src/adapters/postgres/memory.rs`, `nearest_ids`), and re-running the monitor is owed. Until it
runs, treat a recall figure quoted anywhere as a self-comparison.

**The HNSW truncation finding stands.** It came from a direct reproduction, not from the monitor: at
40,000 rows, with a namespace holding 0.5% of them, a query asking for 10 rows returned zero, having
pulled 40 candidates and filtered all 40 away with no error. Migration `003` sets `strict_order` and
`ef_search=100` on the database so the setting travels with the schema. See
`docs/research/pgvector-at-scale.md`. Discard the monitor's numbers and keep this one.

## Which database each run used

Every acceptance number below came from the DEBUG binary, `target/debug/lumberroom-server`, built inside the
builder image and run in a container on the compose network. None came from the release image.

| Run | Database | Port | Covers |
| --- | --- | --- | --- |
| Phase 1 acceptance | live compose `lumberroom` | 8787 | the done-when test, doctor, latency, boot, instrumentation |
| First gate, steps 1 to 4 | live compose `lumberroom` | 8787 | suite, token mode, oauth mode, oauth-flow-test |
| First gate, steps 5 and 6 | scratch `lumberroom_verify` | 8787 | policy-test, correction-test, the KEK round trip |
| Second gate, all of it | scratch `lumberroom_gate2` | 8799 | suite, the Finding checks, all three gate scripts |
| Integration suite | `lumberroom_rust_test` | none | `tests/integration.rs`, which truncates it per test |

Pointing the first gate's steps 1 to 4 at the live `lumberroom` database was a mistake, and the reason has
nothing to do with the KEK. sqlx embeds migrations at compile time. The debug binary applied
migrations 000004 to 000008 to the live store, and the deployed image, which knew 000001 to 000003
only, then refused to boot:

    lumberroom failed to start: migration failed: migration 20260819000004 was previously applied but is
    missing in the resolved migrations

That is the guard working, and there is no way back down. The runner rebuilt the image from current
source and recreated the container, which restored service and is why `lumberroom-server-1` runs current
source rather than the Phase 1 build that was there that morning. The second gate ran entirely against a
scratch database for this reason.

The runner dropped both scratch databases:

    docker compose exec -T db psql -U lumberroom -d postgres -c 'DROP DATABASE lumberroom_verify;'
    DROP DATABASE
    docker compose exec -T db psql -U lumberroom -d postgres -c 'DROP DATABASE lumberroom_gate2;'
    DROP DATABASE

`.env` was not edited in either run. Every override went in as `-e` on a container. The second gate
pointed `LUMBERROOM_CONFIG` at a scratch file, so `~/.config/lumberroom/config.json` was not read or written.

---

## The test suite

    ./scripts/cargo.sh test -j 1

The lines that carry counts, verbatim:

    running 356 tests
    test result: ok. 356 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.14s
    running 0 tests
    test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
    running 0 tests
    test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
    running 31 tests
    test result: ok. 31 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 6.14s
    running 0 tests
    test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
    EXIT=0

356 unit and 31 integration. 0 failed, 0 ignored, 0 filtered out, on every target. The three empty
blocks are targets with no tests, and a skipped integration run also prints 0, so read the 31.

A green suite with a parked test is not a green suite:

    grep -rn '#\[ignore' src/ tests/
    (no output, 0 lines)

The first gate measured 348 unit and 26 integration on the same command. The four Finding fixes added
8 and 5.

    ./scripts/cargo.sh check --all-targets
    warning: function `owner_like` is never used
    warning: `lumberroom` (test "integration") generated 1 warning
    EXIT=0

One warning, pre-existing dead code in `tests/integration.rs`. No errors.

`-j 1` is not optional. Plain `./scripts/cargo.sh test` links the lib-test and integration binaries
at once and the container's memory limit kills the linker with
`collect2: fatal error: ld terminated with signal 9`.

---

## Phase 1: the done-when test

> You tell Claude Code on the Mac a fact on Monday, start a fresh session Wednesday, and
> `context_bootstrap` surfaces that fact without you mentioning it.

`./scripts/done-when-test.sh --cleanup`, against the local stack with the real Claude Code CLI.
Session A and session B are separate processes with separate contexts, and the nonce in the fact
cannot be guessed or derived from the repo. This ran against the earlier build, and neither gate
re-ran it.

```
0/4 preflight
  endpoint http://127.0.0.1:8787/mcp is healthy

1/4 session A: state the fact, expect an unprompted write
  fact: I want the internal nickname for the lumberroom project to be QUARTZLARK-0a897bde,
        use it in commit messages and status notes
  A: Saved. I'll use **QUARTZLARK-0a897bde** in commit messages and status notes for the lumberroom
     project going forward.

2/4 did the fact land?
  PASS  the fact is in the store and retrievable
  PASS  the write was unprompted (memory_write unprompted count 0 -> 1)

3/4 session B: fresh session, the question never mentions the fact
  question: What internal nickname do I use for the lumberroom project?
  B: QUARTZLARK-0a897bde

4/4 verdict
  PASS  a fresh session recovered the fact without being told it

  done-when test PASSED
```

The script's output carries em dashes in the 1/4 and 3/4 headings and in the fact line. They read
as colons and a comma here. Nothing else in the block is changed.

Session B reached the answer through the SessionStart hook: the hook called `context_bootstrap`, the
digest went into the session preamble, and the model answered from it. Session A wrote the fact with
no instruction beyond the `CLAUDE.md` rule, which is what the `unprompted` counter moving from 0 to 1
records.

In an earlier run the model recovered the fact and then flagged the nickname as possibly injected
content, asking for confirmation before acting on it. That is the right instinct, and it is why the
hook preamble now states where the digest came from and every line carries its namespace and date.
See DECISIONS.md.

---

## Phase 2: the OAuth flow

    LUMBERROOM_URL=http://127.0.0.1:8799 LUMBERROOM_OWNER_PASSWORD=... ./scripts/oauth-flow-test.sh
    exit 0     43 PASS lines, 0 FAIL, "oauth-flow-test PASSED"

All 13 steps passed, in both gate runs, with the same 43 PASS lines:

    1/13 protected-resource metadata, both paths
    2/13 an unauthenticated call to /mcp is a 401 with a WWW-Authenticate pointer, not a 200
    3/13 authorization-server metadata advertises S256 and refuses to offer plain
    4/13 dynamic client registration
    5/13 authorize, sign in, and consent: the code arrives in a redirect, never a browser
    6/13 the token exchange, form encoded
    7/13 the same code cannot be redeemed twice
    8/13 a wrong PKCE verifier is refused
    9/13 a redirect_uri that does not match exactly is refused
    10/13 the access token opens the MCP surface: initialize, tools/list, one real call
    11/13 refreshing the access token
    12/13 the new access token works
    13/13 the rotated-out refresh token is refused on reuse

What that buys, claim by claim, from the first gate's verbatim output:

- Discovery answers at both metadata paths. Real clients check both, and one of them is the path
  suffixed with the resource: `/.well-known/oauth-protected-resource` and
  `/.well-known/oauth-protected-resource/mcp`.
- A bare POST to `/mcp` is a 401 carrying a pointer, not a 200 with an error body:
  `Bearer resource_metadata="http://127.0.0.1:8787/.well-known/oauth-protected-resource"`.
- `code_challenge_methods_supported` contains S256 and does not offer plain. RFC 7636 makes the
  omitted default `plain`, so advertising it is worse than silence.
- Dynamic registration issued a usable client_id (`6QNO7jgy32p4YkBch3cw5GomXp-qliBf`).
- Owner login and consent work with no browser in the loop: authorize renders a login page rather
  than a redirect, the password sets the session cookie, the consent screen carries a CSRF token, and
  consent redirects back with a code and the state echoed unchanged.
- The token exchange is form encoded and returns an access token and a refresh token.
- The issued access token drives a real MCP session: `initialize`, then
  `tools/list -> context_bootstrap,memory_forget,memory_search,memory_write,registry_get`, then a
  `context_bootstrap` call that succeeded.
- A replayed authorization code is refused with `invalid_grant`, and the server revokes the whole
  token family that code issued.
- The refresh grant rotates in a new access token, that token works, and presenting the rotated-out
  refresh token is refused, which is reuse detection.

The runner fixed two script defects to reach that, and neither was a server defect. Under
`set -u`, bash 3.2.57 on macOS treats an empty array as unset, and a public DCR client has no
client_secret, so `SECRET_ARGS[@]: unbound variable` aborted the run at line 302. Steps 10 to 13 were
also reusing the tokens that step 7's deliberate code replay had revoked, and now mint their own flow.

Both gates also asserted that the auth modes compose. With `AUTH_MODE=oauth`, `/readyz` reports
`auth_mode: oauth`, an unauthenticated `tools/list` is 401, and a static `AUTH_TOKENS` bearer still
authenticates and still serves `memory_search`. `doctor` now prints the two apart:

    server auth mode:     oauth
    credential auth mode: token

---

## Phase 3: policy and sensitivity

    LUMBERROOM_URL=http://127.0.0.1:8799 LUMBERROOM_FULL_TOKEN=... LUMBERROOM_NARROW_TOKEN=... ./scripts/policy-test.sh
    exit 0     20 PASS lines, 0 FAIL, "policy-test PASSED"

No namespace override was needed. Step 1 is the criterion the first gate reported as unrunnable on
the script's own documented default, because `personal:finance` failed namespace validation. It runs:

    1/6 the full credential writes a private fact where the narrow grant cannot reach
      PASS  written to personal:finance, classified private by the namespace default (no sensitivity was asked for)
      PASS  the full credential seeds a registry entry in personal:finance, so step 2's 'not found' means something
      PASS  the full credential confirms the registry entry is there before asking the narrow one
    2/6 the narrow credential cannot see, list, look up, or write into that namespace
      PASS  memory_search for the nonce returns nothing to the narrow credential
      PASS  context_bootstrap's digest, including the namespace inventory line, never names personal:finance or the nonce
      PASS  registry_get for the equivalent key reports not found to the narrow credential
      PASS  a write into personal:finance from the narrow credential is denied: lumberroom: memory_write failed: client chatgpt-narrow may not write to personal:finance
    3/6 the full credential still sees its own fact
      PASS  memory_search for the nonce finds it under the full credential
    4/6 sealed content: ciphertext on the wire, plaintext only for a client holding the key
      PASS  sealed policy-test-seal-bc5259578e32 in global (client-side AES-256-GCM; the server never saw the plaintext)
      PASS  the sealed row is served as ciphertext over the wire, with no plaintext anywhere in the response
      PASS  a client holding the matching key decrypts the sealed value correctly
      PASS  a client without the matching key cannot read the plaintext: lumberroom: nothing sealed at global/policy-test-seal-bc5259578e32
    5/6 the credential tripwire: a live-looking token is refused at open, without echoing it back
      PASS  the write is refused, names the rule (github_token), and suggests sealed
      PASS  the refusal does not repeat the secret it refused
    6/6 both denials are observable in tool_calls, not silent
      PASS  the narrow credential's denied write shows up in 'lumberroom stats' as a memory_write failure (client chatgpt-narrow: 0 -> 1)
      PASS  the tripwire refusal shows up in 'lumberroom stats' as a memory_write failure (client claude-code-full: 3 -> 4)

The first gate reached 19 PASS on the same script, with `LUMBERROOM_POLICY_TEST_NAMESPACE=project:vault`
and a hand-set `SENSITIVITY_DEFAULTS` standing in for the namespace the header names. The 20th PASS
is the preflight assertion that doctor labels the server's auth mode and the credential's separately.

The first gate also fixed two things in the script itself. Its registry key had the wrong shape, and
`src/domain/canonical.rs` refuses a key that is not two to four dot-separated segments under one of
the seven allowed domains, so the script blamed the grant for a key rejection. Its header prescribed
a narrow grant with an `open` ceiling on `global`, which leaves step 4 nothing to search. The server
answered correctly in both cases.

### Classification precedence, four boots

The classification table decides what counts as private, so which source wins matters more than the
table's contents. A discriminator row that exists only in the table settles it:

    psql -d lumberroom_gate2 -c "INSERT INTO sensitivity_default (tenant_id, pattern, sensitivity)
      VALUES ('me','project:vault','private') ON CONFLICT ... DO UPDATE SET sensitivity='private';"
    INSERT 0 1

BOOT A, `SENSITIVITY_DEFAULTS` unset:

    {"message":"classification table","source":"sensitivity_default table","rules":"personal:finance=private,...,project:vault=private,project:*=open,user:me=open,global=open,*=open"}
    write --namespace project:vault -> {"id":"4c5f145e-...","sensitivity":"private"}

BOOT B, `-e SENSITIVITY_DEFAULTS=`, present and empty, the exact shape docker-compose.yml passes
through as `${SENSITIVITY_DEFAULTS:-}`:

    {"message":"classification table","source":"sensitivity_default table","rules":"...,project:vault=private,..."}
    write --namespace project:vault -> {"id":"9662c649-...","sensitivity":"private"}

BOOT C, `-e SENSITIVITY_DEFAULTS='project:vault=open,*=open'`:

    {"message":"classification table","source":"SENSITIVITY_DEFAULTS","rules":"project:vault=open,*=open"}
    write --namespace project:vault -> {"id":"3aa5a5e1-...","sensitivity":"open"}

BOOT D, `-e KEK_PROVIDER=none`, table unchanged:

    {"level":"WARN","message":"a namespace defaults to private but KEK_PROVIDER=none, so writes to it will be refused rather than stored unencrypted"}
    write --namespace personal:finance ->
    lumberroom: memory_write failed: this content classifies as private and no encryption key is configured.
    Set KEK_PROVIDER and KEK_PATH, or write it to a namespace that defaults to open. Storing it in
    plaintext is not an option this server takes.

The variable wins when it holds rules. The table wins when the variable is silent, and
present-and-empty counts as silent, which is the compose regression that would have made the axis
inert on a default install. A private namespace with no key configured warns at boot and refuses the
write rather than storing plaintext. `seeded()` in `src/domain/policy.rs` is the last resort under an
empty table, and no boot here ran against an empty table, so that arm has no evidence. The runner
deleted the discriminator row afterwards.

`grep -c SENSITIVITY_DEFAULTS .env` returns 0, and expanding `[${SENSITIVITY_DEFAULTS-UNSET}]` after
sourcing `.env` prints `[UNSET]`, which is what makes the table the thing under test above.

### The raw row holds no plaintext

The private row written by hand for the classification check above, before any gate script ran,
read straight out of Postgres:

    psql -d lumberroom_gate2 -x -c "SELECT id, namespace, sensitivity, content, enc_alg, kek_id,
      length(content_ct), encode(content_ct,'hex'), encode(content_nonce,'hex'),
      encode(dek_wrapped,'hex'), source_client FROM memory WHERE namespace='personal:finance';"

    id            | 3f092442-04a5-4cbe-9342-5e685ff04689
    namespace     | personal:finance
    sensitivity   | private
    content       |                                   <- NULL
    enc_alg       | aes-256-gcm/envelope-v1
    kek_id        | kek-1
    ct_len        | 102
    ct_hex        | f5a79df7ad67148ae539b1b7a405ecb8f6a9cf53c0c6df946b455dfb34d7e5d7...
    c_nonce       | ed228e78a5582f2f5f52016f
    dek           | 869262118387788b4bf2972a389d662a4e2f8273d88493cd75c3c9ac788d7583aa1bd8eac89fe1da60ff6737088a7722
    source_client | claude-code-full

`content` is NULL, the ciphertext is 102 bytes, and the wrapped DEK is 48. Then the whole row, every
column including the embedding, cast to text and searched for the test marker and the content words:

    psql -tAc "SELECT m::text FROM memory m WHERE namespace='personal:finance';" > rawrow.txt   # 10031 bytes
    grep -c e0a9a3c2900e rawrow.txt                -> 0
    grep -ci 'household\|reserve fund' rawrow.txt  -> 0

The 12-byte AES nonce is in that row, as it must be. The plaintext and the marker are not. The
lexical axis is where this codebase's known leak would be, so the runner checked it:

    psql -tAc "SELECT sensitivity, to_tsvector('english', coalesce(content,'')) FROM memory WHERE namespace='personal:finance';"
    private|

An empty tsvector, and `\d memory` shows why by construction:

    "memory_content_fts" gin (to_tsvector('english'::regconfig, content)) WHERE sensitivity = 'open'::text

### Ceilings decide who reads it

    LUMBERROOM_TOKEN=<claude-code-full>   # read [{namespace:"*", max:"sealed"}], sealedCapable
    node bin/lumberroom.mjs search "household reserve fund reference" --json
    hits: [{"ns":"personal:finance","sens":"private","content":"the household reserve fund reference is GATE2-e0a9a3c2900e, tracked fo..."}]

    LUMBERROOM_TOKEN=<claude-code-mac>    # read ["*"] resolves to {namespace:"*", max:"open"}
    { "also_searched": ["personal:finance"], "hits": [], "namespaces": ["user:me","global"] }

    LUMBERROOM_TOKEN=<chatgpt-narrow>     # no grant on personal:finance at all
    { "also_searched": [], "hits": [], "namespaces": ["user:me","global"] }

The open-ceiling credential's grant covers the namespace through the `*` pattern, so the ceiling is
the only thing that stopped it. The narrow credential is not told a namespace was skipped.

A `credentials:` namespace refuses plaintext and points at the client-side path:

    node bin/lumberroom.mjs write "AKIA-GATE2-... placeholder access key material" --namespace credentials:aws
    lumberroom: memory_write failed: credentials:aws holds client-encrypted items and memory_write takes
    plaintext. Use `lumberroom seal <key> --namespace credentials:aws`, which encrypts on the client and
    stores the ciphertext by key. This server holds no key for it and never will.

    SELECT count(*) FROM memory WHERE namespace LIKE 'credentials:%';  -> 0
    SELECT count(*) FROM sealed_item;                                  -> 0

Over `/mcp` that refusal arrives as HTTP 200 with `isError: true`, which is the MCP contract. A
sealed read the ceiling does not admit answers 403 and says which rule refused it:

    curl -H 'authorization: Bearer <claude-code-mac>' '/admin/sealed?namespace=global&key_hmac=deadbeef...'
    {"detail":"client claude-code-mac has no namespace among the ones named whose ceiling reaches sealed. A sealed read needs a ceiling of sealed on the namespace holding the item, and a grant at open or private does not admit it however the item was named.","error":"unseal_failed"}
    [status 403]

---

## Phase 4: corrections

    LUMBERROOM_URL=http://127.0.0.1:8799 LUMBERROOM_TOKEN=<claude-code-mac> ./scripts/correction-test.sh
    exit 0     13 PASS lines, 0 FAIL, "correction-test PASSED"

13 PASS in both gates, with no script change needed in either. The first gate's verbatim output
carries the row ids:

    2/6 write the corrected version with supersedes, assert the correction is accepted
      PASS  written: bb58677d-0945-438b-b84c-a7367b34faca, and the write reports it superseded d8c1e4e7-4cf7-4521-a1db-2dada734eab7
    3/6 search for the question: the new value is there, the old one is not
      PASS  the answer contains the new value and does not contain the old one
    4/6 the old row survives, with superseded_by set: history was not deleted
      PASS  the old row (d8c1e4e7-...) is still in the database, superseded_by is bb58677d-...
    5/6 the numeric guard: near-identical text with different digits must not collapse
      PASS  wrote the port-8787 fact: 8e5324bd-b1dc-428b-ab00-56d3a3e68f7d
      PASS  the port-8080 write is a new row (1b5f853c-571b-4542-b59d-1554a87af17c), not a collapse into 8e5324bd-..., despite the two texts differing by one digit run
      PASS  the old port fact came back as a possible_conflicts candidate on the new write
    6/6 resolving the flagged conflict through 'lumberroom supersede' behaves exactly like an inline correction
      PASS  lumberroom supersede 8e5324bd-... 1b5f853c-...: 8e5324bd-... is now superseded by 1b5f853c-...
      PASS  after resolving the conflict, search answers 8080 and not 8787
      PASS  the retired port-8787 row is still in the database with superseded_by set to 1b5f853c-...

Two texts differing by one digit run stayed two rows. The older one came back flagged as a possible
conflict, and resolving it through `lumberroom supersede` left the same end state as an inline correction:
the new value answers, the old one does not, and the retired row survives with `superseded_by` set.

---

## The KEK round trip

Both legs ran in the first gate, against `lumberroom_verify`, with the key file bind-mounted read-only at
`/run/secrets/lumberroom-kek`, the same target docker-compose.yml uses.

    docker run --rm -v "$PWD:/app" lumberroom-builder /app/target/debug/lumberroom-server generate-kek > kek.hex
    exit 0        # 64 hex characters on stdout

First boot with `KEK_PROVIDER=file` sealed the store and `/readyz` said so:

    {"level":"INFO","message":"recorded the encryption key for this store","kek_id":"kek-1","provider":"file"}
    {"auth_mode":"oauth",...,"kek_provider":"file","kek_verified":true,"ok":true}

    SELECT * FROM kek_state;
    tenant_id   | me
    kek_id      | kek-1
    fingerprint | 01f3a7d98a612e9decf7d68e6ab67e69
    provider    | file

**Same key, after a restart.** The runner wrote a private row, removed the container, and started it
again on the same key file. The row still opened:

    {"level":"INFO","message":"encryption key verified","kek_id":"kek-1"}
    node bin/lumberroom.mjs search "KEKROUNDTRIP-cfe627082cb8" --json
    "content": "the vault combination is KEKROUNDTRIP-cfe627082cb8",
    "id": "7c045784-82ee-4175-8088-65ab2e165e68",
    "sensitivity": "private",

**A different key, caught at boot.** A second `generate-kek` file against the same store:

    lumberroom-server verify-kek
    kek_id:       kek-1
    fingerprint:  bb5218913e7558abfa600eb5a462078a
    verified:     NO
    This store was sealed under kek-1, which is a different key. Private writes are refused and
    existing private rows will not open under the configured key. Restore the original key.
    exit 3

    {"level":"ERROR","message":"the configured KEK is not the key this store was sealed with; private writes stay refused and existing private rows will not open..."}
    /readyz -> {...,"kek_provider":"file","kek_verified":false,"ok":true}

    node bin/lumberroom.mjs write "a fact that must not be stored under the wrong key" --namespace project:vault
    lumberroom: memory_write failed: this content classifies as private and the encryption key was not
    verified at boot. Check the server log for the KEK fingerprint mismatch before writing private
    content.

    node bin/lumberroom.mjs search "KEKROUNDTRIP-cfe627082cb8" --json
    # the private row is absent from hits; only open rows come back

`kek_id` is a label the operator sets, so both keys report `kek-1`. The fingerprint distinguishes
them, and it did.

---

## Health, readiness and boot

    node bin/lumberroom.mjs doctor
    endpoint: http://127.0.0.1:8799/mcp
    healthz:  200 {"name":"lumberroom","ok":true,"version":"0.1.0"}
    readyz:   200 {"auth_mode":"token","db_ms":8,"embedder":"Xenova/bge-base-en-v1.5@q8",
                   "embedder_degraded":false,"embedding_dim":768,"kek_provider":"file",
                   "kek_verified":true,"ok":true}
    credential: static token
    whoami:   200 {"client":"claude-code-full",...,"mode":"token","read":[{"max":"sealed","namespace":"*"}],"sealed_capable":true,...}
    server auth mode:     token
    credential auth mode: token
    tools:    context_bootstrap, memory_forget, memory_search, memory_write, registry_get
    all checks passed
    exit=0

Boot order, from the second gate's log:

    {"message":"starting","auth_mode":"token","dcr_enabled":false,"embed_provider":"Local","kek_provider":"file","tenant":"me","clients":"[\"claude-code-mac\", \"claude-code-full\", \"chatgpt-narrow\"]"}
    {"message":"migrations up to date"}
    {"message":"schema checked","embedding_dim":768}
    {"message":"classification table","source":"sensitivity_default table","rules":"personal:finance=private,personal:health=private,credentials:*=sealed,project:*=open,user:me=open,global=open,*=open"}
    {"message":"recorded the encryption key for this store","kek_id":"kek-1","provider":"file"}
    {"message":"embedder ready","id":"Xenova/bge-base-en-v1.5@q8","degraded":false}
    {"message":"listening","addr":"0.0.0.0:8799","path":"/mcp"}

Nothing accepts traffic until the model has produced a real vector, so the first tool call a model
makes is a fast one. The embedder is the real bge-base-en-v1.5 at q8 in every run above, with
`EMBED_ALLOW_FALLBACK=false`, not a hash fallback.

In token mode the server advertises no authorization server, and unauthenticated calls are refused:

    curl -si localhost:8787/mcp -X POST -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
    401  WWW-Authenticate: Bearer

    curl -s -o /dev/null -w '%{http_code}\n' localhost:8787/.well-known/oauth-protected-resource
    404

`/statsz`, `/admin/whoami` and `/admin/registry` all return 401 unauthenticated.

## Latency, from Phase 1

PRD §5 puts the bootstrap budget near 200 ms. Server-side, from the `tool_calls` table on the Phase 1
store, which held tens of rows:

```
  memory_search        14 calls     4 unprompted  p50  44ms  p95 238ms
  context_bootstrap    12 calls     1 unprompted  p50   4ms  p95  30ms
  memory_write          7 calls     4 unprompted  p50 184ms  p95 197ms
  registry_get          1 calls     0 unprompted  p50   5ms  p95   5ms
```

`context_bootstrap` does no embedding work and is served from a 30 second cache, hence the 4 ms
median. Searches and writes pay for one embedding pass, 11 to 16 ms of model time plus process
overhead. End to end through `lumberroom`, including Node startup, a bootstrap takes about 160 ms. Neither
gate re-measured latency, and no measurement exists at a store size worth the name.

## Instrumentation

`unprompted` is the number that matters (PRD §7). A call arriving without an `X-Memory-Invocation`
header counts as model-initiated, and `lumberroom` and the SessionStart hook always send one. Both
directions were seen over the wire in Phase 1:

```
       tool        |     client      | unprompted | count
-------------------+-----------------+------------+-------
 context_bootstrap | claude-code-mac | f          |     6   -- hook and cli
 memory_search     | claude-code-mac | t          |     1   -- model chose to
 memory_search     | claude-code-mac | f          |     1
 memory_write      | claude-code-mac | f          |     3
```

Policy-test step 6 closes the other half: both denials, the narrow credential's refused write and the
tripwire refusal, show up in `lumberroom stats` as `memory_write` failures with the counter moving.

## Two protocol bugs the CLI cross-check caught

Keeping `lumberroom` as dependency-free JavaScript over HTTP was deliberate: it is a client the rewrite
could not accidentally accommodate. Pointed at the Rust server it returned

```
422: Unexpected message, expect initialize request
```

`rmcp` defaults to legacy session mode, which rejects a client that posts `initialize` and a tool call
as two independent requests. Reading the crate source found why it matters. Per SEP-2567, sessions
were removed from the protocol in the 2026-07-28 revision, which is the version Phase 1 PRD §3 asks
for. The old service negotiated 2025-06-18 and got sessions by accident. Both sides now use
2026-07-28.

The second: the tools returned

```
context_bootstrap failed: request reached a tool without an authenticated client
```

That is the fail-closed guard working. `rmcp` does not copy axum request extensions onto the tool
context. It injects the whole `http::request::Parts`, and middleware state lives one level in.
Guessing an identity there would have been the wrong recovery.

## Deploy scripts, from Phase 1

- `deploy/install.sh` ran in a clean `ubuntu:24.04` arm64 container, `--dry-run` and for real with a
  stubbed Docker, through secret generation, `.env` templating, the firewall branch and the backup
  cron. The real run stops at the readiness poll, as it should when nothing is listening.
- `client/wire-mac.sh` ran against a throwaway `HOME`. It registered the MCP server, appended its hook
  to a `settings.json` that already had one without disturbing it, wrote the `CLAUDE.md` block, and
  passed its own verification. The real `~/.claude` was not touched.
- `deploy/backup.sh` produced `backups/memory-2026-08-19.sql.gz` at mode 600, containing
  `COPY public.memory`, `registry`, `tool_calls` and `schema_migrations`, with the row data present.

---

## What is NOT verified

Read this at the same weight as everything above.

- **The release image has no acceptance evidence of its own.** Every result in this file came from the
  debug binary through the builder image. The runner rebuilt the release image with LTO during the
  first gate to recover from the migration incident, not to gather evidence, and the only image-level
  checks that ran against it are `/healthz`, `/readyz`, `doctor` and `tools/list`. No OAuth, policy, correction or
  KEK result describes the shipped binary.
- **The image on 8787 predates the four Finding fixes**, proven above. Until the lead rebuilds it and
  recreates the container, this file describes code the owner is not talking to.
- **Nothing is deployed.** No VM, no certificate issued by Let's Encrypt, and no hosted MCP client,
  browser or mobile, has seen any of this. Claude Code's fallback probing masks a whole class of
  metadata bug, so a green result from it says nothing about what a browser surface will do with the
  discovery documents. The 43 PASS lines came from curl, which is the point of the script, and curl is
  not a browser client either.
- **OIDC mode has never run against a live Logto tenant.** `AUTH_MODE=oidc` is covered by tests
  against a configured issuer, and the switch procedure in `deploy/logto.md` is written from Logto's
  documentation rather than from a run.
- **The dedupe and conflict thresholds are uncalibrated guesses.** `DEDUPE_THRESHOLD` is 0.97 and
  `CONFLICT_THRESHOLD` is 0.90, both design targets picked before any real data existed. The guard's
  own boundaries are guesses too, and it is English-only. Calibration needs a few hundred real rows
  and a person reading the pairs above 0.85.
- **The recall monitor's figures are withdrawn and it has not been re-run.** See the section above.
  It also samples open rows only, because the repository cannot read a private row, and nothing that
  renders its report says so.
- **No load testing.** The recall monitor and `lumberroom stats` have never run against a store larger than
  a few dozen rows, so index behaviour at size is unmeasured here. The one number at scale is the 40k
  truncation reproduction, which is a research finding rather than a monitor reading. `conflicts()` is
  O(n²) with no index able to help, seconds at a few thousand rows, and unmeasured.
- **The done-when test has not been re-run against current source.** Its transcript above is from the
  earlier build.
- **`src/domain/policy.rs` was not diffed.** Its mtime is later than the binary the fix agent tested
  and later than every file in its own change list. The suite compiled whatever is on disk and passed.
  Worth a look before committing.

## Residue the runs left behind

`oauth-flow-test.sh` registers a client per run through dynamic registration, and the first gate ran
it against the live `lumberroom` database three times. Those three rows are still there, along with a
refresh token live for `OAUTH_REFRESH_TTL_SECS`, 60 days by default, on the `full` profile. The value
never left this machine. The rows should still go, and the sandbox refused the statement, so this is
owed:

    docker compose exec -T db psql -U lumberroom -d lumberroom -c "
    DELETE FROM oauth_token   WHERE client_id IN (SELECT client_id FROM oauth_client WHERE client_name LIKE 'oauth-flow-test-%');
    DELETE FROM oauth_refresh WHERE client_id IN (SELECT client_id FROM oauth_client WHERE client_name LIKE 'oauth-flow-test-%');
    DELETE FROM oauth_code    WHERE client_id IN (SELECT client_id FROM oauth_client WHERE client_name LIKE 'oauth-flow-test-%');
    DELETE FROM oauth_client  WHERE client_name LIKE 'oauth-flow-test-%';"

The `oauth-flow-test-` prefix is set by the script, so the pattern touches nothing else. Every run
against a real deployment leaves a client behind, and the script does not clean up after itself.

Neither gate wrote a memory, registry or sealed row to the live store. Telemetry is the exception: the second gate's refused probe against 8787 left one `tool_calls` row, because recording
denials is the contract policy-test step 6 asserts.

    SELECT * FROM tool_calls WHERE created_at > now() - interval '30 minutes';
    client     | claude-code-mac
    tool       | memory_write
    succeeded  | f
    unprompted | t
    namespace  | personal:finance

## LongMemEval-S retrieval, 20 August 2026

The first retrieval number this project has that survives. Run through the real `memory_write` and
`memory_search` tools against a scratch server, 500 questions, 23,867 sessions, 1,023 seconds.
Configuration: session-as-document, scoped, embedder `all-MiniLM-L6-v2@q8`, depth 20.
Report: `docs/results/longmemeval-scoped-20260820.json`. Reproduce with
`./scripts/eval-longmemeval.sh --dataset <path>`.

| metric | lumberroom | agentmemory, published | delta |
|---|---|---|---|
| recall_any@5 | 93.2% | 95.2% | -2.0 |
| recall_any@10 | 96.0% | 98.6% | -2.6 |
| recall_any@20 | 98.4% | 99.4% | -1.0 |
| NDCG@10 | 83.0% | 87.9% | -4.9 |
| MRR | 83.3% | 88.2% | -4.9 |

By question type, against their published per-type table. Counts match theirs exactly, so the
question set is the same.

| type | n | lumberroom R@5 | theirs | delta |
|---|---|---|---|---|
| multi-session | 133 | 96.2% | 97.7% | -1.5 |
| temporal-reasoning | 133 | 91.7% | 95.5% | -3.8 |
| knowledge-update | 78 | 98.7% | 98.7% | 0.0 |
| single-session-user | 70 | 84.3% | 90.0% | -5.7 |
| single-session-assistant | 56 | 98.2% | 96.4% | +1.8 |
| single-session-preference | 30 | 83.3% | 83.3% | 0.0 |

Run quality: 0 questions whose haystack failed to store, 0 sessions never stored, 0 hits carrying an
id no session owned. 8 questions of 500 found no gold session in the top 20.

**Read the gap in the right place.** The ordering gap (NDCG and MRR, both -4.9) is more than twice
the surfacing gap (recall@5, -2.0). Their fusion combines ranks through RRF, so BM25 and vectors
carry comparable weight. lumberroom adds raw scores: measured on live data, a strong three-term match
scores `ts_rank` 0.259, which the 0.35 weight turns into 0.091 against a cosine near 0.7 at weight
1.0. The lexical arm supplies candidates and barely orders them.

**What this does not measure.** Ranking on synthetic chat, and nothing else. Supersession, policy,
the registry and provenance are where lumberroom's value sits and none of them appear here. The stacks
also differ: their lexical side stems, expands synonyms and matches prefixes. Decision 0007 states
what the gate is and is not for.

**Two other configurations, same 25-question slice.** `--isolate` reproduces their fresh index per
question and scored identically to scoped, so the namespace filter costs nothing at 1,325 rows.
`--corpus-wide` drops the filter and fell from 80.0% to 64.0% R@5, with 406 retrieved rows
belonging to other questions. That is the distractor cost and it is the number that will move as a
corpus grows.

### Chunked protocol, 100 questions, 20 August 2026

Same 100 questions, same embedder, same everything except how a session becomes rows. Chunked cuts
each session on turn boundaries at 2,000 characters; session-as-document stores the whole transcript
as one row. `docs/results/` holds neither; the run is reproducible with `--protocol chunked
--limit 100`.

| metric | chunked | session-as-document | agentmemory, published |
|---|---|---|---|
| recall_any@5 | 96.0% | 88.0% | 95.2% |
| recall_any@10 | 98.0% | 93.0% | 98.6% |
| recall_any@20 | 98.0% | 98.0% | 99.4% |
| NDCG@10 | 90.5% | 78.0% | 87.9% |
| MRR | 89.7% | 75.8% | 88.2% |

**recall@20 does not move and everything else does.** Candidate generation was never the problem, so
chunking changed nothing about what reaches the top twenty and a great deal about the order inside
it. That is the hypothesis the rank distribution predicted, confirmed: the median rendered session
runs 10,506 characters and the embedder's window covers roughly the first 2,000, so a whole-session
row was ranked on text that often did not contain the answer.

35,664 rows, 0 write failures, 0 sessions never stored, 0 hits with no owner, 68ms median search.

This is not comparable to agentmemory's published run and the column is there for scale rather than
for a claim: their harness stored one document per session, so the fair comparison to their 95.2% is
the session-as-document column at 88.0%.

### Phase 7 changed no retrieval number, and rank fusion moved one, 20 August 2026

Two full 500-question runs against the phase-7 build, session-as-document, scoped, on a fresh
`lumberroom_eval` each time. Both clean: 0 write failures, 0 sessions never stored, 0 unmapped hits.

| metric | before phase 7 | phase 7, linear | phase 7, rrf |
|---|---|---|---|
| recall_any@5 | 93.2% | 93.2% | 93.6% |
| recall_any@10 | 96.0% | 96.0% | 96.0% |
| recall_any@20 | 98.4% | 98.4% | 98.4% |
| NDCG@10 | 83.0% | 83.0% | 83.6% |
| MRR | 83.3% | 83.3% | 84.5% |

**Phase 7 is neutral to the digit, which is the result it was designed for.** The recency term ships
at weight zero, an as-of read only happens when a caller asks for one, and the benchmark records no
aliases. Valid time added two columns, a capability and four routes without touching ranking. Read
the middle column as a regression check that passed rather than as a feature that failed.

**Rank fusion buys about a point of MRR and almost no recall.** +1.2 MRR, +0.6 NDCG@10, +0.4
recall@5, and nothing at all at 10 or 20. That is the same shape the 100-question 2x2 showed: RRF
reorders rows already retrieved and finds nothing new. It is smaller here than on the 100-question
slice, where it was worth +2.0 recall@5, so the earlier figure was flattered by a sample weighted
towards one question type.

`SEARCH_FUSION` still defaults to `linear`. A point of MRR on a benchmark whose documents are chat
sessions is not enough to move production ranking for a store that holds one-sentence facts.
