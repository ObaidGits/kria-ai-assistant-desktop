"""
Multi-Step Planner
==================
For AGENT_LOOP requests, the planner decomposes the user's goal into a
numbered sequence of concrete steps before execution.

This gives the ReAct loop a clearer path to follow and reduces the chance
of the LLM losing track of the overall goal in long tool chains.

Resilience: if the LLM call fails or returns unparseable JSON, the planner
returns an empty list and the ReAct loop proceeds without an explicit plan.
"""
import json
import logging
import re

from kria.agent.llm_client import llm_client

logger = logging.getLogger("kria.planner")

_SYSTEM = """\
Given the user's request, decompose it into a numbered action plan.
Return a JSON array of objects, each with:
  "step":        integer (1-based)
  "action":      short verb phrase describing what to do
  "description": one sentence with specific details

Keep the plan to 10 steps or fewer.
Return ONLY the JSON array — no markdown fences, no explanations.\
"""


class Planner:
    async def create_plan(self, user_request: str) -> list[dict]:
        try:
            result = await llm_client.chat(
                messages=[
                    {"role": "system", "content": _SYSTEM},
                    {"role": "user", "content": user_request},
                ],
                temperature=0.2,
                max_tokens=1024,
            )
            if not result:
                return []

            content = result["choices"][0]["message"]["content"].strip()
            # Strip potential markdown fences
            content = re.sub(r"^```(?:json)?\s*", "", content)
            content = re.sub(r"\s*```$", "", content)
            return json.loads(content)
        except Exception as exc:
            logger.warning("Planner failed — proceeding without plan: %s", exc)
            return []


planner = Planner()
