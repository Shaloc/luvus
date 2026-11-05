// Explicitly provision only a test build below an existing remote checkout.
import assert from 'node:assert/strict';
import { execFile, spawn } from 'node:child_process';
import { basename, dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { once } from 'node:events';
import { pipeline } from 'node:stream/promises';

const [host, checkout] = process.argv.slice(2);
if (!host || !checkout || !/^[A-Za-z0-9_.][\w.-]*$/.test(host) || !/^\/[\w./-]+$/.test(checkout)
    || checkout.split('/').includes('..')) throw new Error('Expected SSH alias and absolute existing checkout path');
const repo = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const options = ['-T', '-o', 'BatchMode=yes', '-o', 'StrictHostKeyChecking=yes', '-o', 'ForwardAgent=no',
  '-o', 'ControlMaster=no', '-o', 'ControlPath=none', '-o', 'ClearAllForwardings=yes', '-o', 'PermitLocalCommand=no',
  '-o', 'ConnectTimeout=8', host];
const ssh = '/usr/bin/ssh';
const execute = promisify(execFile);
const created = await execute(ssh, [...options,
  `set -eu; cd '${checkout}'; test "$(pwd -P)" = '${checkout}'; test "$(git rev-parse --show-toplevel)" = '${checkout}'; test -d target; test ! -L target; umask 077; r=$(mktemp -d "$PWD/target/app-e2e-real-XXXXXXXX"); mkdir "$r/source" "$r/home" "$r/state" "$r/workspace" "$r/node_modules"; printf '%s\\n' "$r" > "$r/.isolated-app"; printf '%s\\n' "$r"`]);
const root = created.stdout.trim();
assert.ok(root.startsWith(checkout + '/target/app-e2e-real-') && /^\/[\w./-]+$/.test(root));
console.log('Isolated owner fixture: ' + root);
async function archive(tarArgs, destination, list) {
  const tar = spawn('/usr/bin/tar', ['--no-xattrs', '--no-mac-metadata', '-czf', '-', ...tarArgs], { cwd: repo, stdio: ['pipe', 'pipe', 'inherit'] });
  const remote = spawn(ssh, [...options, `test "$(cat '${root}/.isolated-app')" = '${root}' && tar -xzf - -C '${destination}'`], { stdio: ['pipe', 'inherit', 'inherit'] });
  const result = Promise.all([once(tar, 'exit'), once(remote, 'exit'), pipeline(tar.stdout, remote.stdin)]);
  tar.stdin.end(list);
  const [tarExit, sshExit] = await result;
  assert.equal(tarExit[0], 0); assert.equal(sshExit[0], 0);
}
const source = await execute('git', ['ls-files', '-co', '--exclude-standard', '-z', '--', 'Cargo.toml', 'Cargo.lock',
  'build.rs', 'LICENSE', 'README.md', 'install.sh', 'src', 'vendor', 'protocol', 'changelog', 'skills/luvus'],
  { cwd: repo, encoding: 'buffer', maxBuffer: 4 * 1024 * 1024 });
await archive(['--null', '-T', '-'], root + '/source', source.stdout);
await archive(['-C', 'app/tests', 'real-owner.py', 'owner-cdp.mjs','agent-browser.mjs'], root);
const tool=process.env.LUVUS_TEST_AGENT_BROWSER_LINUX;
if (tool) {
  assert.equal(basename(tool),'agent-browser-linux-musl-x64');
  await archive(['-C',dirname(resolve(tool)),basename(tool)],root);
  await execute(ssh,[...options,`test "$(cat '${root}/.isolated-app')" = '${root}' && chmod u+x '${root}/agent-browser-linux-musl-x64'`]);
}
await archive(['-C', 'app/node_modules', 'playwright-core'], root + '/node_modules');
const build = spawn(ssh, [...options,
  `set -eu; cd '${root}/source'; if ! command -v cargo >/dev/null; then export PATH="$HOME/.rustup/toolchains/stable-$(uname -m)-unknown-linux-gnu/bin:$PATH"; fi; export CARGO_TARGET_DIR='${root}/build' CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 LUVUS_BUILD_ID='${root.split('/').at(-1)}'; cargo build --offline --locked --bin luvus -j 6; sha256sum "$CARGO_TARGET_DIR/debug/luvus"`], { stdio: 'inherit' });
assert.deepEqual(await once(build, 'exit'), [0, null]);
console.log(`Run: npm --prefix app run test:ssh -- ${host} ${root}`);
