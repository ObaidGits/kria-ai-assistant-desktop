"""
Interaction Tools
=================
Provides the ``ask_user`` tool that lets the LLM present
multi-choice questions to the human operator.
"""
from kria.agent.interaction import interaction_gateway
from kria.infra.isolation import isolated
from kria.tools.registry import tool_registry


@isolated
async def ask_user(
    question: str,
    options: list[str],
    recommended: int = 0,
    context: str = "",
) -> dict:
    """Present a question with numbered options to the user and wait for their choice."""
    return await interaction_gateway.ask_user(
        question=question,
        options=options,
        recommended=recommended,
        context=context,
    )


tool_registry.register(
    name="ask_user",
    func=ask_user,
    description=(
        "Present a multi-choice question to the user and wait for their response. "
        "Use when you need the user to choose between distinct alternatives "
        "(e.g. which application, which file, which approach). "
        "Returns the chosen option text and index."
    ),
    parameters_schema={
        "type": "object",
        "properties": {
            "question": {
                "type": "string",
                "description": "Clear, concise question to present to the user.",
            },
            "options": {
                "type": "array",
                "items": {"type": "string"},
                "description": "List of 2-5 distinct choices for the user.",
            },
            "recommended": {
                "type": "integer",
                "description": "0-based index of the recommended option (auto-selected on timeout).",
            },
            "context": {
                "type": "string",
                "description": "Optional brief context about why this question matters.",
            },
        },
        "required": ["question", "options"],
    },
)
