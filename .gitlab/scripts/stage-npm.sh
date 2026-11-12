#!/bin/sh
# Stages the npm packages for packing: each platform package's cutwater and
# third-party notices, from the release archive of its target in dist/, and
# LICENSE in every package.
# Where dist/ holds SHA256SUMS, as in the release commit's pipeline after
# signing, the archives must be those it lists, so that npm ships the
# executables of the signed archives.
#
# The release merge request's pipeline always stages, for cutwater-goal.
# Otherwise only the push pipeline of a commit that releases cutwater, as
# `cutwater released` names it, stages: it alone builds and signs the
# archives, and one that is missing there fails it. Any other pipeline, a
# later commit's or one scheduled or started by hand on the release commit,
# stages nothing; its cutwater-publish passes where the release's tag and
# Release exist, and fails where the release is unpublished, as the archives
# its assets name are missing.
#
# usage: .gitlab/scripts/stage-npm.sh   (with cutwater on the PATH)
set -eu

if [ -z "${CI_MERGE_REQUEST_IID:-}" ]; then
  if [ "${CI_PIPELINE_SOURCE:-}" != push ]; then
    echo "a ${CI_PIPELINE_SOURCE:-local} pipeline builds no release files, so no npm package is staged"
    exit 0
  fi
  released=$(cutwater released)
  if [ -z "$released" ]; then
    echo "this commit releases nothing, so no npm package is staged"
    exit 0
  fi
fi

version=$(sed -n '/^\[package\]/,/^\[/s/^version = "\(.*\)"$/\1/p' Cargo.toml)
[ -n "$version" ] || { echo "cannot read the version from Cargo.toml" >&2; exit 1; }
if [ -f dist/SHA256SUMS ]; then
  (cd dist && sha256sum --check --strict SHA256SUMS)
fi
for entry in \
  x86_64-unknown-linux-musl:linux-x64 \
  aarch64-unknown-linux-musl:linux-arm64 \
  x86_64-apple-darwin:darwin-x64 \
  aarch64-apple-darwin:darwin-arm64; do
  target=${entry%%:*}
  package="npm/cutwater-bin-${entry#*:}"
  archive="dist/cutwater-$version-$target.tar.gz"
  if [ ! -f "$archive" ]; then
    case "$target" in
      *-apple-darwin)
        echo "dist/ lacks ${archive#dist/}. The macOS runner builds it, in a merge request pipeline only when the release branch and the target branch are protected and the project allows merge request pipelines to access protected runners (CONTRIBUTING.md, Releasing cutwater)." >&2
        ;;
      *) echo "dist/ lacks ${archive#dist/}" >&2 ;;
    esac
    exit 1
  fi
  rm -f "$package/cutwater" "$package/THIRD-PARTY-NOTICES.txt" "$package/RUST-LIBRARY-COPYRIGHT.html"
  tar -xzf "$archive" -C "$package" cutwater THIRD-PARTY-NOTICES.txt RUST-LIBRARY-COPYRIGHT.html
  echo "staged $package/cutwater from $archive"
done
for package in npm/cutwater npm/cutwater-bin-*; do
  cp LICENSE "$package/LICENSE"
done
