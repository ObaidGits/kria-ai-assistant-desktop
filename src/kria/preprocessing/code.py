"""
Codebase Preprocessing Module
===============================
Generate AST-based **Skeleton Maps** from source code files using Tree-sitter.
Extracts only class/function definitions, signatures, decorators, docstrings,
and import statements — skips function bodies entirely.

Dramatically reduces token count while preserving the structural overview
needed by the LLM for code understanding tasks.
"""
from __future__ import annotations

import asyncio
import logging
import re
from pathlib import Path
from typing import Optional

logger = logging.getLogger("kria.preprocessing.code")

_TREE_SITTER_AVAILABLE: Optional[bool] = None

# Extension → Tree-sitter language name
_LANG_MAP = {
    ".py": "python",
    ".js": "javascript",
    ".jsx": "javascript",
    ".ts": "typescript",
    ".tsx": "tsx",
    ".java": "java",
    ".c": "c",
    ".h": "c",
    ".cpp": "cpp",
    ".hpp": "cpp",
    ".go": "go",
    ".rs": "rust",
    ".rb": "ruby",
    ".php": "php",
    ".cs": "c_sharp",
    ".swift": "swift",
    ".kt": "kotlin",
    ".scala": "scala",
    ".lua": "lua",
    ".sh": "bash",
    ".bash": "bash",
}


def _check_tree_sitter() -> bool:
    global _TREE_SITTER_AVAILABLE
    if _TREE_SITTER_AVAILABLE is None:
        try:
            import tree_sitter_languages  # noqa: F401
            _TREE_SITTER_AVAILABLE = True
        except ImportError:
            _TREE_SITTER_AVAILABLE = False
            logger.info("tree-sitter-languages not installed — using regex fallback")
    return _TREE_SITTER_AVAILABLE


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------

async def preprocess_code(
    source: str,
    *,
    content: Optional[bytes] = None,
    max_tokens: int = 3500,
) -> "PreprocessedPayload":
    """Generate a skeleton map of a source code file.

    1. Tree-sitter AST parse → extract definitions, imports, docstrings.
    2. Fallback: regex-based extraction.
    3. Smart-crop to *max_tokens*.
    """
    from kria.preprocessing.dispatcher import PreprocessedPayload
    from kria.preprocessing.token_budget import estimate_tokens, smart_crop

    ext = Path(source).suffix.lower()
    lang = _LANG_MAP.get(ext, "")

    def _extract() -> tuple[str, dict]:
        if content:
            code = content.decode("utf-8", errors="replace")
        else:
            code = Path(source).read_text(encoding="utf-8", errors="replace")

        # If the file is small enough, return as-is
        from kria.preprocessing.token_budget import estimate_tokens as _est
        if _est(code) <= max_tokens:
            return code, {"parser": "full", "language": lang or ext, "skeleton": False}

        # Try Tree-sitter
        if lang and _check_tree_sitter():
            try:
                skeleton = _skeleton_tree_sitter(code, lang)
                if skeleton:
                    return skeleton, {"parser": "tree-sitter", "language": lang, "skeleton": True}
            except Exception as exc:
                logger.debug("Tree-sitter failed for %s: %s", source, exc)

        # Regex fallback
        skeleton = _skeleton_regex(code, lang or ext)
        return skeleton, {"parser": "regex", "language": lang or ext, "skeleton": True}

    text, meta = await asyncio.to_thread(_extract)
    meta["path"] = source

    text, truncated = smart_crop(text, max_tokens, strategy="code")
    tokens = estimate_tokens(text)

    return PreprocessedPayload(
        text=text,
        token_estimate=tokens,
        source_type="code",
        metadata=meta,
        truncated=truncated,
    )


# ---------------------------------------------------------------------------
# Tree-sitter skeleton extraction
# ---------------------------------------------------------------------------

# Node types we want to keep across languages
_DEFINITION_TYPES = {
    "python": {
        "import_statement", "import_from_statement",
        "class_definition", "function_definition", "decorated_definition",
    },
    "javascript": {
        "import_statement", "export_statement",
        "class_declaration", "function_declaration", "method_definition",
        "arrow_function", "lexical_declaration",
    },
    "typescript": {
        "import_statement", "export_statement",
        "class_declaration", "function_declaration", "method_definition",
        "interface_declaration", "type_alias_declaration",
        "arrow_function", "lexical_declaration",
    },
    "tsx": {
        "import_statement", "export_statement",
        "class_declaration", "function_declaration", "method_definition",
        "interface_declaration", "type_alias_declaration",
    },
    "java": {
        "import_declaration", "package_declaration",
        "class_declaration", "interface_declaration", "method_declaration",
        "constructor_declaration", "annotation",
    },
    "c": {
        "preproc_include", "preproc_define",
        "function_definition", "declaration", "struct_specifier",
    },
    "cpp": {
        "preproc_include", "preproc_define",
        "function_definition", "class_specifier", "declaration",
        "namespace_definition",
    },
    "go": {
        "import_declaration", "package_clause",
        "function_declaration", "method_declaration", "type_declaration",
    },
    "rust": {
        "use_declaration", "mod_item",
        "function_item", "struct_item", "impl_item", "trait_item", "enum_item",
    },
}


