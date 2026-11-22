# Privacy

Kensaku is local-first. Everything below describes the shipped behavior.

## What stays on your machine

- The index (metadata, full-text, embeddings) lives in your data directory, by default
  in your OS config directory.
- No account, no sign-in, no cloud search.
- No telemetry and no analytics: search queries, file names, and usage data are
  never sent anywhere.
- The binary does not check for updates. Nothing contacts GitHub or any other service
  unless you download a pack or a release yourself.
- Model and OCR downloads fetch files from their configured source (Hugging Face, GitHub,
  or a mirror). The OCR pack for the engine you selected downloads on its own when OCR is
  on and the pack is missing. The static embedding table, the smart model, and the visual
  pack download only when you run `kensaku packs download`. Packs are verified against
  pinned SHA-256 checksums.

## What leaves the machine

- Nothing about your files or searches during normal search.
- Pack downloads (see above), which go to the model host, not to Kensaku servers (there
  are none).

## MCP server and HTTP API

- The MCP server (`kensaku mcp`) is read-only by default. It can read the local index and
  file metadata/contents that Kensaku has already indexed.
- It cannot delete or modify files. The only write-capable tool (`reindex`) is
  disabled unless `mcp_allow_reindex = true` in config, and it needs the daemon.
- `kensaku serve` listens on `127.0.0.1` only. Every request needs the bearer token in
  `<data dir>/http.token`. Write methods on `/v1` need `--allow-writes`.
- `agent_scoped_roots` can restrict MCP, CLI, and HTTP results to specific roots.
- The MCP server does not expose arbitrary filesystem access beyond the index.
