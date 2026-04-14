"""
Macro Recorder
==============
Records sequences of tool calls and replays them on demand.
"""
import json
import logging
from datetime import datetime
from pathlib import Path

from kria.infra.config import settings

logger = logging.getLogger("kria.automation.macro")


class MacroRecorder:
    def __init__(self):
        self._recording = False
        self._current_macro: list[dict] = []
        self._macros_dir = Path(settings.workflows_dir).expanduser() / "macros"
        self._macros_dir.mkdir(parents=True, exist_ok=True)

    def start_recording(self, name: str = "") -> str:
        self._recording = True
        self._current_macro = []
        self._current_name = name or f"macro_{datetime.now().strftime('%Y%m%d_%H%M%S')}"
        logger.info("Recording macro: %s", self._current_name)
        return self._current_name

    def record_step(self, tool_name: str, params: dict):
        if self._recording:
            self._current_macro.append({"tool": tool_name, "params": params})

    def stop_recording(self) -> dict:
        self._recording = False
        name = getattr(self, "_current_name", "unnamed")
        path = self._macros_dir / f"{name}.json"
        path.write_text(json.dumps(self._current_macro, indent=2))
        steps = len(self._current_macro)
        self._current_macro = []
        logger.info("Macro saved: %s (%d steps)", name, steps)
        return {"name": name, "steps": steps, "path": str(path)}

    @property
    def is_recording(self) -> bool:
        return self._recording

    def list_macros(self) -> list[dict]:
        macros = []
        for f in self._macros_dir.glob("*.json"):
            try:
                data = json.loads(f.read_text())
                macros.append({"name": f.stem, "steps": len(data)})
            except Exception:
                continue
        return macros

    async def replay(self, name: str) -> dict:
        from kria.tools.registry import tool_registry

        path = self._macros_dir / f"{name}.json"
        if not path.exists():
            return {"error": f"Macro '{name}' not found"}

        steps = json.loads(path.read_text())
        results = []
        for step in steps:
            result = await tool_registry.execute(step["tool"], step["params"])
            results.append({
                "tool": step["tool"],
                "success": result.success,
                "result": result.data if result.success else result.error,
            })
        return {"macro": name, "steps_executed": len(results), "results": results}

    def delete_macro(self, name: str) -> bool:
        path = self._macros_dir / f"{name}.json"
        if path.exists():
            path.unlink()
            return True
        return False


macro_recorder = MacroRecorder()
