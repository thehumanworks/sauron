#!/usr/bin/env node

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const cargoTomlPath = path.join(repoRoot, 'Cargo.toml');
const cargoLockPath = path.join(repoRoot, 'Cargo.lock');
const packageJsonPath = path.join(repoRoot, 'package.json');

function readText(filePath) {
  return fs.readFileSync(filePath, 'utf8');
}

function writeText(filePath, nextText) {
  const currentText = readText(filePath);
  if (currentText !== nextText) {
    fs.writeFileSync(filePath, nextText);
  }
}

function parseSemver(version) {
  const match = /^(\d+)\.(\d+)\.(\d+)$/.exec(version);
  if (!match) {
    throw new Error(`Unsupported version format: ${version}`);
  }

  return match.slice(1).map((value) => Number.parseInt(value, 10));
}

function compareSemver(left, right) {
  const leftParts = parseSemver(left);
  const rightParts = parseSemver(right);

  for (let index = 0; index < leftParts.length; index += 1) {
    if (leftParts[index] !== rightParts[index]) {
      return leftParts[index] - rightParts[index];
    }
  }

  return 0;
}

function nextPatchVersion(...versions) {
  const knownVersions = versions.filter(Boolean);
  if (knownVersions.length === 0) {
    throw new Error('At least one version is required');
  }

  const highestVersion = knownVersions.sort(compareSemver).at(-1);
  const [major, minor, patch] = parseSemver(highestVersion);
  return `${major}.${minor}.${patch + 1}`;
}

function readCargoTomlVersion() {
  const cargoToml = readText(cargoTomlPath);
  const match = cargoToml.match(/^\[package\][\s\S]*?^version = "([^"]+)"$/m);
  if (!match) {
    throw new Error('Could not locate package.version in Cargo.toml');
  }

  return match[1];
}

function readCargoLockVersion() {
  const cargoLock = readText(cargoLockPath);
  const match = cargoLock.match(/^\[\[package\]\]\nname = "sauron"\nversion = "([^"]+)"/m);
  if (!match) {
    throw new Error('Could not locate root package version in Cargo.lock');
  }

  return match[1];
}

function readPackageJsonVersion() {
  const packageJson = JSON.parse(readText(packageJsonPath));
  if (typeof packageJson.version !== 'string') {
    throw new Error('Could not locate package.json version');
  }

  return packageJson.version;
}

function readCurrentVersion() {
  const cargoTomlVersion = readCargoTomlVersion();
  const cargoLockVersion = readCargoLockVersion();
  const packageJsonVersion = readPackageJsonVersion();
  const versions = new Set([cargoTomlVersion, cargoLockVersion, packageJsonVersion]);

  if (versions.size !== 1) {
    throw new Error(
      `Version mismatch detected: Cargo.toml=${cargoTomlVersion}, Cargo.lock=${cargoLockVersion}, package.json=${packageJsonVersion}`
    );
  }

  return cargoTomlVersion;
}

function setCargoTomlVersion(nextVersion) {
  const cargoToml = readText(cargoTomlPath);
  const nextCargoToml = cargoToml.replace(
    /^(\[package\][\s\S]*?^version = ")([^"]+)(")$/m,
    `$1${nextVersion}$3`
  );

  if (cargoToml === nextCargoToml) {
    throw new Error('Cargo.toml version replacement did not change any content');
  }

  writeText(cargoTomlPath, nextCargoToml);
}

function setCargoLockVersion(nextVersion) {
  const cargoLock = readText(cargoLockPath);
  const nextCargoLock = cargoLock.replace(
    /^(\[\[package\]\]\nname = "sauron"\nversion = ")([^"]+)(")$/m,
    `$1${nextVersion}$3`
  );

  if (cargoLock === nextCargoLock) {
    throw new Error('Cargo.lock version replacement did not change any content');
  }

  writeText(cargoLockPath, nextCargoLock);
}

function setPackageJsonVersion(nextVersion) {
  const packageJson = JSON.parse(readText(packageJsonPath));
  packageJson.version = nextVersion;
  writeText(packageJsonPath, `${JSON.stringify(packageJson, null, 2)}\n`);
}

function setVersion(nextVersion) {
  parseSemver(nextVersion);
  const currentVersion = readCurrentVersion();

  if (currentVersion === nextVersion) {
    return currentVersion;
  }

  setCargoTomlVersion(nextVersion);
  setCargoLockVersion(nextVersion);
  setPackageJsonVersion(nextVersion);
  return nextVersion;
}

const [command, ...args] = process.argv.slice(2);

switch (command) {
  case 'current':
    console.log(readCurrentVersion());
    break;
  case 'next-patch':
    console.log(nextPatchVersion(...args));
    break;
  case 'set':
    if (args.length !== 1) {
      throw new Error('Usage: node scripts/release-version.mjs set <version>');
    }

    console.log(setVersion(args[0]));
    break;
  default:
    throw new Error(
      'Usage: node scripts/release-version.mjs <current|next-patch|set> [version...]'
    );
}
