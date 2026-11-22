# Security

## Reporting

Please **do not** open a public issue for a vulnerability. Use GitHub's private
vulnerability reporting (Security → Advisories → Report a vulnerability) or email
the maintainer listed on the repository profile. You will get an acknowledgement
within a few days.

## Threat model

Kensaku is a local command-line tool, a background daemon, and a read-only search
backend. It is designed to be safe when pointed at untrusted files.

**In scope**

- Parsing untrusted documents and images (PDF, DOCX/PPTX/XLSX, EPUB, RTF, images)
  without code execution, unbounded memory, or hangs.
- Path traversal or symlink tricks escaping watched roots.
- Command injection through file paths or file contents.
- Unauthorized access to the daemon endpoint or the HTTP API, or bypass of the token.
- The agent surface leaking data or permitting writes.
- Secrets ending up in the index or in logs.

**Out of scope**

- An attacker who can already read/write your user account or the index directory.
- Malicious OS-level components (kernel, filesystem).
- Denial of service by indexing an enormous corpus on purpose.

## Controls

- **Extraction isolation.** Parsers run on a bounded worker pool with a per-file
  timeout and panic isolation. XML external entities/DTDs are not resolved.
- **Decode caps.** Images are limited to 50 MP; text extraction is capped at
  100 MB per file and 4 MB of indexed text, with 48 semantic units per document.
  Dense images and scanned PDFs get up to 45 s; other extraction timeouts scale
  with file size.
- **No shell.** Files are opened through OS APIs; the agent surface has no
  shell or network tools. External extractors run without a shell, with a cleared
  environment and a hard timeout. The only write action is `reindex`, and it is opt-in.
- **Read-only agent access.** `kensaku mcp` and `kensaku serve` forward to the daemon, which
  is the only writer. A read-only session without a daemon opens the index without the
  writer lock and cannot mutate it.
- **Daemon endpoint.** The daemon listens on a per-data-directory Unix socket (mode 0600) or
  a named pipe. Each request carries a 32-byte random token from `<data dir>/daemon.token`
  and the protocol version; a mismatch is refused.
- **Default exclusions** skip hidden files and common junk directories.
- **Local only.** No telemetry and no data about your files or searches leaves
  the machine. The only outbound requests are pack downloads (model, OCR, and Tesseract
  data). The OCR pack for the selected engine downloads automatically when OCR is on;
  the other packs download only when you ask for them.

## Sensitive data

The index contains filenames and extracted text from your watched folders. Treat
the data directory (`~/.config/kensaku/` on Linux, unless overridden) as sensitive. It is
created with user-only permissions where the platform supports it, and should not be
synced to a shared location.

Default-deny covers hidden entries, `.ssh`, `.gnupg`, `.aws`, `.kube`, `.docker`,
`.password-store`, common password-manager folders, `.env*`, and credential
extensions (`.pem`, `.key`, `.pfx`, `.p12`, `.crt`, `.cer`). Additional paths
(browser profiles outside hidden app-data directories, custom secret stores)
should still be added to exclusions with `kensaku exclude add`.

## Agent surface

- **stdio by default.** No network listener unless you explicitly run `kensaku serve`,
  which binds loopback only, requires the bearer token in `<data dir>/http.token`, and
  checks the peer on every connection.
- **Read-only.** No shell and no write tools. `reindex` is listed only when
  `mcp_allow_reindex=true` and the daemon is running; the HTTP transport never
  enables it. Write methods on `/v1` need `kensaku serve --allow-writes`.
- **Scoped reads.** `agent_scoped_roots` restricts results and single-path reads.
  Paths are resolved before comparison, so `..` and symlink escapes are rejected.
  Credential files and excluded extensions are refused even inside a scope.
- Logs go to stderr, never stdout, so protocol streams stay clean.
- Tool responses are token-budgeted; file content is data, never instructions.
- No `.gitignore` bypass by default — agents see what the index contains.

## Release integrity

Release archives are published with `SHA256SUMS`. Binaries are not code-signed yet.
Verify downloads against `SHA256SUMS` on the GitHub release page. The binary has no self-updater and
does not download replacements for itself. See `docs/INSTALL.md` and `docs/SIGNING.md`.
