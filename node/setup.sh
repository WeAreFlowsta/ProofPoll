#!/usr/bin/env bash
# ProofPoll Always-On Node Setup
#
# Run this on a fresh GCE e2-micro instance (Debian/Ubuntu).
# Prerequisites: SSH access, the ProofPoll hApp file.
#
# Usage:
#   1. Create GCE instance:
#      gcloud compute instances create proofpoll-node \
#        --zone=us-central1-a \
#        --machine-type=e2-micro \
#        --boot-disk-size=10GB \
#        --image-family=debian-12 \
#        --image-project=debian-cloud \
#        --tags=proofpoll-node
#
#   2. SSH in and run:
#      sudo bash setup.sh
#
#   3. Copy the hApp file to the server:
#      gcloud compute scp ../src-tauri/resources/proofpoll_v1_0_happ.happ \
#        proofpoll-node:~/node/ --zone=us-central1-a
#
#   4. Install the hApp:
#      bash install-happ.sh

set -euo pipefail

echo "=== ProofPoll Node Setup ==="

# Install Docker if not present
if ! command -v docker &>/dev/null; then
  echo "Installing Docker..."
  apt-get update
  apt-get install -y ca-certificates curl gnupg
  install -m 0755 -d /etc/apt/keyrings
  curl -fsSL https://download.docker.com/linux/debian/gpg -o /etc/apt/keyrings/docker.asc
  chmod a+r /etc/apt/keyrings/docker.asc
  echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.asc] \
    https://download.docker.com/linux/debian $(. /etc/os-release && echo "$VERSION_CODENAME") stable" \
    > /etc/apt/sources.list.d/docker.list
  apt-get update
  apt-get install -y docker-ce docker-ce-cli containerd.io docker-compose-plugin
  systemctl enable docker
  systemctl start docker
  echo "Docker installed."
else
  echo "Docker already installed."
fi

# Add current user to docker group (for non-root usage)
if [ -n "${SUDO_USER:-}" ]; then
  usermod -aG docker "$SUDO_USER"
  echo "Added $SUDO_USER to docker group (re-login to take effect)."
fi

# Pull the edge node image
echo "Pulling edge node image..."
docker pull ghcr.io/holo-host/edgenode:v0.0.10-hc0.6.0-go-pion

# Start the container
echo "Starting ProofPoll node..."
docker compose up -d

echo ""
echo "=== Container running ==="
echo "Next steps:"
echo "  1. Copy the hApp file to this directory:"
echo "     scp proofpoll_v1_0_happ.happ <this-server>:~/node/"
echo "  2. Run: bash install-happ.sh"
echo ""
