#!/usr/bin/env node
/**
 * lumberroom, the operator and hook client.
 *
 * It speaks MCP over Streamable HTTP with plain fetch: no SDK, no node_modules, so it can be
 * copied to a Mac or a VM on its own. Every call it makes carries X-Memory-Invocation, which is
 * how the instrumentation tells "the hook asked" apart from "the model chose to" (PRD §7).
 *
 * Usage:
 *   lumberroom doctor                                  connectivity, auth, readiness
 *   lumberroom login                                   OAuth 2.1 + PKCE via a loopback listener
 *   lumberroom clients                                 list registered OAuth clients
 *   lumberroom bootstrap [--project <path|slug>]       print the digest (markdown)
 *   lumberroom bootstrap --hook                        emit Claude Code SessionStart JSON
 *   lumberroom search "<query>" [--project p] [--limit n] [--namespace ns]
 *   lumberroom write "<fact>" --namespace user:me [--tags a,b] [--supersedes uuid]
 *   lumberroom forget <id> [--dry-run]                 delete one memory, with confirmation
 *   lumberroom forget --query "..." [--pick 1,3 | --all] [--dry-run] [--limit n]
 *   lumberroom review [--stale] [--conflicts] [--registry] [--days n] [--limit n] [--min-similarity f]
 *   lumberroom supersede <old-id> <new-id>
 *   lumberroom registry get <kind> <key> [--namespace ns] [--project p]
 *   lumberroom registry set <kind> <key> <json-value> --namespace ns
 *   lumberroom registry alias <alias> <canonical> --namespace ns --kind k
 *   lumberroom registry history <kind> <key> [--namespace ns] [--project p] [--limit n]
 *   lumberroom memory history <id> [--namespace ns]      every version of one fact, oldest first
 *   lumberroom alias set <alias> <canonical> --namespace ns [--since t] [--until t] [--origin o]
 *   lumberroom alias list [--namespace ns]
 *   lumberroom alias forget <alias> --namespace ns
 *   lumberroom recall [--sample 25] [--k 10]             approximate vs exact search, measured
 *   lumberroom stats [--hours 168] [--by-client]
 *   lumberroom export --obsidian <path> [--max-sensitivity open]
 *   lumberroom eval [--fixture <path>] [--no-index-check]  recall@1, recall@5, MRR, anti-cases
 *   lumberroom seal <key> --namespace ns [--value "..."]   client-side AES-256-GCM; reads stdin if no --value
 *   lumberroom unseal <key> --namespace ns
 *   lumberroom hash-password                            prints the docker command, does not hash here
 *   lumberroom tools                                    list the tool surface
 *   lumberroom help                                     this list, without touching the network
 *
 * Exit codes: 0 success, 1 failure, 2 auth refused or no credential, 3 timeout. The acceptance
 * scripts branch on these, so a new command reuses them rather than inventing a fourth.
 *
 * Config, in precedence order:
 *   flags --url / --token
 *   env LUMBERROOM_URL / LUMBERROOM_TOKEN
 *   ~/.config/lumberroom/config.json  {"url":"https://...","token":"...","oauth":{...}}
 *
 * config.json's "token" field is a static bearer credential and always wins over "oauth" if both
 * are present: wire-mac.sh writes one or the other, never both, and `lumberroom login` refuses to
 * clobber an existing static token silently. The "oauth" block is written by `lumberroom login` and
 * refreshed automatically by every command whenever a call comes back 401 and a refresh token is
 * on file: {client_id, client_secret, access_token, refresh_token, token_type, expires_at}.
 *
 * Eval fixture format (client/eval-fixture.example.jsonl documents it further): JSON Lines, one
 * case per line:
 *   {"question": "...", "expect_id": "<uuid>", "origin": "..."}   normal case
 *   {"question": "...", "expect": "none", "origin": "..."}         anti-case: must return nothing
 * JSON, not YAML, since this CLI has no YAML parser and adding one is not allowed. An anti-case is
 * pass/fail regardless of the aggregate: any hit at all is a violation.
 */
import { readFileSync, writeFileSync, mkdirSync, chmodSync } from 'node:fs';
import { homedir, platform } from 'node:os';
import { join, dirname } from 'node:path';
import { createServer } from 'node:http';
import { execFile } from 'node:child_process';
import { EventEmitter } from 'node:events';
import { createInterface } from 'node:readline/promises';
import { stdin, stdout } from 'node:process';
import {
  randomBytes,
  createHash,
  createHmac,
  createCipheriv,
  createDecipheriv,
} from 'node:crypto';

// `lumberroom memory history <id> | head` closes the pipe mid-write, and Node turns that into an
// unhandled EPIPE with a stack trace where a person expects three lines and a prompt.
process.stdout.on('error', err => {
  if (err.code === 'EPIPE') process.exit(0);
  throw err;
});

const CONFIG_PATH =
  process.env.LUMBERROOM_CONFIG ?? join(homedir(), '.config', 'lumberroom', 'config.json');

function loadFileConfig() {
  try {
    return JSON.parse(readFileSync(CONFIG_PATH, 'utf8'));
  } catch {
    return {};
  }
}

/** Merges and persists config.json, always ending at mode 0600 regardless of prior permissions. */
function saveConfig(patch) {
  const merged = { ...fileConfig, ...patch };
  mkdirSync(dirname(CONFIG_PATH), { recursive: true });
  writeFileSync(CONFIG_PATH, `${JSON.stringify(merged, null, 2)}\n`, { mode: 0o600 });
  // writeFileSync's mode is only honoured when the file is created; an existing file keeps its
  // old bits, so the credential file is re-chmodded explicitly every time it is touched.
  chmodSync(CONFIG_PATH, 0o600);
  fileConfig = merged;
  return merged;
}

function parseArgs(argv) {
  const positional = [];
  const flags = {};
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg.startsWith('--')) {
      const [key, inline] = arg.slice(2).split('=');
      if (inline !== undefined) {
        flags[key] = inline;
      } else if (i + 1 < argv.length && !argv[i + 1].startsWith('--')) {
        flags[key] = argv[++i];
      } else {
        flags[key] = true;
      }
    } else {
      positional.push(arg);
    }
  }
  return { positional, flags };
}

const { positional, flags } = parseArgs(process.argv.slice(2));
let fileConfig = loadFileConfig();

const baseUrl = String(
  flags.url ?? process.env.LUMBERROOM_URL ?? fileConfig.url ?? 'http://127.0.0.1:8787',
).replace(/\/+$/, '');
// Static tokens win over OAuth on purpose: wire-mac.sh --token-mode writes "token" and never
// touches "oauth"; --oauth-mode never writes "token". A stray "token" left over from a mode
// switch would otherwise silently defeat a freshly minted OAuth credential.
let token = String(
  flags.token ?? process.env.LUMBERROOM_TOKEN ?? fileConfig.token ?? fileConfig.oauth?.access_token ?? '',
);
const invocation = String(flags.invocation ?? (flags.hook ? 'hook' : 'cli'));
const timeoutMs = Number.parseInt(String(flags.timeout ?? process.env.LUMBERROOM_TIMEOUT_MS ?? '15000'), 10);
const mcpUrl = baseUrl.endsWith('/mcp') ? baseUrl : `${baseUrl}/mcp`;
const httpBase = baseUrl.replace(/\/mcp$/, '');

function die(message, code = 1) {
  process.stderr.write(`lumberroom: ${message}\n`);
  process.exit(code);
}

function headers(extra = {}) {
  const h = {
    'content-type': 'application/json',
    accept: 'application/json, text/event-stream',
    'x-memory-invocation': invocation,
    ...extra,
  };
  if (token) h.authorization = `Bearer ${token}`;
  return h;
}

const MAX_REDIRECTS = 5;

function sameOrigin(a, b) {
  return a.protocol === b.protocol && a.hostname === b.hostname && a.port === b.port;
}

