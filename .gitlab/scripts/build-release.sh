#!/bin/sh
# Builds the release executable of cutwater for one target and packs it as
# dist/cutwater-<version>-<target>.tar.gz, <version> being Cargo.toml's,
# with cutwater, LICENSE, README.md and the third-party notices at its root,
# as Cargo.toml's [package.metadata.binstall] describes it. xtask writes the
# notices, THIRD-PARTY-NOTICES.txt and RUST-LIBRARY-COPYRIGHT.html, with
# cargo-about and the toolchain's rust-src component, and fails when a
# crate's license is not one about.toml accepts. The Linux targets are linked
# statically against musl with cargo-zigbuild and zig, on any Linux host; the
# macOS targets are built on macOS with the Xcode Command Line Tools, which
# Apple's SDK license requires, and signed ad hoc, as Apple silicon runs only
# signed code.
#
# usage: .gitlab/scripts/build-release.sh <target>
set -eu

target=${1:?usage: build-release.sh <target>}
case "$target" in
  x86_64-unknown-linux-musl | aarch64-unknown-linux-musl) ;;
  x86_64-apple-darwin | aarch64-apple-darwin)
    [ "$(uname -s)" = Darwin ] || { echo "$target is built on macOS" >&2; exit 1; }
    # The oldest macOS both executables run on, Rust's minimum for Apple silicon.
    MACOSX_DEPLOYMENT_TARGET=11.0
    export MACOSX_DEPLOYMENT_TARGET
    ;;
  *) echo "not a release target: $target" >&2; exit 1 ;;
esac

version=$(sed -n '/^\[package\]/,/^\[/s/^version = "\(.*\)"$/\1/p' Cargo.toml)
[ -n "$version" ] || { echo "cannot read the version from Cargo.toml" >&2; exit 1; }

# Paths of the build host stay out of the executable's panic locations. The
# flags go to build.rustflags, which RUSTFLAGS would replace.
[ -z "${RUSTFLAGS:-}${CARGO_ENCODED_RUSTFLAGS:-}" ] || {
  echo "RUSTFLAGS would replace the flags this script sets; unset it" >&2
  exit 1
}
toml_string() { printf '"%s"' "$(printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g')"; }
workspace=$(pwd -P)
cargo_home=${CARGO_HOME:-$HOME/.cargo}
flags="$(toml_string "--remap-path-prefix=$workspace=cutwater"), $(toml_string "--remap-path-prefix=$cargo_home=cargo")"
set -- --release --locked --target "$target" --config "build.rustflags = [$flags]"
case "$target" in
  *-linux-musl) cargo zigbuild "$@" ;;
  *-apple-darwin) cargo build "$@" ;;
esac

name="cutwater-$version-$target"
stage="target/stage/$name"
rm -rf "$stage"
mkdir -p "$stage"
cp "target/$target/release/cutwater" LICENSE README.md "$stage/"
cargo run --locked --quiet -p xtask -- notices "$stage" "$target"
# The archive's files, in their order in it.
set -- cutwater LICENSE README.md THIRD-PARTY-NOTICES.txt RUST-LIBRARY-COPYRIGHT.html
(cd "$stage" && chmod 0644 "$@" && chmod 0755 cutwater)

case "$target" in
  *-linux-musl)
    # A static executable has no program interpreter and needs no library.
    headers=$(readelf --program-headers --dynamic "$stage/cutwater")
    if printf '%s\n' "$headers" | grep -Eq 'INTERP|\(NEEDED\)'; then
      echo "the $target executable is not statically linked" >&2
      exit 1
    fi
    ;;
  *-apple-darwin) codesign --force --sign - "$stage/cutwater" ;;
esac

# An executable the host can run reports the version it was built as; an
# Apple silicon Mac runs the x86_64 one through Rosetta, when installed.
run=""
case "$(uname -s)-$(uname -m)-$target" in
  Linux-x86_64-x86_64-unknown-linux-musl | Linux-aarch64-aarch64-unknown-linux-musl | Darwin-arm64-aarch64-apple-darwin) run=native ;;
  Darwin-arm64-x86_64-apple-darwin) arch -x86_64 /usr/bin/true 2>/dev/null && run=rosetta ;;
esac
case "$run" in
  native) reported=$("$stage/cutwater" --version) ;;
  rosetta) reported=$(arch -x86_64 "$stage/cutwater" --version) ;;
  *) reported="" ;;
esac
if [ -n "$run" ] && [ "$reported" != "cutwater $version" ]; then
  echo "the $target executable reports '$reported', not cutwater $version" >&2
  exit 1
fi

# The archive carries no owner, time or extended attribute of the build host.
mkdir -p dist
archive="dist/$name.tar.gz"
epoch=$(git log -1 --format=%ct)
if tar --version 2>/dev/null | grep -q 'GNU tar'; then
  tar -C "$stage" --sort=name --owner=0 --group=0 --numeric-owner --mtime="@$epoch" \
    -cf "$workspace/$stage.tar" "$@"
else
  stamp=$(TZ=UTC0 date -r "$epoch" +%Y%m%d%H%M.%S)
  (cd "$stage" && TZ=UTC0 touch -t "$stamp" "$@")
  COPYFILE_DISABLE=1 tar -C "$stage" --uid 0 --gid 0 --uname '' --gname '' --no-xattrs --no-mac-metadata \
    -cf "$workspace/$stage.tar" "$@"
fi
gzip -n -c "$stage.tar" > "$archive"
rm "$stage.tar"
echo "built $archive${run:+, which reports $reported}"
