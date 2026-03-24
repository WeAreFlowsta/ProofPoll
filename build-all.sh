#!/bin/bash

# Build all ProofPoll DNA versions.
#
# All hApp bundles must be present in src-tauri/resources/ for the
# migration system to work. Run this script before `cargo tauri dev`
# or `cargo tauri build`.

set -e

echo "=== Building ProofPoll DNA v1.0 ==="
(cd dna && bash build.sh)

echo ""
echo "=== Building ProofPoll DNA v1.1 ==="
(cd dna/v1.1 && bash build.sh)

echo ""
echo "=== Building ProofPoll DNA v1.2 ==="
(cd dna/v1.2 && bash build.sh)

echo ""
echo "=== Building ProofPoll DNA v1.3 ==="
(cd dna/v1.3 && bash build.sh)

echo ""
echo "=== Build complete ==="
echo ""
echo "Resources:"
ls -lh src-tauri/resources/*.happ