/**
 * Follows a redirect only to the origin the call started at. A `Bearer` token in `Authorization`
 * is the bearer's whole identity: if the server (or anything on path to it, which is the threat a
 * redirect exists to let in) answers with a redirect to another origin, `fetch`'s built-in
 * following resends every header verbatim and the token leaves for wherever the Location line
 * points. A cross-origin hop is refused outright, with an error naming both origins, rather than
 * followed without the header: this client talks to one server, an unauthenticated request to a
 * different one is never what the caller meant, and refusing also keeps the client_secret in the
 * two /oauth/token POST bodies at home. Same-origin hops follow with the method and body rules
 * browsers apply.
 */
async function fetchManagedRedirects(url, init) {
  let current = new URL(url);
  let currentInit = init;
  for (let hop = 0; hop <= MAX_REDIRECTS; hop++) {
    const res = await fetch(current, { ...currentInit, redirect: 'manual' });
    if (res.status < 300 || res.status >= 400 || !res.headers.has('location')) return res;
    if (hop === MAX_REDIRECTS) {
      throw new Error(`too many redirects (${MAX_REDIRECTS}) fetching ${url}`);
    }
    const next = new URL(res.headers.get('location'), current);
    const crossOrigin = !sameOrigin(next, current);
    if (crossOrigin) {
      throw new Error(
        `refusing a cross-origin redirect from ${current.origin} to ${next.origin}. ` +
          'A credential is never sent past the origin it was configured for.',
      );
    }
    // 303 always downgrades to GET with no body; 301/302 do the same for a non-GET/HEAD request,
    // matching every browser's de facto behaviour even though the original RFC said to preserve
    // the method. 307/308 are the only codes that keep method and body across the hop.
    const method = String(currentInit?.method ?? 'GET').toUpperCase();
    const nextHeaders = { ...(currentInit?.headers ?? {}) };
    let nextBody = currentInit?.body;
    let nextMethod = method;
    if (res.status === 303 || ((res.status === 301 || res.status === 302) && method !== 'GET' && method !== 'HEAD')) {
      nextMethod = 'GET';
      nextBody = undefined;
    }
    current = next;
    currentInit = { ...currentInit, method: nextMethod, headers: nextHeaders, body: nextBody };
  }
  throw new Error(`too many redirects (${MAX_REDIRECTS}) fetching ${url}`);
}

/**
 * One refresh, one retry, no loop. A refresh token that itself fails just falls through and lets
 * the caller's own 401 handling report the failure: retrying a bad refresh token is how a
 * revoked credential turns into a hang instead of an error.
 */
let refreshInFlight = null;
async function refreshAccessToken() {
  if (refreshInFlight) return refreshInFlight;
  refreshInFlight = (async () => {
    const oauth = fileConfig.oauth;
    if (!oauth?.refresh_token || !oauth?.client_id) return false;
    let res;
    try {
      res = await fetchManagedRedirects(`${httpBase}/oauth/token`, {
        method: 'POST',
        headers: { 'content-type': 'application/x-www-form-urlencoded' },
        // /oauth/token takes form encoding, never JSON: a stack wired only for JSON returns 415
        // here while /oauth/register (JSON) keeps working, which reads as almost-working.
        body: new URLSearchParams({
          grant_type: 'refresh_token',
          refresh_token: oauth.refresh_token,
          client_id: oauth.client_id,
          ...(oauth.client_secret ? { client_secret: oauth.client_secret } : {}),
        }).toString(),
        signal: AbortSignal.timeout(timeoutMs),
      });
    } catch {
      return false;
    }
    if (!res.ok) return false;
    const json = await res.json().catch(() => null);
    if (!json?.access_token) return false;
    saveConfig({
      oauth: {
        ...oauth,
        access_token: json.access_token,
        refresh_token: json.refresh_token ?? oauth.refresh_token,
        token_type: json.token_type ?? 'Bearer',
        expires_at: new Date(Date.now() + (json.expires_in ?? 3600) * 1000).toISOString(),
      },
    });
    token = json.access_token;
    return true;
  })();
  const ok = await refreshInFlight;
  refreshInFlight = null;
  return ok;
}

/**
 * fetch() with exactly one automatic OAuth refresh on 401. buildInit is a thunk rather than a
 * plain object so the second attempt picks up the refreshed token from headers() rather than
 * replaying the stale Authorization header that caused the 401.
 */
async function fetchWithAuth(url, buildInit) {
  let res = await fetchManagedRedirects(url, buildInit());
  if (res.status === 401 && fileConfig.oauth?.refresh_token) {
    const refreshed = await refreshAccessToken();
    if (refreshed) res = await fetchManagedRedirects(url, buildInit());
  }
  return res;
}

/** Streamable HTTP replies with either JSON or a single SSE frame; accept both. */
async function readBody(res) {
  const text = await res.text();
  const type = res.headers.get('content-type') ?? '';
  if (type.includes('text/event-stream')) {
    const payloads = text
      .split('\n')
      .filter(line => line.startsWith('data:'))
      .map(line => line.slice(5).trim())
      .filter(Boolean);
    const last = payloads[payloads.length - 1];
    if (!last) throw new Error(`empty SSE response: ${text.slice(0, 200)}`);
    return JSON.parse(last);
  }
  try {
    return JSON.parse(text);
  } catch {
    throw new Error(`unexpected response (${res.status}): ${text.slice(0, 300)}`);
  }
}

let requestId = 0;
async function rpc(method, params) {
  const res = await fetchWithAuth(mcpUrl, () => ({
    method: 'POST',
    headers: headers(),
    body: JSON.stringify({ jsonrpc: '2.0', id: ++requestId, method, params }),
    signal: AbortSignal.timeout(timeoutMs),
  }));
  if (res.status === 401 || res.status === 403) {
    const detail = await res.text().catch(() => '');
    die(`auth rejected (${res.status}). Check the token. ${detail.slice(0, 200)}`, 2);
  }
  const body = await readBody(res);
  if (body.error) throw new Error(`${method}: ${body.error.message ?? JSON.stringify(body.error)}`);
  return body.result;
}

/**
 * Stateless Streamable HTTP still expects the initialize handshake on each connection.
 * Sending it before every call keeps lumberroom dependency-free and restart-proof.
 */
async function initialize() {
  await rpc('initialize', {
    // 2026-07-28 removed sessions from the protocol (SEP-2567), which is what makes a bare
    // initialize-then-call pair valid without carrying a session id.
    protocolVersion: '2026-07-28',
    capabilities: {},
    clientInfo: { name: `lumberroom-${invocation}`, version: '0.1.0' },
  });
}

async function callTool(name, args) {
  await initialize();
  const result = await rpc('tools/call', { name, arguments: args });
  if (result?.isError) {
    const text = result.content?.map(c => c.text).join('\n') ?? 'tool error';
    throw new Error(text);
  }
  return {
    structured: result?.structuredContent,
    text: result?.content?.map(c => c.text).filter(Boolean).join('\n') ?? '',
  };
}

async function httpRequest(method, path, body) {
  const res = await fetchWithAuth(`${httpBase}${path}`, () => ({
    method,
    headers: headers(),
    body: body !== undefined ? JSON.stringify(body) : undefined,
    signal: AbortSignal.timeout(timeoutMs),
  }));
  const text = await res.text();
  let json;
  try {
    json = JSON.parse(text);
  } catch {
    json = { raw: text.slice(0, 300) };
  }
  return { status: res.status, json };
}

async function httpGet(path) {
  return httpRequest('GET', path);
}

function out(value) {
  process.stdout.write(typeof value === 'string' ? `${value}\n` : `${JSON.stringify(value, null, 2)}\n`);
}

function requireToken() {
  if (!token) {
    die(`no token. Pass --token, set LUMBERROOM_TOKEN, run 'lumberroom login', or write ${CONFIG_PATH}`, 2);
  }
}

