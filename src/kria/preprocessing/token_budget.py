"""
Token Budget Estimator & Smart-Crop
====================================
Estimate token counts and enforce a strict budget (default 3500) to stay
within the 4096-token context window used by the local Qwen2.5-VL model.

Strategies:
 1. ``estimate_tokens`` — fast heuristic OR tiktoken when available.
 2. ``smart_crop``      — truncate to budget while preserving semantic boundaries.
 3. ``chunk_text``      — split into budget-sized chunks for multi-pass.
"""
from __future__ import annotations

import logging
import re
from typing import Optional

logger = logging.getLogger("kria.preprocessing.token_budget")

# ---------------------------------------------------------------------------
# Token estimation
# ---------------------------------------------------------------------------

_tokenizer = None
_CHARS_PER_TOKEN = 4  # conservative heuristic


def _get_tokenizer():
    """Lazy-load tiktoken encoder (cl100k_base, used by most modern models)."""
    global _tokenizer
    if _tokenizer is not None:
        return _tokenizer
    try:
        import tiktoken
        _tokenizer = tiktoken.get_encoding("cl100k_base")
    except ImportError:
        _tokenizer = False  # sentinel: not available
    return _tokenizer


def estimate_tokens(text: str) -> int:
    """Return an estimated token count for *text*.

    Uses tiktoken (cl100k_base) when available; falls back to a ~4 chars/token
    heuristic which slightly over-estimates (safe for budget enforcement).
    """
    if not text:
        return 0
    enc = _get_tokenizer()
    if enc:
        return len(enc.encode(text, disallowed_special=()))
    return max(1, len(text) // _CHARS_PER_TOKEN)


def estimate_image_tokens(width: int, height: int) -> int:
    """Estimate visual tokens for Qwen2.5-VL.

    Formula (from Qwen2-VL docs): ceil(h/28) * ceil(w/28) / 4
    """
    import math
    return max(1, math.ceil(height / 28) * math.ceil(width / 28) // 4)


# ---------------------------------------------------------------------------
# Smart-crop strategies
# ---------------------------------------------------------------------------

_SENTENCE_RE = re.compile(r"(?<=[.!?])\s+")
_HEADING_RE = re.compile(r"^#{1,6}\s", re.MULTILINE)


def smart_crop(
    text: str,
    max_tokens: int = 3500,
    *,
    strategy: Optional[str] = None,
) -> tuple[str, bool]:
    """Truncate *text* to fit within *max_tokens*, preserving semantics.

    Returns ``(cropped_text, was_truncated)``.

    Strategy selection (auto if *strategy* is None):
      - **markdown** — heading-aware: keep headings + first paragraphs.
      - **prose**    — sentence-boundary truncation.
      - **code**     — keep top definitions + tail summary.
    """
    current = estimate_tokens(text)
    if current <= max_tokens:
        return text, False

    if strategy is None:
        strategy = _detect_strategy(text)

    if strategy == "markdown":
        return _crop_markdown(text, max_tokens), True
    if strategy == "code":
        return _crop_code(text, max_tokens), True
    return _crop_prose(text, max_tokens), True


def _detect_strategy(text: str) -> str:
    """Heuristic: pick the best truncation strategy for this text."""
    lines = text[:2000].split("\n")
    heading_count = sum(1 for ln in lines if _HEADING_RE.match(ln))
    code_indicators = sum(
        1 for ln in lines
        if ln.strip().startswith(("def ", "class ", "function ", "import ", "from "))
    )
    if heading_count >= 3:
        return "markdown"
    if code_indicators >= 3:
        return "code"
    return "prose"


def _crop_prose(text: str, max_tokens: int) -> str:
    """Truncate at the last complete sentence that fits."""
    sentences = _SENTENCE_RE.split(text)
    result: list[str] = []
    used = 0
    for s in sentences:
        cost = estimate_tokens(s)
        if used + cost > max_tokens - 20:  # leave room for ellipsis marker
            break
        result.append(s)
        used += cost
    if not result:
        # Fallback: hard character cut
        chars = max_tokens * _CHARS_PER_TOKEN
        return text[:chars] + "\n\n[... truncated]"
    return " ".join(result) + "\n\n[... truncated]"


def _crop_markdown(text: str, max_tokens: int) -> str:
    """Keep headings and the first paragraph under each heading."""
    sections = re.split(r"(^#{1,6}\s.*$)", text, flags=re.MULTILINE)
    result: list[str] = []
    used = 0
    budget = max_tokens - 20

    for part in sections:
        cost = estimate_tokens(part)
        if _HEADING_RE.match(part):
            # Always try to include headings (they're cheap)
            if used + cost <= budget:
                result.append(part)
                used += cost
            continue
        # Body text — include up to the budget
        if used + cost <= budget:
            result.append(part)
            used += cost
        else:
            # Partial inclusion of this section
            remaining = budget - used
            if remaining > 50:
                partial = _crop_prose(part, remaining)
                result.append(partial)
            break

    if not result:
        return _crop_prose(text, max_tokens)
    return "\n".join(result) + "\n\n[... truncated]"


def _crop_code(text: str, max_tokens: int) -> str:
    """Keep imports, class/function signatures, and docstrings; trim bodies."""
    lines = text.split("\n")
    # Collect definition lines + docstrings
    keep: list[str] = []
    used = 0
    budget = max_tokens - 30
    i = 0
    while i < len(lines):
        line = lines[i]
        stripped = line.strip()
        is_def = stripped.startswith(("def ", "class ", "async def ", "import ", "from "))
        is_decorator = stripped.startswith("@")
        is_docstring_start = stripped.startswith(('"""', "'''", 'r"""', "r'''"))

        if is_def or is_decorator or is_docstring_start or not stripped:
            cost = estimate_tokens(line)
            if used + cost <= budget:
                keep.append(line)
                used += cost
                # If docstring, include full docstring block
                if is_docstring_start and not (stripped.endswith('"""') and len(stripped) > 3):
                    i += 1
                    while i < len(lines):
                        dline = lines[i]
                        dcost = estimate_tokens(dline)
                        if used + dcost > budget:
                            break
                        keep.append(dline)
                        used += dcost
                        if '"""' in dline or "'''" in dline:
                            break
                        i += 1
            else:
                break
        i += 1

    if not keep:
        return _crop_prose(text, max_tokens)
    return "\n".join(keep) + "\n\n# ... (body code omitted, skeleton only)"


# ---------------------------------------------------------------------------
# Chunking for multi-pass
# ---------------------------------------------------------------------------

def chunk_text(text: str, chunk_tokens: int = 3500, overlap_tokens: int = 200) -> list[str]:
    """Split *text* into overlapping chunks of approximately *chunk_tokens*.

    Uses sentence boundaries where possible to avoid splitting mid-sentence.
    """
    if estimate_tokens(text) <= chunk_tokens:
        return [text]

    sentences = _SENTENCE_RE.split(text)
    chunks: list[str] = []
    current: list[str] = []
    current_tokens = 0

    for s in sentences:
        cost = estimate_tokens(s)
        if current_tokens + cost > chunk_tokens and current:
            chunks.append(" ".join(current))
            # Overlap: keep last few sentences
            overlap: list[str] = []
            overlap_used = 0
            for prev in reversed(current):
                pc = estimate_tokens(prev)
                if overlap_used + pc > overlap_tokens:
                    break
                overlap.insert(0, prev)
                overlap_used += pc
            current = overlap
            current_tokens = overlap_used
        current.append(s)
        current_tokens += cost

    if current:
        chunks.append(" ".join(current))

    return chunks
