"""
Tests for the Local Multimodal Preprocessing Pipeline.
"""
import asyncio
import io
import os
import textwrap

import pytest


# ===========================================================================
# Token Budget
# ===========================================================================

class TestTokenBudget:
    """Tests for token_budget.py — estimation, smart-crop, chunking."""

    def test_estimate_tokens_empty(self):
        from kria.preprocessing.token_budget import estimate_tokens
        assert estimate_tokens("") == 0

    def test_estimate_tokens_short(self):
        from kria.preprocessing.token_budget import estimate_tokens
        tokens = estimate_tokens("Hello, world!")
        assert 1 <= tokens <= 10

    def test_estimate_tokens_long_text(self):
        from kria.preprocessing.token_budget import estimate_tokens
        text = "word " * 1000  # ~1000 words ≈ ~1000-1500 tokens
        tokens = estimate_tokens(text)
        assert 800 <= tokens <= 2000

    def test_estimate_image_tokens(self):
        from kria.preprocessing.token_budget import estimate_image_tokens
        # 1280x720 → ceil(720/28) * ceil(1280/28) / 4
        tokens = estimate_image_tokens(1280, 720)
        assert tokens > 0
        # Smaller image = fewer tokens
        small = estimate_image_tokens(224, 224)
        assert small < tokens

    def test_smart_crop_no_truncation(self):
        from kria.preprocessing.token_budget import smart_crop
        text = "Short text."
        result, truncated = smart_crop(text, max_tokens=3500)
        assert result == text
        assert truncated is False

    def test_smart_crop_truncates_long_text(self):
        from kria.preprocessing.token_budget import estimate_tokens, smart_crop
        text = "This is a sentence. " * 500  # ~2000+ tokens
        result, truncated = smart_crop(text, max_tokens=500)
        assert truncated is True
        assert estimate_tokens(result) <= 550  # some margin for the "[... truncated]" marker

    def test_smart_crop_markdown_strategy(self):
        from kria.preprocessing.token_budget import smart_crop
        md = "# Title\n\nParagraph 1.\n\n## Section A\n\nLong text. " * 200
        result, truncated = smart_crop(md, max_tokens=500, strategy="markdown")
        assert truncated is True
        assert "# Title" in result or "## Section" in result

    def test_smart_crop_code_strategy(self):
        from kria.preprocessing.token_budget import smart_crop
        code = textwrap.dedent("""\
        import os
        import sys

        def foo(x, y):
            \"\"\"Add two numbers.\"\"\"
            result = x + y
            return result

        class Bar:
            \"\"\"A bar class.\"\"\"
            def method(self):
                pass
        """ * 100)
        result, truncated = smart_crop(code, max_tokens=200, strategy="code")
        assert truncated is True
        assert "import os" in result or "def foo" in result

    def test_chunk_text_single_chunk(self):
        from kria.preprocessing.token_budget import chunk_text
        text = "Short text."
        chunks = chunk_text(text, chunk_tokens=3500)
        assert len(chunks) == 1
        assert chunks[0] == text

    def test_chunk_text_multiple_chunks(self):
        from kria.preprocessing.token_budget import chunk_text
        text = "This is a sentence. " * 500
        chunks = chunk_text(text, chunk_tokens=200)
        assert len(chunks) >= 2


# ===========================================================================
# Dispatcher
# ===========================================================================

class TestDispatcher:
    """Tests for dispatcher.py — source type detection and routing."""

    def test_detect_image(self):
        from kria.preprocessing.dispatcher import detect_source_type
        assert detect_source_type(path="photo.jpg") == "image"
        assert detect_source_type(path="file.PNG") == "image"
        assert detect_source_type(path="img.webp") == "image"

    def test_detect_document(self):
        from kria.preprocessing.dispatcher import detect_source_type
        assert detect_source_type(path="report.pdf") == "document"
        assert detect_source_type(path="file.docx") == "document"
        assert detect_source_type(path="data.xlsx") == "document"
        assert detect_source_type(path="data.csv") == "document"

    def test_detect_video(self):
        from kria.preprocessing.dispatcher import detect_source_type
        assert detect_source_type(path="clip.mp4") == "video"
        assert detect_source_type(path="movie.mkv") == "video"

    def test_detect_audio(self):
        from kria.preprocessing.dispatcher import detect_source_type
        assert detect_source_type(path="song.mp3") == "audio"
        assert detect_source_type(path="rec.wav") == "audio"

    def test_detect_code(self):
        from kria.preprocessing.dispatcher import detect_source_type
        assert detect_source_type(path="main.py") == "code"
        assert detect_source_type(path="app.ts") == "code"
        assert detect_source_type(path="lib.rs") == "code"

    def test_detect_web(self):
        from kria.preprocessing.dispatcher import detect_source_type
        assert detect_source_type(url="https://example.com") == "web"
        assert detect_source_type(url="http://test.org/page") == "web"

    def test_detect_text(self):
        from kria.preprocessing.dispatcher import detect_source_type
        assert detect_source_type(path="readme.txt") == "text"
        assert detect_source_type(path="notes.md") == "text"
        assert detect_source_type(path="config.yaml") == "text"

    def test_detect_unknown(self):
        from kria.preprocessing.dispatcher import detect_source_type
        assert detect_source_type(path="file.xyz123") == "unknown"


# ===========================================================================
# Image Module
# ===========================================================================

