# Releasing

The workflow builds, the maintainer publishes. This keeps release creation out of
GitHub Actions (no release-token permissions to fight) and makes the published
assets explicit.

## 1. Prepare

```bash
git checkout master && git pull
# bump [workspace.package] version in the root Cargo.toml (both crates inherit it),
# then let Cargo update Cargo.lock:
cargo +1.98 check
# update CHANGELOG.md
cargo +1.98 test --all-targets --locked
cargo +1.98 clippy --all-targets --locked -- -D warnings
make smoke
git add -A && git commit -m "chore(release): vX.Y.Z"
git push origin master
```

## 2. Build on CI

```bash
git tag -a vX.Y.Z -m "Kensaku vX.Y.Z"
git push origin vX.Y.Z
gh run watch   # or: gh run list --workflow=release.yml
```

Pushing a `v*` tag runs `.github/workflows/release.yml`. It builds `kensaku` for five
targets and packs each one (Windows signing runs only once SignPath is configured).
Push one tag at a time; never `git push --tags`. The upstream `v1.0.x` tags are not
kept in this repo, and every pushed `v*` tag starts a release build. The workflow names archives from
the tag without the `v`, so use the same version in the tag and in `Cargo.toml`.

| Target | Archive |
|---|---|
| `x86_64-unknown-linux-gnu` | `kensaku-<ver>-x86_64-unknown-linux-gnu.tar.gz` |
| `aarch64-apple-darwin` | `kensaku-<ver>-aarch64-apple-darwin.tar.gz` |
| `x86_64-apple-darwin` | `kensaku-<ver>-x86_64-apple-darwin.tar.gz` (no bundled ONNX Runtime) |
| `x86_64-pc-windows-msvc` | `kensaku-<ver>-x86_64-pc-windows-msvc.zip` |
| `aarch64-pc-windows-msvc` | `kensaku-<ver>-aarch64-pc-windows-msvc.zip` |

Each archive holds the `kensaku` binary, the ONNX Runtime library (not on Intel macOS),
and the LICENSE and README. The workflow smoke-tests the packaged binary with `--version`
and `methods`.

Windows signing is skipped until the SignPath variables are set, so releases are
currently unsigned. See [SIGNING.md](SIGNING.md).

## 3. Review and publish the draft

The tag push builds every target and assembles a **draft** GitHub release with all
archives and `SHA256SUMS`. Re-run failed jobs from the Actions page if needed. The draft step
runs only for tag pushes.

```bash
gh run list --workflow=release.yml --limit 1
gh release view vX.Y.Z

# check the published hashes
mkdir -p release-check && gh release download vX.Y.Z -D release-check
(cd release-check && sha256sum -c SHA256SUMS)
```

Check that the expected assets are present:

| Asset | Target |
|---|---|
| `kensaku-<ver>-x86_64-unknown-linux-gnu.tar.gz` | Linux x86_64 |
| `kensaku-<ver>-aarch64-apple-darwin.tar.gz` | macOS Apple silicon |
| `kensaku-<ver>-x86_64-apple-darwin.tar.gz` | macOS Intel |
| `kensaku-<ver>-x86_64-pc-windows-msvc.zip` | Windows x86_64 |
| `kensaku-<ver>-aarch64-pc-windows-msvc.zip` | Windows ARM64 |
| `SHA256SUMS` | All assets |

Then publish the draft (or edit the notes first):

```bash
gh release edit vX.Y.Z --draft=false --latest
```

## 4. Verify

- The release page lists all five archives and `SHA256SUMS`.
- Download one archive, check its checksum, unpack it, and run `kensaku --version`.
- Run `kensaku setup --root <dir> --wait` against a throwaway `--data-dir` and check
  `kensaku search` against it.

## Signing and notarization

- Windows: not signed yet. SignPath signing is wired into `release.yml` and runs once
  the variables in [SIGNING.md](SIGNING.md) are set.
- macOS: archives are not signed or notarised. Notarisation needs an Apple Developer ID,
  which the release workflow does not use yet.
