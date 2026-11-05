import assert from 'node:assert/strict';
import { mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';
import test from 'node:test';
import { build } from 'esbuild';

test('desktop checks the running protocol, not the package version, before binary attachment', async () => {
  const root = await mkdtemp(fileURLToPath(new URL('../../target/app-e2e-preflight-', import.meta.url)));
  const compiled = join(root, 'core-client.cjs'), binary = join(root, 'fixture-core');
  await build({ entryPoints: [fileURLToPath(new URL('../apps/desktop/src/core-client.ts', import.meta.url))],
    bundle: true, platform: 'node', format: 'cjs', outfile: compiled });
  const { CoreClient, IncompatibleCoreError } = createRequire(import.meta.url)(compiled);
  await writeFile(binary, `#!${process.execPath}
const fs = require('node:fs');
for (const key of ['LUVUS_SOCKET_PATH','LUVUS_SESSION','LUVUS_REMOTE_HOST','LUVUS_REMOTE_SESSION','LUVUS_PANE_ID','LUVUS_API_ADDRESS']) {
  if (process.env[key]) throw Error('inherited route leaked: ' + key);
}
const args = process.argv.slice(2);
fs.appendFileSync(process.env.FIXTURE_CALLS, JSON.stringify(args) + '\\n');
if (args[2] === 'uhp') console.log(JSON.stringify({result: {remote_display: JSON.parse(process.env.FIXTURE_DISPLAY)}}));
if (args[2] === 'client') { process.stdin.resume(); process.stdin.on('end', () => process.exit(0)); }
`, { mode: 0o700 });
  for (const [name, display, compatible] of [
    ['released', { transport: 20, compatible_transports: [19, 20], capabilities: [] }, false],
    ['missing-feature', { transport: 21, compatible_transports: [20, 21], capabilities: [] }, false],
    ['future-incompatible', { transport: 22, compatible_transports: [22], capabilities: ['workspace_app.v1'] }, false],
    ['compatible', { transport: 21, compatible_transports: [20, 21], capabilities: ['workspace_app.v1'] }, true],
  ]) {
    const calls = join(root, name + '.jsonl');
    const client = new CoreClient(binary, 'chosen', { ...process.env, FIXTURE_CALLS: calls, FIXTURE_DISPLAY: JSON.stringify(display),
      LUVUS_SOCKET_PATH: 'must-not-use', LUVUS_SESSION: 'wrong', LUVUS_REMOTE_HOST: 'wrong', LUVUS_PANE_ID: 'wrong' });
    try {
      if (compatible) await client.start();
      else await assert.rejects(client.start(), IncompatibleCoreError);
    } finally { await client.close(); }
    const seen = (await readFile(calls, 'utf8')).trim().split('\n').map(JSON.parse);
    assert.deepEqual(seen, [
      ['--session', 'chosen', 'server', 'start'],
      ['--session', 'chosen', 'uhp', 'capabilities'],
      ...(compatible ? [['--session', 'chosen', 'client', '--surface', 'app', '--stdio']] : []),
    ]);
  }
});
