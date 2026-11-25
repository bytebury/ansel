# Publishing

The client SDK is published to [crates.io](https://crates.io/crates/ansel) as
`ansel`.

## One-time setup

1. Create an API token at <https://crates.io/settings/tokens> with the
   `publish-update` scope (and `publish-new` for the very first release).
2. Add it to the GitHub repository as an Actions secret named
   `CARGO_REGISTRY_TOKEN`.

## Releasing a version

1. Update `version` in `Cargo.toml`, following semver. Changes to the HTTP API
   or the SDK's public types are breaking changes.
2. Run the checks locally:

   ```sh
   cargo fmt --check
   cargo clippy --all-targets --all-features -- -D warnings
   cargo test --all-features
   cargo publish --dry-run
   ```

3. Commit, then tag and push:

   ```sh
   git tag v0.1.0
   git push origin v0.1.0
   ```

The [`release` workflow](.github/workflows/release.yml) checks that the tag
matches `Cargo.toml`, runs the tests, and runs `cargo publish`.
