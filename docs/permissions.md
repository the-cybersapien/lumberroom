# Permissions: writing an AUTH_TOKENS grant

## The asymmetry that bites

An absent `read` list means unrestricted, and unrestricted implies `sealedCapable`: the owner's
own client, holding nothing back. It never implies `mayDelete`, `mayIngest`, or `mayReadHistory`.
Those three read the grant field alone, with no unrestricted-read shortcut.

That gap produced a real 403. On 21 August 2026 the owner's credential carried
`"read":[{"namespace":"*","max":"sealed"}]` and no `mayDelete`, and `lumberroom forget` refused it. The
grant looked total because the read ceiling was total. `effective_may_delete` does not consult
`read` at all:

```rust
pub fn effective_may_delete(&self) -> bool {
    self.may_delete
}
```

Compare `effective_sealed_capable`, which does:

```rust
pub fn effective_sealed_capable(&self) -> bool {
    self.sealed_capable || self.read.is_none()
}
```

The reasoning in `ClientGrant`'s own comments: an unrestricted grant is the operator's own client
by definition, so it gets sealed capability for free. Delete, ingest, and history are each an
action the owner grants by name, on his own client too, because a client that can silently act
(delete a memory, fill an approval queue, read a retired fact that was corrected for a reason) is
a worse failure than one that only reads too much. Fix a `lumberroom forget` 403 on your own credential
by adding `"mayDelete":true`, not by widening `read`; widening `read` does not touch this flag at
all.

## The JSON shape

`AUTH_TOKENS` is a JSON array of these objects, one per static bearer client.

```json
[
  {
    "client": "claude-code-mac",
    "token": "CHANGE_ME_openssl_rand_hex_32",
    "read": [{"namespace": "*", "max": "sealed"}],
    "write": [{"namespace": "*", "max": "sealed"}],
    "sealedCapable": true,
    "mayDelete": true,
    "mayIngest": false,
    "mayReadHistory": true,
    "registryWrite": true
  },
  {
    "client": "browser-widget",
    "token": "CHANGE_ME_second_token",
    "read": ["user:me", "global"],
    "write": []
  }
]
```

The first is a full owner credential: sealed everywhere, every capability except ingest. The
second is narrow: open-ceiling read on two namespaces (a bare string means `max: "open"`), no
write, and every capability flag left at its default of `false`.

## Every field

- **`client`** (required, string). The stable identifier recorded as `source_client` on every row
  this credential touches and on every tool call. Wrong value: memories written by this client
  attribute to the wrong name, and nothing else breaks.

- **`token`** (optional, string). The bearer value presented in the `Authorization` header. Absent
  or blank makes the entry inert; it matches no request. There is no default that makes a token
  work without one.

- **`read`** (optional, array of namespace grants). Absent means unrestricted at every level: this
  is the shape reserved for the owner's own client. An explicitly empty array, `[]`, means exactly
  what it says, no readable namespace, and must never be treated as equivalent to absent. See "The
  two axes" below for what an entry in the array looks like.

- **`write`** (optional, array of namespace grants). Same shape and same absent/empty distinction
  as `read`, evaluated independently. A client can read broadly and write narrowly, or the
  reverse.

- **`registryWrite`** (default `false`). The registry holds credential locations (see
  `docs/decisions/`), so writing to it is an operator action, not a default a namespace grant
  implies. Opens `registry_set` and `alias_set`. Wrong (left `false` for a client that needs it):
  every `registry_set` and `alias_set` call 403s regardless of the namespace grants.

- **`sealedCapable`** (default `false`, but see the asymmetry above). Asserts the client can
  decrypt sealed content locally. A client holding a sealed ceiling without this flag still only
  ever receives ciphertext, never plaintext, so the ceiling alone does not leak sealed rows to a
  client that cannot open them.

- **`mayDelete`** (default `false`). A client that can silently delete memories is a worse failure
  than one that hoards them, which is why this reads only the explicit flag. Opens
  `memory_forget`.

- **`mayIngest`** (default `false`). Gates the ingest routes and the `/admin/cleanup/*` routes:
  `admin_cleanup_run`, `admin_cleanup_list`, `admin_cleanup_post`, `admin_cleanup_show`,
  `admin_cleanup_apply`, `admin_cleanup_reject`. A client that can post proposals can fill the
  queue the owner has to read, and a queue he stops reading is an approval gate in name only, so
  cleanup rides the same flag as ingestion rather than a separate one. No MCP tool sits behind
  this; the routes are HTTP-only, for `lumberroom ingest` and the process the owner runs by hand.

- **`mayReadHistory`** (default `false`). Whether this client may read facts that no longer hold.
  A retired fact can be more revealing than the one that replaced it, so this is off unless the
  grant says otherwise, even for a client with a full read ceiling. Opens `memory_history` and
  `registry_history`.

## The two axes

A namespace grant is not a plain list of strings; each entry is a namespace glob paired with a
sensitivity ceiling. Namespace answers whose facts, sensitivity answers how exposed. The three
levels, low to high: `open < private < sealed`.

