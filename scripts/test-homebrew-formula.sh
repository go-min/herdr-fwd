#!/bin/sh
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
temporary=$(mktemp -d "${TMPDIR:-/tmp}/herdr-fwd-homebrew-test.XXXXXX")
trap 'rm -rf "$temporary"' EXIT HUP INT TERM

checksums="$temporary/SHA256SUMS"
formula="$temporary/herdr-fwd.rb"
cat > "$checksums" <<'EOF'
1111111111111111111111111111111111111111111111111111111111111111  herdr-fwd-linux-x86_64.tar.gz
2222222222222222222222222222222222222222222222222222222222222222  herdr-fwd-linux-aarch64.tar.gz
3333333333333333333333333333333333333333333333333333333333333333  herdr-fwd-macos-x86_64.tar.gz
4444444444444444444444444444444444444444444444444444444444444444  herdr-fwd-macos-aarch64.tar.gz
EOF

sh "$root/scripts/render-homebrew-formula.sh" v1.2.3 "$checksums" "$formula"
ruby -c "$formula" >/dev/null
grep -F 'class HerdrFwd < Formula' "$formula" >/dev/null
if grep -F 'version "1.2.3"' "$formula" >/dev/null; then
  printf 'formula test: renderer emitted redundant version metadata\n' >&2
  exit 1
fi
grep -F 'herdr-fwd-linux-x86_64.tar.gz' "$formula" >/dev/null
grep -F 'sha256 "1111111111111111111111111111111111111111111111111111111111111111"' "$formula" >/dev/null
grep -F 'herdr-fwd-linux-aarch64.tar.gz' "$formula" >/dev/null
grep -F 'sha256 "2222222222222222222222222222222222222222222222222222222222222222"' "$formula" >/dev/null
grep -F 'herdr-fwd-macos-x86_64.tar.gz' "$formula" >/dev/null
grep -F 'sha256 "3333333333333333333333333333333333333333333333333333333333333333"' "$formula" >/dev/null
grep -F 'herdr-fwd-macos-aarch64.tar.gz' "$formula" >/dev/null
grep -F 'sha256 "4444444444444444444444444444444444444444444444444444444444444444"' "$formula" >/dev/null
grep -F 'bin.install "hfwd"' "$formula" >/dev/null
grep -F 'shell_output("#{bin}/hfwd --version")' "$formula" >/dev/null

HERDR_FWD_RELEASE_BASE=https://artifacts.example.test/v1.2.3 \
  sh "$root/scripts/render-homebrew-formula.sh" v1.2.3 "$checksums" "$formula"
grep -F 'homepage "https://github.com/go-min/herdr-fwd"' "$formula" >/dev/null
grep -F 'url "https://artifacts.example.test/v1.2.3/herdr-fwd-macos-aarch64.tar.gz"' \
  "$formula" >/dev/null

if sh "$root/scripts/render-homebrew-formula.sh" \
  'v1.2.3/../../bad' "$checksums" "$formula" \
  >"$temporary/tag-stdout" 2>"$temporary/tag-stderr"; then
  printf 'formula test: renderer accepted an unsafe release tag\n' >&2
  exit 1
fi
grep -F 'invalid release tag: v1.2.3/../../bad' "$temporary/tag-stderr" >/dev/null

sed '/herdr-fwd-linux-aarch64.tar.gz/d' "$checksums" > "$temporary/incomplete"
if sh "$root/scripts/render-homebrew-formula.sh" \
  v1.2.3 "$temporary/incomplete" "$formula" >"$temporary/stdout" 2>"$temporary/stderr"; then
  printf 'formula test: renderer accepted incomplete checksums\n' >&2
  exit 1
fi
grep -F 'missing checksum for herdr-fwd-linux-aarch64.tar.gz' "$temporary/stderr" >/dev/null

printf 'Homebrew formula renderer tests passed.\n'
