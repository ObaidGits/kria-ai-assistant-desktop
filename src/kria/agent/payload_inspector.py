"""
Payload Inspector
==================
Inspects incoming message arrays to detect vision content (images).
Used by the ModelRouter to automatically route vision payloads to
the configured vision model.

All detection patterns are config-driven via VisionDetectionConfig —
no hardcoded image extensions or MIME types.
"""
import re
from enum import Enum
from typing import Optional

from kria.agent.config_models import VisionDetectionConfig


class PayloadType(Enum):
    TEXT = "text"
    VISION = "vision"


class PayloadInspector:
    """Stateless inspector that classifies message payloads as TEXT or VISION."""

    def __init__(self, config: Optional[VisionDetectionConfig] = None) -> None:
        self._config = config or VisionDetectionConfig()
        self._path_re = re.compile(self._config.path_pattern, re.IGNORECASE)

    def inspect(self, messages: list[dict]) -> PayloadType:
        """
        Scan the message list for vision data.

        Checks the last user message for:
          1. OpenAI-style content arrays with image_url blocks
          2. Base64-encoded image data URIs in text
          3. File paths with image extensions
        """
        # Find the last user message
        user_msg = None
        for msg in reversed(messages):
            if msg.get("role") == "user":
                user_msg = msg
                break

        if user_msg is None:
            return PayloadType.TEXT

        content = user_msg.get("content")
        if content is None:
            return PayloadType.TEXT

        # Case 1: OpenAI content array (e.g. [{"type": "image_url", ...}, {"type": "text", ...}])
        if isinstance(content, list):
            if self._has_image_url_block(content):
                return PayloadType.VISION
            # Also check text parts for base64 or paths
            for part in content:
                if isinstance(part, dict):
                    text = part.get("text", "")
                    if text and self._has_vision_in_text(text):
                        return PayloadType.VISION
            return PayloadType.TEXT

        # Case 2: Plain string content
        if isinstance(content, str):
            if self._has_vision_in_text(content):
                return PayloadType.VISION

        return PayloadType.TEXT

    def _has_image_url_block(self, content: list) -> bool:
        """Check for image_url type blocks in an OpenAI content array."""
        for part in content:
            if not isinstance(part, dict):
                continue
            if part.get("type") == "image_url":
                return True
            # Also check for base64 in image_url.url
            url = (part.get("image_url") or {}).get("url", "")
            if url and url.startswith(self._config.base64_prefix):
                return True
        return False

    def _has_vision_in_text(self, text: str) -> bool:
        """Check for base64 image URIs or image file paths in text."""
        if self._config.base64_prefix in text:
            return True
        if self._path_re.search(text):
            return True
        return False
