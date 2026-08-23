# Research — encryption, and what each sensitivity level actually protects

Commissioned for Phase 3, August 2026. The headline: the system PRD's account of `private` is an
understatement, and correcting it changes the design rather than just the wording.

---

## Threat model

Four attackers. "Encrypted" means nothing without saying encrypted from whom.

| | Attacker | Capability | Realistic here |
|---|---|---|---|
| A | Stolen disk, snapshot, or backup file | Bytes at rest. No running process, no keys in RAM | **Yes.** A misconfigured snapshot or a backup file synced somewhere it should not be is the most likely way this data ever leaves the box |
| B | Live compromise of the running box | Anything the Postgres and Node processes can read, including decrypted content and any key held in memory | Plausible for any always-on internet-facing host. No on-box encryption scheme defeats it |
| C | A misused tool credential | Whatever that grant carries, and no more | The reason namespace and sensitivity grants exist |
| D | Cloud provider insider, or a compromised OCI account | Console and API access to the tenancy | Low likelihood. The design does not defend against it and should say so |

Mapped onto the three levels: **open** defends nothing by design. **private** defends against A
only, and partially. **sealed** defends against A and B, at the cost of being unsearchable and
unreadable outside local clients.

---

## 1. How much does the index leak? The PRD claim, checked

**Verdict: true, but too soft. This needs a design change, not a wording change.**

### The embedding

