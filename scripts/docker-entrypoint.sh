#!/bin/bash
# ============================================================
# K.R.I.A. — Docker Entrypoint
# Handles PUID/PGID override for volume permissions, then
# drops to non-root user via gosu.
# ============================================================
set -e

# ── PUID/PGID override ──────────────────────────────────────
if [ -n "$PUID" ] && [ -n "$PGID" ]; then
  # Adjust the kria group/user IDs to match host
  groupmod -o -g "$PGID" kria 2>/dev/null || true
  usermod -o -u "$PUID" -g "$PGID" kria 2>/dev/null || true
  chown -R kria:kria /app/data /app/models 2>/dev/null || true
fi

# ── Auto-provision models if directory is empty ──────────────
if [ -d "/app/models/llm" ] && [ -z "$(ls -A /app/models/llm/ 2>/dev/null)" ]; then
  echo "[kria] No models found in /app/models/llm/ — provisioning will run on first startup."
fi

# ── Launch ───────────────────────────────────────────────────
if [ -n "$PUID" ] && [ -n "$PGID" ]; then
  exec gosu kria /app/kria-server "$@"
else
  # Default: run as kria user (already set in Dockerfile)
  exec gosu kria /app/kria-server "$@"
fi
