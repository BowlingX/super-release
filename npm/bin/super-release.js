#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  createWriteStream,
  readFileSync,
  unlinkSync,
  chmodSync,
} from "node:fs";
import { dirname, join } from "node:path";
import { platform, arch, tmpdir } from "node:os";
import { get } from "node:https";
import { pipeline } from "node:stream/promises";
import { fileURLToPath } from "node:url";
import pkg from "../../package.json" with { type: "json" };

const __dirname = dirname(fileURLToPath(import.meta.url));
const binPath = join(
  __dirname,
  platform() === "win32" ? "super-release.exe" : "super-release"
);

if (!existsSync(binPath)) {
  await install();
}

try {
  execFileSync(binPath, process.argv.slice(2), {
    stdio: "inherit",
    env: { ...process.env, SUPER_RELEASE_VERSION: pkg.version },
  });
  process.exit(0);
} catch (err) {
  if (err.code === "ENOENT" || err.code === "EACCES") {
    console.error(`super-release: binary not found or not executable at ${binPath}`);
    console.error(err);
  } else if (err.status === null) {
    // No exit status = binary couldn't run at all (wrong libc, missing interpreter)
    console.error(`super-release: failed to execute binary at ${binPath}`);
    console.error(`If running on Alpine/musl, ensure the musl build is being downloaded.`);
    console.error(err);
  }
  process.exit(err.status ?? 1);
}

function isMusl() {
  try {
    // ldd --version outputs to stderr on musl and exits non-zero
    const result = execFileSync("ldd", ["--version"], { stdio: ["pipe", "pipe", "pipe"] });
    return result.toString().includes("musl");
  } catch (err) {
    // On musl, ldd exits with error but stderr contains "musl"
    if (err.stderr && err.stderr.toString().includes("musl")) {
      return true;
    }
    return existsSync("/lib/ld-musl-x86_64.so.1") || existsSync("/lib/ld-musl-aarch64.so.1");
  }
}

async function install() {
  const REPO = "bowlingx/super-release";
  const musl = platform() === "linux" && isMusl();
  const PLATFORM_MAP = {
    "linux-x64": musl ? "super-release-linux-x86_64-musl" : "super-release-linux-x86_64",
    "linux-arm64": "super-release-linux-aarch64",
    "darwin-x64": "super-release-darwin-x86_64",
    "darwin-arm64": "super-release-darwin-aarch64",
    "win32-x64": "super-release-windows-x86_64",
  };

  const { version } = pkg;
  const key = `${platform()}-${arch()}`;
  const artifact = PLATFORM_MAP[key];

  if (!artifact) {
    console.error(
      `Unsupported platform: ${key}. Supported: ${Object.keys(PLATFORM_MAP).join(", ")}`
    );
    process.exit(1);
  }

  const isWindows = platform() === "win32";
  const ext = isWindows ? "zip" : "tar.gz";
  const url = `https://github.com/${REPO}/releases/download/v${version}/${artifact}.${ext}`;
  console.error(`Downloading super-release v${version} for ${key}...`);

  mkdirSync(__dirname, { recursive: true });

  const tmpFile = join(tmpdir(), `super-release-${Date.now()}.${ext}`);
  try {
    const response = await download(url);
    await pipeline(response, createWriteStream(tmpFile));

    const hashFile = join(__dirname, `${artifact}.${ext}.sha256`);
    if (existsSync(hashFile)) {
      const expectedHash = readFileSync(hashFile, "utf8").trim().split(/\s+/)[0].toLowerCase();
      const fileBuffer = readFileSync(tmpFile);
      const hashBuffer = await crypto.subtle.digest("SHA-256", fileBuffer);
      const actualHash = Array.from(new Uint8Array(hashBuffer)).map(b => b.toString(16).padStart(2, "0")).join("");
      if (actualHash !== expectedHash) {
        console.error(`Hash mismatch for ${artifact}.${ext}!`);
        console.error(`  Expected: ${expectedHash}`);
        console.error(`  Actual:   ${actualHash}`);
        console.error(`This may indicate a tampered or corrupted download.`);
        unlinkSync(tmpFile);
        process.exit(1);
      }
      console.error(`Hash verified for ${artifact}.${ext}`);
    } else {
      console.error(`No hash file found at ${hashFile}, cannot verify download integrity.`);
      unlinkSync(tmpFile);
      process.exit(1);
    }

    if (isWindows) {
      execFileSync("powershell", ["-Command", `Expand-Archive -Path '${tmpFile}' -DestinationPath '${__dirname}' -Force`], { stdio: "ignore" });
    } else {
      execFileSync("tar", ["xzf", tmpFile, "-C", __dirname], { stdio: "ignore" });
      chmodSync(binPath, 0o755);
    }
    unlinkSync(tmpFile);
    console.error(`Installed super-release v${version}`);
  } catch (err) {
    console.error(`Failed to install super-release: ${err.message}`);
    console.error(
      `Install manually from: https://github.com/${REPO}/releases/tag/v${version}`
    );
    process.exit(1);
  }
}

function download(url) {
  return new Promise((resolve, reject) => {
    get(url, (res) => {
      if (
        res.statusCode >= 300 &&
        res.statusCode < 400 &&
        res.headers.location
      ) {
        return download(res.headers.location).then(resolve, reject);
      }
      if (res.statusCode !== 200) {
        reject(new Error(`Download failed: HTTP ${res.statusCode}`));
        return;
      }
      resolve(res);
    }).on("error", reject);
  });
}
