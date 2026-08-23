# 4. The KEK sits behind a provider, and the local providers defend less than a KMS

**Date:** 19 August 2026 · **Status:** accepted · **Decided by:** the owner

## Decision

`KEK_PROVIDER` selects `none`, `file` or `env`. The default is `none`, which refuses a private write
rather than storing private content in plaintext. An external KMS becomes a third implementation
behind the same trait, `KeyProvider` in `src/crypto/kek.rs`, with no change to a caller.

## The context that forced it

[`docs/specs/phase-3-policy-encryption.md`](../specs/phase-3-policy-encryption.md) §3 specified OCI
Vault software-protected keys with an instance principal, and it was right for what it was written
for: one Oracle Always Free ARM instance with no vTPM. Its words were "The deployment target's
Always Free ARM shape has no vTPM, so every TPM-sealed design is out. OCI Vault software-protected
keys are free on that platform and are the recommendation."

The product now has to come up wherever compose runs. That breaks the spec's premise rather than its
reasoning, so this record departs from it in the open. Two reasons. A key source that depends on one
cloud's free tier cannot be the default for a product that installs anywhere, and the spec itself
flagged the load-bearing claim as unverified: "Verify the pricing claim before building on it; it
carries the whole design." Nobody verified it.

## The threat model, per provider, at its true strength

| Provider | Defends | Does not defend |
|---|---|---|
| `none` | nothing, because no private row exists to defend | |
| `file` | a stolen database dump, a leaked backup | a stolen disk or disk image, root on the box, a live compromise |
| `env` | a stolen dump, a leaked backup | the same, and anything that can read the container's environment |

Stated plainly for `file`: a KEK in a file on the same disk as the database defends a dump and a
backup, and does not defend the disk. Anyone who takes the whole volume takes both halves. The
provider refuses a key file readable by group or other, which is hygiene rather than a boundary.

`env` is weaker again. A container's environment is readable by anything that can inspect the
container, including `docker inspect`, `/proc` and a core dump. It ships because it is the only
provider that works on a platform offering environment variables and no writable secret path.

Nothing software-only defends a live compromise. The server has to decrypt to answer a search, so an
attacker who is the server reads what the server reads. This is not a gap in the local providers:
[`docs/research/encryption-and-sensitivity.md`](../research/encryption-and-sensitivity.md) §3 reaches
the same conclusion for OCI Vault, because root on the box calls the same API with the same instance
identity. The documentation has to say this rather than let "encrypted at rest" imply otherwise.

## What was considered, and why each lost

**OCI Vault as the only key source**, the spec's recommendation. Lost to the deploy requirement, and
kept as the first KMS implementation to write when a deployment needs one.

**TPM sealing.** Out on the original target for lack of a vTPM, per the research, and unsuitable as a
default because it ties the store to one machine and turns a hardware replacement into data loss.

**Storing private content in plaintext when no key is configured.** Lost. A level named `private`
that stores plaintext without saying so is worse than a write that fails with a reason, because
the owner acts on the label. `none` refuses, and the boot check warns when a configured namespace
default is `private` while `KEK_PROVIDER=none`, so the refusal is predicted rather than discovered
at the first write.

## The fingerprint check, and why no encrypted row is written before a restart proves the key

`kek_state` (migration `20260819000008_encryption.sql`) holds one row per tenant: the `kek_id`, a
fingerprint, the provider that supplied the key, and when it was verified. The fingerprint is
HMAC-SHA256 of a frozen label under the KEK, truncated to 128 bits, so it names a key without
helping anyone recover it. At boot the server compares the live key against the stored fingerprint. A
mismatch means a swapped, rotated or wrong key, and the server reports it rather than encrypting
under it.

`REQUIRE_VERIFIED_KEK=true` refuses the first encrypted write until that check has passed. This
makes step 4 of the spec's migration order a rule the code holds rather than a line in a runbook:
"do not write an encrypted row until a restart has proved the key can be recovered." Step 4 is the
one that can strand data, and stranded data here has no repair. A row encrypted under a key the box
cannot fetch after a reboot is unreadable for good, in the database and in every backup taken since.

## Open question for the owner, not answered here

**KEK escrow.** Losing the key makes every private row permanently unreadable, backups included.
DEKs are wrapped in-row, so a perfect restore of a perfect dump yields ciphertext and nothing else.
The research doc calls escrow "a requirement, not an edge case", and both it and the spec carry it as
an open question. This record leaves it open on purpose.

What the owner has to decide is whether a wrapped offline copy of the KEK exists, and if it does,
where it lives and who can reach it. The default by omission is no escrow, which is a single point of
failure nobody has agreed to. Answer this before turning on encryption for new private writes, not
after.

## What it costs, accepted

Three implementations to keep honest rather than one, and a docs burden that comes with them: every
place that says private content is encrypted has to say what that stops and what it does not. A key
the owner now holds and can lose, which the open question above exists to address. Rotation stays the
owner's job: `KEK_ID` names the key on every row, so a rotation is distinguishable from data loss,
and rewrapping is per row by design.

## What this is not for

It is not a defence against a live compromise of the running box, and no provider listed here
becomes one. It is not the backup key: dumps are `age`-encrypted to a separate recipient whose
private key lives on the Mac and one offline copy, and conflating the two means one compromise opens
every historical archive. It is not a key store for `sealed` content, which the server never holds a
key for and cannot read under any circumstance.

## Reversal condition

Add a KMS provider when a deployment's threat model outruns the local ones: a host whose disk the
owner does not control, or a requirement to keep key material off the machine entirely. That is an
addition behind the existing trait, and the only thing it reverses is the default, which moves from
`none` to the KMS for that deployment. Reverse the refuse-by-default rule for `none` only if someone
can name a case where a plaintext private row is better than a failed write, and this record's
position is that no such case exists.
