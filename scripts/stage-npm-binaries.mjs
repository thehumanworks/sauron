#!/usr/bin/env node

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const sourceRoot = path.resolve(process.argv[2] ?? path.join(repoRoot, 'dist', 'release-artifacts'));
const destinationRoot = path.resolve(process.argv[3] ?? path.join(repoRoot, 'npm', 'bin'));
const targetConfigPath = path.join(repoRoot, 'distribution', 'targets.json');
const targetConfig = JSON.parse(fs.readFileSync(targetConfigPath, 'utf8'));

for (const target of targetConfig.targets) {
  const destinationDir = path.join(destinationRoot, target.triple);
  fs.rmSync(destinationDir, { force: true, recursive: true });
}

for (const target of targetConfig.targets) {
  const sourceBinary = path.join(sourceRoot, target.triple, targetConfig.binaryName);

  if (!fs.existsSync(sourceBinary)) {
    throw new Error(`Missing binary for ${target.triple}: ${sourceBinary}`);
  }

  const destinationDir = path.join(destinationRoot, target.triple);
  const destinationBinary = path.join(destinationDir, targetConfig.binaryName);
  fs.mkdirSync(destinationDir, { recursive: true });
  fs.copyFileSync(sourceBinary, destinationBinary);
  fs.chmodSync(destinationBinary, 0o755);
  console.log(`${target.triple}: ${destinationBinary}`);
}
