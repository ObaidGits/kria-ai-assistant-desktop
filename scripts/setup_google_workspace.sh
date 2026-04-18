#!/usr/bin/env bash
# One-time Google Workspace OAuth setup for KRIA.
#
# Run this ONCE before starting KRIA for the first time.
# It will open a browser for Google OAuth consent.
#
# Usage:
#   bash scripts/setup_google_workspace.sh
#   bash scripts/setup_google_workspace.sh work    # use a different account name
#
# IMPORTANT — Required Google Cloud APIs:
#   Before running this script, ensure the following APIs are ENABLED in your
#   Google Cloud project at https://console.cloud.google.com/apis/library
#   (use the same project that issued your OAuth credentials):
#
#     Gmail API         → https://console.cloud.google.com/apis/api/gmail.googleapis.com
#     Calendar API      → https://console.cloud.google.com/apis/api/calendar-json.googleapis.com
#     Drive API         → https://console.cloud.google.com/apis/api/drive.googleapis.com
#     Docs API          → https://console.cloud.google.com/apis/api/docs.googleapis.com
#     Sheets API        → https://console.cloud.google.com/apis/api/sheets.googleapis.com
#     Slides API        → https://console.cloud.google.com/apis/api/slides.googleapis.com
#     Forms API         → https://console.cloud.google.com/apis/api/forms.googleapis.com
#
#   After enabling each API, wait ~1 minute for Google to propagate the change.
#
set -euo pipefail

ACCOUNT_NAME="${1:-personal}"
CONFIG_DIR="${HOME}/.google-mcp"
CREDS_FILE="${CONFIG_DIR}/credentials.json"

echo ""
echo "=== KRIA Google Workspace Setup ==="
echo ""

# 0. Remind user to enable APIs
echo "────────────────────────────────────────────────────────"
echo "STEP 0 — Ensure required Google Cloud APIs are ENABLED:"
echo "  Gmail    : https://console.cloud.google.com/apis/api/gmail.googleapis.com"
echo "  Calendar : https://console.cloud.google.com/apis/api/calendar-json.googleapis.com"
echo "  Drive    : https://console.cloud.google.com/apis/api/drive.googleapis.com"
echo "  Docs     : https://console.cloud.google.com/apis/api/docs.googleapis.com"
echo "  Sheets   : https://console.cloud.google.com/apis/api/sheets.googleapis.com"
echo "  Slides   : https://console.cloud.google.com/apis/api/slides.googleapis.com"
echo "  Forms    : https://console.cloud.google.com/apis/api/forms.googleapis.com"
echo "If any shows 'ENABLE', click it, wait 1 minute, then continue here."
echo "────────────────────────────────────────────────────────"
echo ""

# 1. Check credentials file
if [ ! -f "${CREDS_FILE}" ]; then
  echo "ERROR: ${CREDS_FILE} not found."
  echo ""
  echo "Please create it with your Google Cloud OAuth credentials:"
  echo '  {"installed":{"client_id":"YOUR_ID","client_secret":"YOUR_SECRET","redirect_uris":["http://localhost"],"auth_uri":"https://accounts.google.com/o/oauth2/auth","token_uri":"https://oauth2.googleapis.com/token"}}'
  exit 1
fi

echo "credentials.json found at ${CREDS_FILE}"

# Print project number so user can verify they're enabling the right project
PROJECT_NUM=$(python3 -c "
import json, sys
try:
    d = json.load(open('${CREDS_FILE}'))
    cid = d.get('installed', d.get('web', {})).get('client_id', '')
    print(cid.split('-')[0] if '-' in cid else 'unknown')
except Exception:
    print('unknown')
" 2>/dev/null || echo "unknown")
echo "  → OAuth project number: ${PROJECT_NUM}"
echo "    Verify APIs are enabled for this project!"
echo ""

# 2. Check npx is available
if ! command -v npx &>/dev/null; then
  echo "ERROR: npx not found. Install Node.js >= 18 from https://nodejs.org"
  exit 1
fi

# 3. Run setup check
echo ""
echo "--- Running google-workspace-mcp setup check ---"
GOOGLE_MCP_CONFIG_DIR="${CONFIG_DIR}" npx -y google-workspace-mcp setup || true

# 4. Add account (opens browser)
echo ""
echo "--- Adding Google account '${ACCOUNT_NAME}' ---"
echo "A browser window will open. Sign in with your Google account and click Allow."
echo "Grant ALL requested permissions (Gmail, Calendar, Drive, Docs, Sheets, Slides, Forms)."
echo ""
GOOGLE_MCP_CONFIG_DIR="${CONFIG_DIR}" npx google-workspace-mcp accounts add "${ACCOUNT_NAME}"

# 5. Verify
echo ""
echo "--- Verifying account ---"
GOOGLE_MCP_CONFIG_DIR="${CONFIG_DIR}" npx google-workspace-mcp accounts list

echo ""
echo "=== Setup complete! ==="
echo ""
echo "Account '${ACCOUNT_NAME}' is now authorised."
echo "KRIA will use this account name when calling Google Workspace tools."
echo ""
echo "If you used a name other than 'personal', set this env var before starting KRIA:"
echo "  export KRIA_GW_ACCOUNT=${ACCOUNT_NAME}"
echo ""
echo "If you see 'API is disabled' errors in KRIA, enable the missing API at:"
echo "  https://console.cloud.google.com/apis/library?project=${PROJECT_NUM}"
echo "  Then re-run this script so fresh OAuth tokens include the new scope."
echo ""