function commaList(value) {
  if (value === undefined || value === true) return undefined;
  return String(value)
    .split(',')
    .map(s => s.trim())
    .filter(Boolean);
}

/**
 * Exit 2 for 401 and 403, exit 1 for everything else, following the convention in the header.
 * Both history routes refuse a client that holds no history grant, and that refusal is an answer
 * about the credential rather than a broken server.
 */
function failHttp(label, status, json) {
  const detail = typeof json === 'string' ? json : JSON.stringify(json);
  die(`${label} (${status}): ${detail}`, status === 401 || status === 403 ? 2 : 1);
}

/** Timestamps to the minute. Seconds and a zone offset cost a line's width and settle nothing. */
function stamp(value) {
  if (!value) return 'unknown';
  const d = new Date(value);
  if (Number.isNaN(d.getTime())) return String(value);
  return d.toISOString().replace('T', ' ').slice(0, 16);
}

/**
 * The ratio to read before any recall figure, and the one number that separates a recall
 * measurement from an exact scan compared against an exact scan.
 *
 * Older servers report the two timings and no ratio, so compute it here rather than printing
 * nothing: the version that omits the field is exactly the version whose reports nobody has
 * checked. Returns null when the timings are too small to divide.
 */
function twoArmSpeedup(report) {
  if (typeof report?.exact_speedup === 'number') return report.exact_speedup;
  const indexMs = Number(report?.index_ms);
  const exactMs = Number(report?.exact_ms);
  if (!Number.isFinite(indexMs) || !Number.isFinite(exactMs) || indexMs <= 0) return null;
  return exactMs / indexMs;
}

/** Below this the two arms took comparable time, which means one plan ran twice. */
const SELF_COMPARISON_SPEEDUP = 2;

/** The header comment is the one list of commands. Reading it back keeps `help` from drifting. */
function usageText() {
  let src;
  try {
    src = readFileSync(new URL(import.meta.url), 'utf8');
  } catch {
    return 'read the header of bin/lumberroom.mjs for the command list';
  }
  const lines = src.split('\n');
  const start = lines.findIndex(l => l.trim() === '* Usage:');
  if (start < 0) return 'read the header of bin/lumberroom.mjs for the command list';
  const body = [];
  for (let i = start; i < lines.length; i++) {
    const text = lines[i].replace(/^\s*\* ?/, '').replace(/^\s*\*$/, '');
    // The config paragraph ends the part a person wants at the terminal.
    if (i > start && text.startsWith('Config,')) break;
    body.push(text.replace(/\s+$/, ''));
  }
  return body.join('\n').replace(/\n+$/, '');
}

async function readStdin() {
  const chunks = [];
  for await (const chunk of stdin) chunks.push(chunk);
  return Buffer.concat(chunks).toString('utf8');
}

function openBrowser(url) {
  const plat = platform();
  try {
    if (plat === 'darwin') execFile('open', [url]);
    else if (plat === 'win32') execFile('cmd', ['/c', 'start', '""', url]);
    else execFile('xdg-open', [url]);
  } catch {
    // best effort only; the URL is always printed too
  }
}

/**
 * A one-shot loopback HTTP listener for the OAuth redirect. RFC 8252: a native/CLI app cannot
 * hold a fixed redirect URI a browser would accept as a real origin, so it binds an ephemeral
 * port, registers that exact URI with the server, and shuts itself down after one callback.
 */
function createLoopbackServer(path, expectedState) {
  const events = new EventEmitter();
  const server = createServer((req, res) => {
    let url;
    try {
      url = new URL(req.url, 'http://127.0.0.1');
    } catch {
      res.writeHead(400).end();
      return;
    }
    if (url.pathname !== path) {
      res.writeHead(404).end('not found');
      return;
    }
    const err = url.searchParams.get('error');
    const code = url.searchParams.get('code');
    const gotState = url.searchParams.get('state');
    res.writeHead(200, { 'content-type': 'text/html; charset=utf-8' });
    if (err) {
      res.end('<html><body>lumberroom: sign-in was cancelled or failed. You can close this window.</body></html>');
      events.emit('error', new Error(`authorization server returned error=${err}`));
    } else if (gotState !== expectedState) {
      res.end('<html><body>lumberroom: state mismatch, aborting. You can close this window.</body></html>');
      events.emit('error', new Error('state parameter mismatch on the OAuth callback'));
    } else if (!code) {
      res.end('<html><body>lumberroom: no authorization code received. You can close this window.</body></html>');
      events.emit('error', new Error('callback carried no authorization code'));
    } else {
      res.end('<html><body>lumberroom: signed in. You can close this window and return to the terminal.</body></html>');
      events.emit('code', code);
    }
  });
  return { server, events };
}

function sealKeyPath() {
  return process.env.LUMBERROOM_SEAL_KEY ?? join(homedir(), '.config', 'lumberroom', 'seal-key');
}

/** The server never sees this key. Generated on first use, 0600, base64, 32 bytes. */
function loadOrCreateSealKey() {
  const p = sealKeyPath();
  try {
    return Buffer.from(readFileSync(p, 'utf8').trim(), 'base64');
  } catch {
    const key = randomBytes(32);
    mkdirSync(dirname(p), { recursive: true });
    writeFileSync(p, `${key.toString('base64')}\n`, { mode: 0o600 });
    chmodSync(p, 0o600);
    return key;
  }
}

/** The storage key: an HMAC-SHA256 of the canonical name, so the server cannot enumerate what a
 * namespace holds even by guessing keys. NUL-separated so "a"+"bc" and "ab"+"c" never collide. */
function sealedKeyHmac(sealKey, namespace, key) {
  return createHmac('sha256', sealKey).update(`${namespace}\u0000${key}`).digest('hex');
}

function writeObsidianNote(root, m) {
  const dir = join(root, String(m.namespace).replace(/[:/]/g, '-'));
  mkdirSync(dir, { recursive: true });
  const tags = Array.isArray(m.tags) && m.tags.length ? `[${m.tags.map(t => JSON.stringify(t)).join(', ')}]` : '[]';
  const front = [
    '---',
    `id: ${m.id}`,
    `namespace: ${m.namespace}`,
    `sensitivity: ${m.sensitivity}`,
    `source_client: ${m.source_client}`,
    `created_at: ${m.created_at}`,
    `tags: ${tags}`,
    '---',
    '',
  ].join('\n');
  const body = m.content ?? '*(no plaintext content at this sensitivity level)*';
  writeFileSync(join(dir, `${m.id}.md`), `${front}${body}\n`);
}

