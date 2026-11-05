import { spawnSync } from "node:child_process";
import { access, copyFile, mkdir, readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

export const appRoot = fileURLToPath(new URL("..", import.meta.url));
export const repoRoot = path.resolve(appRoot, "..");

export function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: appRoot,
    env: process.env,
    stdio: "inherit",
    ...options,
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`${command} failed (${result.signal ?? result.status})`);
  }
}

export async function prepareElectron(env) {
  const directory = path.join(appRoot, "node_modules/electron");
  const cache = path.join(appRoot, ".build/electron-cache");
  await mkdir(cache, { recursive: true });
  run(process.execPath, [path.join(directory, "install.js")], {
    timeout: 600_000,
    env: {
      ...env,
      electron_config_cache: cache,
      ELECTRON_INSTALL_PLATFORM: "darwin",
      ELECTRON_INSTALL_ARCH: "arm64",
    },
  });
  const manifest = JSON.parse(await readFile(path.join(directory, "package.json"), "utf8"));
  const installed = (await readFile(path.join(directory, "dist/version"), "utf8")).trim().replace(/^v/, "");
  if (installed !== manifest.version) throw new Error("Downloaded Electron version differs from the npm package");
  const executable = path.join(directory, "dist/Electron.app/Contents/MacOS/Electron");
  const architecture = spawnSync("/usr/bin/lipo", ["-archs", executable], { encoding: "utf8" });
  if (architecture.error) throw architecture.error;
  if (architecture.status !== 0 || architecture.stdout.trim() !== "arm64") {
    throw new Error("The installed Electron binary must be macOS arm64");
  }
  return executable;
}

export function buildCore({ release = false, env = process.env } = {}) {
  const binary = path.join(repoRoot, "target", release ? "release" : "debug", "luvus");
  const args = [
    "build", "--locked", "--bin", "luvus", "--target-dir", path.join(repoRoot, "target"),
    "--message-format=json-render-diagnostics", ...(release ? ["--release"] : []),
  ];
  const result = spawnSync("cargo", args, {
    cwd: repoRoot,
    env,
    stdio: ["ignore", "pipe", "inherit"],
    encoding: "utf8",
    maxBuffer: 16 * 1024 * 1024,
  });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`Core build failed (${result.signal ?? result.status})`);
  const artifacts = result.stdout.split("\n").filter(Boolean).map((line) => JSON.parse(line));
  if (!artifacts.some((artifact) => artifact.reason === "compiler-artifact" && artifact.executable === binary)) {
    throw new Error(`Cargo did not build the exact expected executable: ${binary}`);
  }
  return binary;
}

export function isMain(url) {
  return Boolean(process.argv[1]) && pathToFileURL(path.resolve(process.argv[1])).href === url;
}

export async function requireFile(relativePath) {
  const filename = path.join(appRoot, relativePath);
  try {
    await access(filename);
  } catch {
    throw new Error(`Required implementation is missing: ${filename}`);
  }
  return filename;
}

export async function buildProtocol() {
  const declarations = await requireFile("packages/protocol/generated/contracts.d.ts");
  await requireFile("packages/protocol/generated/schema.json");
  run(path.join(appRoot, "node_modules/.bin/tsc"), [
    "--project", path.join(appRoot, "packages/protocol/tsconfig.json"),
  ]);
  // TypeScript does not copy input declaration files into the declaration tree.
  const generated = path.join(appRoot, "packages/protocol/dist/generated");
  await mkdir(generated, { recursive: true });
  await copyFile(declarations, path.join(generated, "contracts.d.ts"));
  const { build } = await import("esbuild");
  await build({
    absWorkingDir: appRoot,
    entryPoints: [await requireFile("packages/protocol/src/index.ts")],
    outfile: path.join(appRoot, "packages/protocol/dist/index.js"),
    bundle: true,
    format: "esm",
    platform: "neutral",
    target: "es2023",
    external: ["ajv"],
    legalComments: "eof",
  });
}

export async function buildLibraries() {
  for (const workspace of ["protocol", "terminal-view"]) {
    await requireFile(`packages/${workspace}/src/index.ts`);
    run("npm", ["run", "build", "--workspace", `packages/${workspace}`]);
  }
}

export async function buildDesktop() {
  const { build } = await import("esbuild");
  const outdir = path.join(appRoot, "apps/desktop/dist");
  await mkdir(outdir, { recursive: true });
  const shared = {
    absWorkingDir: appRoot,
    bundle: true,
    format: "cjs",
    external: ["electron"],
    sourcemap: false,
    legalComments: "eof",
    logLevel: "info",
    metafile: true,
  };
  // Only Electron is external: ESM workspace packages are bundled into CJS.
  await build({
    ...shared,
    entryPoints: [await requireFile("apps/desktop/src/main.ts")],
    outfile: path.join(outdir, "main.cjs"),
    platform: "node",
    target: "node24",
  });
  // A sandboxed preload cannot load Node modules or sibling output files.
  const preload = await build({
    ...shared,
    entryPoints: [await requireFile("apps/desktop/src/preload.ts")],
    outfile: path.join(outdir, "preload.cjs"),
    platform: "browser",
    target: "es2023",
    write: false,
  });
  for (const output of Object.values(preload.metafile.outputs)) {
    for (const dependency of output.imports) {
      if (dependency.path !== "electron" || !dependency.external) {
        throw new Error(`Sandboxed preload has an unsupported import: ${dependency.path}`);
      }
    }
  }
  const { writeFile } = await import("node:fs/promises");
  for (const output of preload.outputFiles) await writeFile(output.path, output.contents);
}

export function generateProtocol(binary) {
  run(process.execPath, [path.join(appRoot, 'scripts/generate-protocol.mjs')], {
    env: { ...process.env, LUVUS_APP_BINARY: binary },
  });
}

export async function buildApp({ desktopOnly = false } = {}) {
  generateProtocol(buildCore());
  await buildLibraries();
  run("npm", ["run", "check", "--workspace", "@luvus/desktop"]);
  await buildDesktop();
  if (!desktopOnly) {
    await requireFile("apps/renderer/src/main.tsx");
    run("npm", ["run", "check", "--workspace", "@luvus/app-renderer"]);
    const { build } = await import("vite");
    await build({ configFile: path.join(appRoot, "apps/renderer/vite.config.ts") });
  }
}

if (isMain(import.meta.url)) {
  const args = process.argv.slice(2);
  if (args.length > 1 || (args.length === 1 && !["--desktop", "--protocol"].includes(args[0]))) {
    throw new Error("Usage: node scripts/build.mjs [--desktop | --protocol]");
  }
  if (args[0] === "--protocol") await buildProtocol();
  else await buildApp({ desktopOnly: args[0] === "--desktop" });
}
