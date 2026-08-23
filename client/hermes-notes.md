# Wiring Hermes Agent to lumberroom

Hermes is Nous Research's open-source agent CLI. It speaks remote MCP natively and takes a bearer
token today, which puts it in the same easy tier as Claude Code and OpenWebUI (`docs/specs/phase-2-surfaces.md`
§1): no OAuth, no Logto, no external approval needed before it can read and write the same store
as every other surface.

## The one thing that matters before anything else

**Hermes is documented to identify itself as `"Claude Code"` in its MCP `clientInfo.name` in some
configurations.** That is not a typo and not a Hermes bug worth reporting: it is `clientInfo`,
which is free text the client sends about itself, self-declared, unverified, and never used for
policy in lumberroom (`docs/specs/phase-2-surfaces.md` §3). If you grant by `clientInfo` or by anything
Hermes says about itself, you will hand a Nous Research agent the grant meant for your own Claude
Code install, silently, and the log line will look correct while being wrong.

**Identity comes from the credential and only the credential.** Issue Hermes its own token in
`AUTH_TOKENS`, distinct from every other client's, and let the server key the grant off that. This
is the one mechanism the spec calls an actual boundary: everything else (`clientInfo`, User-Agent,
egress IP) is logged for the per-client rate numbers in §5 and never checked for authorization.

## Setup

1. Issue a token, distinct from every other client's:

   ```bash
   openssl rand -hex 32
   ```

2. Add it to the server's `AUTH_TOKENS`, with a grant appropriate for what you want Hermes to see.
   Phase 2's starting position is coarse (`docs/specs/phase-2-surfaces.md` §3 table); tighten it
   once the sensitivity axis lands in Phase 3:

   ```
   AUTH_TOKENS=[{"client":"hermes","token":"<the token above>","read":["*"],"write":["*"]}]
   ```

   If you want Hermes narrower than that from day one because it is a third-party agent rather
   than your own tooling, scope it the way the spec scopes ChatGPT instead:

   ```
   AUTH_TOKENS=[{"client":"hermes","token":"<...>","read":["user:me","global"],"write":["user:me"]}]
   ```

3. Point Hermes at the MCP endpoint with that token as a plain bearer header. Hermes's own docs
   cover the exact config surface for adding a remote MCP server; the header shape lumberroom expects is
   the same one every other bearer-token surface uses:

   ```
   Authorization: Bearer <the token above>
   ```

   Endpoint: `https://<your-domain>/mcp`.

4. Verify with the CLI, using Hermes's own token rather than your Claude Code one, so you are
   testing the grant Hermes actually has and not a different client's:

   ```bash
   LUMBERROOM_URL=https://<your-domain> LUMBERROOM_TOKEN=<hermes token> lumberroom doctor
   ```

## No SessionStart hook

Hermes has no documented lifecycle hook equivalent to Claude Code's `SessionStart`
(`docs/specs/phase-2-surfaces.md` §1, "Auto-recall: none"). Recall depends entirely on Hermes
choosing to call `context_bootstrap` or `memory_search` on its own. That makes Hermes one of the
surfaces the Phase 2 measurement in §5 is watching: if its unprompted read and write rates stay
near zero, the fallback ladder there (sharper tool descriptions, then per-account instructions,
then a browser-extension-equivalent if one exists for Hermes) applies to it the same as ChatGPT.

## Acceptance

Run the per-surface harness from `docs/specs/phase-2-surfaces.md` §6 by hand, since Hermes has no
`scripts/done-when-test.sh` equivalent:

1. From Hermes, state a fact carrying a nonce. Do not tell it to save anything.
2. Check the store (`lumberroom search "<nonce>"`) and `lumberroom stats --by-client` for a write attributed to
   `hermes` with `unprompted = true`.
3. From a different surface, ask the question. Assert the answer contains the nonce.
4. Record whether step 1 needed prompting. That is the data point Phase 2 §5 wants, and it only
   exists if it is written down at the time.
