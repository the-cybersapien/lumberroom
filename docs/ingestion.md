# Ingesting transcripts

**Status at time of writing: implemented, not run.** `crates/lumberroom/src/ingest/` and the server's
`/admin/ingest` routes exist against the design in `docs/specs/phase-6-ingestion.md`. None of it has
been run end to end against a real transcript directory. The gate that settles that is
`scripts/ingest-test.sh` against a live server, plus the first real run below, watched.

Read `docs/ingestion-providers.md` for how a provider is reached, what `reasoning` and
`json_mode` do, and which model behaviours were measured rather than assumed.

## What this is

Lumberroom remembers what the owner tells it directly. Ingestion is the other source: the transcripts
already sitting in `~/.claude/projects` and (once built) `~/.codex/sessions`, months of sessions that
never went through `memory_write`. Reading them back in recovers durable facts that were said once
and never repeated.

## The three stages

**`plan`** walks the transcript files, excludes what should never reach an extractor, cuts what
remains into spans and chunks, and writes `worklist.json`. Deterministic, reads only, advances
nothing. Prints the exclusion table every time; read it, do not skip past it.

**Extraction** turns each chunk into zero or more candidate facts. Three ways to run it:

- Mode A, `/lumberroom-ingest` inside a Claude Code session: the skill dispatches one subagent per chunk
  and each writes `out/chunk-NN.json`. No provider key, no network call the owner didn't already
  trust the session with.
- Mode B, `lumberroom ingest extract --provider <name>`: the CLI calls an OpenAI-compatible endpoint (or
  Anthropic) directly. Needed for a corpus too large to dispatch as subagents.
- Mode C, `lumberroom ingest extract --run <id> --batch`: the same chunks and the same prompt as Mode B,
  posted as one job the provider answers within a day. Around half the per-token price where a
  provider publishes a discount.

All three fill the same `out/` directory in the same shape, so `submit` cannot tell which one ran.

`--batch` covers the whole of Mode C's lifecycle: it submits a run that has no batch, polls one that
is still running, and splits a finished one into `out/`. `--batch-status` asks and creates nothing,
`--batch-fetch` splits and refuses while the job is still running, and `--retry-failed` on a spent
batch sends a second one for the chunks that failed. The id lands in `state.json`, so a scheduler
can call `--batch` on a timer and a lost `out/` costs a re-fetch rather than a re-send. OpenRouter's
endpoint is built in; anyone else declares theirs at `ingest.providers.<name>.batch.endpoint` in
`~/.config/lumberroom/config.json`, and a provider with no entry gets an error rather than a fall back to
the synchronous path.

**What the owner agrees to when he types `--batch`.** The spans leave the machine and sit in the
provider's object storage until the batch is deleted. OpenRouter writes batch inputs and results to
Google Cloud Storage and keeps them 30 days, which is a different exposure from a synchronous call
where the spans live in the request path. 24h is the only completion window on offer, so one round
of tuning the prompt costs a day: calibrate with Mode A or Mode B first. The confirmation prints the
destination host and the retention whatever `--yes` says.

No batch has gone to a real provider. The lifecycle runs against a stub in
`scripts/ingest-test.sh` step 7 and in the crate's own tests; what neither settles is whether
OpenRouter's replies carry the fields this code reads out of them.

**`submit`** reads `out/`, posts every candidate fact to the server, and only then advances the
watermark for each file it read from. A run that dies between `plan` and `submit` has moved nothing;
re-running `plan` re-reads the same bytes rather than losing them.

## First run: one project, one week

Small enough to read line by line, and cheap to throw away if something looks wrong.

```
lumberroom ingest plan --source claude --project lumberroom --since 7d --max-files 40
/lumberroom-ingest --no-auto
lumberroom ingest list --state proposed
lumberroom ingest clean --run <id>
```

`--project lumberroom --since 7d` holds the walk to tens of files instead of the whole corpus, so the
exclusion table stays short enough to actually check. `--max-files 40` caps the walk and stamps
`traversal_capped` if it fires, so a directory bigger than expected surprises the owner in the
counter, not in the queue. `--no-auto` is why the first run uses it: without it, a fact `plan`
classified as the owner's own words writes straight to the store; with it, everything lands in the
queue at `state = 'proposed'` and nothing reaches the store except through `lumberroom ingest approve`.
Read the queue before trusting auto-approval with a keystroke.

`lumberroom ingest clean --run <id>` deletes the run directory. Nothing it did outside that directory and
the queue rows survives the delete, because `submit` is the only path into the store and it went
through the review above.

## Reading the exclusion table

`plan` prints something like:

```
files      184 seen, 6 skipped (sensitive path 4, symlink 1, unparseable 1)
           1 ingest artifact (agent-a3f9.jsonl, run 4c1e), 0 held back
entries    91,204 seen, 87,331 excluded
           attachment 41,882 · tool_result 38,004 · memory_tool 1,317
           system 5,910 · sensitive 218 · ingest_fence 96
speakers   owner_typed 619 · main_model 2,701 · subagent 553
spans      1,204 cut into 34 chunks
fences     96 entries dropped, 0 closed without an end marker
unknown    entry types: 0   attachment subtypes: nested_memory 12
```

If the tripwire scan drops a span before it ever reaches a chunk file, a `tripwire` line appears
between `spans` and `fences` naming the count and the rule; a clean run prints nothing there.

