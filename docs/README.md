# Kensaku documentation

Start with the [README](../README.md) for what Kensaku is and how to install it.
These pages go deeper.

## Using Kensaku

| Page | What it covers |
|---|---|
| [USAGE.md](USAGE.md) | The `kensaku` commands, global flags, output and exit codes, the query language, plain-language intent, indexing behavior. |
| [INSTALL.md](INSTALL.md) | Release archives, signature and checksum checks, building from source, OCR engines and packs, first run, start-at-login, reset and removal. |
| [CONFIG.md](CONFIG.md) | Every `config.json` key with its real default, data-directory overrides, migrations, and keys the headless build no longer reads. |
| [LANGUAGES.md](LANGUAGES.md) | CJK tokenization, code/data formats, document formats, OCR language support. |
| [FAQ.md](FAQ.md) | Privacy, memory, OCR, the daemon, agents, and common "why no results?" questions. |
| [PRIVACY.md](PRIVACY.md) | What stays local, what leaves the machine, and what the MCP server can read. |

## For agents and integrations

| Page | What it covers |
|---|---|
| [AGENTS.md](AGENTS.md) | MCP setup, tool schemas, pagination, scoped roots, the CLI, and the read-only contract. |
| [API.md](API.md) | The API method reference, the daemon socket, the `/v1` HTTP routes, and the Rust `Engine` and `Client`. |
| [EXTRACTORS.md](EXTRACTORS.md) | Opt-in external extractors for formats Kensaku does not decode natively. |

## For contributors

| Page | What it covers |
|---|---|
| [ARCHITECTURE.md](ARCHITECTURE.md) | Process model, daemon and transports, storage, index tiers, query pipeline, extraction, module map. |
| [BENCHMARKS.md](BENCHMARKS.md) | Harnesses, methodology, 100k Windows/Linux reference numbers, indexing/extraction measurements, caveats. |
| [CONTRIBUTING.md](CONTRIBUTING.md) | Setup, the checks CI runs, test expectations, style, performance changes. |
| [RELEASING.md](RELEASING.md) | Version bump, CI release build, draft release review, asset checklist. |
| [SIGNING.md](SIGNING.md) | Planned Windows signing through SignPath (not active yet). |

## Conventions in these docs

- **Shipped** means the code in the current `2.0.0-dev` tree does it.
- **Reference** numbers are measured on the stated machine and corpus. They are
  not guarantees; see [BENCHMARKS.md](BENCHMARKS.md) for the caveats.
- **Not shipped** is called out explicitly. Visual search, native OCR, and
  bundled LLM/chat are not in the production path.

The desktop application (GUI, Tauri shell, installers, self-updater) was removed
in 2.0.0. Its documentation is gone; the [CHANGELOG](../CHANGELOG.md) records the
change.
