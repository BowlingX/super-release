#!/usr/bin/env node

import { execFileSync, execSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  createWriteStream,
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
  execFileSync(binPath, process.argv.slice(2), { stdio: "inherit" });
} catch (err) {
  process.exit(err.status ?? 1);
}

async function install() {
  const REPO = "bowlingx/super-release";
  const PLATFORM_MAP = {
    "linux-x64": "super-release-linux-x86_64",
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

    if (isWindows) {
      execSync(`powershell -Command "Expand-Archive -Path '${tmpFile}' -DestinationPath '${__dirname}' -Force"`, { stdio: "ignore" });
    } else {
      execSync(`tar xzf ${tmpFile} -C ${__dirname}`, { stdio: "ignore" });
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
