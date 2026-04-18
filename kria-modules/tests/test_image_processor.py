from pathlib import Path

import pytest

pytest.importorskip("PIL")
pytest.importorskip("cv2")

from PIL import Image, ImageDraw

from kria_modules.processors import image as image_processor


def _make_test_image(path: Path, width: int = 640, height: int = 480) -> Path:
    img = Image.new("RGB", (width, height), color=(242, 242, 242))
    draw = ImageDraw.Draw(img)
    draw.rectangle((30, 30, width - 30, height - 30), outline=(32, 32, 32), width=2)
    draw.text((60, 80), "KRIA SIDE-CAR TEST", fill=(20, 20, 20))
    img.save(path, format="PNG")
    return path


def test_mode_selection_prefers_ocr_only_for_text_intent(monkeypatch, tmp_path: Path) -> None:
    image_path = _make_test_image(tmp_path / "mode_text.png")

    monkeypatch.setattr(
        image_processor,
        "_fast_probe",
        lambda _img: {
            "mean_luma": 140.0,
            "std_luma": 40.0,
            "blur_score": 80.0,
            "edge_density": 0.08,
            "text_area_ratio": 0.45,
            "largest_text_region_ratio": 0.22,
            "textness": 0.82,
        },
    )
    monkeypatch.setattr(image_processor, "_extract_text_rois", lambda _img: [])
    monkeypatch.setattr(
        image_processor,
        "_run_ocr_pipeline",
        lambda _img, _rois, _tier, _mode: {
            "text": "Fatal error at line 42",
            "confidence": 0.91,
            "engine": "mock",
            "regions": [],
        },
    )

    result = image_processor.analyze(
        {
            "file_path": str(image_path),
            "operations": ["metadata", "ocr", "thumbnail"],
            "intent": "text_reading",
            "_tier": "standard",
        }
    )

    assert result["mode_selected"] == "ocr_only"
    assert result["selected_images"] == []
    assert result["token_accounting"]["visual_tokens"] == 0
    assert result["ocr"]["confidence"] == pytest.approx(0.91, abs=1e-3)


def test_token_budgeting_caps_ocr_and_visual_tokens(monkeypatch, tmp_path: Path) -> None:
    image_path = _make_test_image(tmp_path / "budget.png")

    monkeypatch.setattr(
        image_processor,
        "_fast_probe",
        lambda _img: {
            "mean_luma": 126.0,
            "std_luma": 52.0,
            "blur_score": 120.0,
            "edge_density": 0.16,
            "text_area_ratio": 0.04,
            "largest_text_region_ratio": 0.02,
            "textness": 0.10,
        },
    )
    monkeypatch.setattr(
        image_processor,
        "_run_ocr_pipeline",
        lambda _img, _rois, _tier, _mode: {
            "text": "A" * 5000,
            "confidence": 0.35,
            "engine": "mock",
            "regions": [],
        },
    )

    result = image_processor.analyze(
        {
            "file_path": str(image_path),
            "operations": ["metadata", "ocr", "thumbnail"],
            "intent": "scene_understanding",
            "ocr_token_cap": 64,
            "_tier": "standard",
        }
    )

    accounting = result["token_accounting"]
    assert accounting["ocr_tokens_raw"] > accounting["ocr_tokens_used"]
    assert accounting["ocr_tokens_used"] <= 64
    assert accounting["visual_token_cap"] <= accounting["available_image_tokens"]
    assert accounting["visual_tokens"] <= accounting["visual_token_cap"]
    assert result["thumbnail_base64"]


def test_fallback_drops_visual_payload_when_cap_too_low(monkeypatch, tmp_path: Path) -> None:
    image_path = _make_test_image(tmp_path / "fallback.png", width=960, height=640)

    monkeypatch.setattr(
        image_processor,
        "_fast_probe",
        lambda _img: {
            "mean_luma": 150.0,
            "std_luma": 38.0,
            "blur_score": 115.0,
            "edge_density": 0.12,
            "text_area_ratio": 0.01,
            "largest_text_region_ratio": 0.01,
            "textness": 0.04,
        },
    )

    result = image_processor.analyze(
        {
            "file_path": str(image_path),
            "operations": ["metadata", "thumbnail"],
            "intent": "scene_understanding",
            "hard_visual_token_cap": 64,
            "model_profile": {
                "model_name": "minicpm-v-2.6",
                "patch_size": 14,
                "patch_merge": 1,
                "effective_patch": 14,
                "max_visual_tokens_6gb": 576,
                "min_visual_tokens": 64,
                "max_images_per_turn": 3,
            },
            "_tier": "standard",
        }
    )

    assert result["mode_selected"] == "full_frame"
    assert result["fallback_level_applied"] == 3
    assert result["selected_images"] == []
    assert result["token_accounting"]["visual_tokens"] == 0
    assert result["thumbnail_base64"]
