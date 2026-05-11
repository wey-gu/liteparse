#!/bin/bash
# Compare dataset outputs and set GitHub Actions output variables
# Usage: ./compare-outputs.sh <expected-dataset-path>

set -u

EXPECTED_DATASET="${1:-expected-dataset}"
OUTPUT_FILE="comparison-output.txt"

# Run comparison script
set +e
./scripts/compare-dataset.sh "$EXPECTED_DATASET" > "$OUTPUT_FILE" 2>&1
EXIT_CODE=$?
set -e

cat "$OUTPUT_FILE"

if [ $EXIT_CODE -eq 0 ]; then
  echo "has_changes=false" >> "$GITHUB_OUTPUT"
  echo "✓ No output changes detected"
elif [ $EXIT_CODE -eq 1 ]; then
  echo "has_changes=true" >> "$GITHUB_OUTPUT"
  echo "⚠ Output changes detected - requires approval"
else
  echo "has_changes=error" >> "$GITHUB_OUTPUT"
  echo "✗ Error running comparison"
  exit 1
fi
