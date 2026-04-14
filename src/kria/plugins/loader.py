"""
Plugin Loader
==============
Discover, load, and unload plugins from the plugins directory.
Each plugin is a subdirectory with a plugin.yml manifest and __init__.py.
"""
import importlib.util
import logging
from pathlib import Path

import yaml

from kria.infra.config import settings

logger = logging.getLogger("kria.plugins.loader")


class PluginLoader:
    def __init__(self):
        self._plugins_dir = Path(settings.plugins_dir).expanduser()
        self._plugins_dir.mkdir(parents=True, exist_ok=True)
        self._loaded: dict[str, dict] = {}

    def discover(self) -> list[dict]:
        """Find all plugins in the plugins directory."""
        plugins = []
        for d in self._plugins_dir.iterdir():
            if d.is_dir():
                manifest = d / "plugin.yml"
                if manifest.exists():
                    try:
                        data = yaml.safe_load(manifest.read_text())
                        data["path"] = str(d)
                        data["enabled"] = data.get("enabled", True)
                        plugins.append(data)
                    except Exception as e:
                        logger.warning("Bad plugin manifest %s: %s", d.name, e)
        return plugins

    def load(self, plugin_name: str) -> bool:
        """Load a plugin by name."""
        plugin_dir = self._plugins_dir / plugin_name
        manifest = plugin_dir / "plugin.yml"
        init_file = plugin_dir / "__init__.py"

        if not manifest.exists() or not init_file.exists():
            logger.error("Plugin '%s' missing manifest or __init__.py", plugin_name)
            return False

        try:
            spec = importlib.util.spec_from_file_location(
                f"kria_plugin_{plugin_name}", str(init_file)
            )
            if spec is None or spec.loader is None:
                logger.error("Plugin '%s': could not create module spec", plugin_name)
                return False

            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)

            if hasattr(module, "setup"):
                module.setup()

            self._loaded[plugin_name] = {
                "module": module,
                "manifest": yaml.safe_load(manifest.read_text()),
            }
            logger.info("Plugin loaded: %s", plugin_name)
            return True
        except Exception as e:
            logger.error("Plugin load failed '%s': %s", plugin_name, e)
            return False

    def unload(self, plugin_name: str) -> bool:
        plugin = self._loaded.pop(plugin_name, None)
        if plugin and hasattr(plugin["module"], "teardown"):
            try:
                plugin["module"].teardown()
            except Exception as e:
                logger.error("Plugin teardown error '%s': %s", plugin_name, e)
        return plugin is not None

    def list_loaded(self) -> list[str]:
        return list(self._loaded.keys())

    def get_manifest(self, plugin_name: str) -> dict | None:
        plugin = self._loaded.get(plugin_name)
        return plugin["manifest"] if plugin else None


plugin_loader = PluginLoader()
