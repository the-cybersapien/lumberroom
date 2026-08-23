# 5. Private content drops lexical search

**Date:** 19 August 2026 · **Status:** accepted · **Decided by:** the owner

## Decision

The lexical index is partial and covers `sensitivity = 'open'` only. Private rows are semantic-only,
and exact-phrase search does not reach them. Migration `20260819000004_sensitivity.sql` builds the
GIN index with `WHERE sensitivity = 'open'`.

## The context that forced it

The system PRD §4.6 described the searchable index as leaking "a good deal of the meaning" of private
content. That is an understatement, and correcting it changes the design rather than the wording.

A Postgres `tsvector` is not an index over the document. It is the document, stemmed, minus
stopwords, with positions. Numbers, proper nouns and identifiers come through stemming intact.
Recovering private content from it takes no attack and no model, because reading the column is the
attack. Encrypting `content` while leaving a `tsvector` of the same content in the next column
defends nothing against the one threat `private` exists to stop, and it looks like protection to
anyone reading the schema.

## The embedding stays plaintext, and the leak is stated out loud

Search has to work, so the embedding is not encrypted. There is no ANN index over ciphertext, and an
unsearchable `private` level would push everything the owner cares about into `open`.

That choice leaks the gist, and the evidence sits in
[`docs/research/encryption-and-sensitivity.md`](../research/encryption-and-sensitivity.md) §1. Morris
et al., *Text Embeddings Reveal (Almost) As Much As Text* ([arXiv:2310.06816](https://arxiv.org/abs/2310.06816)),
recover 92% of 32-token inputs exactly from the embedding with black-box query access, at BLEU 97.3.
A 2025 reproduction ([arXiv:2507.07700](https://arxiv.org/pdf/2507.07700)) confirms the result, with
recovery falling as inputs lengthen, and short content is the shape a personal memory store holds.
ALGEN ([arXiv:2502.11308](https://arxiv.org/pdf/2502.11308)) removes the last obstacle by dropping the
need for the victim's model or a large paired corpus, and `bge-base-en-v1.5` is a public download, so
anyone holding a stolen database can build an inverter offline. Those are the research doc's figures
from published work, not measurements taken on this system.

So the claim the project makes is that `private` leaks the gist of a row to whoever holds the
database, and not its verbatim text. That claim is defensible and it goes in the user-facing docs
rather than staying in a research file.

## What was considered, and why each lost

**Keep the lexical index for private rows.** It cancels the encryption it sits beside, which is the
whole finding.

**Searchable or property-preserving encryption.** Naveed, Kamara and Wright broke this class with
frequency analysis for anything but high-cardinality uniform values
([Inference Attacks on Property-Preserving Encrypted Databases](https://cs.brown.edu/people/seny/pubs/edb.pdf)),
and a plaintext `tsvector` already hands an attacker more than those schemes leak. Research doc §5.

**Harden the embedding instead.** STEER ([arXiv:2507.18518](https://arxiv.org/pdf/2507.18518)) cuts
inversion BLEU below 5% for about 1% recall loss, and it needs the embedding space realigned, which
the research doc calls a research lift rather than something to bolt on. Quantization, noise and
rotation get routed around.

## What it costs, accepted

**Exact-phrase search over private notes stops working.** A rare identifier or an exact quote is the
case where lexical retrieval wins and semantic retrieval is weakest, and after this change a private
row holding one is reachable by meaning alone.

The owner may find that unacceptable, and the spec's honest alternative is recorded here rather than
argued away: if exact-phrase search over private content is genuinely needed, then `private` gains
little over `open` against a stolen database, and the documentation has to say that instead of
implying a protection it does not provide. This is open question 1 in both the spec and the research
doc, and it belongs to the owner. This record ships the recommended default and keeps the reversal
cheap for as long as it can stay cheap.

**A search behaves differently depending on a row's classification**, and no client can see why. A
result that a lexical match would have surfaced is absent with no signal that a filter removed it.

## What this is not for

It is not protection against the live server, which decrypts private content to answer a search and
therefore reads it. It is not protection against a compromise of the running box, for the same
reason. It is not `sealed`, which the server cannot read at all and which is not searchable by any
means. It removes one plaintext derivative from the database and claims nothing beyond that.

## Reversal condition, and the migration note that goes with it

On an all-open store, rebuilding the index with the `WHERE` clause changes no behaviour at all, which
is why it shipped in migration 004 at the cheapest possible moment. Every row is `open` today, so
nothing loses a search result on the day it lands.

Reversing it means dropping the predicate and reindexing. That stays cheap only until private
encryption turns on for new writes, because an encrypted row has `content` set to NULL (migration
`20260819000008_encryption.sql`) and the index expression has nothing to read. After that point, a
reversal means decrypting private rows to reindex them and then holding a plaintext derivative beside
the ciphertext, which is the arrangement this record rejects.

So the reversal condition is a deadline as much as a trigger. If the owner wants exact-phrase search
over private content, decide it before step 5 of the spec's migration order, and accept in writing
that `private` then protects little more than `open` against a stolen database.
