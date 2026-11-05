import { spawnSync } from "node:child_process";
import { constants } from "node:fs";
import { access, mkdir, readFile, readdir, realpath, writeFile } from "node:fs/promises";
import path from "node:path";
import { appRoot, buildApp, buildCore, isMain, prepareElectron, run } from "./build.mjs";
import { check } from "./check.mjs";
import { isolatedEnvironment } from "./dev.mjs";

export async function buildIcon() {
  // The same unmodified brand asset is served at luvus.dev/brand/luvus-logo.png.
  const source = path.resolve(appRoot, '../website/public/brand/luvus-logo.png');
  const iconset = path.join(appRoot, '.build/Luvus.iconset');
  await mkdir(iconset, { recursive: true });
  for (const size of [16, 32, 128, 256, 512]) {
    for (const scale of [1, 2]) {
      run('/usr/bin/sips', ['-z', String(size * scale), String(size * scale), source,
        '--out', path.join(iconset, `icon_${size}x${size}${scale === 2 ? '@2x' : ''}.png`)]);
    }
  }
  run('/usr/bin/iconutil', ['-c', 'icns', iconset, '-o', path.join(appRoot, '.build/icon.icns')]);
}

export async function writeNotices() {
  const lock = JSON.parse(await readFile(path.join(appRoot, "package-lock.json"), "utf8"));
  const notices = ["Third-party notices for the JavaScript and CSS bundled with Luvus desktop.\n"];
  for (const [relative, entry] of Object.entries(lock.packages)) {
    if (!relative.includes("node_modules/") || entry.link) continue;
    if (entry.dev && relative !== "node_modules/tailwindcss") continue;
    const directory = path.join(appRoot, relative);
    let files;
    try {
      files = await readdir(directory);
    } catch (error) {
      if (error.code === "ENOENT" && entry.optional) continue;
      throw error;
    }
    const manifest = JSON.parse(await readFile(path.join(directory, "package.json"), "utf8"));
    const licenseFiles = files.filter((name) => /^(licen[cs]e|copying|notice)(\.|$)/i.test(name)).sort();
    notices.push(`\n--- ${manifest.name}@${manifest.version} (${manifest.license ?? entry.license ?? "see package notices"}) ---\n`);
    if (licenseFiles.length === 0) {
      throw new Error(`Missing third-party license notice: ${manifest.name}`);
    }
    for (const filename of licenseFiles) notices.push(await readFile(path.join(directory, filename), "utf8"));
  }
  const directory = path.join(appRoot, ".build");
  await mkdir(directory, { recursive: true });
  await writeFile(path.join(directory, "third-party-notices.txt"), notices.join("\n"));
}

export async function pack() {
  if (process.platform !== "darwin" || process.arch !== "arm64") {
    throw new Error("pack:mac requires macOS arm64; cross-platform packaging is not configured");
  }
  await check();
  await buildApp();
  const env = isolatedEnvironment();
  // Credentials and discovery are excluded even if inherited from the calling shell.
  for (const key of Object.keys(env)) {
    if (/^(CSC_|WIN_CSC_|APPLE_|ASC_|GH_TOKEN$|GITHUB_TOKEN$|BT_TOKEN$|EP_)/.test(key)) delete env[key];
  }
  env.CSC_IDENTITY_AUTO_DISCOVERY = "false";
  const binary = buildCore({ release: true, env });
  await access(binary, constants.X_OK);
  if (await realpath(binary) !== binary) throw new Error("Release binary must not be a symlink");
  const architecture = spawnSync("/usr/bin/lipo", ["-archs", binary], { encoding: "utf8" });
  if (architecture.error) throw architecture.error;
  if (architecture.status !== 0 || architecture.stdout.trim() !== "arm64") {
    throw new Error("The embedded release binary must be macOS arm64");
  }
  await writeNotices();
  await buildIcon();
  await prepareElectron(env);
  run(path.join(appRoot, "node_modules/.bin/electron-builder"), [
    "--config", path.join(appRoot, "electron-builder.config.cjs"),
    "--mac", "dmg", "--arm64", "--publish", "never",
  ], { env });
  const version = JSON.parse(await readFile(path.join(appRoot, "package.json"), "utf8")).version;
  const application = path.join(appRoot, "release/mac-arm64/Luvus.app");
  const embedded = path.join(application, "Contents/Resources/luvus");
  if (!(await readFile(binary)).equals(await readFile(embedded))) {
    throw new Error("Packaged core differs from the exact target/release/luvus binary");
  }
  const dmg = path.join(appRoot, `release/Luvus-${version}-mac-arm64.dmg`);
  await access(dmg);
  console.log(`Local macOS artifacts (not notarized or published):\n${application}\n${dmg}`);
}

if (isMain(import.meta.url)) {
  if (process.argv.length > 2) throw new Error("Usage: node scripts/pack.mjs");
  await pack();
}
