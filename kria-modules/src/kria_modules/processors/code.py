"""
Code processor — Pre-Cognitive source-code analysis.

Parses source files with tree-sitter to extract AST structure:
functions, classes, imports, metrics. Tier-aware depth.
"""

import logging
import os
from pathlib import Path
from typing import Any

logger = logging.getLogger("kria.processors.code")

METHODS = ["analyze"]

# Extension → tree-sitter language name
_LANG_MAP = {
    ".py": "python",
    ".js": "javascript",
    ".ts": "typescript",
    ".jsx": "javascript",
    ".tsx": "typescript",
    ".rs": "rust",
    ".go": "go",
    ".c": "c",
    ".h": "c",
    ".cpp": "cpp",
    ".hpp": "cpp",
    ".java": "java",
    ".rb": "ruby",
    ".cs": "c_sharp",
}

# Tier → max file size for full AST parse
_MAX_SIZE = {"lite": 32_000, "standard": 128_000, "performance": 512_000, "high": 2_000_000}


def analyze(params: dict) -> dict:
    """
    Analyze a source code file.

    Params:
        file_path: str — path to source file
        language: str — override language detection (optional)
    """
    file_path = params.get("file_path", "")
    if not file_path or not os.path.isfile(file_path):
        raise FileNotFoundError(file_path)

    tier = params.get("_tier", "standard")
    max_size = _MAX_SIZE.get(tier, 128_000)
    ext = Path(file_path).suffix.lower()
    lang = params.get("language") or _LANG_MAP.get(ext)

    file_size = os.path.getsize(file_path)
    with open(file_path, "r", encoding="utf-8", errors="replace") as f:
        source = f.read(max_size)

    result: dict[str, Any] = {
        "file_path": file_path,
        "language": lang or ext.lstrip("."),
        "size_kb": round(file_size / 1024, 1),
        "line_count": source.count("\n") + 1,
    }

    # Try tree-sitter AST parsing
    if lang:
        try:
            ast_info = _tree_sitter_parse(source, lang, tier)
            result.update(ast_info)
        except Exception as e:
            logger.warning("tree-sitter parse failed for %s: %s", lang, e)
            result.update(_fallback_parse(source, lang))
    else:
        result.update(_fallback_parse(source, ext.lstrip(".")))

    # Metrics
    lines = source.split("\n")
    blank = sum(1 for l in lines if not l.strip())
    comment = sum(1 for l in lines if l.strip().startswith(("#", "//", "/*", "*", "--")))
    result["metrics"] = {
        "total_lines": len(lines),
        "blank_lines": blank,
        "comment_lines": comment,
        "code_lines": len(lines) - blank - comment,
    }

    # Summary
    funcs = result.get("functions", [])
    classes = result.get("classes", [])
    imports = result.get("imports", [])
    result["summary"] = (
        f"{result['language']} | {result['line_count']} lines | "
        f"{len(funcs)} functions, {len(classes)} classes, {len(imports)} imports"
    )

    return result


def _tree_sitter_parse(source: str, lang: str, tier: str) -> dict:
    import tree_sitter

    try:
        language = tree_sitter.Language(f"tree-sitter-{lang}")
    except Exception:
        # Try alternate import path
        try:
            import importlib
            mod = importlib.import_module(f"tree_sitter_{lang}")
            language = mod.language()
        except Exception:
            return _fallback_parse(source, lang)

    parser = tree_sitter.Parser(language)
    tree = parser.parse(source.encode("utf-8"))

    functions = []
    classes = []
    imports = []

    def visit(node: Any, depth: int = 0) -> None:
        ntype = node.type

        if ntype in ("function_definition", "function_declaration", "method_definition",
                      "function_item", "fn_item"):
            name_node = node.child_by_field_name("name")
            name = name_node.text.decode("utf-8") if name_node else "<anonymous>"
            entry = {
                "name": name,
                "line": node.start_point[0] + 1,
                "end_line": node.end_point[0] + 1,
            }
            if tier in ("performance", "high"):
                params_node = node.child_by_field_name("parameters")
                if params_node:
                    entry["params"] = params_node.text.decode("utf-8")
                ret_node = node.child_by_field_name("return_type")
                if ret_node:
                    entry["return_type"] = ret_node.text.decode("utf-8")
            functions.append(entry)

        elif ntype in ("class_definition", "class_declaration", "struct_item",
                        "impl_item"):
            name_node = node.child_by_field_name("name")
            name = name_node.text.decode("utf-8") if name_node else "<anonymous>"
            classes.append({
                "name": name,
                "line": node.start_point[0] + 1,
                "end_line": node.end_point[0] + 1,
            })

        elif ntype in ("import_statement", "import_from_statement", "use_declaration",
                        "import_declaration"):
            imports.append({
                "text": node.text.decode("utf-8").strip(),
                "line": node.start_point[0] + 1,
            })

        for child in node.children:
            visit(child, depth + 1)

    visit(tree.root_node)

    return {"functions": functions, "classes": classes, "imports": imports}


def _fallback_parse(source: str, lang: str) -> dict:
    """Regex-based fallback when tree-sitter is unavailable."""
    import re

    functions = []
    classes = []
    imports = []

    for i, line in enumerate(source.split("\n"), 1):
        stripped = line.strip()

        # Imports
        if stripped.startswith(("import ", "from ", "use ", "require(", "#include")):
            imports.append({"text": stripped, "line": i})

        # Functions
        if lang == "python" and stripped.startswith("def "):
            m = re.match(r"def\s+(\w+)", stripped)
            if m:
                functions.append({"name": m.group(1), "line": i})
        elif lang in ("javascript", "typescript") and re.match(r"(export\s+)?(async\s+)?function\s+\w+", stripped):
            m = re.search(r"function\s+(\w+)", stripped)
            if m:
                functions.append({"name": m.group(1), "line": i})
        elif lang == "rust" and re.match(r"(pub\s+)?(async\s+)?fn\s+\w+", stripped):
            m = re.search(r"fn\s+(\w+)", stripped)
            if m:
                functions.append({"name": m.group(1), "line": i})

        # Classes
        if stripped.startswith("class "):
            m = re.match(r"class\s+(\w+)", stripped)
            if m:
                classes.append({"name": m.group(1), "line": i})
        elif lang == "rust" and re.match(r"(pub\s+)?struct\s+\w+", stripped):
            m = re.search(r"struct\s+(\w+)", stripped)
            if m:
                classes.append({"name": m.group(1), "line": i})

    return {"functions": functions, "classes": classes, "imports": imports}