Morris et al., *Text Embeddings Reveal (Almost) As Much As Text* (EMNLP 2023,
[arXiv:2310.06816](https://arxiv.org/abs/2310.06816)) introduced Vec2Text: given only the embedding
and black-box query access to the model, an attacker iteratively refines candidate text toward the
target vector. **92% exact recovery of 32-token inputs**, BLEU 97.3. A 2025 reproducibility study
([arXiv:2507.07700](https://arxiv.org/pdf/2507.07700)) confirms the result holds, with recovery
degrading as inputs lengthen. The 92% is specifically a short-input number, which is exactly the
shape of content a personal memory store holds.

ALGEN ([arXiv:2502.11308](https://arxiv.org/pdf/2502.11308)) removes the remaining obstacle: an
attacker needs neither the victim's embedding model nor a large paired corpus, just a few example
pairs. Our model is `bge-base-en-v1.5`, a public download. Anyone holding a stolen database already
has everything needed to build an inverter offline, with no access to the live server.

Defenses do not hold up. Quantization, noise, shuffling and rotations are
[routed around or cost proportional retrieval quality](https://aclanthology.org/2025.acl-long.1185.pdf).
The one with real numbers, [STEER](https://arxiv.org/pdf/2507.18518), cuts BLEU below 5% for about
1% recall loss, but requires realigning the embedding space, which is a research lift rather than
something to bolt on.

### The tsvector, which is the part the PRD understates

Framing the embedding and the lexical index together as "leaking a good deal of meaning" buries the
real problem. **A Postgres `tsvector` is not an index that leaks the document. It is the document**,
stemmed and minus stopwords, with positions. Numbers, proper nouns and identifiers survive stemming
essentially intact. Recovering a usable paraphrase needs no model and no attack, just reading the
column.

This is the failure mode Naveed, Kamara and Wright documented for property-preserving encrypted
databases ([Inference Attacks on Property-Preserving Encrypted Databases](https://cs.brown.edu/people/seny/pubs/edb.pdf)),
and a `tsvector` is strictly worse than what they broke, because it is not an encrypted-then-indexed
value at all. It is a plaintext derivative sitting in the next column.

**So if the database is stolen today, reconstruction of a private memory from `embedding` plus
`tsvector` is not "a good deal of the meaning." It is close to total for short content, and it
requires no attack.**

### The consequence

Encrypting `content` while leaving a plaintext `tsvector` of that same content beside it is theatre
against precisely the threat `private` exists to stop. **Recommendation: `private` drops lexical
search and is semantic-only.** The embedding stays plaintext because search has to work and there is
no practical alternative (§5), but the residual leak is then "gist," not "verbatim," which is a
materially different and defensible claim.

### Backups leak the same way

Phase 1's plaintext `pg_dump` contains raw `embedding` and `tsvector` values for every row. Before
any encryption work lands, backup theft already exposes private content exactly as disk theft would.
The backup fix is not polish; it closes the same hole through a second door.

---

## 2. Recommended design per level

### open

Unchanged from Phase 1. Plaintext content, plaintext embedding, tsvector indexed. Defends nothing,
which is the point of having the level.

### private

```sql
CREATE TABLE kek_version (
  kek_id     text PRIMARY KEY,          -- a label. Key bytes are never stored in Postgres
  created_at timestamptz NOT NULL DEFAULT now(),
  retired_at timestamptz
);

ALTER TABLE memory
  ADD COLUMN sensitivity        text NOT NULL DEFAULT 'open'
      CHECK (sensitivity IN ('open','private')),
  ADD COLUMN content_ciphertext bytea,   -- set when sensitivity='private'; content stays NULL
  ADD COLUMN content_nonce      bytea,   -- 12-byte AES-GCM nonce
  ADD COLUMN wrapped_dek        bytea,   -- per-row DEK, wrapped by the KEK named in kek_id
  ADD COLUMN kek_id             text REFERENCES kek_version(kek_id);

-- The load-bearing change: no lexical index for private rows.
ALTER TABLE memory
  ADD COLUMN content_tsv tsvector
  GENERATED ALWAYS AS (
    CASE WHEN sensitivity = 'open' THEN to_tsvector('english', content) END
  ) STORED;
```

**Envelope encryption, per row rather than per namespace.** A fresh 256-bit DEK per row, content
encrypted with AES-256-GCM, the DEK wrapped by the current KEK. Per-row grain makes crypto-shredding
trivial (delete the row, the key is gone, which is what a governance delete needs) and makes KEK
rotation "rewrap a thousand small keys" instead of "re-encrypt gigabytes."

Write path: embed the plaintext, generate the DEK, encrypt, store ciphertext, nonce, wrapped DEK,
`kek_id` and the plaintext embedding. Read path: ANN search runs on plaintext embeddings exactly as
for open rows; matched rows have their DEK unwrapped with the in-memory KEK and are decrypted
transiently, then returned only if the caller's grant permits. That transient decryption is attacker
B's territory and is disclosed as such.

### sealed

The server never holds plaintext, not even transiently. A separate table, deliberately without any
of the search columns so nothing can quietly reintroduce them:

```sql
CREATE TABLE sealed_item (
  lookup_hmac   bytea PRIMARY KEY,   -- HMAC-SHA256(index_key, canonical_name), computed client-side
  tenant_id     text NOT NULL,
  namespace     text NOT NULL,
  ciphertext    bytea NOT NULL,      -- age, multi-recipient
  source_client text,
  created_at    timestamptz NOT NULL DEFAULT now(),
  supersedes    bytea REFERENCES sealed_item(lookup_hmac)
);
```

**Encryption:** [`age`](https://github.com/FiloSottile/age) with multi-recipient encryption. The
client encrypts once to the public keys of each trusted local agent, producing one ciphertext any of
them can open with its own private key. Better than a shared symmetric key because revoking a
machine means dropping it from future recipient lists, rather than rotating for everyone at once and
redistributing out of band.

**Client keys on a Mac:** an `age` identity file, permission-locked and ideally passphrase-protected,
or [`age-plugin-se`](https://github.com/remko/age-plugin-se) to back the identity with the Secure
Enclave behind a Touch ID prompt. Identity files are adequate at this scale; the Secure Enclave is
worth it only if you want decryption to require a physical confirmation.

**Lookup key:** HMAC-SHA256 of the canonical item name under a dedicated 32-byte index key shared by
the trusted clients, separate from any client's age identity. The server sees an opaque deterministic
tag and looks it up with `=`. Leakage is exactly equality, which is the minimum a blind index can
leak, and canonical names are effectively unique so the frequency analysis that breaks deterministic
encryption of low-cardinality values does not apply. Do not reach for anything fuzzier.

**Rotation, honestly:** adding or removing a client means decrypting every sealed row with a valid
recipient and re-encrypting to the new list. `age` has no rewrap without touching plaintext. At
personal scale that is a script run occasionally, not infrastructure to design. Same for rotating the
index key.

**Browser clients get ciphertext, permanently.** No key a browser holds is safe from anything else on
the page. State it as permanent rather than as a gap to close.

---

## 3. Key management on an Oracle Always Free A1

**No vTPM.** Oracle's [shielded instances documentation](https://docs.oracle.com/en-us/iaas/Content/Compute/References/shielded-instances.htm)
lists supported shapes, and Ampere A1 Flex is not among them; shielded instances are Intel-only. So
`systemd-cryptenroll --tpm2` and every TPM-sealed-LUKS design is unavailable. Do not design around a
TPM this box does not have.

**But the "no KMS budget" premise is wrong for this platform.** OCI Vault software-protected keys and
their API calls are free; only HSM-protected keys are billed. A real KMS is already sitting on the
deployment target. *(Verify current pricing before relying on it, since this claim carries the whole
key-management design.)*

**Recommended:**

- One OCI Vault, software-protected, holding the KEK.
- The app authenticates with an **instance principal**: short-lived, automatically rotated credentials
  tied to the instance's own identity, with no static credential file on disk.
- IAM scoped to a dynamic group matching *only this instance's OCID*, permitted to `use` and not
  `manage` exactly this one key. Instance-principal auth is itself a surface; tight scoping is what
  keeps the blast radius at "this VM" rather than "the tenancy."
- On start, unwrap the KEK into memory once. It lives in RAM for the process lifetime and is
  refetched on restart. No key material ever touches the boot disk.

This resolves the tension in the brief better than a TPM would have: **a stolen disk image contains no
KEK material, because the KEK was never on the disk.** It does not solve attacker B, and cannot: root
on the box can call the same Vault API with the same instance identity. That is the honest limit of
software-only key management for a single always-on host.

**Full-disk encryption:** with no vTPM, the options are a passphrase at every reboot (kills unattended
restart), [Tang and Clevis](https://access.redhat.com/articles/6987053) network-bound unlock (needs a
second always-on host you control, since Tang on the same disk protects nothing), or a keyfile on the
boot volume (protects nothing against a stolen image). **Recommendation: skip LUKS on root.** OCI
already encrypts volumes at rest with Oracle-managed keys as a baseline, and the two things that
actually needed encrypting here, private content and backups, are handled by column encryption and
encrypted dumps whose keys live off the disk.

**Do encrypt swap** with a random per-boot ephemeral key (`dm-crypt` plain mode, regenerated each
boot, never persisted). Zero key-management burden, and it closes the gap where decrypted content or
an in-memory DEK gets paged out.

---

## 4. Backups

```bash
pg_dump -Fc lumberroom | age -r age1<backup-recipient-pubkey> -o /backups/lumberroom-$(date +%F).dump.age
```

- The backup recipient's **private key lives only on the Mac**, plus one offline copy. Never on the
  box, never beside the dumps.
- It is a **different key from the KEK**. Conflating them means a Vault compromise also opens every
  historical backup.
- Because DEKs are wrapped in-row, an unencrypted dump alone no longer suffices to read private
  content; you would also need the KEK. The outer `age` wrap is still worth it, because it covers
  embeddings and open-row tsvectors.
- Sealed rows pass through unchanged. They are already ciphertext; the outer wrap is defence in depth.

**Disaster restore, box lost, only a backup file and a key:**

1. Fresh box, Postgres 16 with pgvector, schema recreated.
2. `age -d -i backup-identity.txt lumberroom-2026-08-19.dump.age | pg_restore -d lumberroom`
3. Vault usually survives, since it is a separate control-plane resource from the instance. If it
   does, nothing further is needed.
4. **If the Vault was also lost, private content is unrecoverable.** Wrapped DEKs are useless without
   the KEK. KEK escrow is therefore a requirement, not an edge case. See open questions.
5. Repoint the instance-principal policy at the new instance OCID.
6. Actually `SELECT` a known private row and confirm it decrypts. A bad key produces a restore that
   looks successful and is silently useless. **Schedule a quarterly restore drill**, because the
   backup is a hypothesis until it has been restored.

---

## 5. Searching encrypted content

| Approach | Status | Verdict here |
|---|---|---|
| FHE over vectors | Research. [Additive-HE inner product](https://arxiv.org/pdf/2503.05850) exists; encrypted ANN at index scale does not ship | Research toy at this scale |
| Partially homomorphic cosine | More practical for one comparison, but breaks the index structure, so it degenerates to an encrypted brute-force scan | Does not fit an ANN index. Skip |
| TEE / confidential computing | Real and shipping on some hardware | **Not available on the Always Free A1 shape.** Also shifts trust to the vendor's firmware rather than removing it |
| Property-preserving / searchable encryption | Shipped commercially, [known broken](https://cs.brown.edu/people/seny/pubs/edb.pdf) by frequency analysis for anything but high-cardinality uniform values | This is what a plaintext tsvector already gives an attacker. A fancier version of the same leak |
| **Server decrypts in memory** | What every practical deployed system does | **This is the answer** |

For a single-user system where the server is already the thing you trust to run the process, adding
cryptography so that the process reading the plaintext does not read the plaintext buys complexity
and bugs. The PRD's framing, that private trusts the server and does not trust the disk, is correct.
Phase 3 should not try to be cleverer than that.

---

## 6. Honest limitations, for user-facing docs

> **open** protects nothing beyond the namespace grant. Anyone with a grant, or anyone holding the
> database file, reads it in full.
>
> **private** protects a stolen database or backup: without the key, which is never stored beside
> the data, the content cannot be read. It does **not** protect against the server itself. To make
> private content searchable by meaning, the server stores a plaintext embedding of every private
> memory, and published research recovers the majority of short texts from their embeddings alone
> ([arXiv:2310.06816](https://arxiv.org/abs/2310.06816)). It also does not protect against a live
> compromise of the running server, which can read private content exactly as the service does,
> because the service must decrypt to answer a search.
>
> **sealed** means the server cannot read the content under any circumstance, including full
> compromise. Only client applications holding a key can. The cost is that it cannot be searched at
> all, only fetched by an exact known key, and it can never be read from a browser client. That
> limitation is permanent by design.
>
> **Nothing here protects against losing your own keys.** If every machine holding a sealed key is
> lost at once, that content is gone. A server that could recover it would be a server that could
> read it.

---

## 7. Open questions for the owner

1. **Confirm dropping lexical search for `private`.** This is the load-bearing correction. If exact
   phrase search over private notes is genuinely required, the honest alternative is that private
   gains no real protection over open against a stolen database, and the docs must say so.
2. **KEK escrow.** Losing the Vault KEK makes all private content permanently unrecoverable even
   with perfect backups. Decide whether a wrapped offline copy goes with the sealed backup identity.
   Left alone, this is a single point of failure.
3. **LUKS on root.** Recommendation is to skip it. Confirm, given the alternative costs are either
   an SSH unlock at every reboot or running a second always-on host as a Tang server.
4. **Sealed recipients.** Confirm the list of trusted local clients, and decide whether a fourth
   offline-only recipient exists purely as insurance. Optional, but worth a deliberate answer rather
   than a default by omission.
5. **Vault IAM scope.** Decide who, if anyone, may `manage` rather than `use` the KEK. This
   determines whether an OCI account compromise alone is enough to read private content.
