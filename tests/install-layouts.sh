#!/usr/bin/env bash
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/mock"
cat > "$tmp/mock/curl" <<'SH'
#!/usr/bin/env bash
while [[ $# -gt 0 ]]; do
  if [[ "$1" == "-o" ]]; then cp "$FIXTURE" "$2"; exit; fi
  shift
done
exit 1
SH
chmod +x "$tmp/mock/curl"
case "$(uname -m)" in arm64|aarch64) arch=aarch64;; *) arch=x86_64;; esac
case "$(uname -s)" in Darwin) target="$arch-apple-darwin";; *) target="$arch-unknown-linux-gnu";; esac
for layout in flat nested empty; do
  dir="$tmp/$layout"
  mkdir -p "$dir/archive" "$dir/prefix"
  bins="$dir/archive"
  if [[ $layout == nested ]]; then bins="$bins/trx-v0.9.0-$target/bin"; mkdir -p "$bins"; fi
  if [[ $layout != empty ]]; then
    for component in trx trx-tui trx-api trx-mcp; do
      printf '#!/bin/sh\necho "trx 0.9.0"\n' > "$bins/$component"
      chmod +x "$bins/$component"
    done
  fi
  tar -czf "$dir/fixture.tar.gz" -C "$dir/archive" .
  if PATH="$tmp/mock:$PATH" FIXTURE="$dir/fixture.tar.gz" bash "$repo/install.sh" --version 0.9.0 --prefix "$dir/prefix" > "$dir/log" 2>&1; then
    [[ $layout != empty ]]
    for component in trx trx-tui trx-api trx-mcp; do [[ -x "$dir/prefix/$component" ]]; done
  else
    [[ $layout == empty ]]
    grep -q 'No binaries found' "$dir/log"
  fi
done
echo 'Installer flat/nested/empty archive tests passed'
