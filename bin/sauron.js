#!/usr/bin/env node

'use strict';

const { spawnSync } = require('node:child_process');
const { existsSync, readFileSync } = require('node:fs');
const path = require('node:path');

const repoRoot = path.resolve(__dirname, '..');
const targetConfig = JSON.parse(
  readFileSync(path.join(repoRoot, 'distribution', 'targets.json'), 'utf8')
);

function resolveTarget() {
  return targetConfig.targets.find(
    (target) => target.platform === process.platform && target.arch === process.arch
  );
}

function supportedTargetList() {
  return targetConfig.targets
    .map((target) => `${target.platform}/${target.arch} -> ${target.triple}`)
    .join(', ');
}

const target = resolveTarget();

if (!target) {
  console.error(
    `Unsupported host ${process.platform}/${process.arch}. Supported targets: ${supportedTargetList()}`
  );
  process.exit(1);
}

const binaryPath = path.join(repoRoot, 'npm', 'bin', target.triple, targetConfig.binaryName);

if (!existsSync(binaryPath)) {
  console.error(
    `Missing bundled binary for ${target.triple} at ${binaryPath}. Reinstall @nothumanwork/sauron or fetch a release asset for this target.`
  );
  process.exit(1);
}

const result = spawnSync(binaryPath, process.argv.slice(2), { stdio: 'inherit' });

if (result.error) {
  console.error(`Failed to launch ${binaryPath}: ${result.error.message}`);
  process.exit(1);
}

if (result.signal) {
  process.kill(process.pid, result.signal);
}

process.exit(result.status ?? 1);
