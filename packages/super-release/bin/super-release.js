#!/usr/bin/env node

import { spawnSync, execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { arch, platform } from "node:os";
import { fileURLToPath } from "node:url";
import pkg from "../package.json" with { type: "json" };

function isMusl() {
  try {
    const result = execFileSync("ldd", ["--version"], {
      stdio: ["pipe", "pipe", "pipe"],
    });
    return result.toString().includes("musl");
  } catch (err) {
    if (err.stderr && err.stderr.toString().includes("musl")) {
      return true;
    }
    return (
      existsSync("/lib/ld-musl-x86_64.so.1") ||
      existsSync("/lib/ld-musl-aarch64.so.1")
    );
  }
}

function getBinaryPath() {
  const os = platform() === "win32" ? "windows" : platform();
  const cpu = arch();
  const pkgName = `super-release-${os}-${cpu}`;

  try {
    if (os === "linux" && isMusl()) {
      return fileURLToPath(import.meta.resolve(`${pkgName}/musl`));
    }
    return fileURLToPath(import.meta.resolve(pkgName));
  } catch {
    throw new Error(
      `Unsupported platform: ${os}-${cpu}. Install the platform-specific package "${pkgName}" manually.`
    );
  }
}

const result = spawnSync(getBinaryPath(), process.argv.slice(2), {
  stdio: "inherit",
  env: { ...process.env, SUPER_RELEASE_VERSION: pkg.version },
});

process.exit(result.status ?? 1);
