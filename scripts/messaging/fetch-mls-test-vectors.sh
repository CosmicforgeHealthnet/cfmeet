#!/usr/bin/env bash
# Downloads the MLS working group's RFC 9420 test vectors at a PINNED commit and verifies
# their SHA-256 before use (packages/messaging-crypto/tests/rfc9420_vectors.rs). They are
# not vendored: their repository (github.com/mlswg/mls-implementations) has no licence file.
#
#   fetch-mls-test-vectors.sh <output directory>
#
# Changing the commit or a digest is a reviewed change (Part V §144 / Appendix N).
set -euo pipefail
out="${1:?output directory}"
commit="2034fe72e0c805a683ba76165660c834f2a8142a"
declare -A sha256=(
  [messages.json]="b194abe1561995223482dbad51c180146920dc2f637e74d01e07a388308791fb"
  [crypto-basics.json]="17cfcf89af9f51d0f2aa7af77f6f9ec99376a039214b6d42a6f11646b83e8c29"
)
mkdir -p "$out"
for file in "${!sha256[@]}"; do
  curl -sSf --retry 4 --retry-delay 2 -o "$out/$file" \
    "https://raw.githubusercontent.com/mlswg/mls-implementations/$commit/test-vectors/$file"
  echo "${sha256[$file]}  $out/$file" | sha256sum --check --strict -
done