Nothing on it is a total to compute by hand; every exclusion is counted by the rule that made it.
Three lines matter before scaling past the first run:

- **`attachment` must be non-zero** on any real corpus. A zero here means the exclusion that keeps
  images and file blobs out of an extractor's prompt did not fire, and that is a bug to chase before
  running anything larger.
- **`ingest_fence`, `held back`, and `unknown` should all read zero on a first run.** They are the
  ones that matter starting on the second: `ingest_fence` is entries dropped as part of an earlier
  ingest conversation, `held back` is a file whose watermark did not advance because a chunk went
  missing, and `unknown` is an entry or attachment shape the parser has not seen and is refusing to
  guess about.
- **Compare the proposal count against `owner_typed` spans.** That ratio is what says whether
  extraction on a corpus this size is worth the tokens, before paying to run it on the other 600
  files.

Run the same fixture twice, `plan` then `submit` then `plan` again. The second `plan` should cut zero
new spans; if it cuts more than zero, the watermark did not advance and nothing past this point
should be trusted until that is fixed.

## Where things live

The run directory: `${XDG_STATE_HOME:-~/.local/state}/lumberroom/ingest/<run-id>/`, holding
`worklist.json`, `state.json`, `spans/` and `out/`. Directories 0700, files 0600 from the moment
each is created, never inside a repo. `spans/` holds excerpts of real transcript text in plaintext
on disk; that is an accepted cost, not an oversight, and it is why `lumberroom ingest clean --run <id>`
exists and why `lumberroom ingest run` (the cron path) deletes `spans/` and `out/` on a clean exit.
`plan` also sweeps run directories older than `INGEST_RUN_RETENTION_DAYS` (default 7) at the top of
every invocation, so a scheduled run that is never manually cleaned still stops accumulating.

The queue table on the server holds each proposal's text in the clear while it waits, whatever
namespace it is bound for. That is the one retention this design accepts: the owner reads the text
from the queue and approval writes it through the ordinary write path, so nothing sealed can stand
in for it until he decides. Every other state clears it. Approval into a namespace that encrypts
blanks the proposal's text in the same statement (migration 000018), sealing the memory later does
the same (000022), forgetting the memory does the same, and rejecting a fact bound for a namespace
above `open` blanks it on the spot. The fingerprint stays in every case, so a rejected fact stays
blocked without its sentence being kept.

## The ingest fence

Three markers, each `lumberroom-ingest-<begin|run|end>:<run-id>`, the literal prefix immediately
followed by the run's uuid with no separator. `plan` prints the begin marker to this session's own
transcript before it walks anything, so the conversation this ingest run happens in never eats
itself; `lumberroom ingest run` prints the end marker on every exit path once a run id exists; the
skill's subagent dispatch (Mode A) prints the run marker, since a subagent needs to open and close
its own fence around one chunk's worth of conversation rather than the whole session's.

Both parsers bind a marker to that trailing uuid rather than matching the prefix as a bare
substring: a begin marker parses only when the next 36 characters are a valid uuid, and a close
marker only ends the fence that uuid opened, not any fence. A line that merely quotes the marker
text (a grep hit over this repository, a fetched page, a README) has no uuid after it and falls
through to ordinary parsing instead of opening a fence with nothing in the file able to close it;
counted under `unknown` as `fence_marker:begin_no_uuid` when that happens, and `close_mismatch`
when a close marker's uuid does not match the fence it would close.

A fence still open when a file's read ceiling arrives holds that file's watermark at the byte
offset where it opened, not at the ceiling: everything from there on was excluded as fenced content
and never became a span, so advancing past it would drop those bytes permanently the moment a real
run's end marker finally closes the fence on a line this walk never reached. The next `plan` re-reads
from that offset, which costs a second read of the fenced region and nothing else.

The provider key, for Mode B only: `~/.config/lumberroom/config.json`, under `ingest.providers.<name>`,
written by `lumberroom ingest keys set <provider>` reading from stdin. That file is created at 0600 and the
CLI refuses to read it if it has become group- or world-readable.

**The provider key must never live in `.env`.** `.env` belongs to the server, Docker Compose reads
it, several shell scripts source it, and `AUTH_TOKENS` already showed how easily that file's
contents end up somewhere unintended. There is also no `--api-key` flag: every argument of a running
process is readable through `ps`, and an interactive shell writes the command into its history file.
One environment variable per provider is the only alternative to the key file, and it carries the
same exposure as any other secret in the environment.

## Abandoning a run

Nothing a run does reaches the store except through `lumberroom ingest approve` on a specific proposal.
Stop at any point and run `lumberroom ingest clean --run <id>` (or `--all` to sweep every run directory).
That deletes the working files; nothing was written to the store, so there is nothing else to undo.
If proposals were posted with `--no-auto` and never approved, they sit in the queue at
`state = 'proposed'` and `ingest clean` leaves them there; reject them with `lumberroom ingest reject` or
leave them for the next retention sweep to age out on the server side.

## What is deferred

Codex parsing, corpus-wide ingestion beyond the first project, and supersession from an offline
extractor are all out of scope for the first run and named as such in
`docs/specs/phase-6-ingestion.md` §13. Do not read their absence as a bug in what is here. Mode C
was on this list and has come off it: it is built and reachable, and the entry above says what it
still owes.
