"""
Email Composer (GREEN tier — drafts only, does NOT send)
=========================================================
Create email drafts and open in default email client.
"""
import logging
import urllib.parse
import webbrowser

from kria.infra.isolation import ToolResult, isolated
from kria.tools.registry import tool_registry

logger = logging.getLogger("kria.tools.email_composer")


@isolated
async def compose_email(to: str, subject: str, body: str) -> dict:
    """Draft an email. Does NOT send — returns mailto URI for review."""
    params = urllib.parse.urlencode(
        {"subject": subject, "body": body}, quote_via=urllib.parse.quote
    )
    mailto_uri = f"mailto:{to}?{params}"
    return {
        "mailto_uri": mailto_uri,
        "to": to,
        "subject": subject,
        "body": body,
        "note": "Email draft ready. Use open_email_draft to open in email client.",
    }


@isolated
async def open_email_draft(mailto_uri: str) -> str:
    """Open an email draft in the default email client via mailto: URI."""
    webbrowser.open(mailto_uri)
    return "Email draft opened in default email client"


# ── Register ──────────────────────────────────────────────────────

tool_registry.register("compose_email", compose_email,
    description="Draft an email. Does NOT send — opens in default email client for review.",
    parameters_schema={
        "to": {"type": "string", "description": "Recipient email(s), comma-separated"},
        "subject": {"type": "string", "description": "Email subject"},
        "body": {"type": "string", "description": "Email body text"},
    })

tool_registry.register("open_email_draft", open_email_draft,
    description="Open an email draft in the default email client via mailto: URI.",
    parameters_schema={
        "mailto_uri": {"type": "string", "description": "mailto: URI from compose_email"},
    })
