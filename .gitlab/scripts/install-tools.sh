#!/bin/sh
# Installs pinned tools into <dir> after verifying the SHA-256 checksum of
# each download; there is no fallback to another source or to a build. The
# executables go to <dir>/bin. The pipeline's jobs run on x86_64 Linux, the
# group's runner, and the tools are pinned for it alone. A new version of a
# tool needs both its version and its checksum below; a checksum is the
# download's SHA-256, as its release lists it and as a download shows it.
#
# usage: .gitlab/scripts/install-tools.sh <dir> <tool>...
#   tools: cargo-deny, shellcheck, node (with npm), pnpm
set -eu

dir=${1:?usage: install-tools.sh <dir> <tool>...}
shift
host="$(uname -s)-$(uname -m)"
[ "$host" = Linux-x86_64 ] || { echo "the pinned tools are for x86_64 Linux, not $host" >&2; exit 1; }
mkdir -p "$dir/bin"
dir=$(cd "$dir" && pwd -P)

fetch() { # <url> <sha256> <file>
  curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 --output "$3" "$1"
  echo "$2  $3" | sha256sum --check --strict --quiet -
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
      version=0.20.2
      unpack "https://github.com/EmbarkStudios/cargo-deny/releases/download/$version/cargo-deny-$version-x86_64-unknown-linux-musl.tar.gz" \
        9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f \
        "cargo-deny-$version-x86_64-unknown-linux-musl/cargo-deny"
      ;;
    shellcheck)
      version=0.11.0
      unpack "https://github.com/koalaman/shellcheck/releases/download/v$version/shellcheck-v$version.linux.x86_64.tar.xz" \
        8c3be12b05d5c177a04c29e3c78ce89ac86f1595681cab149b65b97c4e227198 \
        "shellcheck-v$version/shellcheck"
      ;;
    node)
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
      version=12.18.0
      unpack_all "https://github.com/pnpm/pnpm/releases/download/v$version/pnpm-linux-x64.tar.gz" \
        3726060e236c0db3e7124c0a5af123c840ba0cb76682149b63ea0baf290017c2 \
        pnpm
      ln -sf ../pnpm/pnpm "$dir/bin/pnpm"
      ;;
    *)
      echo "unknown tool: $tool" >&2
      exit 1
      ;;
  esac
  echo "installed $tool"
done
