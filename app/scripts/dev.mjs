import { spawn } from "node:child_process";
import { constants } from "node:fs";
import { access, chmod, mkdir, mkdtemp, readFile, realpath, writeFile } from "node:fs/promises";
import path from "node:path";
import { appRoot, repoRoot, buildApp, buildCore, isMain, prepareElectron, requireFile } from "./build.mjs";

export function isolatedEnvironment(inherited = process.env) {
  // Preserve LUVUS_ENV: isolation must never bypass the product's nested-client guard.
  return Object.fromEntries(Object.entries(inherited).filter(([key]) =>
    (!key.startsWith("LUVUS_") || key === "LUVUS_ENV") &&
    !key.toLowerCase().startsWith("electron_") && !key.startsWith("npm_config_electron_") &&
    key !== "npm_config_arch" && key !== "npm_config_platform" &&
    key !== "NODE_OPTIONS" && key !== "NODE_PATH",
  ));
}

export async function createFixture(binary) {
  const target = path.join(repoRoot, "target");
  await mkdir(target, { recursive: true });
  if (await realpath(target) !== target) throw new Error("The fixture target directory must not be a symlink");
  const expectedBinary = path.join(target, "debug/luvus");
  if (binary !== expectedBinary || await realpath(binary) !== expectedBinary) {
    throw new Error("Development requires the exact target/debug/luvus binary");
  }
  await access(binary, constants.X_OK);
  const root = await mkdtemp(path.join(target, "app-e2e-dev-"));
  await chmod(root, 0o700);
  const marker = { root, binary, session: "app-dev", ownerPid: process.pid };
  await writeFile(path.join(root, ".isolated-app"), JSON.stringify(marker), { flag: "wx", mode: 0o600 });
  const home = path.join(root, "home");
  const workspace = path.join(root, "workspace");
  const userData = path.join(root, "electron");
  for (const directory of [home, workspace, userData]) await mkdir(directory, { mode: 0o700 });
  await writeFile(path.join(home, 'config.json'), JSON.stringify({ check_updates: false }), { flag: 'wx', mode: 0o600 });
  const env = {
    ...isolatedEnvironment(),
    HOME: home,
    XDG_CONFIG_HOME: path.join(home, 'config'),
    XDG_DATA_HOME: path.join(home, 'data'),
    XDG_CACHE_HOME: path.join(home, 'cache'),
    XDG_STATE_HOME: path.join(home, 'state'),
    LUVUS_APP_BINARY: binary,
    LUVUS_HOME: home,
    LUVUS_SESSION: marker.session,
    LUVUS_APP_FIXTURE: root,
    LUVUS_APP_USER_DATA: userData,
  };
  return { ...marker, home, workspace, userData, env };
}

export async function dev() {
  if (process.platform !== "darwin" || process.arch !== "arm64") {
    throw new Error("The desktop development target is macOS arm64");
  }
  if (process.env.LUVUS_ENV === "1") {
    throw new Error("Run app development outside a managed Luvus pane; allow_nested is not bypassed");
  }
  const env = isolatedEnvironment();
  await requireFile("apps/renderer/src/main.tsx");
  await buildApp({ desktopOnly: true });
  const binary = buildCore({ env });
  const fixture = await createFixture(binary);
  const { createServer } = await import("vite");
  const vite = await createServer({ configFile: path.join(appRoot, "apps/renderer/vite.config.ts") });
  let child;
  let interrupt;
  let terminate;
  try {
    await vite.listen();
    const url = "http://127.0.0.1:5173";
    const marker = JSON.parse(await readFile(path.join(fixture.root, ".isolated-app"), "utf8"));
    if (marker.root !== fixture.root || marker.binary !== fixture.binary || marker.session !== "app-dev") {
      throw new Error("Development fixture marker changed before launch");
    }
    const electron = await prepareElectron(fixture.env);
    console.log(JSON.stringify({ binary: fixture.binary, home: fixture.home, session: fixture.session, renderer: url }));
    child = spawn(electron, [appRoot, "--session", "app-dev", `--user-data-dir=${fixture.userData}`], {
      cwd: fixture.workspace,
      env: { ...fixture.env, LUVUS_APP_DEV_URL: url },
      stdio: "inherit",
    });
    interrupt = () => child.kill("SIGINT");
    terminate = () => child.kill("SIGTERM");
    process.once("SIGINT", interrupt);
    process.once("SIGTERM", terminate);
    const code = await new Promise((resolve, reject) => {
      child.once("error", reject);
      child.once("exit", (status, signal) => resolve(status ?? (signal === "SIGINT" ? 130 : 1)));
    });
    process.exitCode = code;
  } finally {
    if (interrupt) process.removeListener("SIGINT", interrupt);
    if (terminate) process.removeListener("SIGTERM", terminate);
    if (child && child.exitCode === null && child.signalCode === null) child.kill("SIGTERM");
    await vite.close();
    // Client exit intentionally leaves its isolated server/PTYS and marked home intact.
    console.log(`Development fixture retained (not deleted or stopped): ${fixture.root}`);
  }
}

if (isMain(import.meta.url)) {
  if (process.argv.length > 2) throw new Error("Usage: node scripts/dev.mjs");
  await dev();
}
