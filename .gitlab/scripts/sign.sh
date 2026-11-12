#!/bin/sh
# Signs a release's archives in dist/ with cosign, keyless: the job's GitLab
# ID token (SIGSTORE_ID_TOKEN, audience sigstore) obtains a short-lived
# Sigstore certificate naming this project's CI configuration on the default
# branch, and the signature is recorded in Sigstore's transparency log. It
# writes SHA256SUMS over the four archives and its bundle,
# SHA256SUMS.sigstore.json, and verifies the bundle against the identity the
# CI/CD component verifies it with.
#
# usage: .gitlab/scripts/sign.sh   (with cosign on the PATH)
set -eu

: "${SIGSTORE_ID_TOKEN:?the job has no ID token for Sigstore}"
: "${CI_SERVER_URL:?}" "${CI_PROJECT_PATH:?}" "${CI_CONFIG_PATH:?}" "${CI_DEFAULT_BRANCH:?}"
[ "${CI_COMMIT_BRANCH:-}" = "$CI_DEFAULT_BRANCH" ] || {
  echo "a release is signed in a pipeline of the default branch, $CI_DEFAULT_BRANCH" >&2
  exit 1
}

version=$(sed -n '/^\[package\]/,/^\[/s/^version = "\(.*\)"$/\1/p' Cargo.toml)
[ -n "$version" ] || { echo "cannot read the version from Cargo.toml" >&2; exit 1; }
cd dist
set -- \
  "cutwater-$version-x86_64-unknown-linux-musl.tar.gz" \
  "cutwater-$version-aarch64-unknown-linux-musl.tar.gz" \
  "cutwater-$version-x86_64-apple-darwin.tar.gz" \
  "cutwater-$version-aarch64-apple-darwin.tar.gz"
for archive in "$@"; do
  [ -f "$archive" ] || { echo "dist/ lacks $archive" >&2; exit 1; }
done
sha256sum -- "$@" > SHA256SUMS

identity="$CI_SERVER_URL/$CI_PROJECT_PATH//$CI_CONFIG_PATH@refs/heads/$CI_DEFAULT_BRANCH"
cosign sign-blob --yes --bundle SHA256SUMS.sigstore.json SHA256SUMS
cosign verify-blob --bundle SHA256SUMS.sigstore.json \
  --certificate-identity "$identity" --certificate-oidc-issuer "$CI_SERVER_URL" SHA256SUMS
cat SHA256SUMS
