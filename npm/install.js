import { execSync } from "node:child_process";
import { createWriteStream, existsSync, mkdirSync, unlinkSync, chmodSync, readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { tmpdir, platform, arch } from "node:os";
import { get } from "node:https";
import { pipeline } from "node:stream/promises";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const { version } = JSON.parse(readFileSync(join(__dirname, "..", "package.json"), "utf8"));
const REPO = "bowlingx/super-release";

const PLATFORM_MAP = {
  "linux-x64": "super-release-linux-x86_64",
  "linux-arm64": "super-release-linux-aarch64",
  "darwin-x64": "super-release-darwin-x86_64",
  "darwin-arm64": "super-release-darwin-aarch64",
};

function download(url) {
  return new Promise((resolve, reject) => {
    get(url, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
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

const key = `${platform()}-${arch()}`;
const artifact = PLATFORM_MAP[key];

if (!artifact) {
  console.error(
    `Unsupported platform: ${key}. Supported: ${Object.keys(PLATFORM_MAP).join(", ")}`
  );
  process.exit(1);
}

const binDir = join(__dirname, "bin");
const binPath = join(binDir, "super-release");

if (existsSync(binPath)) {
  process.exit(0);
}

mkdirSync(binDir, { recursive: true });

const url = `https://github.com/${REPO}/releases/download/v${version}/${artifact}.tar.gz`;
console.log(`Downloading super-release v${version} for ${key}...`);

try {
  const response = await download(url);
  const tmpFile = join(tmpdir(), `super-release-${Date.now()}.tar.gz`);
  await pipeline(response, createWriteStream(tmpFile));
  execSync(`tar xzf ${tmpFile} -C ${binDir}`, { stdio: "ignore" });
  unlinkSync(tmpFile);
  chmodSync(binPath, 0o755);
  console.log(`Installed super-release v${version}`);
} catch (err) {
  console.error(`Failed to install super-release: ${err.message}`);
  console.error(`You can install manually from: https://github.com/${REPO}/releases/tag/v${version}`);
  process.exit(1);
}
