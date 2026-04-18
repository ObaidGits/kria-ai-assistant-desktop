#!/usr/bin/env bash
# Live Google Workspace smoke matrix for KRIA.
#
# Verifies account readiness and per-service API permission health using
# google-workspace-mcp CLI diagnostics.
#
# Usage:
#   bash scripts/google_workspace_smoke_matrix.sh
#   bash scripts/google_workspace_smoke_matrix.sh work
#
# Optional env vars:
#   GOOGLE_MCP_CONFIG_DIR=/path/to/.google-mcp
#   KRIA_GW_ACCOUNT=personal
set -euo pipefail

ACCOUNT_NAME="${1:-${KRIA_GW_ACCOUNT:-personal}}"
CONFIG_DIR="${GOOGLE_MCP_CONFIG_DIR:-${HOME}/.google-mcp}"

if ! command -v npx >/dev/null 2>&1; then
  echo "ERROR: npx not found. Install Node.js >= 18 and retry."
  exit 1
fi

echo ""
echo "=== KRIA Google Workspace Live Smoke Matrix ==="
echo "Account      : ${ACCOUNT_NAME}"
echo "Config dir   : ${CONFIG_DIR}"
echo "Timestamp    : $(date -Iseconds)"
echo ""

echo "--- Server status ---"
if ! GOOGLE_MCP_CONFIG_DIR="${CONFIG_DIR}" npx -y google-workspace-mcp status; then
  echo ""
  echo "Smoke failed: google-workspace-mcp status check failed."
  exit 1
fi

echo ""
echo "--- Service permission matrix ---"
set +e
PERM_OUTPUT="$(GOOGLE_MCP_CONFIG_DIR="${CONFIG_DIR}" npx -y google-workspace-mcp accounts test-permissions "${ACCOUNT_NAME}" 2>&1)"
PERM_EXIT=$?
set -e
printf "%s\n" "${PERM_OUTPUT}"

if [[ ${PERM_EXIT} -ne 0 ]]; then
  echo ""
  echo "Smoke failed: permission check command exited with code ${PERM_EXIT}."
  exit ${PERM_EXIT}
fi

missing_service=0
for service in Drive Docs Sheets Gmail Calendar Slides Forms; do
  if ! grep -Eq "[[:space:]](✅|❌)[[:space:]]${service}" <<<"${PERM_OUTPUT}"; then
    echo "Smoke failed: did not find '${service}' result line in permission output."
    missing_service=1
  fi
done

if [[ ${missing_service} -ne 0 ]]; then
  exit 1
fi

if grep -Eq "[[:space:]]❌[[:space:]]" <<<"${PERM_OUTPUT}"; then
  echo ""
  echo "Smoke failed: at least one Google service is not authorized for '${ACCOUNT_NAME}'."
  echo "Fix path:"
  echo "  1) Enable missing Google APIs in Google Cloud Console"
  echo "  2) Re-run: bash scripts/setup_google_workspace.sh ${ACCOUNT_NAME}"
  exit 2
fi

echo ""
echo "PASS: Gmail, Calendar, Drive, Docs, Sheets, Slides, and Forms are authorized."
echo ""
echo "Meet-link note:"
echo "  google-workspace-mcp CLI does not expose direct calendar-event creation commands."
echo "  KRIA validates Meet-link rendering via mocked UI regression in"
echo "  ui/src/components/MessageBubble.test.tsx (Join Meet assertion)."
