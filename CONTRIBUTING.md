# Contributing

Issues and pull requests are welcome. For larger changes, open an issue first
to discuss the intended behaviour. Contributions are licensed under the project's
MIT licence.

Build on Windows 10/11 x64 with the MSVC C++ build tools and Windows SDK.
Rustup installs the pinned toolchain from `rust-toolchain.toml`.

Before opening a pull request, run:

```powershell
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release --target x86_64-pc-windows-msvc
```

Add tests for behavioural changes, especially file moves, collision handling,
cancellation, and cache use. Use disposable files for manual testing. Desktop
smoke tests are opt-in; see the README. Include reproduction steps and expected
behaviour in bug reports. Remove connection keys and personal paths from logs.

## Maintainer releases

1. Update `version` in `Cargo.toml`, run `cargo check` to update `Cargo.lock`,
   and commit both files with the release changes.
2. Push `main` and wait for CI to pass.
3. Create and push a matching version tag, for example:

   ```powershell
   git tag -a v0.1.0 -m "CleanDL for Windows v0.1.0"
   git push origin v0.1.0
   ```

The tag workflow repeats checks and builds the x64 portable ZIP. A separate job
verifies its SHA-256 checksum and publishes it on GitHub Releases. Only this job
has repository write permission; no personal access token is needed. Tags must
match the package version. Versions containing a hyphen become prereleases.
Published releases are left intact on reruns; interrupted draft releases can be
completed by rerunning the workflow. Publish corrections as a new version.

To check packaging locally after building the explicit Windows target:

```powershell
./scripts/package.ps1
```

Output is written to `dist/`. The executable is unsigned.
