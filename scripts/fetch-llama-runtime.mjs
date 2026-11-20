#!/usr/bin/env node
// Downloads the minimal official llama.cpp `llama-server` runtime (Metal on macOS arm64,
// Vulkan on Windows x64) into `src-tauri/resources/llama/` for bundling inside installers.

import { spawnSync } from 'node:child_process';
import {
  chmodSync,
  copyFileSync,
  existsSync,
  lstatSync,
  mkdirSync,
  readdirSync,
  readlinkSync,
  rmSync,
  symlinkSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const LLAMA_TAG = 'b12596';
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const destDir = join(root, 'src-tauri', 'resources', 'llama');
mkdirSync(destDir, { recursive: true });

const isMac = process.platform === 'darwin';
const isWin = process.platform === 'win32';
const serverBin = isWin ? 'llama-server.exe' : 'llama-server';

if (existsSync(join(destDir, serverBin))) {
  console.log(`[llama-runtime] Already present at ${join(destDir, serverBin)}`);
  process.exit(0);
}

let url;
let archiveName;
if (isMac && process.arch === 'arm64') {
  archiveName = `llama-${LLAMA_TAG}-bin-macos-arm64.tar.gz`;
  url = `https://github.com/ggml-org/llama.cpp/releases/download/${LLAMA_TAG}/${archiveName}`;
} else if (isWin && process.arch === 'x64') {
  archiveName = `llama-${LLAMA_TAG}-bin-win-vulkan-x64.zip`;
  url = `https://github.com/ggml-org/llama.cpp/releases/download/${LLAMA_TAG}/${archiveName}`;
} else {
  console.log(`[llama-runtime] Skipping prebuilt download on ${process.platform}-${process.arch}`);
  process.exit(0);
}

const workDir = join(tmpdir(), `truongphoto-llama-${LLAMA_TAG}-${Date.now()}`);
mkdirSync(workDir, { recursive: true });
const archivePath = join(workDir, archiveName);

try {
  console.log(`[llama-runtime] Downloading ${url}...`);
  const dl = spawnSync('curl', ['-fSL', '--retry', '3', '-o', archivePath, url], {
    stdio: 'inherit',
  });
  if (dl.status !== 0) {
    throw new Error(`curl exited with status ${dl.status}`);
  }

  const extractDir = join(workDir, 'extract');
  mkdirSync(extractDir, { recursive: true });

  if (archiveName.endsWith('.tar.gz')) {
    const untar = spawnSync('tar', ['-xzf', archivePath, '-C', extractDir], { stdio: 'inherit' });
    if (untar.status !== 0) throw new Error(`tar exited with status ${untar.status}`);
  } else {
    const unzip = spawnSync(
      'powershell',
      ['-NoProfile', '-Command', `Expand-Archive -Path '${archivePath}' -DestinationPath '${extractDir}' -Force`],
      { stdio: 'inherit' },
    );
    if (unzip.status !== 0) throw new Error(`Expand-Archive exited with status ${unzip.status}`);
  }

  // Locate directory containing llama-server
  const findBinDir = (dir) => {
    if (existsSync(join(dir, serverBin))) return dir;
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      if (entry.isDirectory()) {
        const found = findBinDir(join(dir, entry.name));
        if (found) return found;
      }
    }
    return null;
  };

  const binDir = findBinDir(extractDir);
  if (!binDir) {
    throw new Error(`Could not find ${serverBin} inside extracted archive`);
  }

  // Copy llama-server and required shared libraries only
  const keepFile = (name) => {
    if (name === serverBin) return true;
    if (isMac) {
      return (
        name.endsWith('.dylib') &&
        (name.startsWith('libllama') ||
          name.startsWith('libggml') ||
          name.startsWith('libmtmd'))
      );
    }
    if (isWin) {
      return name.toLowerCase().endsWith('.dll');
    }
    return false;
  };

  let copied = 0;
  for (const name of readdirSync(binDir)) {
    if (!keepFile(name)) continue;
    const src = join(binDir, name);
    const dst = join(destDir, name);
    if (existsSync(dst)) rmSync(dst, { force: true });

    const stat = lstatSync(src);
    if (stat.isSymbolicLink()) {
      symlinkSync(readlinkSync(src), dst);
    } else {
      copyFileSync(src, dst);
      if (isMac && name === serverBin) {
        chmodSync(dst, 0o755);
      }
    }
    copied += 1;
  }

  console.log(`[llama-runtime] Staged ${copied} runtime files into ${destDir}`);
} finally {
  rmSync(workDir, { recursive: true, force: true });
}
