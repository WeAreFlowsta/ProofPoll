#!/usr/bin/env bash
# Install the ProofPoll hApp into the running edge node container.
#
# Prerequisites:
#   - Container "proofpoll-node" is running (docker compose up -d)
#   - proofpoll_v1_0_happ.happ file is in this directory
#
# Usage: bash install-happ.sh

set -euo pipefail

CONTAINER="proofpoll-node"
HAPP_FILE="proofpoll_v1_0_happ.happ"
CONFIG_FILE="proofpoll-happ-config.json"
APP_ID="proofpoll_v1_0"

# Check the hApp file exists locally
if [ ! -f "$HAPP_FILE" ]; then
  echo "Error: $HAPP_FILE not found in current directory."
  echo "Copy it from: ProofPoll/src-tauri/resources/$HAPP_FILE"
  exit 1
fi

# Check container is running
if ! docker ps --format '{{.Names}}' | grep -q "^${CONTAINER}$"; then
  echo "Error: Container '$CONTAINER' is not running."
  echo "Run: docker compose up -d"
  exit 1
fi

echo "=== Installing ProofPoll hApp ==="

# Create hApp directory inside the container
docker exec "$CONTAINER" mkdir -p /data/happ

# Copy files into the container
echo "Copying hApp file..."
docker cp "$HAPP_FILE" "$CONTAINER":/data/happ/
docker cp "$CONFIG_FILE" "$CONTAINER":/data/happ/

# Wait for conductor to be ready
echo "Waiting for conductor to be ready..."
for i in $(seq 1 30); do
  if docker exec "$CONTAINER" su - nonroot -c "list_happs 2>/dev/null" &>/dev/null; then
    echo "Conductor ready."
    break
  fi
  if [ "$i" -eq 30 ]; then
    echo "Error: Conductor not ready after 30s. Check logs:"
    echo "  docker logs $CONTAINER"
    exit 1
  fi
  sleep 1
done

# Install the hApp (install_happ also enables it automatically)
echo "Installing hApp..."
docker exec "$CONTAINER" su - nonroot -c "install_happ /data/happ/$CONFIG_FILE"

# Verify
echo ""
echo "=== Installed hApps ==="
docker exec "$CONTAINER" su - nonroot -c "list_happs"

echo ""
echo "ProofPoll node is now running and participating in the DHT."
echo "Desktop clients will discover it via the bootstrap server."
