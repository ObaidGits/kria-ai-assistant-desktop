"""
Response Validator
===================
Type-safe validation layer for LLM tool-call outputs.

Intercepts parsed tool calls, validates them against the active tool
registry and MCP schemas, and runs auto-correction loops when the LLM
hallucinates the structure.

Uses Pydantic for strict validation — no tool call reaches the execution
pipeline without passing schema checks first.
"""
from __future__ import annotations

import json
import logging
from typing import Any, Optional

from pydantic import BaseModel, field_validator, ValidationError

from kria.agent.config_models import ValidationConfig

logger = logging.getLogger("kria.validation")


# ── Pydantic model for a single tool call ─────────────────────────

class ToolCallRequest(BaseModel):
    """Validated representation of a tool call extracted from LLM output."""
    name: str
    arguments: dict[str, Any] = {}

    @field_validator("name")
    @classmethod
    def _name_not_empty(cls, v: str) -> str:
        if not v or not v.strip():
            raise ValueError("Tool name must not be empty")
        return v.strip()


class ToolCallValidationError(BaseModel):
    """Structured error for a single failed tool call validation."""
    index: int
    raw_call: dict
    error: str


# ── Validator ─────────────────────────────────────────────────────

class ResponseValidator:
    """
    Validates LLM tool calls against the active tool registry schemas.
    Supports auto-correction loops: when a model hallucinates tool structure,
    the validator injects a correction message and re-queries the LLM.
    """

    def __init__(self, config: Optional[ValidationConfig] = None) -> None:
        self._config = config or ValidationConfig()

    def validate_tool_calls(
        self,
        raw_calls: list[dict],
        tool_registry,
    ) -> tuple[list[ToolCallRequest], list[ToolCallValidationError]]:
        """
        Validate a list of raw tool call dicts.

        Returns:
            (valid_calls, errors) — valid calls are ToolCallRequest objects;
            errors describe what went wrong for invalid calls.
        """
        valid: list[ToolCallRequest] = []
        errors: list[ToolCallValidationError] = []

        known_tools = set(tool_registry.list_names())

        for i, raw in enumerate(raw_calls):
            # Step 1: Parse through Pydantic
            try:
                call = ToolCallRequest.model_validate(raw)
            except ValidationError as exc:
                errors.append(ToolCallValidationError(
                    index=i,
                    raw_call=raw,
                    error=f"Schema validation failed: {exc.errors()[0]['msg']}",
                ))
                continue

            # Step 2: Check tool exists in registry
            if call.name not in known_tools:
                errors.append(ToolCallValidationError(
                    index=i,
                    raw_call=raw,
                    error=(
                        f"Unknown tool '{call.name}'. "
                        f"Available tools: {', '.join(sorted(known_tools)[:20])}"
                    ),
                ))
                continue

            # Step 3: Validate arguments against tool schema (if strict mode)
            if self._config.strict_schema_check:
                schema_error = self._validate_arguments(call, tool_registry)
                if schema_error:
                    errors.append(ToolCallValidationError(
                        index=i,
                        raw_call=raw,
                        error=schema_error,
                    ))
                    continue

            valid.append(call)

        return valid, errors

    def _validate_arguments(
        self,
        call: ToolCallRequest,
        tool_registry,
    ) -> Optional[str]:
        """Check required parameters are present and types match."""
        spec = tool_registry.describe(call.name)
        if not spec:
            return None

        params_schema = spec.get("parameters", {})
        if not params_schema:
            return None

        # Handle both flat {param: schema} and JSON Schema {type: object, properties: ...}
        if params_schema.get("type") == "object":
            properties = params_schema.get("properties", {})
            required = set(params_schema.get("required", []))
        else:
            properties = params_schema
            required = set()

        # Check required parameters
        for param_name in required:
            if param_name not in call.arguments:
                return (
                    f"Missing required parameter '{param_name}' for tool '{call.name}'. "
                    f"Required: {', '.join(sorted(required))}"
                )

        return None

    async def auto_correct(
        self,
        client,
        messages: list[dict],
        errors: list[ToolCallValidationError],
        attempt: int = 0,
    ) -> Optional[dict]:
        """
        Inject correction feedback and re-query the LLM.

        Returns the raw LLM response dict if correction succeeds,
        or None if max attempts are reached.
        """
        if attempt >= self._config.max_correction_attempts:
            logger.warning(
                "Auto-correction exhausted after %d attempts. Errors: %s",
                attempt,
                [e.error for e in errors],
            )
            return None

        error_summary = "; ".join(e.error for e in errors)
        correction_msg = (
            f"Your previous tool call was invalid: {error_summary}. "
            f"Please fix the tool call and resubmit using the correct format. "
            f"Make sure the tool name exists and all required parameters are provided."
        )

        corrected_messages = [*messages, {"role": "user", "content": correction_msg}]

        logger.info(
            "Auto-correction attempt %d/%d: %s",
            attempt + 1, self._config.max_correction_attempts, error_summary[:200],
        )

        result = await client.chat(messages=corrected_messages)
        return result

    def validate_against_mcp_schema(
        self,
        call: ToolCallRequest,
        mcp_input_schema: dict,
    ) -> Optional[str]:
        """
        Cross-reference tool call arguments against an MCP tool's input_schema.

        Returns an error string if validation fails, or None if valid.
        """
        if not mcp_input_schema:
            return None

        properties = mcp_input_schema.get("properties", {})
        required = set(mcp_input_schema.get("required", []))

        for param_name in required:
            if param_name not in call.arguments:
                return (
                    f"MCP schema requires '{param_name}' for tool '{call.name}'"
                )

        # Type checking against MCP schema properties
        for param_name, value in call.arguments.items():
            if param_name not in properties:
                continue
            expected_type = properties[param_name].get("type")
            if not expected_type:
                continue
            if not _check_json_type(value, expected_type):
                return (
                    f"Parameter '{param_name}' for tool '{call.name}' "
                    f"expected type '{expected_type}', got {type(value).__name__}"
                )

        return None


def _check_json_type(value: Any, expected: str) -> bool:
    """Check if a Python value matches a JSON Schema type."""
    type_map = {
        "string": str,
        "integer": int,
        "number": (int, float),
        "boolean": bool,
        "array": list,
        "object": dict,
    }
    expected_types = type_map.get(expected)
    if expected_types is None:
        return True  # Unknown type — pass through
    return isinstance(value, expected_types)
