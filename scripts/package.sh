#!/usr/bin/env bash
# Build one target's static library and pack it as a release artifact.
# usage: scripts/package.sh <rust-target> [artifact-name]
set -euo pipefail

target="${1:?usage: scripts/package.sh <rust-target> [artifact-name]}"
artifact="${2:-$target}"

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

log="$(mktemp)"
trap 'rm -f "$log"' EXIT
if ! cargo build --locked --release --target "$target" --lib >"$log" 2>&1; then
  cat "$log"
  # 把错误尾行变成 GitHub annotation，公开仓库也能在 API 里看到失败原因。
  msg="$(tail -n 80 "$log" | sed 's/%/%25/g; s/\r$//' | awk 'BEGIN { ORS = "%0A" } { print }')"
  echo "::error title=cargo build ($target)::$msg"
  exit 1
fi

lib="target/$target/release/liblegado_engine.a"
if [[ ! -f "$lib" ]]; then
  lib="target/$target/release/legado_engine.lib"
fi
if [[ ! -f "$lib" ]]; then
  echo "no static library for $target" >&2
  exit 1
fi

out="dist/$artifact"
rm -rf "$out" "dist/legado-engine-$artifact.tar.gz"
mkdir -p "$out"
cp "$lib" "$out/"
cp legado.h LICENSE UPSTREAM.md README.md "$out/"
tar -czf "dist/legado-engine-$artifact.tar.gz" -C dist "$artifact"
echo "dist/legado-engine-$artifact.tar.gz"
