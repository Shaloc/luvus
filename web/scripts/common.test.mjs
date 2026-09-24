import assert from "node:assert/strict";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { settings, isolatedEnv } from "./common.mjs";

test("managed panes do not reuse their parent session or home", () => {
  const config = settings({
    LUVUS_SOCKET_PATH: "/owner/session.sock",
    LUVUS_PANE_ID: "17",
    LUVUS_SESSION: "owner",
    LUVUS_HOME: "/owner/home",
  });

  assert.equal(config.session, "web-dev");
  assert.equal(config.home, path.join(os.homedir(), ".luvus-dev"));
});

test("dedicated web selectors override inherited pane selectors", () => {
  const config = settings({
    LUVUS_SOCKET_PATH: "/owner/session.sock",
    LUVUS_SESSION: "owner",
    LUVUS_HOME: "/owner/home",
    LUVUS_WEB_SESSION: "browser-test",
    LUVUS_WEB_HOME: "/isolated/web-home",
  });

  assert.equal(config.session, "browser-test");
  assert.equal(config.home, path.resolve("/isolated/web-home"));
});

test("legacy selectors remain available outside a managed pane", () => {
  const config = settings({
    LUVUS_SESSION: "standalone-test",
    LUVUS_HOME: "/standalone/home",
  });

  assert.equal(config.session, "standalone-test");
  assert.equal(config.home, path.resolve("/standalone/home"));
});

test("isolated launch drops remote owner selectors", () => {
  const keys = ["LUVUS_REMOTE_HOST", "LUVUS_REMOTE_SESSION", "LUVUS_API_ADDRESS", "LUVUS_SOCKET_PATH"];
  const before = keys.map((key) => process.env[key]);
  try {
    for (const key of keys) process.env[key] = "production-owner";
    const env = isolatedEnv({ home: "/isolated" });
    for (const key of keys) assert.equal(env[key], undefined);
    assert.equal(env.LUVUS_HOME, "/isolated");
  } finally {
    keys.forEach((key, index) => {
      if (before[index] === undefined) delete process.env[key];
      else process.env[key] = before[index];
    });
  }
});
