#!/usr/bin/env bash
# Fetch the ONNX Runtime library from Microsoft's NuGet package (its GitHub releases are gone).
# Usage: fetch-onnxruntime.sh [rid] [dest-dir] [cache-dir]; prints the library path.
# rid: linux-x64, linux-arm64, osx-arm64, win-x64, win-arm64 (default: this machine).
set -euo pipefail

ORT_VERSION="1.31.0"
ORT_NUPKG_SHA256="7e35fdb4f5adb62f01e60798e2e5ed05108f5fbbe1cca03fbcc37903cf7c5e3b"

detect_rid() {
  local os arch
  case "$(uname -s)" in
    Linux*) os=linux ;;
    Darwin*) os=osx ;;
    MINGW*|MSYS*|CYGWIN*) os=win ;;
    *) echo "unsupported OS: $(uname -s)" >&2; exit 1 ;;
  esac
  case "$(uname -m)" in
    x86_64|amd64) arch=x64 ;;
    arm64|aarch64) arch=arm64 ;;
    *) echo "unsupported architecture: $(uname -m)" >&2; exit 1 ;;
  esac
  [ "$os-$arch" = osx-x64 ] && { echo "no ONNX Runtime build for Intel macOS; OCR uses Tesseract" >&2; exit 1; }
  echo "$os-$arch"
}

rid="${1:-$(detect_rid)}"
dest="${2:-target/release}"
cache="${3:-target/ort-nuget}"

pkg="$cache/onnxruntime.$ORT_VERSION.nupkg"
if [ ! -f "$pkg" ]; then
  mkdir -p "$cache"
  curl -fsSL --retry 3 -o "$pkg.part" \
    "https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime/$ORT_VERSION/microsoft.ml.onnxruntime.$ORT_VERSION.nupkg"
  mv "$pkg.part" "$pkg"
fi
if command -v sha256sum >/dev/null; then sum=$(sha256sum < "$pkg"); else sum=$(shasum -a 256 < "$pkg"); fi
[ "${sum%% *}" = "$ORT_NUPKG_SHA256" ] || { echo "ONNX Runtime package checksum mismatch" >&2; rm -f "$pkg"; exit 1; }

case "$rid" in
  win-*) lib=onnxruntime.dll ;;
  osx-*) lib=libonnxruntime.dylib ;;
  *) lib=libonnxruntime.so ;;
esac
mkdir -p "$dest"
unzip -o -j -q "$pkg" "runtimes/$rid/native/$lib" -d "$dest"
echo "$dest/$lib"
