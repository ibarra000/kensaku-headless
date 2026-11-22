# Contributing

Thanks for helping. Bug reports, docs fixes, and focused PRs are all welcome.

## Before you start

- Open an issue for anything larger than a small fix, so we can agree on scope.
- Read [`ARCHITECTURE.md`](ARCHITECTURE.md) for the index/search shape and the
  daemon-owns-the-index model, and [`AGENTS.md`](AGENTS.md) for the read-only agent contract.
- Keep the local-first, read-only-by-default thesis intact.
- Security issues: do **not** open a public issue. See [`../SECURITY.md`](../SECURITY.md).

## Setup

```bash
git clone https://github.com/ibarra000/kensaku-headless
cd kensaku
rustup toolchain install 1.98
cargo +1.98 build -p kensaku
make smoke       # end-to-end check against a throwaway data directory
```

System prerequisites: [`INSTALL.md`](INSTALL.md#prerequisites).

## The checks CI runs

CI runs these on Linux, macOS, and Windows with Rust 1.98:

```bash
cargo test --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build -p kensaku --locked
tools/headless-smoke.sh target/debug/kensaku
```

Run them locally with `make test`, `make lint`, and `make smoke`. Format with `make fmt`
before you send a PR. All four checks must pass. The workspace has about 600 test
functions. Do not add `#[allow]` to silence a lint without a comment explaining why.

## Tests

The suite is hermetic: temp directories, no network, no user data. When you touch:

| Area | Add |
|---|---|
| query parser / AST | a test asserting on the AST, not strings |
| config | a migration/save test using the disk-free helpers |
| extraction | a malformed-input case that must not panic or hang |
| ranking | a fixture where the expected file must land in the top 3 |
| API methods | a dispatch test with a temp index. Every `METHODS` entry needs a handler. |
| MCP / CLI | a request-handler test with a temp index |
| SQLite | a prefix/duplicate/grouping test with `open_at(temp)` |

`tools/headless-smoke.sh` covers the command-line surface end to end. Extend it when you
add a command or change exit codes.

## Style

- Rust 2024, `rustfmt` for new files. The repo is not globally reformatted; do not
  reformat unrelated files in a PR.
- Errors are explicit. Library code returns typed errors; the CLI maps `ApiError` to the
  exit codes in `crates/kensaku-core/src/api/error.rs`.
- Commits: imperative subject, a body that explains *why*. Keep each commit
  focused; a PR may contain several.
- Do not commit build output (`target/`, `dist/`).

## Adding an API method

1. Add the entry to `METHODS` in `crates/kensaku-core/src/api/methods.rs` with the correct
   access class (`Read`, `FileRead`, or `Write`). The class decides whether HTTP allows it.
2. Handle it in `crates/kensaku-core/src/api/dispatch.rs`.
3. Add a CLI command in `crates/kensaku/src/args.rs` if people will call it by hand. `kensaku
   call` already reaches every method.
4. Update [`API.md`](API.md) so the method list matches `METHODS`, and add a line to the
   `CHANGELOG.md` entry.

## Performance changes

If you touch the query path or the indexer, include before/after numbers from
`cargo run --release -p kensaku-core --example bench -- <dir>` (or `make bench BENCH_DIR=<dir>`;
methodology in [`BENCHMARKS.md`](BENCHMARKS.md)). Regressions above ~20% need
justification.

## What we are not accepting (for now)

- An in-app LLM or chat interface — Kensaku is the search backend for agents.
- Cloud sync, accounts, telemetry.
- Writing through the agent API.
- Bundling large language-model runtimes by default (ONNX Runtime for OCR is
  already bundled with the release archives; a multi-gigabyte chat model is a different product).
- A GUI or desktop shell. The project is headless.

Keep changes scoped to the shipped surface; larger product directions should
start as an issue.
