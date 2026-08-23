---
name: lumberroom-ingest
description: Use when the owner asks to ingest agent transcripts into lumberroom, extract memories from past sessions, or run a lumberroom ingest.
---

# lumberroom-ingest

Mode A of transcript ingestion (`docs/specs/phase-6-ingestion.md` §8, §11): this session dispatches
subagents to extract facts instead of calling a provider. No provider key, and no span crosses the
network to a third party. Read `docs/ingestion-mode-a.md` for when this mode is the right call over
Mode B (`lumberroom ingest extract --provider ...`).

`plan` and `submit` are deterministic and already built. This skill is the dispatch loop between
them: one subagent per chunk, all in one message, each writing its own output file.

## Hard rules

- Call no memory tool during a run: nothing whose name starts with `mcp__lumberroom__` or
  `mcp__agentmemory__`, from this skill or from any subagent it dispatches. A run that writes to the
  store while extracting from transcripts closes the loop this design exists to prevent.
- Run no git command.
- Write no file outside the run directory (`~/.local/state/lumberroom/ingest/runs/<run_id>/`).
- Never tell the owner a fact was verified. It was extracted, and the queue is where verification
  happens, by the owner.
- Every marker this skill prints carries the id `plan` minted for the run in progress, and no other
  id. This file's own text below uses `<run_id>` as a placeholder for that reason: a placeholder
  never parses as a fence, a real-looking uuid might.

## Step by step

### 1. Plan

Run, filling in whatever scope the owner gave and defaulting to the current project and the last 7
days when they gave none:

```
./scripts/lumberroom.sh ingest plan --source claude [--project <path|slug>] [--since 7d] [--max-files N]
```

Print the exclusion table exactly as `plan` printed it. Do not summarize it, do not compress it to a
sentence: the owner reads it to decide whether the run is sane before any token is spent extracting.

`plan`'s first line of output is `lumberroom-ingest-begin:<run_id>`, and the fence this run runs inside
opens there, at that line, whether or not this skill echoes it again (spec §11). Read the run id off
that output. Immediately after `plan` returns, echo `lumberroom-ingest-begin:<run_id>` again as plain text,
with the real id substituted, as the redundant copy the spec asks for.

### 2. Read the worklist

Path: `~/.local/state/lumberroom/ingest/runs/<run_id>/worklist.json` (`~/.local/state/lumberroom` unless
`LUMBERROOM_STATE_DIR` is set to something else). Read the `chunks` array; its length is the number of
subagents to dispatch, and each entry's `index` is the chunk number, zero-based, matching
`spans/chunk-NN.json` and `out/chunk-NN.json` (`NN` zero-padded to at least two digits; past chunk 99
it is three). Tell the owner the chunk count before dispatching anything.

### 3. Build each subagent's prompt from `prompt.rs`, not from a copy in this file

The extraction prompt lives in `crates/lumberroom/src/ingest/prompt.rs`, in the constants `SPEAKERS`
and `BODY` and the function `agent_prompt`. Read that file at dispatch time and assemble the same
text `agent_prompt` builds. Do not paraphrase it and do not carry a cached copy here: `prompt.rs` is
what ships in the binary, and a second copy in this skill goes stale the day someone edits one and
not the other.

For chunk with `worklist.json` index `NN`, `chunk_num` is `NN + 1` (the human-readable count
`agent_prompt` uses) and `total` is the chunk count from step 2. Assemble:

```
lumberroom-ingest-run:<run_id>

You are extracting durable facts from chunk <chunk_num> of <total> of one person's agent
transcripts. Read <chunk_path>. Write your result to <out_path>. Touch no other file.

Do not call any memory tool. Do not call any tool whose name starts with mcp__lumberroom__ or
mcp__agentmemory__. Do not write to any store. Your only output is the JSON file.

<SPEAKERS, verbatim from prompt.rs>

<BODY, verbatim from prompt.rs>

If the chunk holds no durable fact, write exactly {"facts": [], "refusal": "<no-facts/>"} to
<out_path> and stop. That is a correct and expected answer. Write the file even if facts is empty.
A missing file reads as a crashed agent.
```

`<chunk_path>` is `~/.local/state/lumberroom/ingest/runs/<run_id>/spans/chunk-NN.json`, `<out_path>` is the
same run's `out/chunk-NN.json`. Substitute the real run id, chunk number, total, and paths; nothing
else in this text changes between chunks.

### 4. Dispatch every chunk in one message

Call the Agent tool once per chunk, `subagent_type: general-purpose`, every call inside one assistant
message. Two ways to get this wrong, both silent:

- Separate messages dispatch the chunks one after another rather than in parallel, and buy nothing
  over reading every span file inline first.
- Any `subagent_type` other than `general-purpose` (a read-only type, for instance) cannot write the
  chunk's output file, and the chunk comes back missing with no error anywhere.

A chunk count too large to dispatch in one message, in practice more than a few dozen, is a run to
narrow with `--max-files` or a shorter `--since` on the next `plan`, not a run to split across
messages.

### 5. Verify what came back

Wait for every dispatch to return. For each chunk index, check that `out/chunk-NN.json` exists and
parses to the shape the prompt asks for: `{"facts": [...]}` or the no-facts refusal. Warn by chunk
number for every file that is missing or does not parse.

If more than half the chunks are missing or unparseable, stop here. Do not run `submit`. Go straight
to step 7 to close the fence, then tell the owner which chunks failed and that a re-run of `plan` on
the same scope, or a manual re-dispatch of the named chunks, is the next step.

If half or fewer are missing, re-dispatch only the named chunks, same run id, same prompt, same
paths, rather than restarting the whole run.

### 6. Submit

```
./scripts/lumberroom.sh ingest submit --run <run_id> [--no-auto]
```

Pass `--no-auto` through when the owner asked for it or invoked this skill with it. Print the report
verbatim: written, queued, reinforced, confirmed, refused, held-back files, fence counters. A
`fences_unclosed` count or a held-back file is exactly what the owner needs to see here; do not
smooth it into a summary sentence.

### 7. Close the fence

Echo `lumberroom-ingest-end:<run_id>` as plain text on every exit path from step 3 onward: the ordinary
finish, the more-than-half-missing abort in step 5, and a `submit` failure or refusal. The bound in
spec §7.3 closes an unclosed fence on the next `plan` regardless, but a run report is honest only
when the end marker fires everywhere the begin marker did.

### 8. Report and stop

Tell the owner what `submit` wrote and queued, in the numbers the CLI printed, and give them the
commands to read and act on the queue:

```
lumberroom ingest list --state proposed --run <run_id>
lumberroom ingest show <id>
lumberroom ingest approve <id>...
lumberroom ingest approve --run <run_id> [--speaker owner_typed] [--yes]
lumberroom ingest reject <id> --reason "..."
```

Approve nothing on the owner's behalf beyond what `submit` already auto-approved under `auto = true`
(spec §2.4, §8.3 step 8). This skill's job ends at a queue the owner reads; it is not the reviewer.