Two shapes both parse:

```json
"read": [
  "user:me",
  {"namespace": "personal:finance", "max": "private"},
  {"namespace": "*", "max": "sealed"}
]
```

A bare string, `"user:me"`, means a ceiling of `open` on that namespace. That default exists so a
grant written before the sensitivity axis landed never silently gains access to private or sealed
rows the day the axis ships; every Phase 1 grant kept meaning exactly what it said. An object
without a `max` field means the same thing, `open`, for the same reason.

When more than one entry matches a namespace, the ceiling is the highest of them, not the lowest:
two grants touching the same namespace is the caller having been granted both, and the more
generous one is what was actually granted.

The filter this produces runs inside the query, never as a pass over results after the fact. A row
a client's ceiling does not admit must never enter that client's process, because filtering after
retrieval is a leak waiting on the one code path that forgets to filter.

## Which tools each capability opens

| Tool | Capability required | Grant field |
|---|---|---|
| `context_bootstrap` | Open | none |
| `memory_search` | Open | none |
| `memory_write` | Open | none |
| `registry_get` | Open | none |
| `alias_list` | Open | none (namespace-filtered inside the call) |
| `memory_forget` | MayDelete | `mayDelete` |
| `memory_history` | MayReadHistory | `mayReadHistory` |
| `registry_history` | MayReadHistory | `mayReadHistory` |
| `registry_set` | RegistryWrite | `registryWrite` |
| `alias_set` | RegistryWrite | `registryWrite` |

Open still means every authenticated client, not every request: the namespace and sensitivity
ceilings apply inside the call regardless of which capability gated the tool's visibility.
`registryWrite` carries `alias_set` along with `registry_set` because an alias is a naming fact of
the same class as a registry key; nobody gains alias-write who did not already hold the
higher-trust flag, and no client gains any capability it was not granted by name. This table is
`TOOL_CAPABILITIES` in `src/mcp/capability.rs`, held against the router's own tool list by a test
there so a tool added without an entry fails the build instead of shipping ungated.

## Setting AUTH_TOKENS without breaking the JSON

`.env` is parsed by Docker Compose itself, so a single-quoted JSON array there is fine:

```
AUTH_TOKENS='[{"client":"claude-code-mac","token":"...", ...}]'
```

Sourcing that same file through `sh` (`source .env`, or a shell script that reads it) strips the
double quotes and hands the process invalid JSON, because `sh` does not know the value is meant to
stay a JSON string. Keep the single quotes; do not `source .env` and expect the token config to
survive it.

## Issuing a client from the console

`/console/clients` creates an OAuth client without touching `.env` and without a restart. It writes
to `oauth_client` beside the clients that register themselves, so nothing here changes how
`AUTH_TOKENS` works: a credential you put in `.env` keeps working exactly as it did.

Pick one of four shapes and the form fills the grant in:

| shape | reads | writes | capabilities |
|---|---|---|---|
| Read only | `*@sealed` | nothing | none |
| Read and write | `*@sealed` | `*@open` | none |
| Ingest bot | nothing | nothing | `mayIngest` |
| Full | `*@sealed` | `*@sealed` | everything except `mayDelete` |

**No shape grants `mayDelete`**, and a test holds that. Deletion is reachable only by opening the
advanced view and ticking it, which is the point: a client that can silently remove a memory is a
worse failure than one that hoards them.

The advanced view **replaces** the shape rather than merging with it. Tick its own box and every
field below applies, including the ones you left unticked. Without that rule a checkbox nobody
expanded would silently clear a capability the shape granted.

Two things worth knowing before you use it:

**Creating a client consents to it.** `set_client_grant` writes `consented_at` with the grant, and
an owner filling in this form behind his own password has approved it more directly than the consent
screen asks. A client that registers itself still waits at that screen, because nobody approved that
one yet.

**A client secret is shown once.** Tick "Issue a client secret" and the page displays it at
creation; the store keeps only `hash_token` of it. Losing it means issuing another client. That is
stricter than `AUTH_TOKENS`, which holds its tokens in plaintext, and deliberately so: a console
reachable from a browser must not be a place every credential can be read out of. Leave it unticked
for anything running in a browser, which cannot keep a secret and is bound by PKCE instead.

## Changing a grant

For a static bearer client, edit its entry in `AUTH_TOKENS` in `.env` and restart the server;
`AUTH_TOKENS` is read once at boot and there is no live-reload path for it.

An OAuth client is different: its grant is a row in `oauth_client`, not a line in `.env`, and it
changes without a restart. `docs/decisions/0003-grants-in-the-database.md` records why: a
dynamically registered client does not exist until it registers, while the server is already
running, so it cannot live in an environment variable read once at boot. Authority follows the
credential, and the two never copy into each other: a static token's grant lives only in
`AUTH_TOKENS`, an OAuth client's grant lives only in its database row.