def _skeleton_tree_sitter(code: str, lang: str) -> str:
    """Parse *code* with Tree-sitter and extract a skeleton map."""
    import tree_sitter_languages

    parser = tree_sitter_languages.get_parser(lang)
    tree = parser.parse(code.encode("utf-8"))
    root = tree.root_node

    keep_types = _DEFINITION_TYPES.get(lang, set())
    if not keep_types:
        # Generic: grab anything that looks like a definition
        keep_types = {
            "import_statement", "import_from_statement", "import_declaration",
            "class_definition", "class_declaration",
            "function_definition", "function_declaration", "method_definition",
            "decorated_definition",
        }

    lines: list[str] = []
    _walk_skeleton(root, code, keep_types, lines, lang, depth=0)
    return "\n".join(lines)


def _walk_skeleton(
    node, code: str, keep_types: set, lines: list[str], lang: str, depth: int
) -> None:
    """Recursively walk the AST, emitting definition signatures + docstrings."""
    if node.type in keep_types:
        # Emit the signature (first 1-3 lines) not the full body
        text = code[node.start_byte:node.end_byte]
        sig_lines = _extract_signature(text, lang, node.type)
        for sl in sig_lines:
            lines.append(sl)
        lines.append("")  # blank separator
        return  # don't descend further — we already got what we need

    # For module-level / class bodies, recurse into children
    for child in node.children:
        _walk_skeleton(child, code, keep_types, lines, lang, depth + 1)


def _extract_signature(text: str, lang: str, node_type: str) -> list[str]:
    """Extract the signature portion of a definition, including docstring."""
    all_lines = text.split("\n")
    result: list[str] = []

    if lang == "python":
        # Keep decorator + def/class line + docstring
        in_docstring = False
        docstring_delim = None
        for ln in all_lines:
            stripped = ln.strip()
            if not result:
                # First line: decorator or def/class
                result.append(ln)
                continue
            if len(result) == 1 and stripped.startswith(("def ", "async def ", "class ")):
                # The actual def after a decorator
                result.append(ln)
                continue
            # Check for docstring start
            if not in_docstring and stripped.startswith(('"""', "'''", 'r"""', "r'''")):
                in_docstring = True
                docstring_delim = stripped[:3]
                if stripped[:3] == 'r"""':
                    docstring_delim = '"""'
                elif stripped[:3] == "r'''":
                    docstring_delim = "'''"
                result.append(ln)
                # One-liner docstring
                if stripped.count(docstring_delim) >= 2:
                    in_docstring = False
                continue
            if in_docstring:
                result.append(ln)
                if docstring_delim and docstring_delim in stripped:
                    in_docstring = False
                continue
            # Body line — emit placeholder and stop
            result.append("    ...")
            break
    else:
        # For other languages: keep the opening line(s) up to the first {
        brace_depth = 0
        for ln in all_lines:
            result.append(ln)
            brace_depth += ln.count("{") - ln.count("}")
            if "{" in ln:
                break
            if len(result) >= 3:
                break
        if brace_depth > 0:
            result.append("    // ...")
            result.append("}")

    return result


# ---------------------------------------------------------------------------
# Regex fallback skeleton extraction
# ---------------------------------------------------------------------------

# Patterns that match definition lines across common languages
_REGEX_PATTERNS = [
    # Python
    re.compile(r"^(\s*(?:@\w+.*\n)*\s*(?:async\s+)?(?:def|class)\s+\w+.*?:)", re.MULTILINE),
    # JS/TS
    re.compile(r"^(\s*(?:export\s+)?(?:async\s+)?(?:function|class|interface|type)\s+\w+)", re.MULTILINE),
    # Java/C#/C++
    re.compile(r"^(\s*(?:public|private|protected|static|abstract|virtual|override)?\s*(?:class|interface|struct|enum|void|int|string|bool|float|double|var|auto)\s+\w+)", re.MULTILINE),
    # Go
    re.compile(r"^(func\s+.*?\{?$)", re.MULTILINE),
    re.compile(r"^(type\s+\w+\s+(?:struct|interface))", re.MULTILINE),
    # Rust
    re.compile(r"^(\s*(?:pub\s+)?(?:fn|struct|enum|impl|trait|mod|use)\s+)", re.MULTILINE),
    # Import/include lines (universal)
    re.compile(r"^(\s*(?:import|from|#include|using|require|use)\s+.+)$", re.MULTILINE),
]


def _skeleton_regex(code: str, lang_or_ext: str) -> str:
    """Best-effort skeleton extraction using regex patterns."""
    lines = code.split("\n")
    kept: list[str] = []
    in_docstring = False
    docstring_delim = ""

    for i, line in enumerate(lines):
        stripped = line.strip()

        # Always keep empty lines between definitions (readability)
        if not stripped:
            if kept and kept[-1].strip():
                kept.append("")
            continue

        # Track Python docstrings
        if in_docstring:
            kept.append(line)
            if docstring_delim in stripped:
                in_docstring = False
            continue

        # Check if this line matches any definition pattern
        is_def = False
        for pat in _REGEX_PATTERNS:
            if pat.match(line):
                is_def = True
                break

        if is_def:
            kept.append(line)
            # Check for docstring on next line
            if i + 1 < len(lines):
                next_stripped = lines[i + 1].strip()
                if next_stripped.startswith(('"""', "'''", 'r"""', "r'''")):
                    in_docstring = True
                    docstring_delim = next_stripped[:3].lstrip("r")
                    kept.append(lines[i + 1])
                    if next_stripped.count(docstring_delim) >= 2:
                        in_docstring = False
            continue

        # Also keep decorator lines
        if stripped.startswith("@"):
            kept.append(line)
            continue

    skeleton = "\n".join(kept).strip()
    if skeleton:
        skeleton += "\n\n# ... (function/method bodies omitted — skeleton only)"
    return skeleton or code[:5000]  # if nothing matched, return truncated raw
