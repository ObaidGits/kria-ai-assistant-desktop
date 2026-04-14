"""
File Watcher (GREEN tier — monitoring)
=======================================
Watch directories for file changes and emit events.
"""
import logging
from pathlib import Path

from kria.infra.isolation import ToolResult, isolated
from kria.tools.registry import tool_registry

logger = logging.getLogger("kria.tools.file_watcher")


class FileWatcherManager:
    def __init__(self):
        self._observers: dict[str, "Observer"] = {}

    def watch(self, directory: str) -> bool:
        if directory in self._observers:
            return False
        try:
            from watchdog.observers import Observer
            from watchdog.events import FileSystemEventHandler

            class KriaFileHandler(FileSystemEventHandler):
                def on_created(self, event):
                    if not event.is_directory:
                        try:
                            from kria.automation.event_bus import event_bus
                            event_bus.emit("file_created", {"path": event.src_path})
                        except Exception:
                            pass

                def on_modified(self, event):
                    if not event.is_directory:
                        try:
                            from kria.automation.event_bus import event_bus
                            event_bus.emit("file_modified", {"path": event.src_path})
                        except Exception:
                            pass

                def on_deleted(self, event):
                    if not event.is_directory:
                        try:
                            from kria.automation.event_bus import event_bus
                            event_bus.emit("file_deleted", {"path": event.src_path})
                        except Exception:
                            pass

            observer = Observer()
            observer.schedule(KriaFileHandler(), directory, recursive=False)
            observer.start()
            self._observers[directory] = observer
            logger.info("Watching directory: %s", directory)
            return True
        except ImportError:
            logger.warning("watchdog not installed — file watching disabled")
            return False

    def unwatch(self, directory: str) -> bool:
        obs = self._observers.pop(directory, None)
        if obs:
            obs.stop()
            obs.join(timeout=5)
            return True
        return False

    def list_watched(self) -> list[str]:
        return list(self._observers.keys())

    def stop_all(self):
        for obs in self._observers.values():
            obs.stop()
        for obs in self._observers.values():
            obs.join(timeout=5)
        self._observers.clear()


file_watcher = FileWatcherManager()


@isolated
async def watch_directory(directory: str) -> dict:
    """Start watching a directory for file changes."""
    started = file_watcher.watch(directory)
    return {
        "directory": directory,
        "watching": started,
        "note": "Already watching" if not started else "Watching started",
    }


@isolated
async def unwatch_directory(directory: str) -> dict:
    """Stop watching a directory."""
    stopped = file_watcher.unwatch(directory)
    return {"directory": directory, "stopped": stopped}


@isolated
async def list_watched_directories() -> dict:
    """List all currently watched directories."""
    return {"directories": file_watcher.list_watched()}


# ── Register ──────────────────────────────────────────────────────

tool_registry.register("watch_directory", watch_directory,
    description="Start watching a directory for file changes (creates, modifications, deletions).",
    parameters_schema={
        "directory": {"type": "string", "description": "Directory path to watch"},
    })

tool_registry.register("unwatch_directory", unwatch_directory,
    description="Stop watching a directory for file changes.",
    parameters_schema={
        "directory": {"type": "string", "description": "Directory to stop watching"},
    })

tool_registry.register("list_watched_directories", list_watched_directories,
    description="List all currently watched directories.")