class TestImagePreprocessing:
    """Tests for image.py — resize, EXIF, compression."""

    @pytest.mark.asyncio
    async def test_small_image_passthrough(self):
        """Image smaller than max_edge is still processed (JPEG + EXIF)."""
        from PIL import Image

        from kria.preprocessing.image import preprocess_image

        img = Image.new("RGB", (200, 100), color="red")
        buf = io.BytesIO()
        img.save(buf, format="PNG")
        img_bytes = buf.getvalue()

        payload = await preprocess_image("test.png", content=img_bytes)
        assert payload.source_type == "image"
        assert len(payload.images) == 1
        assert payload.metadata["processed_size"] == [200, 100]
        assert payload.truncated is False

    @pytest.mark.asyncio
    async def test_large_image_resized(self):
        """Image larger than max_edge is resized down."""
        from PIL import Image

        from kria.preprocessing.image import preprocess_image

        img = Image.new("RGB", (4000, 3000), color="blue")
        buf = io.BytesIO()
        img.save(buf, format="PNG")
        img_bytes = buf.getvalue()

        payload = await preprocess_image("big.png", content=img_bytes, max_edge=1280)
        assert payload.source_type == "image"
        w, h = payload.metadata["processed_size"]
        assert max(w, h) <= 1280
        assert payload.truncated is True

    @pytest.mark.asyncio
    async def test_grayscale_conversion(self):
        """Grayscale flag converts to single-channel."""
        from PIL import Image

        from kria.preprocessing.image import preprocess_image

        img = Image.new("RGB", (100, 100), color="green")
        buf = io.BytesIO()
        img.save(buf, format="PNG")

        payload = await preprocess_image("test.png", content=buf.getvalue(), grayscale=True)
        assert payload.metadata["grayscale"] is True

    @pytest.mark.asyncio
    async def test_token_estimate_positive(self):
        """Visual token estimate is positive for any image."""
        from PIL import Image

        from kria.preprocessing.image import preprocess_image

        img = Image.new("RGB", (640, 480))
        buf = io.BytesIO()
        img.save(buf, format="PNG")

        payload = await preprocess_image("test.png", content=buf.getvalue())
        assert payload.token_estimate > 0


# ===========================================================================
# Code Module
# ===========================================================================

class TestCodePreprocessing:
    """Tests for code.py — skeleton map extraction."""

    @pytest.mark.asyncio
    async def test_small_file_returned_fully(self):
        """Code file smaller than token budget is returned as-is."""
        from kria.preprocessing.code import preprocess_code

        code = "def hello():\n    print('hi')\n"
        payload = await preprocess_code("test.py", content=code.encode())
        assert payload.source_type == "code"
        assert "def hello" in payload.text
        assert payload.metadata.get("skeleton") is False  # full text, not skeletonized

    @pytest.mark.asyncio
    async def test_large_file_skeletonized(self):
        """Large code file is reduced to a skeleton."""
        from kria.preprocessing.code import preprocess_code

        code = textwrap.dedent("""\
        import os
        import sys

        def function_a(x):
            \"\"\"Docstring for a.\"\"\"
            result = x * 2
            for i in range(100):
                result += i
            return result

        class MyClass:
            \"\"\"A class.\"\"\"
            def method_b(self, y):
                \"\"\"Method b doc.\"\"\"
                return y + 1

        """) * 50  # Repeat to exceed token budget

        payload = await preprocess_code("big.py", content=code.encode(), max_tokens=300)
        assert payload.source_type == "code"
        assert payload.metadata.get("skeleton") is True
        # Should contain definitions but not full bodies
        assert "import os" in payload.text or "def function_a" in payload.text

    @pytest.mark.asyncio
    async def test_regex_fallback_for_unknown_lang(self):
        """Files in unsupported languages still get regex-based skeletons."""
        from kria.preprocessing.code import preprocess_code

        code = textwrap.dedent("""\
        import Foundation

        func greet(name: String) {
            print("Hello, \\(name)")
        }

        class Person {
            var name: String
            init(name: String) {
                self.name = name
            }
        }
        """) * 50

        payload = await preprocess_code("test.swift", content=code.encode(), max_tokens=200)
        assert payload.source_type == "code"


# ===========================================================================
# Token Budget Enforcement (Integration)
# ===========================================================================

class TestBudgetEnforcement:
    """Verify that all modules respect the token budget."""

    @pytest.mark.asyncio
    async def test_text_preprocess_within_budget(self):
        """Plain text preprocessing stays within token budget."""
        from kria.preprocessing import preprocess
        from kria.preprocessing.token_budget import estimate_tokens

        text = ("This is a long paragraph of text that goes on and on. " * 200).encode()
        payload = await preprocess("test.txt", content=text, max_tokens=500)
        assert estimate_tokens(payload.text) <= 550  # small margin

    @pytest.mark.asyncio
    async def test_code_preprocess_within_budget(self):
        """Code preprocessing stays within token budget."""
        from kria.preprocessing import preprocess
        from kria.preprocessing.token_budget import estimate_tokens

        code = ("def func_{i}(x):\n    return x * {i}\n\n".format(i=i) for i in range(200))
        content = "\n".join(code).encode()
        payload = await preprocess("big.py", content=content, max_tokens=500)
        assert estimate_tokens(payload.text) <= 550


# ===========================================================================
# Web Module (unit-level, no network)
# ===========================================================================

class TestWebPreprocessing:
    """Tests for web.py — HTML extraction logic."""

    def test_strip_html_tags(self):
        from kria.preprocessing.web import _strip_html_tags
        html = "<html><head><title>Test</title></head><body><p>Hello <b>world</b></p><script>alert(1)</script></body></html>"
        text = _strip_html_tags(html)
        assert "Hello" in text
        assert "world" in text
        assert "<script>" not in text
        assert "alert" not in text
