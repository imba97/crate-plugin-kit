#!/usr/bin/env bash
#
# Packages one release archive for crate-plugin-kit.
#
#     scripts/package-release.sh <target-triple> [out-dir]
#
# and it writes
#
#     dist/crate-plugin-kit-<target>.tar.gz
#
# The archive holds exactly two entries, both at the top level: the `plugin-asset` binary
# and `LICENSE`. The layout is an interface, not a preference -- `cargo binstall
# crate-plugin-kit` unpacks the archive and looks for the binary at its root, which is why
# `[package.metadata.binstall] bin-dir` in Cargo.toml has to spell that out; and MIT requires
# the licence notice to travel with a copy of the binary anyway.
#
# The name is an interface too, and a subtle one: `crate-plugin-kit-<target>` is the
# "versionless" filename binstall tries by default, and it carries the **crate** name rather
# than the binary's (`plugin-asset`) because that is what binstall's `{ name }` means. The
# binary name only has to match `{ bin }` inside the archive.
#
# `.tar.gz` is one of the extensions binstall accepts for its `tgz` format, so no `pkg-fmt`
# or `pkg-url` override is needed.
#
# This script is what CI runs, so a release can be reproduced locally with one command.

set -euo pipefail

if [ $# -lt 1 ] || [ $# -gt 2 ]; then
  echo "usage: $0 <target-triple> [out-dir]" >&2
  exit 2
fi

target="$1"
out_dir="${2:-dist}"
root="$(cd "$(dirname "$0")/.." && pwd)"

# `--locked`: a release is built from the committed lockfile, so the same tag yields the
# same dependency set no matter who builds it.
cargo build --release --locked --manifest-path "$root/Cargo.toml" --target "$target"

binary="$root/target/$target/release/plugin-asset"
if [ ! -x "$binary" ]; then
  echo "no executable at $binary" >&2
  exit 1
fi

mkdir -p "$out_dir"
archive="$out_dir/crate-plugin-kit-$target.tar.gz"

# Staged in a temporary directory so the archive carries no path components of its own:
# `tar -C <dir> plugin-asset LICENSE` stores them as `plugin-asset` and `LICENSE`.
staging="$(mktemp -d)"
trap 'rm -rf "$staging"' EXIT
cp "$binary" "$staging/plugin-asset"
cp "$root/LICENSE" "$staging/LICENSE"

tar -czf "$archive" -C "$staging" plugin-asset LICENSE

echo "$archive"
