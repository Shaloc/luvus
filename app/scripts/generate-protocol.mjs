import { execFileSync } from 'node:child_process';
import { mkdir, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { compile } from 'json-schema-to-typescript';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const binary = resolve(process.env.LUVUS_APP_BINARY ?? resolve(root, 'target/debug/luvus'));
const env = Object.fromEntries(Object.entries(process.env).filter(([name]) => !name.startsWith('LUVUS_')));
const document = JSON.parse(execFileSync(binary, ['client', '--surface', 'app', '--schema'], { env, encoding: 'utf8', maxBuffer: 8 * 1024 * 1024, timeout: 10000 }));
const output = resolve(root, 'app/packages/protocol/generated');
await mkdir(output, { recursive: true });
await writeFile(resolve(output, 'schema.json'), JSON.stringify(document, null, 2) + '\n');
const definitions = Object.keys(document.$defs);
const generated = await compile({ ...document, title: 'LuvusAppProtocol' }, 'LuvusAppProtocol', { unreachableDefinitions: true, bannerComment: '', additionalProperties: false, unknownAny: true });
for (const name of definitions) {
  if (!generated.includes(` ${name} `) && !generated.includes(` ${name} =`) && !generated.includes(` ${name} {`)) throw new Error(`missing generated type ${name}`);
}
await writeFile(resolve(output, 'contracts.d.ts'), generated);
