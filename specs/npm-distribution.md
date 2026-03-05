# Spec: GitHub-Driven Binary Releases and npm Distribution

## Decision

- `main` pushes cut the next patch release automatically.
- Release versions are synchronized across `Cargo.toml`, `Cargo.lock`, and `package.json`.
- GitHub Actions builds the supported target matrix on GitHub-hosted runners.
- GitHub Releases publish tarballs for each supported target.
- npm publishes a single scoped package, `@nothumanwork/sauron`, with a Node launcher that selects the correct bundled binary for the host.

## Supported Targets

- `x86_64-unknown-linux-gnu`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`

## Release Flow

1. A push lands on `main`.
2. The release workflow reads the current project version and the newest remote `v*` tag, then increments the higher of the two.
3. Each matrix job rewrites the workspace to that release version and builds `target/<triple>/release/sauron`.
4. The publish job stages the built binaries into `npm/bin/<triple>/sauron`, validates the npm tarball, publishes the npm package, commits the synchronized version bump back to `main`, tags `v<version>`, and creates the GitHub release.

## npm Package Layout

- `bin/sauron.js`: host-router launcher exposed as the `sauron` executable.
- `distribution/targets.json`: release target metadata shared by the launcher and workflow.
- `npm/bin/<triple>/sauron`: staged binary payloads that are included only in the published tarball.

## Verification Checklist

- `cargo test --locked`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo build --release --locked`
- `cargo build --release --target aarch64-apple-darwin`
- `cargo build --release --target x86_64-apple-darwin`
- `node scripts/release-version.mjs current`
- `node scripts/stage-npm-binaries.mjs <artifact-dir>`
- `npm pack --dry-run`
