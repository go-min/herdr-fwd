#!/bin/sh
set -eu

tag=${1:-}
checksums=${2:-}
output=${3:-}
repository=${HERDR_FWD_REPOSITORY_URL:-https://github.com/go-min/herdr-fwd}

die() { printf 'formula: %s\n' "$*" >&2; exit 1; }

[ "$#" -eq 3 ] || \
  die "usage: scripts/render-homebrew-formula.sh <tag> <SHA256SUMS> <output>"
version=${tag#v}
case "$tag:$version" in
  v*:*[!0-9A-Za-z.+-]*|v:*) die "invalid release tag: $tag" ;;
  v*:[0-9]*.[0-9]*.[0-9]*) ;;
  *) die "invalid release tag: $tag" ;;
esac
[ -f "$checksums" ] || die "checksums file not found: $checksums"

checksum_for() {
  asset=$1
  checksum=$(awk -v asset="$asset" '$2 == asset || $2 == "*" asset { print $1; exit }' "$checksums")
  case "$checksum" in
    *[!0-9a-fA-F]*|'') die "missing checksum for $asset" ;;
  esac
  [ "${#checksum}" -eq 64 ] || die "invalid checksum for $asset"
  printf '%s\n' "$checksum"
}

linux_x86_64=$(checksum_for herdr-fwd-linux-x86_64.tar.gz)
linux_aarch64=$(checksum_for herdr-fwd-linux-aarch64.tar.gz)
macos_x86_64=$(checksum_for herdr-fwd-macos-x86_64.tar.gz)
macos_aarch64=$(checksum_for herdr-fwd-macos-aarch64.tar.gz)
base=${HERDR_FWD_RELEASE_BASE:-"$repository/releases/download/$tag"}

cat > "$output" <<EOF
class HerdrFwd < Formula
  desc "Automatic loopback port forwarding for remote Herdr sessions"
  homepage "$repository"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "$base/herdr-fwd-macos-aarch64.tar.gz"
      sha256 "$macos_aarch64"
    else
      url "$base/herdr-fwd-macos-x86_64.tar.gz"
      sha256 "$macos_x86_64"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "$base/herdr-fwd-linux-aarch64.tar.gz"
      sha256 "$linux_aarch64"
    else
      url "$base/herdr-fwd-linux-x86_64.tar.gz"
      sha256 "$linux_x86_64"
    end
  end

  def install
    bin.install "hfwd"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/hfwd --version")
  end
end
EOF
