#!/bin/sh
# Installs pinned tools into <dir> after verifying the SHA-256 checksum of
# each download; there is no fallback to another source or to a build. The
# executables go to <dir>/bin. The pipeline's jobs run on x86_64 Linux, the
# group's runners and GitLab.com's shared runners, and the macOS builds on
# Apple silicon; each tool is pinned for the hosts that run it. A new version
# of a tool needs both its version and its checksums below; a checksum is the
# download's SHA-256, as its release lists it and as a download shows it.
#
# usage: .gitlab/scripts/install-tools.sh <dir> <tool>...
#   tools: cargo-deny, shellcheck, node (with npm), pnpm, cargo-http-registry,
#          cosign, zig, cargo-zigbuild, cargo-about (also on macOS), rustup
#          (rustup-init, on macOS)
set -eu

dir=${1:?usage: install-tools.sh <dir> <tool>...}
shift
host="$(uname -s)-$(uname -m)"
case "$host" in
  Linux-x86_64) sha256() { sha256sum "$@"; } ;;
  Darwin-arm64) sha256() { shasum -a 256 "$@"; } ;;
  *) echo "the pinned tools are for x86_64 Linux and arm64 macOS, not $host" >&2; exit 1 ;;
esac
mkdir -p "$dir/bin"
dir=$(cd "$dir" && pwd -P)

unpinned() {
  echo "$tool is not pinned for $host" >&2
  exit 1
}

fetch() { # <url> <sha256> <file>
  curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 --output "$3" "$1"
  echo "$2  $3" | sha256 --check --strict --quiet - || {
    rm -f "$3"
    echo "$1 does not match its pinned SHA-256" >&2
    exit 1
  }
}

# <url> <sha256> <path of the executable in the archive>
unpack() {
  archive="$dir/download"
  fetch "$1" "$2" "$archive"
  tar -xf "$archive" -C "$dir/bin" --strip-components="$(printf '%s' "$3" | tr -cd / | wc -c)" "$3"
  rm "$archive"
}

# <url> <sha256> <name>: the whole archive into <dir>/<name>, without its
# top directory when it has one.
unpack_all() {
  archive="$dir/download"
  fetch "$1" "$2" "$archive"
  rm -rf "${dir:?}/$3"
  mkdir -p "$dir/$3"
  tar -xf "$archive" -C "$dir/$3" --strip-components="${4:-0}"
  rm "$archive"
}

