#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { dirname, join } from "node:path";
import { platform } from "node:os";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const bin = join(
  __dirname,
  platform() === "win32" ? "super-release.exe" : "super-release"
);

try {
  execFileSync(bin, process.argv.slice(2), { stdio: "inherit" });
} catch (err) {
  if (err.status !== undefined) process.exit(err.status);
  console.error(`Failed to run super-release: ${err.message}`);
  process.exit(1);
}
