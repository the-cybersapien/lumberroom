// Regression tests for the redirect handling in lumberroom.mjs: a same-origin redirect must still
// carry the bearer token, and a cross-origin one must never reach its target at all rather than
// reach it with the token stripped or intact.
//
// Not wired into any existing runner (there is no JS test harness in this repo yet); run directly:
//   node bin/lumberroom-redirect.test.mjs
// It drives the real CLI as a child process against local HTTP stubs, so it exercises the shipped
// fetchManagedRedirects code path rather than a copy of it.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { execFile } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { promisify } from 'node:util';

const execFileAsync = promisify(execFile);
const CLI = join(dirname(fileURLToPath(import.meta.url)), 'lumberroom.mjs');

function listen(handler) {
  return new Promise(resolve => {
    const server = createServer(handler);
    server.listen(0, '127.0.0.1', () => resolve(server));
  });
}

function port(server) {
  return server.address().port;
}

async function runCli(args) {
  try {
    const { stdout, stderr } = await execFileAsync('node', [CLI, ...args], { timeout: 10000 });
    return { code: 0, stdout, stderr };
  } catch (e) {
    return { code: e.code ?? 1, stdout: e.stdout ?? '', stderr: e.stderr ?? '' };
  }
}

test('a same-origin redirect on /admin/alias still reaches the real handler with the token', async () => {
  const seenAuth = [];
  const server = await listen((req, res) => {
    if (req.url.startsWith('/admin/alias') && !req.url.includes('real')) {
      res.writeHead(302, { location: '/admin/alias?real=1' });
      res.end();
      return;
    }
    seenAuth.push(req.headers.authorization);
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ aliases: [] }));
  });
  try {
    const url = `http://127.0.0.1:${port(server)}`;
    const { code, stdout } = await runCli(['alias', 'list', '--url', url, '--token', 'test-token']);
    assert.equal(code, 0, `expected success, stdout: ${stdout}`);
    assert.equal(seenAuth.length, 1, 'the real handler must have been reached exactly once');
    assert.equal(seenAuth[0], 'Bearer test-token', 'the token must survive a same-origin hop');
  } finally {
    server.close();
  }
});

test('a cross-origin redirect on /admin/alias is refused before the token ever leaves this origin', async () => {
  let targetHits = 0;
  const target = await listen((req, res) => {
    targetHits++;
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ aliases: [] }));
  });
  const origin = await listen((req, res) => {
    res.writeHead(302, { location: `http://127.0.0.1:${port(target)}/admin/alias` });
    res.end();
  });
  try {
    const url = `http://127.0.0.1:${port(origin)}`;
    const { code, stderr } = await runCli(['alias', 'list', '--url', url, '--token', 'test-token']);
    assert.notEqual(code, 0, 'a cross-origin redirect must not be treated as success');
    assert.match(stderr, /refusing a cross-origin redirect/);
    assert.equal(targetHits, 0, 'the second origin must never see the request, token or not');
  } finally {
    origin.close();
    target.close();
  }
});
