#!/usr/bin/env node
"use strict";
// Runs the cutwater executable of the platform package that npm installed
// for this machine, one of the optional dependencies.
const { spawnSync } = require("node:child_process");

const PACKAGES = {
  "darwin arm64": "@sakiori/cutwater-bin-darwin-arm64",
  "darwin x64": "@sakiori/cutwater-bin-darwin-x64",
  "linux arm64": "@sakiori/cutwater-bin-linux-arm64",
  "linux x64": "@sakiori/cutwater-bin-linux-x64",
};

const platform = `${process.platform} ${process.arch}`;
const name = PACKAGES[platform];
if (name === undefined) {
  console.error(`cutwater has no executable for ${platform}; it runs on ${Object.keys(PACKAGES).join(", ")}.`);
  process.exit(2);
}
let executable;
try {
  executable = require.resolve(`${name}/cutwater`);
} catch {
  console.error(
    `${name} is not installed, which holds cutwater for ${platform}. ` +
      "npm installs it as an optional dependency of cutwater: install without --omit=optional or --no-optional.",
  );
  process.exit(2);
}
const result = spawnSync(executable, process.argv.slice(2), { stdio: "inherit" });
if (result.error) {
  console.error(`cannot run ${executable}: ${result.error.message}`);
  process.exit(2);
}
if (result.signal) {
  process.kill(process.pid, result.signal);
}
process.exit(result.status ?? 2);
