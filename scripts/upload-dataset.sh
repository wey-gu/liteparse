#!/usr/bin/env bash
# Regenerates and uploads the dataset to HuggingFace
#
# Usage:
#   HF_TOKEN=xxx ./scripts/upload-dataset.sh [dataset-dir] [repo-name]
#
# Arguments:
#   dataset-dir - Directory containing the dataset with data/ subfolder (default: ./dataset)
#   repo-name   - HuggingFace repository name (default: llamaindex/liteparse_cicd_data)
#
# Environment variables:
#   HF_TOKEN - HuggingFace API token with write access
#
# Requires: hf cli (pip install huggingface_hub)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

DATASET_DIR="${1:-$REPO_ROOT/dataset}"
REPO_NAME="${2:-llamaindex/liteparse_cicd_data}"

if [ -z "${HF_TOKEN:-}" ]; then
  echo "Error: HF_TOKEN environment variable is required"
  echo "Get a token from https://huggingface.co/settings/tokens"
  exit 1
fi

DOCUMENTS_DIR="$DATASET_DIR/data"

echo "LiteParse Dataset Upload"
echo "========================"
echo "Dataset: $DATASET_DIR"
echo "Documents: $DOCUMENTS_DIR"
echo "Repo: $REPO_NAME"
echo

# Step 1: Regenerate dataset from documents in the dataset directory
echo "Step 1: Regenerating dataset from existing documents..."
"$SCRIPT_DIR/create-dataset.sh" "$DATASET_DIR" "$DOCUMENTS_DIR"

# Step 2: Upload to HuggingFace
echo
echo "Step 2: Uploading to HuggingFace..."
hf upload "$REPO_NAME" "$DATASET_DIR" --repo-type dataset --token "$HF_TOKEN"

echo
echo "✓ Dataset uploaded successfully!"
echo "  View at: https://huggingface.co/datasets/$REPO_NAME"
