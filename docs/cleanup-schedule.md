# The cleanup schedule

Two halves, two processes, no cron anywhere.

**The deterministic pass runs inside the server**, on a timer set by `CLEANUP_INTERVAL_SECS`
(default 3600, `0` turns it off). Exact duplicates and near-certain pairs, written straight into the
queue. It opens no connection to anyone. Nothing to install: it starts with the server and stops
with it, and one task cannot overlap itself, so it needs no lock.

**The model pass runs as its own container**, `lumberroom cleanup daemon`, under the `cleanup` compose
profile:

```bash
docker compose --profile cleanup up -d
```

`restart: unless-stopped` is the whole scheduler. No cron daemon, no crontab, no host dependency,
and the same `docker compose up -d` that starts everything else.

## Why two processes

This is a boundary rather than a packaging choice. The server holds the key-encryption key, and
[decision 0011](decisions/0011-cleanup-proposes.md) keeps the provider call out of that process, so
no outbound connection to a third party is ever opened from the one holding the key. The daemon
calls a provider and holds no key material of the store's.

The daily run sends open-row text to a provider. The in-server pass sends nothing anywhere.

## Why a wall clock rather than an interval

The daemon sleeps until the next occurrence of `CLEANUP_DAILY_AT` and not for twenty-four hours from
process start. An interval fires the moment the process comes up, so a container in a crash loop
spends the model call on every restart, and a container restarted at noon quietly moves the nightly
run to noon. Waking on the clock keeps the schedule where you put it.

## Settings

| variable | default | what it does |
|---|---|---|
| `CLEANUP_INTERVAL_SECS` | `3600` | seconds between in-server passes; `0` turns it off, and is the only way to |
| `CLEANUP_NAMESPACE` | every namespace | a glob to narrow the in-server pass to |
| `CLEANUP_LIMIT` | `500` | rows one in-server pass considers |
| `LUMBERROOM_CLEANUP_TOKEN` | none | a client token carrying `mayIngest`; the daemon refuses at startup without it |
| `CLEANUP_DAILY_AT` | `04:25` | local clock time for the model pass |
| `CLEANUP_PROVIDER` | `openrouter` | `openrouter`, `zai`, `openai`, `anthropic` or `custom` |
| `CLEANUP_MODEL` | `qwen/qwen3.7-flash` | the tier that decides the undecided pairs |
| `CLEANUP_MIN_SIMILARITY` | `0.65` | the floor for the band the model is asked about |

The provider key comes from `ZAI_API_KEY` or `OPENROUTER_API_KEY` in `.env`, passed to the container
as `LUMBERROOM_INGEST_KEY_<PROVIDER>`. It never reaches a command line.
[`docs/permissions.md`](permissions.md) covers what `mayIngest` opens and why it is off by default.

## What a failed night looks like

The daemon logs the failure and waits for tomorrow. It does not exit: a schedule that stops because
a provider was down for one night is worse than one night with no pass, and nobody finds out until
they go looking. `docker compose logs cleanup` shows every run and every failure.

It refuses two things at startup rather than at 04:25 the next day: a missing credential, and a
`--at` that is not a 24-hour clock time.

## Running either half by hand

```bash
docker compose exec cleanup lumberroom cleanup run --no-model   # the deterministic half
docker compose exec cleanup lumberroom cleanup run              # and the model pass with it
```

There is no cron anywhere and no script to install. `deploy/install.sh` mints the `cleanup` client
and writes `LUMBERROOM_CLEANUP_TOKEN` into `.env`, so turning the profile on is the only remaining step.

## One project on its own cadence


There is nothing to set up per project, on purpose. Grouping is already scoped to a namespace
inside the query the pass runs, and one run walks every namespace, so the shared schedule already
clusters within each project rather than across them.

To narrow the in-server pass to one project, set `CLEANUP_NAMESPACE=project:lumberroom` and restart. To
run one project on demand, on any cadence:

```bash
docker compose exec cleanup lumberroom cleanup run --namespace project:lumberroom --no-model
```