const commands = {
  async doctor() {
    out(`endpoint: ${mcpUrl}`);
    const health = await httpGet('/healthz');
    out(`healthz:  ${health.status} ${JSON.stringify(health.json)}`);
    const ready = await httpGet('/readyz');
    out(`readyz:   ${ready.status} ${JSON.stringify(ready.json)}`);

    const usingOauth = Boolean(fileConfig.oauth?.access_token) && !fileConfig.token;
    const credential = usingOauth ? 'oauth' : token ? 'static token' : 'none configured';
    out(`credential: ${credential}${usingOauth ? ` (client ${fileConfig.oauth.client_id ?? 'unknown'})` : ''}`);
    if (usingOauth) {
      out(`oauth token expires: ${fileConfig.oauth.expires_at ?? 'unknown'}`);
      out(`refresh token on file: ${fileConfig.oauth.refresh_token ? 'yes' : 'no'}`);
    }

    requireToken();
    const who = await httpGet('/admin/whoami');
    out(`whoami:   ${who.status} ${JSON.stringify(who.json)}`);
    // Two modes, two lines. /readyz reports what the SERVER is running; whoami reports the mode of
    // the CREDENTIAL that just authenticated. They differ legitimately: every mode honours static
    // tokens, so an oauth server answers "token" for a static bearer. One line labelled "server
    // auth mode" carrying the credential's mode reads as a bug in the server.
    if (ready.json?.auth_mode) out(`server auth mode:     ${ready.json.auth_mode}`);
    if (who.json?.mode) out(`credential auth mode: ${who.json.mode}`);
    await initialize();
    const tools = await rpc('tools/list', {});
    out(`tools:    ${tools.tools.map(t => t.name).join(', ')}`);
    if (health.status !== 200 || ready.status !== 200 || who.status !== 200) {
      die('one or more checks failed', 1);
    }
    out('all checks passed');
  },

  /**
   * OAuth 2.1 authorization-code flow with PKCE S256, per decision 0002's built-in server.
   * Registers a client once via /oauth/register (RFC 7591), then reuses it on every later login
   * unless --reregister is given.
   */
  async login() {
    const state = randomBytes(16).toString('hex');
    const verifier = randomBytes(32).toString('base64url');
    const challenge = createHash('sha256').update(verifier).digest('base64url');

    // The redirect URI is compared exactly, port included, at both /authorize and /token
    // (migration 20260819000007_oauth.sql: "never prefix-matched"). Binding an ephemeral port
    // (port 0) would work for exactly one login and then fail every subsequent one, since a
    // re-login reuses the persisted client_id but would bind a fresh, different port. So the
    // first-ever login picks a fixed default port, persists the whole redirect_uri alongside the
    // client_id, and every later login re-binds that exact port rather than choosing a new one.
    const DEFAULT_LOOPBACK_PORT = 8976;
    const clientId0 = fileConfig.oauth?.client_id;
    const reregistering = Boolean(flags.reregister) || !clientId0;
    let port;
    if (reregistering) {
      port = Number.parseInt(String(flags.port ?? DEFAULT_LOOPBACK_PORT), 10);
    } else {
      const savedUri = fileConfig.oauth?.redirect_uri;
      port = savedUri ? Number.parseInt(new URL(savedUri).port, 10) : DEFAULT_LOOPBACK_PORT;
      if (flags.port && Number.parseInt(String(flags.port), 10) !== port) {
        out(`note: ignoring --port; reusing port ${port} already registered for client ${clientId0}.`);
        out('Pass --reregister to register a fresh client on a different port.');
      }
    }
    const redirectUri = `http://127.0.0.1:${port}/callback`;

    const { server, events } = createLoopbackServer('/callback', state);
    try {
      await new Promise((resolve, reject) => {
        server.once('error', reject);
        server.listen(port, '127.0.0.1', resolve);
      });
    } catch (e) {
      die(
        `cannot bind the OAuth loopback listener on 127.0.0.1:${port} (${e.code ?? e.message}). ` +
          'Free that port, or run with --reregister --port <n> to register a fresh client on a ' +
          'different one.',
      );
    }

    try {
      let clientId = clientId0;
      let clientSecret = fileConfig.oauth?.client_secret ?? null;
      if (reregistering) {
        const reg = await httpRequest('POST', '/oauth/register', {
          client_name: 'lumberroom',
          redirect_uris: [redirectUri],
          grant_types: ['authorization_code', 'refresh_token'],
          // A DCR handler that rejects unknown fields breaks this login on the first byte; this
          // one is informational only and safe to ignore server-side.
          token_endpoint_auth_method: 'none',
          software_id: 'lumberroom',
          software_version: '0.1.0',
        });
        if (reg.status === 404) die('server has no /oauth/register: it is not running in oauth or oidc mode');
        if (reg.status >= 300) die(`client registration failed (${reg.status}): ${JSON.stringify(reg.json)}`);
        clientId = reg.json.client_id;
        clientSecret = reg.json.client_secret ?? null;
        out(`registered client ${clientId} on redirect ${redirectUri}`);
      } else {
        out(`reusing client ${clientId} on redirect ${redirectUri} (--reregister to start over)`);
      }

      const authorizeUrl = new URL(`${httpBase}/oauth/authorize`);
      authorizeUrl.searchParams.set('response_type', 'code');
      authorizeUrl.searchParams.set('client_id', clientId);
      authorizeUrl.searchParams.set('redirect_uri', redirectUri);
      authorizeUrl.searchParams.set('code_challenge', challenge);
      authorizeUrl.searchParams.set('code_challenge_method', 'S256');
      authorizeUrl.searchParams.set('scope', String(flags.scope ?? 'memory.read memory.write'));
      // RFC 8707: binds the token to this resource. The resource identifier is the MCP endpoint
      // itself, matching AuthConfig.resource_url on the server.
      authorizeUrl.searchParams.set('resource', mcpUrl);
      authorizeUrl.searchParams.set('state', state);

      out('opening your browser to sign in. If nothing opens, visit:');
      out(`  ${authorizeUrl.toString()}`);
      openBrowser(authorizeUrl.toString());

      const code = await new Promise((resolve, reject) => {
        const timer = setTimeout(
          () => reject(new Error('timed out waiting for the browser sign-in (5 minutes)')),
          300000,
        );
        events.once('code', c => {
          clearTimeout(timer);
          resolve(c);
        });
        events.once('error', e => {
          clearTimeout(timer);
          reject(e);
        });
      });

      const tokenRes = await fetchManagedRedirects(`${httpBase}/oauth/token`, {
        method: 'POST',
        headers: { 'content-type': 'application/x-www-form-urlencoded' },
        body: new URLSearchParams({
          grant_type: 'authorization_code',
          code,
          redirect_uri: redirectUri,
          client_id: clientId,
          code_verifier: verifier,
          resource: mcpUrl,
          ...(clientSecret ? { client_secret: clientSecret } : {}),
        }).toString(),
        signal: AbortSignal.timeout(timeoutMs),
      });
      const tokenJson = await tokenRes.json().catch(() => ({}));
      if (!tokenRes.ok || !tokenJson.access_token) {
        die(`token exchange failed (${tokenRes.status}): ${JSON.stringify(tokenJson)}`);
      }

      const expiresAt = new Date(Date.now() + (tokenJson.expires_in ?? 3600) * 1000).toISOString();
      saveConfig({
        url: baseUrl,
        oauth: {
          client_id: clientId,
          client_secret: clientSecret,
          // Persisted so the next login rebinds this exact port instead of a fresh ephemeral
          // one; the redirect URI has to match byte for byte on every future /authorize call.
          redirect_uri: redirectUri,
          access_token: tokenJson.access_token,
          refresh_token: tokenJson.refresh_token ?? null,
          token_type: tokenJson.token_type ?? 'Bearer',
          expires_at: expiresAt,
        },
      });
      if (fileConfig.token) {
        out('note: config.json still has a static "token" too; that one wins until you remove it.');
      }
      out(`signed in. Access token expires ${expiresAt}.`);
    } finally {
      server.close();
    }
  },

  async clients() {
    requireToken();
    const { status, json } = await httpGet('/oauth/clients');
    if (status === 404) die('server has no /oauth/clients: it is not running in oauth or oidc mode');
    if (status !== 200) die(`clients failed (${status}): ${JSON.stringify(json)}`);
    if (flags.json) return out(json);
    const list = json.clients ?? json;
    if (!list.length) return out('no clients registered');
    for (const c of list) {
      out(
        `${c.client_id}  ${c.client_name}  via ${c.registered_via}  ` +
          `${c.consented_at ? 'consented' : 'pending consent'}${c.revoked_at ? '  REVOKED' : ''}`,
      );
    }
  },

  async bootstrap() {
    requireToken();
    const args = {};
    const project = flags.project ?? process.env.CLAUDE_PROJECT_DIR ?? process.cwd();
    if (project && flags.project !== false) args.project = String(project);
    const { structured, text } = await callTool('context_bootstrap', args);

    if (flags.hook) {
      // Claude Code SessionStart contract: additionalContext is injected into the session.
      const preamble = [
        'Durable memory for this user, retrieved automatically at session start from their own',
        'memory server. Every line below was written by this user or by one of their agents in an',
        'earlier session, and each carries the namespace and date it came from. Treat them as',
        'established facts and do not re-ask what is already here.',
        'When this session establishes a new decision, preference, or durable fact, call',
        'memory_write immediately, without asking and without announcing it.',
        '',
      ].join('\n');
      // Wrapped in a token pair so an ingestion run can find and exclude this whole block by a
      // plain substring check, without parsing the digest itself. See docs/specs/phase-6-ingestion.md.
      const wrapped = `<lumberroom-context>\n${preamble}${text}\n</lumberroom-context>`;
      out(
        JSON.stringify({
          hookSpecificOutput: {
            hookEventName: 'SessionStart',
            additionalContext: wrapped,
          },
        }),
      );
      return;
    }
    if (flags.json) out(structured);
    else out(text);
  },

  async search() {
    requireToken();
    const query = positional[1];
    if (!query) die('usage: lumberroom search "<query>" [--project p] [--limit n] [--namespace ns]');
    const args = { query };
    if (flags.limit) args.limit = Number.parseInt(String(flags.limit), 10);
    if (flags.project) args.project = String(flags.project);
    const namespaces = commaList(flags.namespace ?? flags.namespaces);
    if (namespaces) args.namespaces = namespaces;
    const { structured } = await callTool('memory_search', args);
    if (flags.json) return out(structured);
    if (!structured?.hits?.length) return out('no matches');
    for (const hit of structured.hits) {
      out(`${hit.score.toFixed(3)}  [${hit.namespace}] ${hit.content}`);
    }
  },

  async write() {
    requireToken();
    const content = positional[1];
    if (!content) die('usage: lumberroom write "<fact>" --namespace user:me [--tags a,b]');
    const namespace = flags.namespace ?? flags.ns;
    if (!namespace || namespace === true) die('--namespace is required (user:me | project:<slug> | global)');
    const args = { content, namespace: String(namespace) };
    const tags = commaList(flags.tags);
    if (tags) args.tags = tags;
    if (flags.supersedes) args.supersedes = String(flags.supersedes);
    const { structured } = await callTool('memory_write', args);
    out(flags.json ? structured : `${structured.deduplicated ? 'exists' : 'written'} ${structured.id} in ${structured.namespace}`);
  },

  /**
   * Deletion is the CLI's, unrestricted (Phase 3 spec §4). By id, or by --query, which finds
   * candidates through the same memory_search a model would use, prints them, and refuses to
   * touch anything without an explicit "yes" unless --dry-run, which never asks and never deletes.
   */
  async forget() {
    requireToken();
    const idArg = positional[1];
    const query = flags.query;
    const dryRun = Boolean(flags['dry-run']);
    if (!idArg && !query) die('usage: lumberroom forget <id> | --query "..." [--dry-run]');

    let candidates;
    if (idArg) {
      const { status, json } = await httpRequest('GET', `/admin/memory/${encodeURIComponent(idArg)}`);
      if (status === 404) die(`no memory with id ${idArg}`);
      if (status !== 200) die(`lookup failed (${status}): ${JSON.stringify(json)}`);
      // score stays undefined: this row is here because the caller named it, and a similarity
      // printed beside it would be a number nothing measured.
      candidates = [json];
    } else {
      const limit = Number.parseInt(String(flags.limit ?? '20'), 10);
      const { structured } = await callTool('memory_search', { query: String(query), limit });
      candidates = (structured?.hits ?? []).map(h => ({
        id: h.id, namespace: h.namespace, content: h.content, score: h.score,
      }));
      if (!candidates.length) return out('no matches for that query, nothing to forget');
    }

    const line = (c, i) => {
      const n = String(i + 1).padStart(2);
      const score = typeof c.score === 'number' ? `  ${c.score.toFixed(3)}` : '';
      return `  ${n}.${score}  ${c.id}  [${c.namespace}]  ${(c.content ?? '').slice(0, 100)}`;
    };
    out(`${candidates.length} candidate${candidates.length === 1 ? '' : 's'}:`);
    candidates.forEach((c, i) => out(line(c, i)));

    if (dryRun) return out('dry run: nothing deleted');

    // memory_search always returns its limit, so a query naming two rows produces a list of twenty
    // whose tail is whatever scored next. Handing that list to the delete loop behind one "yes" is
    // how "forget these two" becomes "forget everything". The caller names what they meant.
    if (!idArg) {
      if (flags.pick !== undefined) {
        const picked = [];
        for (const part of String(flags.pick).split(',')) {
          const t = part.trim();
          if (!t) continue;
          if (!/^[0-9]+$/.test(t)) die(`--pick takes numbers from the printed list, got "${t}"`);
          const n = Number.parseInt(t, 10);
          if (n < 1 || n > candidates.length) {
            die(`--pick ${n} is outside the list, which has ${candidates.length} entries`);
          }
          if (!picked.includes(n)) picked.push(n);
        }
        if (!picked.length) die('--pick chose nothing');
        candidates = picked.map(n => candidates[n - 1]);
        out('');
        out(`--pick chose ${candidates.length} of them:`);
        candidates.forEach((c, i) => out(line(c, i)));
      } else if (!flags.all) {
        out('');
        out('nothing deleted. A query ranks the whole store, so this list runs from the rows you');
        out('meant to the rows that merely scored next.');
        out('Choose from it with --pick 1,3, or delete every entry above with --all.');
        return;
      }
    }

    // The count rather than "yes". Typing "yes" costs the same whether the list holds two rows or
    // twenty, and the number is the one thing about a wide delete worth reading twice.
    const rl = createInterface({ input: stdin, output: stdout });
    const answer = await rl.question(
      `Delete ${candidates.length} memor${candidates.length === 1 ? 'y' : 'ies'} above? Type "${candidates.length}" to confirm: `,
    );
    rl.close();
    if (answer.trim() !== String(candidates.length)) return out('aborted, nothing deleted');

    let deleted = 0;
    for (const c of candidates) {
      const { status } = await httpRequest('DELETE', `/admin/memory/${encodeURIComponent(c.id)}`);
      if (status === 200) deleted++;
      else out(`  failed to delete ${c.id} (${status})`);
    }
    out(`deleted ${deleted} of ${candidates.length}`);
  },

  /** `lumberroom review --stale --conflicts --registry`; with none given, runs all three. */
  async review() {
    requireToken();
    const doStale = Boolean(flags.stale);
    const doConflicts = Boolean(flags.conflicts);
    const doRegistry = Boolean(flags.registry);
    const all = !doStale && !doConflicts && !doRegistry;
    const limit = Number.parseInt(String(flags.limit ?? '25'), 10);

    if (all || doStale) {
      const days = Number.parseInt(String(flags.days ?? '90'), 10);
      const { status, json } = await httpRequest('GET', `/admin/review/stale?days=${days}&limit=${limit}`);
      if (status !== 200) die(`stale review failed (${status}): ${JSON.stringify(json)}`);
      out(`stale (never retrieved, older than ${days}d): ${json.rows?.length ?? 0}`);
      for (const r of json.rows ?? []) {
        out(`  ${r.id}  [${r.namespace}]  ${r.created_at}  ${(r.content ?? '').slice(0, 80)}`);
      }
      out('');
    }
    if (all || doConflicts) {
      const minSim = Number.parseFloat(String(flags['min-similarity'] ?? '0.9'));
      const { status, json } = await httpRequest('GET', `/admin/review/conflicts?min_similarity=${minSim}&limit=${limit}`);
      if (status !== 200) die(`conflict review failed (${status}): ${JSON.stringify(json)}`);
      out(`possible conflicts: ${json.pairs?.length ?? 0}`);
      for (const p of json.pairs ?? []) {
        out(`  ${p.similarity.toFixed(3)}  older ${p.older.id} [${p.older.namespace}] ${p.older.content.slice(0, 60)}`);
        out(`            newer ${p.newer.id} [${p.newer.namespace}] ${p.newer.content.slice(0, 60)}`);
      }
      out('');
    }
    if (all || doRegistry) {
      const { status, json } = await httpRequest('GET', `/admin/review/registry?limit=${limit}`);
      if (status !== 200) die(`registry review failed (${status}): ${JSON.stringify(json)}`);
      out(`registry due for review: ${json.due_for_review?.length ?? 0}`);
      for (const e of json.due_for_review ?? []) out(`  ${e.namespace} ${e.kind}:${e.key}`);
      out(`non-canonical registry keys: ${json.non_canonical?.length ?? 0}`);
      for (const e of json.non_canonical ?? []) out(`  ${e.namespace} ${e.kind}:${e.key}`);
    }
  },

  async supersede() {
    requireToken();
    const [, oldId, newId] = positional;
    if (!oldId || !newId) die('usage: lumberroom supersede <old-id> <new-id>');
    const { status, json } = await httpRequest('POST', `/admin/memory/${encodeURIComponent(oldId)}/supersede`, {
      new_id: newId,
    });
    if (status !== 200) die(`supersede failed (${status}): ${JSON.stringify(json)}`);
    out(`${oldId} is now superseded by ${newId}`);
  },

  async registry() {
    requireToken();
    const sub = positional[1];
    if (sub === 'get') {
      const [, , kind, key] = positional;
      if (!kind || !key) die('usage: lumberroom registry get <kind> <key> [--namespace ns] [--project p]');
      const args = { kind, key };
      if (flags.namespace) args.namespace = String(flags.namespace);
      if (flags.project) args.project = String(flags.project);
      const { structured } = await callTool('registry_get', args);
      return out(structured);
    }
    if (sub === 'set') {
      const [, , kind, key, rawValue] = positional;
      if (!kind || !key || rawValue === undefined) {
        die('usage: lumberroom registry set <kind> <key> <json-value> --namespace ns');
      }
      const namespace = flags.namespace ?? flags.ns;
      if (!namespace || namespace === true) die('--namespace is required');
      let value;
      try {
        value = JSON.parse(rawValue);
      } catch {
        value = rawValue; // A bare string is a legitimate value; do not force JSON quoting.
      }
      const { status, json } = await httpRequest('POST', '/admin/registry', {
        namespace: String(namespace),
        kind,
        key,
        value,
      });
      if (status !== 200) die(`registry set failed (${status}): ${JSON.stringify(json)}`);
      return out(json);
    }
    if (sub === 'alias') {
      const [, , alias, canonical] = positional;
      if (!alias || !canonical) {
        die('usage: lumberroom registry alias <alias> <canonical> --namespace ns --kind k');
      }
      const namespace = flags.namespace ?? flags.ns;
      const kind = flags.kind;
      if (!namespace || namespace === true) die('--namespace is required');
      if (!kind || kind === true) die('--kind is required');
      const { status, json } = await httpRequest('POST', '/admin/registry/alias', {
        namespace: String(namespace),
        kind: String(kind),
        alias_key: alias,
        canonical,
      });
      if (status !== 200) die(`alias failed (${status}): ${JSON.stringify(json)}`);
      return out(json);
    }
    if (sub === 'history') {
      const [, , kind, key] = positional;
      if (!kind || !key) {
        die('usage: lumberroom registry history <kind> <key> [--namespace ns] [--project p] [--limit n]');
      }
      const query = new URLSearchParams({ kind, key });
      if (flags.namespace && flags.namespace !== true) query.set('namespace', String(flags.namespace));
      if (flags.project && flags.project !== true) query.set('project', String(flags.project));
      if (flags.limit && flags.limit !== true) query.set('limit', String(flags.limit));
      const { status, json } = await httpGet(`/admin/registry/history?${query.toString()}`);
      if (status !== 200) failHttp('registry history failed', status, json);
      if (flags.json) return out(json);

      const entries = [...(json.entries ?? [])].sort((a, b) => (b.version ?? 0) - (a.version ?? 0));
      const where = json.namespace ? ` in ${json.namespace}` : '';
      out(`${json.kind}/${json.key}${where}`);
      if (json.resolved_from) out(`asked as ${json.resolved_from}, answered by ${json.key}`);
      if (Array.isArray(json.searched) && json.searched.length) {
        out(`searched: ${json.searched.join(', ')}`);
      }
      if (entries.length === 0) {
        out('');
        out('no replaced versions. History holds what was overwritten, and `lumberroom registry get`');
        out('shows the value standing now.');
        return;
      }
      out('');
      out(`${entries.length} replaced version${entries.length === 1 ? '' : 's'}, newest first`);
      for (const e of entries) {
        const value = typeof e.value === 'string' ? e.value : JSON.stringify(e.value);
        out('');
        out(`v${e.version}  replaced ${stamp(e.replaced_at)}  ${e.sensitivity ?? 'open'}`);
        out(`  ${value}`);
        const p = e.provenance ?? {};
        const confirmed = p.user_confirmed ? ', confirmed by the owner' : '';
        out(`  written by ${p.source_client ?? 'unknown'}${confirmed}`);
      }
      return;
    }
    die('usage: lumberroom registry <get|set|alias|history> ...');
  },

  /**
   * Every version of one fact, oldest first, as a chain a person reads top to bottom.
   *
   * The route takes a namespace and validates it without letting it bound the walk, since a chain
   * may cross namespaces. `global` is a placeholder that satisfies the check, and --namespace is
   * here for the day the server stops asking.
   */
  async memory() {
    requireToken();
    const sub = positional[1];
    if (sub !== 'history') die('usage: lumberroom memory history <id> [--namespace ns]');
    const id = positional[2];
    if (!id) die('usage: lumberroom memory history <id> [--namespace ns]');
    const namespace = flags.namespace && flags.namespace !== true ? String(flags.namespace) : 'global';
    const { status, json } = await httpGet(
      `/admin/memory/${encodeURIComponent(id)}/history?namespace=${encodeURIComponent(namespace)}`,
    );
    if (status !== 200) failHttp('memory history failed', status, json);
    if (flags.json) return out(json);

    const versions = json.versions ?? [];
    if (versions.length === 0) {
      out(`nothing readable at ${id}.`);
      out('An id this credential may not read looks the same as an id that was never written.');
      return;
    }

    const plural = versions.length === 1 ? '' : 's';
    out(`${versions.length} version${plural} of this fact, oldest first.`);
    versions.forEach((v, i) => {
      out('');
      const marker = v.id === id ? '   (the id you asked for)' : '';
      out(`${i + 1}. ${v.content ?? '(no plaintext at this sensitivity)'}${marker}`);
      const ended = v.superseded_by
        ? `replaced ${stamp(v.superseded_at)} by ${v.superseded_by}`
        : 'still standing';
      out(`   written ${stamp(v.created_at)} by ${v.source_client ?? 'unknown'}, ${ended}`);
      out(`   ${v.namespace}, ${v.sensitivity}${v.tags?.length ? `, tagged ${v.tags.join(', ')}` : ''}`);
      // Valid time, printed only when a row carries it. Most facts have no start date and
      // inventing one from created_at would turn "when we learned it" into "when it began".
      if (v.occurred_at || v.occurred_until) {
        const from = v.occurred_at ? `from ${stamp(v.occurred_at)}` : 'from an unrecorded date';
        const until = v.occurred_until ? `until ${stamp(v.occurred_until)}` : 'onwards';
        out(`   true in the world ${from} ${until}`);
      }
      if (v.last_confirmed_at) out(`   last confirmed ${stamp(v.last_confirmed_at)}`);
    });

    // Both of these say the list is short of the truth, and a chain that lies by omission is the
    // failure this command exists to prevent.
    if (json.withheld > 0) {
      const n = json.withheld;
      out('');
      out(`${n} more version${n === 1 ? ' is' : 's are'} on this chain, outside your grants and not printed here.`);
    }
    if (json.depth_capped) {
      out('');
      out('The walk stopped at its depth cap, so the chain may run past one or both ends of this list.');
    }
  },

  /**
   * Aliases: two names for one thing, which is what lets a search for Lumen find the rows that
   * still say Warden. Separate from `registry alias`, which redirects one registry key to another.
   */
  async alias() {
    requireToken();
    const sub = positional[1];

    if (sub === 'set') {
      const [, , alias, canonical] = positional;
      if (!alias || !canonical) {
        die('usage: lumberroom alias set <alias> <canonical> --namespace ns [--since t] [--until t] [--origin o]');
      }
      const namespace = flags.namespace ?? flags.ns;
      if (!namespace || namespace === true) die('--namespace is required');
      const body = { namespace: String(namespace), alias, canonical };
      if (flags.since && flags.since !== true) body.since = String(flags.since);
      if (flags.until && flags.until !== true) body.until = String(flags.until);
      if (flags.origin && flags.origin !== true) body.origin = String(flags.origin);
      const { status, json } = await httpRequest('POST', '/admin/alias', body);
      if (status !== 200) failHttp('alias set failed', status, json);
      if (flags.json) return out(json);
      return out(`in ${json.namespace}, "${json.alias}" now names the same thing as "${json.canonical}" (${json.origin})`);
    }

    if (sub === 'list' || sub === undefined) {
      const query =
        flags.namespace && flags.namespace !== true
          ? `?namespace=${encodeURIComponent(String(flags.namespace))}`
          : '';
      const { status, json } = await httpGet(`/admin/alias${query}`);
      if (status !== 200) failHttp('alias list failed', status, json);
      if (flags.json) return out(json);
      const rows = json.aliases ?? [];
      if (rows.length === 0) return out('no aliases recorded here');
      const width = Math.max(...rows.map(r => String(r.namespace).length));
      for (const r of rows) {
        const window = [
          r.since ? `since ${stamp(r.since)}` : null,
          r.until ? `until ${stamp(r.until)}` : null,
        ]
          .filter(Boolean)
          .join(' ');
        out(
          `${String(r.namespace).padEnd(width)}  ${r.alias} -> ${r.canonical}  ${r.origin}` +
            (window ? `  ${window}` : ''),
        );
      }
      return;
    }

    if (sub === 'forget') {
      const alias = positional[2];
      if (!alias) die('usage: lumberroom alias forget <alias> --namespace ns');
      const namespace = flags.namespace ?? flags.ns;
      if (!namespace || namespace === true) die('--namespace is required');
      const { status, json } = await httpRequest('DELETE', '/admin/alias', {
        namespace: String(namespace),
        alias,
      });
      if (status !== 200) failHttp('alias forget failed', status, json);
      if (flags.json) return out(json);
      return out(
        json.forgotten
          ? `"${alias}" is no longer an alias in ${namespace}`
          : `no alias "${alias}" in ${namespace}, nothing to forget`,
      );
    }

    die('usage: lumberroom alias <set|list|forget> ...');
  },

  async recall() {
    requireToken();
    const sample = Number.parseInt(String(flags.sample ?? '25'), 10);
    const k = Number.parseInt(String(flags.k ?? '10'), 10);
    const { status, json } = await httpGet(`/admin/recall?sample=${sample}&k=${k}`);
    if (status !== 200) die(`recall failed (${status}): ${JSON.stringify(json)}`);
    if (flags.json) return out(json);
    if (!json.sampled) return out('store is empty, nothing to measure');
    out(`sampled ${json.sampled} stored memories, comparing indexed search against an exact scan`);
    out(`recall@${json.k}: ${(json.recall_at_k * 100).toFixed(1)}%`);
    out(`nearest-neighbour misses: ${json.top_one_misses} of ${json.sampled}`);
    out(`indexed ${json.index_ms}ms total, exact ${json.exact_ms}ms total`);

    // Read this before the recall figure. The monitor has twice compared an exact scan against an
    // exact scan and reported perfect recall: once because SET LOCAL on a pooled connection with no
    // transaction is a warning and no effect, and once because at k=1 the planner declines the HNSW
    // index and both arms run sequentially. Neither showed up in the recall number. Both showed up
    // here, as two timings that match.
    const speedup = twoArmSpeedup(json);
    const selfComparison = speedup !== null && speedup < SELF_COMPARISON_SPEEDUP;
    if (speedup === null) {
      out('');
      out('WARNING: the timings are too small to divide, so nothing here says the index ran at');
      out('all. Raise --sample, or run this against a store with more rows in it.');
    } else {
      out(`exact scan took ${speedup.toFixed(1)}x the indexed time`);
    }
    if (selfComparison) {
      out('');
      out('WARNING: the two arms took comparable time, so this is very likely an exact scan');
      out('compared against an exact scan and the recall figure above means nothing. The');
      out('planner declines the index at small k and on small stores. Raise k, or seed more');
      out('rows, then run it again.');
    }

    if (json.recall_at_k < 0.9 && !selfComparison) {
      out('');
      out('recall is below 90%. Raise hnsw.ef_search, or rebuild the index with a higher');
      out('ef_construction. Weakest queries:');
      for (const w of json.worst) out(`  ${(w.recall * 100).toFixed(0)}%  ${w.query}`);
    }
  },

  async stats() {
    requireToken();
    const hours = Number.parseInt(String(flags.hours ?? '168'), 10);
    const byClient = Boolean(flags['by-client']);
    const { status, json } = await httpGet(`/statsz?hours=${hours}${byClient ? '&by=client' : ''}`);
    if (status !== 200) die(`stats failed (${status}): ${JSON.stringify(json)}`);
    if (flags.json) return out(json);
    out(`window: last ${json.window_hours}h`);

    if (byClient) {
      for (const row of json.by_client ?? []) {
        out(
          `  ${String(row.client).padEnd(18)} calls ${String(row.calls).padStart(4)}  ` +
            `reads ${String(row.reads).padStart(4)}  writes ${String(row.writes).padStart(4)}  ` +
            `write/read ${row.write_to_read_ratio != null ? row.write_to_read_ratio.toFixed(2) : 'n/a'}  ` +
            `unprompted-write ${
              row.unprompted_write_rate != null ? `${(row.unprompted_write_rate * 100).toFixed(0)}%` : 'n/a'
            }`,
        );
      }
      return;
    }

    out(
      `totals: ${json.totals.calls} calls, ${json.totals.failures} failed, ` +
        `unprompted ${json.totals.unprompted} (${json.totals.unprompted_rate ?? 'n/a'})`,
    );
    for (const row of json.by_tool) {
      out(
        `  ${row.tool.padEnd(18)} ${String(row.calls).padStart(4)} calls  ` +
          `${String(row.unprompted).padStart(4)} unprompted  ` +
          `p50 ${row.p50_ms ?? '-'}ms  p95 ${row.p95_ms ?? '-'}ms  [${row.client}]`,
      );
    }
  },

  async export() {
    requireToken();
    const target = flags.obsidian;
    if (!target || target === true) die('usage: lumberroom export --obsidian <path> [--max-sensitivity open]');
    const maxSensitivity = String(flags['max-sensitivity'] ?? 'open');
    const pageSize = 200;
    let offset = 0;
    let total = 0;
    for (;;) {
      const { status, json } = await httpRequest(
        'GET',
        `/admin/export?max_sensitivity=${maxSensitivity}&limit=${pageSize}&offset=${offset}`,
      );
      if (status !== 200) die(`export failed (${status}): ${JSON.stringify(json)}`);
      const rows = json.rows ?? [];
      for (const m of rows) writeObsidianNote(String(target), m);
      total += rows.length;
      if (rows.length < pageSize) break;
      offset += pageSize;
    }
    out(`wrote ${total} notes to ${target}`);
  },

  /**
   * Recall quality against a fixture the owner curated, not a synthetic benchmark. Anti-cases
   * (expect: "none") are reported separately and are pass/fail per case: the aggregate recall
   * numbers must never absorb a leak.
   */
  async eval() {
    requireToken();
    const fixturePath = String(flags.fixture ?? join(homedir(), '.config', 'lumberroom', 'eval-fixture.jsonl'));
    let raw;
    try {
      raw = readFileSync(fixturePath, 'utf8');
    } catch {
      die(`cannot read fixture ${fixturePath}. See client/eval-fixture.example.jsonl for the format.`);
    }
    const lines = raw.split('\n').map(l => l.trim()).filter(Boolean);
    const cases = lines.map((line, i) => {
      try {
        return JSON.parse(line);
      } catch {
        die(`fixture line ${i + 1} is not valid JSON: ${line.slice(0, 80)}`);
        return null;
      }
    });

    let hitAt1 = 0;
    let hitAt5 = 0;
    let mrrSum = 0;
    let normalCount = 0;
    const violations = [];

    for (const c of cases) {
      const limit = 5;
      const { structured } = await callTool('memory_search', { query: c.question, limit });
      const hits = structured?.hits ?? [];
      if (c.expect === 'none') {
        if (hits.length > 0) violations.push({ question: c.question, origin: c.origin, got: hits[0]?.id });
        continue;
      }
      normalCount++;
      const rank = hits.findIndex(h => h.id === c.expect_id);
      if (rank === 0) hitAt1++;
      if (rank >= 0 && rank < 5) hitAt5++;
      if (rank >= 0) mrrSum += 1 / (rank + 1);
    }

    out(`cases: ${cases.length} (${normalCount} normal, ${cases.length - normalCount} anti-case)`);
    if (normalCount > 0) {
      out(`recall@1: ${((hitAt1 / normalCount) * 100).toFixed(1)}%`);
      out(`recall@5: ${((hitAt5 / normalCount) * 100).toFixed(1)}%`);
      out(`MRR:      ${(mrrSum / normalCount).toFixed(3)}`);
    } else {
      out('no normal cases; recall@1/@5/MRR not computed');
    }
    out('');
    out(`anti-case violations: ${violations.length}`);
    for (const v of violations) out(`  FAIL  "${v.question}"  (${v.origin ?? 'unlabelled'})  returned ${v.got}`);

    // Every figure above came out of the same search path, and that path is only an index search
    // when the planner decides it is. One call to the monitor says which plan ran. It cannot fail
    // the eval: a fixture result stands on its own, and this line says how far it carries.
    if (!flags['no-index-check']) {
      out('');
      const { status, json } = await httpGet('/admin/recall?sample=10&k=5');
      const speedup = status === 200 ? twoArmSpeedup(json) : null;
      if (status !== 200) {
        out(`index check: /admin/recall answered ${status}, so nothing here says which plan ran`);
      } else if (speedup === null) {
        out('index check: the monitor timings are too small to divide, so nothing here says which');
        out('plan ran.');
      } else {
        out(`index check: an exact scan costs ${speedup.toFixed(1)}x an indexed search`);
        if (speedup < SELF_COMPARISON_SPEEDUP) {
          out('');
          out('The planner is not reaching for the HNSW index on a store this size, so the figures');
          out('above measure a sequential scan. They are true of this store today and they say');
          out('nothing about the recall a client gets once the index carries the search.');
        }
      }
    }

    if (violations.length > 0) process.exitCode = 1;
  },

  /** Client-side AES-256-GCM. The server receives ciphertext only; it never sees the key. */
  async seal() {
    requireToken();
    const key = positional[1];
    if (!key) die('usage: lumberroom seal <key> --namespace ns [--value "..."] (reads stdin if --value is absent)');
    const namespace = flags.namespace ?? flags.ns;
    if (!namespace || namespace === true) die('--namespace is required');

    let value = flags.value;
    if (value === undefined) {
      value = await readStdin();
      if (!value.trim()) die('no value on stdin and no --value given');
    }

    const sealKey = loadOrCreateSealKey();
    const keyHmac = sealedKeyHmac(sealKey, String(namespace), key);
    const iv = randomBytes(12);
    const cipher = createCipheriv('aes-256-gcm', sealKey, iv);
    const ct = Buffer.concat([cipher.update(String(value), 'utf8'), cipher.final()]);
    const tag = cipher.getAuthTag();
    const blob = Buffer.concat([iv, ct, tag]).toString('base64');

    const { status, json } = await httpRequest('PUT', '/admin/sealed', {
      namespace: String(namespace),
      key_hmac: keyHmac,
      ciphertext: blob,
      alg: 'aes-256-gcm',
      source_client: invocation,
    });
    if (status !== 200) die(`seal failed (${status}): ${JSON.stringify(json)}`);
    out(`sealed ${key} in ${namespace} (the server never saw the plaintext)`);
  },

  async unseal() {
    requireToken();
    const key = positional[1];
    if (!key) die('usage: lumberroom unseal <key> --namespace ns');
    const namespace = flags.namespace ?? flags.ns;
    if (!namespace || namespace === true) die('--namespace is required');

    const sealKey = loadOrCreateSealKey();
    const keyHmac = sealedKeyHmac(sealKey, String(namespace), key);
    const { status, json } = await httpRequest(
      'GET',
      `/admin/sealed?namespace=${encodeURIComponent(String(namespace))}&key_hmac=${keyHmac}`,
    );
    if (status === 404) die(`nothing sealed at ${namespace}/${key}`);
    if (status !== 200) die(`unseal lookup failed (${status}): ${JSON.stringify(json)}`);
    if (json.alg !== 'aes-256-gcm') die(`unsupported alg ${json.alg}; this client only unseals aes-256-gcm`);

    const blob = Buffer.from(json.ciphertext, 'base64');
    const iv = blob.subarray(0, 12);
    const tag = blob.subarray(blob.length - 16);
    const ct = blob.subarray(12, blob.length - 16);
    const decipher = createDecipheriv('aes-256-gcm', sealKey, iv);
    decipher.setAuthTag(tag);
    const plain = Buffer.concat([decipher.update(ct), decipher.final()]).toString('utf8');
    out(plain);
  },

  /** argon2 is not in Node's built-ins and adding a dependency is not allowed, so this tells you
   * the exact command rather than computing a weaker hash locally. */
  'hash-password': async () => {
    out('argon2 is not available in Node built-ins, so this does not compute a hash itself.');
    out('Run it inside the server image, which already links argon2:');
    out('');
    out('  docker run --rm -it lumberroom-server:0.1.0 lumberroom-server hash-password');
    out('');
    out('It prompts for the password on the TTY and prints an argon2 PHC string. Put that in .env');
    out('as OWNER_PASSWORD_HASH and restart the server before switching AUTH_MODE=oauth.');
  },

  /** Offline on purpose: the command a person reaches for when nothing else works. */
  async help() {
    out(usageText());
  },

  async tools() {
    requireToken();
    await initialize();
    const result = await rpc('tools/list', {});
    for (const tool of result.tools) {
      out(`${tool.name}\n  ${tool.description}\n`);
    }
  },
};

// --help is a flag rather than a command in most shells' muscle memory, and falling through to
// doctor sent it at the network.
const command = flags.help ? 'help' : positional[0] ?? 'doctor';
const handler = commands[command];
if (!handler) {
  die(
    'unknown command ' +
      `${command}. Try: doctor, login, clients, bootstrap, search, write, forget, review, ` +
      'supersede, registry, memory, alias, recall, stats, export, eval, seal, unseal, ' +
      'hash-password, tools, help',
  );
}

handler().catch(e => {
  if (e?.name === 'TimeoutError') die(`timed out after ${timeoutMs}ms talking to ${mcpUrl}`, 3);
  die(e?.message ?? String(e), 1);
});
