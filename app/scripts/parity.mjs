// Inventory and release gate. An entrypoint in source is not runtime evidence.
import assert from 'node:assert/strict';
import { access, readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';

const app = fileURLToPath(new URL('..', import.meta.url));
const inventory = JSON.parse(await readFile(join(app, 'tests/feature-parity.json'), 'utf8'));
const schema = JSON.parse(await readFile(join(app, '../protocol/uhp/v1/schema/request.schema.json'), 'utf8'));
const required = new Set(schema.properties.method.enum.map(method => method.split('.')[0]));
const covered = new Set();
let open = 0;
for (const domain of inventory.domains) {
  await access(join(app, 'apps/renderer/src', domain.surface));
  for (const family of domain.methods) {
    assert.ok(required.has(family) && !covered.has(family), 'Unknown or duplicate method family: ' + family);
    covered.add(family);
  }
  for (const name of domain.scenarios) {
    const scenario = inventory.scenarios[name];
    assert.ok(scenario?.check, 'Missing scenario: ' + name);
    await access(join(app, 'tests', scenario.driver));
  }
  assert.ok(domain.scenarios.length || domain.remaining.length, 'Empty parity row: ' + domain.area);
  open += domain.remaining.length;
  console.log(domain.area + ': ' + domain.scenarios.length + ' linked scenarios; ' + domain.remaining.length + ' open gates');
}
assert.deepEqual(covered, required);
console.log(`${open} open gates. Source inventory does not certify GUI behavior.`);
if (process.argv.includes('--require-complete') && open) process.exitCode = 1;
