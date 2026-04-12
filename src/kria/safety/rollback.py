"""
Rollback Manager
================
Creates timestamped snapshots of files (and registry keys) before any
RED-tier destructive action executes.  If the user says "undo the last
action", ``restore()`` reverses the change.

Storage layout:
  ~/.kria/rollback/
  └── 2026-04-11T14-30-00/
      ├── manifest.json        — what was changed + metadata
      └── files/               — file backups
          ├── original_name.txt
          └── ...

Retention: snapshots older than ``settings.rollback_retention_hours`` are
pruned automatically at startup and by the scheduled cleanup task.
"""
import hashlib
import json
import logging
import shutil
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Optional

from kria.infra.config import settings

logger = logging.getLogger("kria.safety.rollback")


class RollbackManager:
    def __init__(self) -> None:
        self._base = Path(settings.rollback_dir).expanduser()

    def _ensure_dir(self) -> None:
        self._base.mkdir(parents=True, exist_ok=True)

    # ── Snapshot creation ─────────────────────────────────────────

    async def create_snapshot(
        self,
        session_id: str,
        action: str,
        risk_level: str,
        files: list[str],
    ) -> Optional[str]:
        """
        Back up *files* into a new snapshot directory.
        Returns the snapshot ID (timestamp string) or None on failure.
        """
        try:
            self._ensure_dir()
            timestamp = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H-%M-%S")
            snap_dir = self._base / timestamp
            files_dir = snap_dir / "files"
            files_dir.mkdir(parents=True, exist_ok=True)

            changes = []
            for path_str in files:
                src = Path(path_str)
                if src.exists() and src.is_file():
                    # Avoid collisions with same filename from different dirs
                    unique_name = f"{src.parent.name}__{src.name}"
                    dest = files_dir / unique_name
                    shutil.copy2(src, dest)
                    file_hash = hashlib.sha256(src.read_bytes()).hexdigest()
                    changes.append({
                        "type": "file_backed_up",
                        "original_path": str(src),
                        "backup_path": str(dest),
                        "hash_sha256": file_hash,
                    })

            manifest = {
                "timestamp": datetime.now(timezone.utc).isoformat(),
                "session_id": session_id,
                "action": action,
                "risk_level": risk_level,
                "changes": changes,
                "rollback_command": "restore_files",
                "expires": (
                    datetime.now(timezone.utc)
                    + timedelta(hours=settings.rollback_retention_hours)
                ).isoformat(),
            }
            (snap_dir / "manifest.json").write_text(
                json.dumps(manifest, indent=2), encoding="utf-8"
            )
            logger.info("Rollback snapshot created: %s (%d files)", timestamp, len(changes))
            return timestamp

        except Exception as exc:
            logger.error("Failed to create rollback snapshot: %s", exc)
            return None

    # ── Restore ───────────────────────────────────────────────────

    async def restore(self, rollback_id: str) -> bool:
        """Restore files from snapshot *rollback_id*. Returns True on success."""
        try:
            snap_dir = self._base / rollback_id
            manifest_path = snap_dir / "manifest.json"
            if not manifest_path.exists():
                logger.error("Rollback manifest not found: %s", rollback_id)
                return False

            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
            restored = 0
            for change in manifest.get("changes", []):
                backup = Path(change["backup_path"])
                original = Path(change["original_path"])
                if backup.exists():
                    original.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copy2(backup, original)
                    logger.info("Restored: %s", original)
                    restored += 1

            logger.info("Rollback complete: %d files restored from %s", restored, rollback_id)
            return True

        except Exception as exc:
            logger.error("Rollback restore failed for %s: %s", rollback_id, exc)
            return False

    # ── List snapshots ────────────────────────────────────────────

    def list_snapshots(self) -> list[dict]:
        if not self._base.exists():
            return []
        snapshots = []
        for entry in sorted(self._base.iterdir(), reverse=True):
            manifest_path = entry / "manifest.json"
            if manifest_path.exists():
                try:
                    m = json.loads(manifest_path.read_text(encoding="utf-8"))
                    snapshots.append({
                        "id": entry.name,
                        "action": m.get("action"),
                        "timestamp": m.get("timestamp"),
                        "expires": m.get("expires"),
                    })
                except Exception:
                    pass
        return snapshots

    # ── Cleanup ───────────────────────────────────────────────────

    async def cleanup_expired(self) -> int:
        """Remove snapshots past their expiry. Returns number removed."""
        if not self._base.exists():
            return 0
        now = datetime.now(timezone.utc)
        removed = 0
        for entry in list(self._base.iterdir()):
            if not entry.is_dir():
                continue
            manifest_path = entry / "manifest.json"
            try:
                if manifest_path.exists():
                    m = json.loads(manifest_path.read_text(encoding="utf-8"))
                    expires = datetime.fromisoformat(m["expires"])
                    if now > expires:
                        shutil.rmtree(entry)
                        logger.info("Pruned expired snapshot: %s", entry.name)
                        removed += 1
            except Exception as exc:
                logger.warning("Could not process snapshot %s: %s", entry.name, exc)
        return removed


rollback_manager = RollbackManager()
