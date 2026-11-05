module.exports = {
  appId: "dev.luvus.app",
  productName: "Luvus",
  electronVersion: "45.2.2",
  electronDist: "node_modules/electron/dist",
  directories: {
    app: ".",
    output: "release",
    buildResources: ".build",
  },
  files: [
    "package.json",
    "apps/desktop/dist/**",
    "apps/renderer/dist/**",
    "!**/*.map",
    "!**/node_modules/**",
  ],
  extraResources: [
    { from: "../target/release/luvus", to: "luvus" },
    { from: "../LICENSE", to: "LICENSE.luvus" },
    { from: ".build/third-party-notices.txt", to: "third-party-notices.txt" },
  ],
  asar: true,
  npmRebuild: false,
  nodeGypRebuild: false,
  forceCodeSigning: false,
  publish: null,
  artifactName: "Luvus-${version}-mac-${arch}.${ext}",
  async afterPack(context) {
    const path = require("node:path");
    const { realpath } = require("node:fs/promises");
    const application = path.join(context.appOutDir, "Luvus.app");
    const expected = path.join(__dirname, "release/mac-arm64/Luvus.app");
    if (application !== expected || await realpath(application) !== expected) {
      throw new Error("Refusing to sign outside the local arm64 build directory");
    }
    // Builder's identity "-" still searches Keychain before its ad-hoc fallback.
    // Use its installed signer directly, with all identity/profile discovery off.
    const { createRequire } = require("node:module");
    const builderRequire = createRequire(require.resolve("app-builder-lib/package.json"));
    const { signAsync } = builderRequire("@electron/osx-sign");
    await signAsync({
      app: application,
      platform: "darwin",
      identity: "-",
      identityValidation: false,
      preAutoEntitlements: false,
      preEmbedProvisioningProfile: false,
      gatekeeperAssess: false,
      // Retain the exact Cargo binary, including its linker-generated signature.
      ignore: (filename) => filename === path.join(application, "Contents/Resources/luvus"),
      optionsForFile: () => ({ hardenedRuntime: false, timestamp: "none" }),
    });
  },
  mac: {
    icon: ".build/icon.icns",
    target: [{ target: "dmg", arch: ["arm64"] }],
    category: "public.app-category.developer-tools",
    identity: null,
    notarize: false,
    hardenedRuntime: false,
    gatekeeperAssess: false,
  },
  dmg: {
    sign: false,
  },
};
