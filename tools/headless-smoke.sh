#!/usr/bin/env bash
# Headless end-to-end smoke test: `tools/headless-smoke.sh [path/to/kensaku]`; throwaway data dir, daemon always stopped.
set -euo pipefail

bin="${1:-target/debug/kensaku}"
[ -x "$bin" ] || [ -x "$bin.exe" ] || { echo "kensaku binary not found: $bin" >&2; exit 1; }

work="$(mktemp -d)"
data="$work/data"
corpus="$work/corpus"
mkdir -p "$corpus"
printf 'the quick brown fox jumps over the lazy dog\n' > "$corpus/notes.txt"
printf '# Invoice 4471\nTotal due: 120 EUR\n' > "$corpus/invoice.md"

k() { "$bin" --data-dir "$data" "$@"; }
cleanup() { k daemon stop >/dev/null 2>&1 || true; rm -rf "$work"; }
trap cleanup EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }
expect_exit() {
  local want="$1"; shift
  set +e; "$@" >/dev/null 2>&1; local got=$?; set -e
  [ "$got" -eq "$want" ] || fail "'$*' exited $got, expected $want"
}

echo "== search before setup exits 3"
expect_exit 3 k search fox

echo "== setup + first scan"
k setup --root "$corpus" --wait

echo "== search finds content and names"
k search "lazy dog" | grep -q notes.txt || fail "content search missed notes.txt"
k search invoice | grep -q invoice.md || fail "name search missed invoice.md"
k search fox --json | grep -q '"results"' || fail "--json search has no results key"

echo "== status, health, roots, config"
k status >/dev/null
k health --json | grep -q '"ok": true' || fail "health not ok"
k roots list | grep -q corpus || fail "roots list missing corpus"
k config set max_results 25
[ "$(k config get max_results)" = "25" ] || fail "config set/get round trip"
expect_exit 2 k config set no_such_key 1

echo "== pause / resume"
k index pause
k index resume

echo "== raw API and method list"
k call ping | grep -q '"ok": true' || fail "call ping"
k methods | grep -q '^search' || fail "methods list"
expect_exit 2 k call no_such_method

echo "== MCP stdio"
# Capture first: `grep -q` closes the pipe early, which pipefail turns into a false failure.
mcp_out="$(printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
                         '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' | k mcp 2>/dev/null)"
grep -q '"tools"' <<<"$mcp_out" || fail "mcp tools/list"

echo "== read-only mode without the daemon"
k daemon stop
k --no-daemon search fox | grep -q notes.txt || fail "--no-daemon search"

echo "== reset"
expect_exit 2 k reset
k reset --yes
expect_exit 3 k search fox

echo "OK"
