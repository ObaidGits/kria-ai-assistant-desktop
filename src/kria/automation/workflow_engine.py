"""
Workflow Engine
===============
YAML-based workflow parser and executor.
Workflows are stored in ~/.kria/workflows/ and consist of sequential tool calls
with variable substitution and simple conditional logic.
"""
import re
import logging
from pathlib import Path

import yaml

from kria.infra.config import settings

logger = logging.getLogger("kria.automation.workflow")


class WorkflowEngine:
    def __init__(self):
        self._workflows_dir = Path(settings.workflows_dir).expanduser()
        self._workflows_dir.mkdir(parents=True, exist_ok=True)

    def list_workflows(self) -> list[dict]:
        workflows = []
        for f in self._workflows_dir.glob("*.yml"):
            try:
                data = yaml.safe_load(f.read_text())
                workflows.append({
                    "file": f.name,
                    "name": data.get("name", f.stem),
                    "trigger": data.get("trigger", {}),
                    "steps": len(data.get("steps", [])),
                })
            except Exception as e:
                logger.warning("Bad workflow %s: %s", f.name, e)
        return workflows

    async def run_workflow(self, name: str, variables: dict | None = None) -> dict:
        from kria.tools.registry import tool_registry

        wf_path = self._workflows_dir / f"{name}.yml"
        if not wf_path.exists():
            return {"error": f"Workflow '{name}' not found"}

        wf = yaml.safe_load(wf_path.read_text())
        variables = variables or {}
        results = []

        for step in wf.get("steps", []):
            step_name = step.get("name", "unnamed")

            condition = step.get("condition")
            if condition and not self._eval_condition(condition, variables):
                results.append({"step": step_name, "skipped": True, "reason": "Condition not met"})
                continue

            params = self._resolve_vars(step.get("params", {}), variables)
            tool_name = step["tool"]

            result = await tool_registry.execute(tool_name, params)
            results.append({
                "step": step_name,
                "tool": tool_name,
                "result": result.data if result.success else result.error,
            })

            save_as = step.get("save_as")
            if save_as and result.success:
                variables[save_as] = result.data

        return {"workflow": name, "steps_executed": len(results), "results": results}

    def save_workflow(self, name: str, content: dict) -> str:
        path = self._workflows_dir / f"{name}.yml"
        path.write_text(yaml.dump(content, default_flow_style=False))
        return str(path)

    def delete_workflow(self, name: str) -> bool:
        path = self._workflows_dir / f"{name}.yml"
        if path.exists():
            path.unlink()
            return True
        return False

    def _resolve_vars(self, params: dict, variables: dict) -> dict:
        resolved = {}
        for k, v in params.items():
            if isinstance(v, str):
                def replace_var(match, _vars=variables):
                    var_path = match.group(1)
                    parts = var_path.split(".")
                    val = _vars
                    for p in parts:
                        if isinstance(val, dict):
                            val = val.get(p, match.group(0))
                        else:
                            return match.group(0)
                    return str(val)
                resolved[k] = re.sub(r"\{\{(\w+(?:\.\w+)*)\}\}", replace_var, v)
            else:
                resolved[k] = v
        return resolved

    def _eval_condition(self, condition: str, variables: dict) -> bool:
        resolved = re.sub(
            r"\{\{(\w+(?:\.\w+)*)\}\}",
            lambda m: str(self._get_nested(variables, m.group(1))),
            condition,
        )
        try:
            # Only allow simple comparisons, not arbitrary code
            allowed = set("0123456789.+-*/<>= !andornotTrue False")
            if all(c in allowed or c.isspace() for c in resolved):
                return bool(eval(resolved))  # noqa: S307
            return False
        except Exception:
            return False

    def _get_nested(self, data: dict, path: str):
        parts = path.split(".")
        val = data
        for p in parts:
            if isinstance(val, dict):
                val = val.get(p)
            else:
                return None
        return val


workflow_engine = WorkflowEngine()