for tool in "$@"; do
  case "$tool" in
    cargo-deny)
      [ "$host" = Linux-x86_64 ] || unpinned
      version=0.20.2
      unpack "https://github.com/EmbarkStudios/cargo-deny/releases/download/$version/cargo-deny-$version-x86_64-unknown-linux-musl.tar.gz" \
        9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f \
        "cargo-deny-$version-x86_64-unknown-linux-musl/cargo-deny"
      ;;
    shellcheck)
      [ "$host" = Linux-x86_64 ] || unpinned
      version=0.11.0
      unpack "https://github.com/koalaman/shellcheck/releases/download/v$version/shellcheck-v$version.linux.x86_64.tar.xz" \
        8c3be12b05d5c177a04c29e3c78ce89ac86f1595681cab149b65b97c4e227198 \
        "shellcheck-v$version/shellcheck"
      ;;
    node)
      [ "$host" = Linux-x86_64 ] || unpinned
      # The release of the Node.js image in .gitlab-ci.yml.
      version=24.22.2
      unpack_all "https://nodejs.org/dist/v$version/node-v$version-linux-x64.tar.xz" \
        48cc0e21e048790e54c0cf5749c2725005313d652651c40b7a21feb9697aaf75 \
        node 1
      for executable in node npm npx; do
        ln -sf "../node/bin/$executable" "$dir/bin/$executable"
      done
      ;;
    pnpm)
      [ "$host" = Linux-x86_64 ] || unpinned
      version=12.18.0
      unpack_all "https://github.com/pnpm/pnpm/releases/download/v$version/pnpm-linux-x64.tar.gz" \
        3726060e236c0db3e7124c0a5af123c840ba0cb76682149b63ea0baf290017c2 \
        pnpm
      ln -sf ../pnpm/pnpm "$dir/bin/pnpm"
      ;;
    cargo-http-registry)
      [ "$host" = Linux-x86_64 ] || unpinned
      # A Cargo registry for the registry tests. crates.io publishes no
      # binary of it, so it is built from its crate with the dependencies
      # that the crate's Cargo.lock pins. It is built outside <dir>, which may
      # lie inside a Cargo workspace that would otherwise claim the crate.
      version=0.1.8
      fetch "https://static.crates.io/crates/cargo-http-registry/cargo-http-registry-$version.crate" \
        193f465b09986c2c07ae55bf52131fb51ab2cd89c18ebfca1672cf13e7eb92cd "$dir/download"
      src=$(mktemp -d)
      tar -xf "$dir/download" -C "$src" --strip-components=1
      rm "$dir/download"
      cargo install --locked --quiet --path "$src" --root "$dir"
      rm -rf "${src:?}"
      ;;
    cosign)
      # The release the CI/CD component pins, templates/release.yml.
      [ "$host" = Linux-x86_64 ] || unpinned
      version=3.1.4
      fetch "https://github.com/sigstore/cosign/releases/download/v$version/cosign-linux-amd64" \
        424e2c97d0b45fd7a6b608a87f090f35633f4c23ece5f092d48bba11aaf4bbfa "$dir/bin/cosign"
      chmod 0755 "$dir/bin/cosign"
      ;;
    zig)
      # The linker and C compiler of the musl builds, through cargo-zigbuild.
      [ "$host" = Linux-x86_64 ] || unpinned
      version=0.17.1
      unpack_all "https://ziglang.org/download/$version/zig-x86_64-linux-$version.tar.xz" \
        3581863e3cbcfd0fce0efc682ce7e9e93da96668a977791d356a26d2a7e9b17a \
        zig 1
      ln -sf ../zig/zig "$dir/bin/zig"
      ;;
    cargo-zigbuild)
      [ "$host" = Linux-x86_64 ] || unpinned
      version=0.24.1
      unpack "https://github.com/rust-cross/cargo-zigbuild/releases/download/v$version/cargo-zigbuild-x86_64-unknown-linux-musl.tar.xz" \
        f09b180a517af0e4dd4bffc921bb88e5ab705452a9dd948c61e489b3020c68c2 \
        cargo-zigbuild-x86_64-unknown-linux-musl/cargo-zigbuild
      ;;
    cargo-about)
      # xtask/src/main.rs refuses to write the notices with another release.
      version=0.9.2
      case "$host" in
        Linux-x86_64)
          triple=x86_64-unknown-linux-musl
          checksum=9099a59e820c38a68b9d65f300662a567d56562f9a10f6aa4c7e86c17c2566af
          ;;
        Darwin-arm64)
          triple=aarch64-apple-darwin
          checksum=ae72f0df0c399a1e96336f696fa55b1b28679fd725632eba8cf8e4568467cc3e
          ;;
      esac
      unpack "https://github.com/EmbarkStudios/cargo-about/releases/download/$version/cargo-about-$version-$triple.tar.gz" \
        "$checksum" "cargo-about-$version-$triple/cargo-about"
      ;;
    rustup)
      # rustup-init for the macOS runner, which installs the toolchain into the
      # job's own directories.
      [ "$host" = Darwin-arm64 ] || unpinned
      version=1.29.1
      fetch "https://static.rust-lang.org/rustup/archive/$version/aarch64-apple-darwin/rustup-init" \
        ec1b9233e7f72990ecd8e62063fa7f6c3dfc2bec8e97f88bff165f9100ac696a "$dir/bin/rustup-init"
      chmod 0755 "$dir/bin/rustup-init"
      ;;
    *)
      echo "unknown tool: $tool" >&2
      exit 1
      ;;
  esac
  echo "installed $tool"
done
