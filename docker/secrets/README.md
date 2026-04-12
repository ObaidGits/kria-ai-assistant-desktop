# This directory holds Docker secrets (not committed to git).
# Populated automatically by scripts/setup.ps1
# 
# Required file: bridge_secret.txt
#   Contains the shared HMAC secret between kria-core (Docker)
#   and kria_bridge.py (Windows host).
#   setup.ps1 copies ~/.kria/bridge_secret.txt here automatically.
