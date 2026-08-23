-- The emission record stops being a verification oracle for private rows.
--
-- recall_emission.content_sha256 was an unkeyed SHA-256 of the normalised plaintext of every row
-- a tool handed out, private rows included, hashed after decryption. Joined to memory on rows
-- holding ciphertext, that gave anyone with a dump and no KEK a way to confirm a guessed sentence
-- byte for byte against content the envelope was meant to keep from them, and gave any credential
-- with the ingest capability the same test through the emission lookup.
--
-- Two changes, one here and one in the server. The server now derives a digest key from the KEK
-- and records an HMAC in this column (crypto::digest), so a dump cannot verify a guess, and it
-- records nothing at all for rows whose content is encrypted. This migration removes the rows
-- already recorded for encrypted content, which are the ones a dump could test against.
--
-- Rows for open content stay. On a deployment with no KEK the digest is unkeyed and those rows
-- still join; on one with a KEK they never match a new proposal again and cost a few bytes each.
-- Deleting them would lose the join on the deployments that still have it.
--
-- The column keeps its name. Renaming it would be a second migration for a label, and every
-- reader of this table is in one adapter.
DELETE FROM recall_emission e
 USING memory m
 WHERE m.id = e.memory_id
   AND m.content_ct IS NOT NULL;
