# Signing

Kensaku's Windows binaries are Authenticode-signed through
[SignPath Foundation](https://signpath.org/), which issues an open-source code
signing certificate and keeps the private key on its HSM. Nothing is signed
on a maintainer machine.

| Platform | Artifact | Signed |
|---|---|---|
| Windows x86_64 | `kensaku.exe` inside `kensaku-<ver>-x86_64-pc-windows-msvc.zip` | Authenticode (SHA-256, timestamped) |
| Windows ARM64 | `kensaku.exe` inside `kensaku-<ver>-aarch64-pc-windows-msvc.zip` | Authenticode (SHA-256, timestamped) |
| macOS | the `kensaku` binary in the `.tar.gz` archives | **No** — needs an Apple Developer ID certificate and notarisation |
| Linux | the `kensaku` binary in the `.tar.gz` archive | **No** — verify with `SHA256SUMS` |

Each Windows archive holds one signed binary. The installers, the portable zip, and the
separate CLI binaries from 1.x are gone. macOS Gatekeeper instructions are in
[INSTALL.md](INSTALL.md). `SHA256SUMS` is still published for every release and
remains the integrity check for all platforms.

## One-time setup (SignPath + GitHub)

Do this once. The workflow fails fast on Windows if anything is missing, so a
tag build can never quietly ship an unsigned binary.

1. Sign in to SignPath and open the Kensaku project created by the Foundation
   acceptance.
2. Connect the GitHub repository (Trusted Build System *GitHub.com*), and
   install the [SignPath GitHub App](https://github.com/apps/signpath) on
   `yjid/kensaku`. The Foundation plan requires every job up to the signing
   request to run on GitHub-hosted runners, which this workflow already does.
3. Create an **Artifact Configuration** from
   [`docs/signing/signpath-artifact-config.xml`](signing/signpath-artifact-config.xml)
   and give it the slug `kensaku-binaries`.
4. Confirm the **Signing Policy** slugs: `test-signing` for the dry run and
   `release-signing` for tagged releases.
5. Create an **API token** for a user with submitter permission on that policy.
6. Add these to the repository:

   | Kind | Name | Value |
   |---|---|---|
   | Variable | `SIGNPATH_ORGANIZATION_ID` | SignPath organization ID |
   | Variable | `SIGNPATH_PROJECT_SLUG` | Project slug, e.g. `kensaku` |
   | Variable | `SIGNPATH_SIGNING_POLICY_SLUG` | `test-signing` (dry run) or `release-signing` |
   | Variable | `SIGNPATH_ARTIFACT_CONFIGURATION_SLUG` | `kensaku-binaries` |
   | Secret | `SIGNPATH_API_TOKEN` | API token from step 5 |

   ```bash
   gh variable set SIGNPATH_ORGANIZATION_ID --body "<org-id>"
   gh variable set SIGNPATH_PROJECT_SLUG --body "kensaku"
   gh variable set SIGNPATH_SIGNING_POLICY_SLUG --body "test-signing"
   gh variable set SIGNPATH_ARTIFACT_CONFIGURATION_SLUG --body "kensaku-binaries"
   gh secret set SIGNPATH_API_TOKEN
   ```

Copy `SIGNPATH_ORGANIZATION_ID` from the SignPath URL (`/Web/<organization-id>/…`)
instead of retyping it. A single transposed character produces an error that
looks like an authentication failure:

```
Unexpected response from the SignPath connector: ""
```

That message means the connector answered `401` with an empty body, which
SignPath returns for an unknown organization *and* for a valid token without
membership in the organization, so it cannot tell the two apart. Check the
organization ID first, then the token.

The first signing request may wait for manual approval in SignPath. The
workflow waits up to 60 minutes (`wait-for-completion-timeout-in-seconds`).

## Rollout order

1. **Prove the pipeline with `test-signing`.** Set
   `SIGNPATH_SIGNING_POLICY_SLUG` to `test-signing`, then run the workflow with
   *Run workflow* (`workflow_dispatch`). The binaries come back signed with the
   test certificate: the whole build → upload → sign → download → verify path
   is exercised, but Windows still warns because that certificate is not
   trusted by the operating system.
2. **Switch to `release-signing`** once SignPath has issued the release
   certificate. Set the variable to `release-signing` and push the version tag;
   the draft release then contains publicly trusted signatures.

A signing policy can show **INVALID** while its certificate is **CSR PENDING**.
That means SignPath has created the key pair in its HSM and a certificate
signing request, but the certificate has not been issued yet. For Foundation
projects SignPath issues it; there is nothing to buy or upload. `test-signing`
is usable in the meantime. If the release certificate is still pending after a
few days, contact [SignPath support](https://signpath.io/support) with the
Organization ID shown on the organization page.

## How the workflow signs

The Windows jobs in `release.yml` run these steps for each target:

1. `cargo build --release --locked -p kensaku --target <triple>` compiles the binary.
2. The unsigned `kensaku.exe` is uploaded as a short-lived workflow artifact.
3. The SignPath GitHub action submits that artifact to the signing policy and waits for
   the signed file.
4. The signed `kensaku.exe` is checked with `Get-AuthenticodeSignature`. With
   `test-signing` the self-signed certificate reports `UnknownError` (untrusted root) even
   though the signature is intact, so `Valid` and `UnknownError` are accepted there. With
   `release-signing` only `Valid` is accepted: a certificate that does not chain to a public
   CA, or a signature that was invalidated after signing, fails the job instead of shipping.
5. The signed binary replaces the staged one, and the archive is built from it.

`SHA256SUMS` in the draft release is computed after signing, so the published hashes are
hashes of the signed files.

## Verifying a download

Unpack the Windows archive, then (PowerShell):

```powershell
Get-AuthenticodeSignature .\kensaku.exe |
  Format-List Status, SignerCertificate, TimeStamperCertificate
```

`Status` should be `Valid` and the signer should be SignPath Foundation
signing on behalf of the Kensaku project.

Any platform:

```bash
sha256sum -c SHA256SUMS
```

## Not covered yet

`SHA256SUMS` itself is not signed. Pinning a signature over the checksum file
(CWE-347 in `GHSA-vmrq-73jw-g4q3`) needs a maintainer-held key and a CI signing
step; SignPath Foundation does not provide detached signatures over arbitrary
files. Check the checksum file against the release page over TLS, and check the Windows
binary with its Authenticode signature.
