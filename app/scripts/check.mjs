import { spawnSync } from "node:child_process";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { isDeepStrictEqual } from "node:util";
import { appRoot, repoRoot, buildCore, buildLibraries, generateProtocol, isMain, requireFile, run } from "./build.mjs";
import { createFixture, isolatedEnvironment } from "./dev.mjs";

async function checkContracts() {
  const binary = buildCore({ env: isolatedEnvironment() });
  generateProtocol(binary);
  const schemaPath = await requireFile("packages/protocol/generated/schema.json");
  const typesPath = await requireFile("packages/protocol/generated/contracts.d.ts");
  const fixture = await createFixture(binary);
  // The reviewed --schema route is offline. Use a marked home even for this export.
  console.log(JSON.stringify({ binary: fixture.binary, home: fixture.home, session: fixture.session, operation: "offline-schema" }));
  const result = spawnSync(fixture.binary, ["--session", fixture.session, "client", "--surface", "app", "--schema"], {
    cwd: fixture.workspace,
    env: fixture.env,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
    timeout: 10_000,
    maxBuffer: 8 * 1024 * 1024,
  });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`Offline core schema export failed (${result.signal ?? result.status})`);
  const document = JSON.parse(result.stdout);
  if (!isDeepStrictEqual(document, JSON.parse(await readFile(schemaPath, "utf8")))) {
    throw new Error("Generated App schema is stale; run scripts/generate-protocol.mjs with the current debug binary");
  }
  const { compile } = await import("json-schema-to-typescript");
  const expected = await compile({ ...document, title: "LuvusAppProtocol" }, "LuvusAppProtocol", {
    unreachableDefinitions: true,
    bannerComment: "",
    additionalProperties: false,
    unknownAny: true,
  });
  if (expected !== await readFile(typesPath, "utf8")) {
    throw new Error("Generated App TypeScript declarations are stale; run scripts/generate-protocol.mjs");
  }
}

export async function check() {
  const manifest = JSON.parse(await readFile(path.join(appRoot, "package.json"), "utf8"));
  const cargo = await readFile(path.join(repoRoot, "Cargo.toml"), "utf8");
  const version = cargo.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
  if (version !== manifest.version) throw new Error("App and core package versions differ");
  for (const directory of ["apps/desktop", "apps/renderer", "packages/protocol", "packages/terminal-view"]) {
    const workspace = JSON.parse(await readFile(path.join(appRoot, directory, "package.json"), "utf8"));
    if (workspace.version !== version) throw new Error(`Version mismatch: ${directory}`);
  }
  await checkContracts();
  await buildLibraries();
  for (const workspace of ["@luvus/desktop", "@luvus/app-renderer"]) {
    run("npm", ["run", "check", "--workspace", workspace]);
  }
  run(path.join(appRoot, "node_modules/.bin/tsc"), [
    "--project", path.join(appRoot, "apps/renderer/tsconfig.node.json"), "--noEmit",
  ]);
}

if (isMain(import.meta.url)) {
  if (process.argv.length > 2) throw new Error("Usage: node scripts/check.mjs");
  await check();
}
