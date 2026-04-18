"""Image processor with multi-stage, token-aware preprocessing for local VLMs.

Pipeline goals:
- Improve OCR quality for text-heavy images.
- Minimize visual-token usage for 4k context windows.
- Prevent OOM on 6GB VRAM by deterministic fallback steps.
"""

from __future__ import annotations

import base64
import io
import logging
import math
import os
from collections import Counter
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Dict, List, Tuple

logger = logging.getLogger("kria.processors.image")

METHODS = ["analyze"]
MAX_IMAGE_SIZE_BYTES = int(os.environ.get("KRIA_MAX_IMAGE_SIZE_MB", "100")) * 1024 * 1024
MAX_IMAGE_PIXELS = int(os.environ.get("KRIA_MAX_IMAGE_PIXELS", "40000000"))

# Tier -> max generic thumbnail dimension for compatibility output.
_THUMB_SIZE = {"lite": 512, "standard": 1024, "performance": 2048, "high": 0}

_TEXT_INTENTS = {
    "text_reading",
    "ui_error_reading",
    "document_scan",
}

_SCENE_INTENTS = {
    "scene_understanding",
}


@dataclass
class ModelProfile:
    model_name: str
    patch_size: int
    patch_merge: int
    effective_patch: int
    max_visual_tokens_6gb: int
    min_visual_tokens: int
    max_images_per_turn: int


_DEFAULT_PROFILES: Dict[str, ModelProfile] = {
    "qwen-vl": ModelProfile(
        model_name="qwen-vl",
        patch_size=14,
        patch_merge=2,
        effective_patch=28,
        max_visual_tokens_6gb=640,
        min_visual_tokens=64,
        max_images_per_turn=3,
    ),
    # Conservative default for MiniCPM-V 2.6: merge=1 yields safer (higher) token estimate.
    "minicpm-v-2.6": ModelProfile(
        model_name="minicpm-v-2.6",
        patch_size=14,
        patch_merge=1,
        effective_patch=14,
        max_visual_tokens_6gb=576,
        min_visual_tokens=64,
        max_images_per_turn=3,
    ),
}


def analyze(params: dict) -> dict:
    """Analyze and preprocess an image for OCR/VLM use.

    Params:
        file_path|file: str
        operations: list[str] from ["metadata", "ocr", "features", "thumbnail"]
        intent: optional request intent hint
        model_profile: optional dict override for patch/token profile
        context_window: optional int, default 4096
        response_reserve: optional int, default 700
        system_reserve: optional int, default 900
        history_reserve: optional int, default 1400
        ocr_token_cap: optional int, default 384
        metadata_token_cap: optional int, default 72
        hard_visual_token_cap: optional int, default profile cap
    """
    try:
        from PIL import Image, ExifTags
    except Exception as e:
        raise RuntimeError(
            "image.analyze requires Pillow for the active Python interpreter. "
            "Install a compatible pillow build for this environment."
        ) from e

    file_path = _resolve_file_path(params)
    if not file_path or not os.path.isfile(file_path):
        raise FileNotFoundError(file_path)

    file_size = os.path.getsize(file_path)
    if file_size > MAX_IMAGE_SIZE_BYTES:
        raise ValueError(
            f"Image file too large ({file_size} bytes > {MAX_IMAGE_SIZE_BYTES} bytes max)"
        )

    Image.MAX_IMAGE_PIXELS = MAX_IMAGE_PIXELS

    tier = str(params.get("_tier", "standard")).lower()
    operations = _resolve_operations(params.get("operations"))
    profile = _resolve_model_profile(params)
    budget_cfg = _resolve_budget_config(params)

    source_img = Image.open(file_path)
    source_img.load()
    img = source_img.convert("RGB")
    result: dict[str, Any] = {
        "file_path": file_path,
        "mode_selected": "full_frame",
        "normalization_plan": {},
        "resize_plan": {},
        "token_accounting": {},
        "fallback_level_applied": 0,
        "model_profile": {
            "model_name": profile.model_name,
            "patch_size": profile.patch_size,
            "patch_merge": profile.patch_merge,
            "effective_patch": profile.effective_patch,
        },
    }

    probe = _fast_probe(img)
    intent = _classify_intent(params.get("intent"), probe)
    mode = _select_mode(intent, probe)
    result["mode_selected"] = mode

    # Build metadata early for compatibility and budgeting diagnostics.
    if "metadata" in operations:
        result["metadata"] = _extract_metadata(file_path, source_img, ExifTags)

    ocr_required = ("ocr" in operations) or (intent in _TEXT_INTENTS)
    rois = _extract_text_rois(img) if mode in ("roi_hybrid", "ocr_only") else []

    ocr_payload = {"text": "", "confidence": 0.0, "engine": "none", "regions": []}
    if ocr_required and tier != "lite":
        ocr_payload = _run_ocr_pipeline(img, rois, tier, mode)

    ocr_text_capped, ocr_tokens_raw, ocr_tokens_used = _cap_ocr_text(
        ocr_payload["text"],
        budget_cfg["ocr_token_cap"],
    )
    ocr_payload["text"] = ocr_text_capped

    if "ocr" in operations:
        result["ocr_text"] = ocr_payload["text"]
        result["ocr"] = {
            "confidence": round(float(ocr_payload["confidence"]), 3),
            "engine": ocr_payload["engine"],
            "regions": ocr_payload["regions"],
        }

    features = _extract_features(img, probe, len(ocr_payload["text"]))
    if "features" in operations and tier != "lite":
        result["features"] = features

    text_centric = intent in _TEXT_INTENTS
    need_visual = _should_use_visual_tokens(
        mode=mode,
        text_centric=text_centric,
        ocr_confidence=float(ocr_payload["confidence"]),
    )

    available_image_tokens = max(
        0,
        budget_cfg["context_window"]
        - budget_cfg["response_reserve"]
        - budget_cfg["system_reserve"]
        - budget_cfg["history_reserve"]
        - budget_cfg["metadata_token_cap"]
        - ocr_tokens_used,
    )

    hard_visual_cap = min(
        int(budget_cfg["hard_visual_token_cap"]),
        int(profile.max_visual_tokens_6gb),
        int(available_image_tokens),
    )

    selected_images: List[Dict[str, Any]] = []
    normalization_plan: Dict[str, Any] = {"mode": mode, "branches": []}
    resize_plan: Dict[str, Any] = {"effective_patch": profile.effective_patch, "images": []}
    fallback_level = 0

    if need_visual and hard_visual_cap > 0:
        frame_specs = _build_frame_specs(
            mode=mode,
            text_heavy=features.get("text_area_ratio", 0.0) >= 0.20,
            rois=rois,
            hard_visual_cap=hard_visual_cap,
            profile=profile,
        )

        normalized_frames: List[Dict[str, Any]] = []
        for spec in frame_specs:
            branch = "text" if spec["kind"] == "roi" else "scene"
            if mode == "ocr_only":
                branch = "text"

            spec_image = spec["image"] if spec["image"] is not None else img

            normalized = _normalize_image(
                spec_image,
                branch=branch,
                for_ocr=False,
            )
            normalization_plan["branches"].append(
                {
                    "kind": spec["kind"],
                    "branch": branch,
                    "bbox": spec.get("bbox"),
                }
            )
            normalized_frames.append(
                {
                    "kind": spec["kind"],
                    "bbox": spec.get("bbox"),
                    "image": normalized,
                    "target_tokens": int(spec["target_tokens"]),
                    "priority": int(spec.get("priority", 100)),
                }
            )

        resized_frames = []
        for frame in normalized_frames:
            resized, info = _resize_to_target_tokens(
                frame["image"],
                target_tokens=frame["target_tokens"],
                effective_patch=profile.effective_patch,
                min_side=224,
                max_side=_max_side_for_frame(mode, frame["kind"]),
            )
            frame_info = {
                "kind": frame["kind"],
                "bbox": frame.get("bbox"),
                "priority": frame["priority"],
                "image": resized,
                "target_tokens": frame["target_tokens"],
                "visual_tokens": _visual_tokens(resized.width, resized.height, profile.effective_patch),
            }
            frame_info.update(info)
            resized_frames.append(frame_info)

        resized_frames, fallback_level = _apply_visual_fallbacks(
            frames=resized_frames,
            hard_visual_cap=hard_visual_cap,
            effective_patch=profile.effective_patch,
            mode=mode,
        )

        selected_images = _encode_selected_images(
            resized_frames,
            max_images=profile.max_images_per_turn,
        )

        for frame in resized_frames:
            resize_plan["images"].append(
                {
                    "kind": frame["kind"],
                    "bbox": frame.get("bbox"),
                    "target_tokens": frame["target_tokens"],
                    "resized_width": frame["resized_width"],
                    "resized_height": frame["resized_height"],
                    "visual_tokens": frame["visual_tokens"],
                }
            )

    # Backward compatible thumbnail keys (always derived from full image
    # to preserve global context even when selected_images are ROI-only).
    if "thumbnail" in operations:
        fallback_thumb = _make_compat_thumbnail(img, tier)
        result["thumbnail_base64"] = fallback_thumb["data_base64"]
        result["thumbnail_mime_type"] = fallback_thumb["mime_type"]
        result["thumbnail_size"] = {
            "width": fallback_thumb["width"],
            "height": fallback_thumb["height"],
        }

    visual_tokens = int(sum(item["visual_tokens"] for item in selected_images))
    prompt_estimate = (
        budget_cfg["system_reserve"]
        + budget_cfg["history_reserve"]
        + budget_cfg["metadata_token_cap"]
        + ocr_tokens_used
        + visual_tokens
    )
    within_context = (prompt_estimate + budget_cfg["response_reserve"]) <= budget_cfg["context_window"]

    result["selected_images"] = selected_images
    result["normalization_plan"] = normalization_plan
    result["resize_plan"] = resize_plan
    result["fallback_level_applied"] = int(fallback_level)
    result["token_accounting"] = {
        "context_window": budget_cfg["context_window"],
        "response_reserve": budget_cfg["response_reserve"],
        "system_reserve": budget_cfg["system_reserve"],
        "history_reserve": budget_cfg["history_reserve"],
        "metadata_token_cap": budget_cfg["metadata_token_cap"],
        "ocr_tokens_raw": ocr_tokens_raw,
        "ocr_tokens_used": ocr_tokens_used,
        "available_image_tokens": available_image_tokens,
        "visual_token_cap": hard_visual_cap,
        "visual_tokens": visual_tokens,
        "prompt_estimate_tokens": prompt_estimate,
        "within_context": within_context,
        "per_image": [
            {
                "kind": item["kind"],
                "bbox": item.get("bbox"),
                "width": item["width"],
                "height": item["height"],
                "visual_tokens": item["visual_tokens"],
            }
            for item in selected_images
        ],
    }

    parts = []
    if "metadata" in result:
        m = result["metadata"]
        parts.append(f"{m['width']}x{m['height']} {m.get('format', '?')} ({m['size_kb']}KB)")
    if result.get("ocr_text"):
        parts.append(f"OCR: {result['ocr_text'][:200]}")
    if "features" in result:
        parts.append(f"Scene: {result['features'].get('scene_type', '?')}")
    parts.append(f"Mode: {mode}")
    parts.append(f"Visual tokens: {visual_tokens}/{hard_visual_cap}")
    result["summary"] = "; ".join(parts) if parts else "Image analyzed"

    return result


def _resolve_file_path(params: dict) -> str:
    file_path = params.get("file_path") or params.get("file") or ""
    return str(file_path).strip()


def _resolve_operations(raw: Any) -> List[str]:
    if raw is None:
        return ["metadata", "ocr", "features", "thumbnail"]
    if isinstance(raw, str):
        return [raw]
    if isinstance(raw, list):
        return [str(x) for x in raw if isinstance(x, (str, int, float))]
    return ["metadata", "ocr", "features", "thumbnail"]


def _resolve_model_profile(params: dict) -> ModelProfile:
    model_name = str(params.get("model_name") or "qwen-vl").lower()
    if "minicpm" in model_name:
        base = _DEFAULT_PROFILES["minicpm-v-2.6"]
    else:
        base = _DEFAULT_PROFILES["qwen-vl"]

    override = params.get("model_profile") or {}
    if not isinstance(override, dict):
        override = {}

    patch_size = int(override.get("patch_size", params.get("patch_size", base.patch_size)))
    patch_merge = int(override.get("patch_merge", params.get("patch_merge", base.patch_merge)))
    effective_patch = int(
        override.get(
            "effective_patch",
            params.get("effective_patch", patch_size * max(1, patch_merge)),
        )
    )

    return ModelProfile(
        model_name=str(override.get("model_name", base.model_name)),
        patch_size=max(1, patch_size),
        patch_merge=max(1, patch_merge),
        effective_patch=max(1, effective_patch),
        max_visual_tokens_6gb=int(override.get("max_visual_tokens_6gb", params.get("max_visual_tokens_6gb", base.max_visual_tokens_6gb))),
        min_visual_tokens=int(override.get("min_visual_tokens", params.get("min_visual_tokens", base.min_visual_tokens))),
        max_images_per_turn=int(override.get("max_images_per_turn", params.get("max_images_per_turn", base.max_images_per_turn))),
    )


def _resolve_budget_config(params: dict) -> Dict[str, int]:
    context_window = int(params.get("context_window", 4096))
    response_reserve = int(params.get("response_reserve", 700))
    system_reserve = int(params.get("system_reserve", 900))
    history_reserve = int(params.get("history_reserve", 1400))
    ocr_token_cap = int(params.get("ocr_token_cap", 384))
    metadata_token_cap = int(params.get("metadata_token_cap", 72))
    hard_visual_token_cap = int(params.get("hard_visual_token_cap", 640))
    return {
        "context_window": max(1024, context_window),
        "response_reserve": max(128, response_reserve),
        "system_reserve": max(128, system_reserve),
        "history_reserve": max(128, history_reserve),
        "ocr_token_cap": max(64, ocr_token_cap),
        "metadata_token_cap": max(16, metadata_token_cap),
        "hard_visual_token_cap": max(64, hard_visual_token_cap),
    }


def _classify_intent(raw_intent: Any, probe: Dict[str, float]) -> str:
    if isinstance(raw_intent, str) and raw_intent.strip():
        normalized = raw_intent.strip().lower()
        if normalized in _TEXT_INTENTS or normalized in _SCENE_INTENTS or normalized == "mixed":
            return normalized

        if any(k in normalized for k in ("ocr", "text", "read", "screenshot", "error", "document")):
            return "text_reading"
        if any(k in normalized for k in ("scene", "object", "describe", "photo")):
            return "scene_understanding"

    if probe.get("text_area_ratio", 0.0) >= 0.30:
        return "text_reading"
    if probe.get("text_area_ratio", 0.0) >= 0.15:
        return "mixed"
    return "scene_understanding"


def _select_mode(intent: str, probe: Dict[str, float]) -> str:
    text_area = float(probe.get("text_area_ratio", 0.0))
    largest = float(probe.get("largest_text_region_ratio", 0.0))

    if intent in _TEXT_INTENTS and text_area >= 0.30:
        return "ocr_only"
    if text_area >= 0.15 and largest <= 0.65:
        return "roi_hybrid"
    if intent in _SCENE_INTENTS:
        return "full_frame"
    if intent == "mixed":
        return "roi_hybrid" if text_area >= 0.10 else "full_frame"
    return "full_frame"


def _fast_probe(img) -> Dict[str, float]:
    import cv2
    import numpy as np
    from PIL import Image

    preview = img.copy()
    if max(preview.width, preview.height) > 768:
        preview.thumbnail((768, 768), Image.LANCZOS)

    arr = np.array(preview.convert("RGB"))
    gray = cv2.cvtColor(arr, cv2.COLOR_RGB2GRAY)

    mean_luma = float(np.mean(gray))
    std_luma = float(np.std(gray))
    blur_score = float(cv2.Laplacian(gray, cv2.CV_64F).var())

    edges = cv2.Canny(gray, 80, 180)
    edge_density = float(np.mean(edges > 0))

    text_area_ratio = 0.0
    largest_text_region_ratio = 0.0
    textness = 0.0
    try:
        thresh = cv2.adaptiveThreshold(
            gray,
            255,
            cv2.ADAPTIVE_THRESH_GAUSSIAN_C,
            cv2.THRESH_BINARY_INV,
            25,
            15,
        )
        kernel = cv2.getStructuringElement(cv2.MORPH_RECT, (3, 3))
        thresh = cv2.morphologyEx(thresh, cv2.MORPH_CLOSE, kernel, iterations=1)
        n_labels, _, stats, _ = cv2.connectedComponentsWithStats(thresh, connectivity=8)

        h, w = gray.shape
        frame_area = float(w * h)
        valid_areas = []
        for i in range(1, n_labels):
            x = int(stats[i, cv2.CC_STAT_LEFT])
            y = int(stats[i, cv2.CC_STAT_TOP])
            bw = int(stats[i, cv2.CC_STAT_WIDTH])
            bh = int(stats[i, cv2.CC_STAT_HEIGHT])
            area = float(stats[i, cv2.CC_STAT_AREA])
            if bw <= 0 or bh <= 0:
                continue
            ar = bw / float(bh)
            if area < 24 or area > frame_area * 0.12:
                continue
            if ar < 0.15 or ar > 20.0:
                continue
            if y <= 1 or x <= 1:
                continue
            valid_areas.append(area)

        if valid_areas:
            total = float(sum(valid_areas))
            text_area_ratio = min(1.0, total / frame_area)
            largest_text_region_ratio = min(1.0, max(valid_areas) / frame_area)
            count_score = min(1.0, len(valid_areas) / 120.0)
            area_score = min(1.0, text_area_ratio * 3.0)
            textness = round((count_score + area_score) / 2.0, 4)
    except Exception as e:
        logger.warning("probe textness failed: %s", e)

    return {
        "mean_luma": round(mean_luma, 3),
        "std_luma": round(std_luma, 3),
        "blur_score": round(blur_score, 3),
        "edge_density": round(edge_density, 5),
        "text_area_ratio": round(text_area_ratio, 5),
        "largest_text_region_ratio": round(largest_text_region_ratio, 5),
        "textness": round(textness, 5),
    }


def _extract_metadata(file_path: str, img, exif_tags_mod) -> Dict[str, Any]:
    meta: Dict[str, Any] = {
        "width": img.width,
        "height": img.height,
        "format": (img.format or Path(file_path).suffix.lstrip(".")).lower(),
        "mode": img.mode,
        "size_kb": round(os.path.getsize(file_path) / 1024.0, 1),
    }
    exif_data: Dict[str, Any] = {}
    try:
        raw_exif = img.getexif()
        for tag_id, value in raw_exif.items():
            tag = exif_tags_mod.TAGS.get(tag_id, str(tag_id))
            if isinstance(value, (str, int, float)):
                exif_data[tag] = value
    except Exception:
        pass
    if exif_data:
        meta["exif"] = exif_data
    return meta


def _extract_features(img, probe: Dict[str, float], ocr_len: int) -> Dict[str, Any]:
    import numpy as np
    from PIL import Image

    small = img.copy()
    small.thumbnail((64, 64), Image.LANCZOS)
    pixels = np.array(small.convert("RGB")).reshape(-1, 3)
    quantized = (pixels // 32) * 32
    tuples = [tuple(c) for c in quantized]
    top_colors = Counter(tuples).most_common(5)
    dominant = [
        {
            "rgb": [int(channel) for channel in c],
            "frequency": round(n / max(1, len(tuples)), 3),
        }
        for c, n in top_colors
    ]

    text_density = (ocr_len / max(img.width * img.height, 1)) * 1e6
    scene_type = "photo"
    if text_density > 40 or probe.get("text_area_ratio", 0.0) > 0.20:
        scene_type = "screenshot_or_document"
    elif probe.get("edge_density", 0.0) > 0.18:
        scene_type = "diagram_or_chart"

    return {
        "dominant_colors": dominant,
        "scene_type": scene_type,
        "edge_density": probe.get("edge_density", 0.0),
        "text_density": round(float(text_density), 3),
        "text_area_ratio": probe.get("text_area_ratio", 0.0),
        "largest_text_region_ratio": probe.get("largest_text_region_ratio", 0.0),
        "blur_score": probe.get("blur_score", 0.0),
        "has_text": bool((ocr_len > 0) or (probe.get("text_area_ratio", 0.0) > 0.08)),
    }


def _extract_text_rois(img) -> List[Dict[str, Any]]:
    import cv2
    import numpy as np

    arr = np.array(img.convert("RGB"))
    gray = cv2.cvtColor(arr, cv2.COLOR_RGB2GRAY)
    bw = cv2.adaptiveThreshold(
        gray,
        255,
        cv2.ADAPTIVE_THRESH_GAUSSIAN_C,
        cv2.THRESH_BINARY_INV,
        31,
        17,
    )
    kernel = cv2.getStructuringElement(cv2.MORPH_RECT, (15, 3))
    merged = cv2.dilate(bw, kernel, iterations=1)
    contours, _ = cv2.findContours(merged, cv2.RETR_EXTERNAL, cv2.CHAIN_APPROX_SIMPLE)

    h, w = gray.shape
    area_frame = float(w * h)
    boxes: List[Tuple[int, int, int, int, float]] = []
    for contour in contours:
        x, y, bw_box, bh_box = cv2.boundingRect(contour)
        area = float(bw_box * bh_box)
        if area < area_frame * 0.002 or area > area_frame * 0.65:
            continue
        ar = bw_box / float(max(1, bh_box))
        if ar < 0.5 or ar > 25:
            continue
        score = float(area * min(ar, 8.0))
        boxes.append((x, y, bw_box, bh_box, score))

    if not boxes:
        return []

    merged_boxes = _merge_boxes_iou(boxes, iou_threshold=0.30)
    merged_boxes.sort(key=lambda b: b[4], reverse=True)

    rois: List[Dict[str, Any]] = []
    for x, y, bw_box, bh_box, score in merged_boxes[:3]:
        pad_x = int(bw_box * 0.12)
        pad_y = int(bh_box * 0.12)
        x1 = max(0, x - pad_x)
        y1 = max(0, y - pad_y)
        x2 = min(w, x + bw_box + pad_x)
        y2 = min(h, y + bh_box + pad_y)
        if x2 <= x1 or y2 <= y1:
            continue
        roi_img = img.crop((x1, y1, x2, y2))
        rois.append(
            {
                "bbox": [x1, y1, x2, y2],
                "area": int((x2 - x1) * (y2 - y1)),
                "score": float(score),
                "image": roi_img,
            }
        )
    return rois


def _merge_boxes_iou(
    boxes: List[Tuple[int, int, int, int, float]],
    iou_threshold: float,
) -> List[Tuple[int, int, int, int, float]]:
    merged: List[Tuple[int, int, int, int, float]] = []
    for box in sorted(boxes, key=lambda b: b[4], reverse=True):
        x, y, bw_box, bh_box, score = box
        x2 = x + bw_box
        y2 = y + bh_box
        matched = False
        for i, m in enumerate(merged):
            mx, my, mbw, mbh, mscore = m
            mx2 = mx + mbw
            my2 = my + mbh
            if _iou([x, y, x2, y2], [mx, my, mx2, my2]) >= iou_threshold:
                nx1 = min(x, mx)
                ny1 = min(y, my)
                nx2 = max(x2, mx2)
                ny2 = max(y2, my2)
                merged[i] = (nx1, ny1, nx2 - nx1, ny2 - ny1, max(score, mscore))
                matched = True
                break
        if not matched:
            merged.append(box)
    return merged


def _iou(a: List[int], b: List[int]) -> float:
    ax1, ay1, ax2, ay2 = a
    bx1, by1, bx2, by2 = b
    ix1 = max(ax1, bx1)
    iy1 = max(ay1, by1)
    ix2 = min(ax2, bx2)
    iy2 = min(ay2, by2)
    iw = max(0, ix2 - ix1)
    ih = max(0, iy2 - iy1)
    inter = float(iw * ih)
    if inter <= 0:
        return 0.0
    area_a = float(max(1, (ax2 - ax1) * (ay2 - ay1)))
    area_b = float(max(1, (bx2 - bx1) * (by2 - by1)))
    return inter / (area_a + area_b - inter)


def _run_ocr_pipeline(img, rois: List[Dict[str, Any]], tier: str, mode: str) -> Dict[str, Any]:
    text_parts: List[str] = []
    confidences: List[float] = []
    regions: List[Dict[str, Any]] = []

    if mode == "roi_hybrid" and rois:
        targets = rois
    else:
        targets = [{"bbox": [0, 0, img.width, img.height], "image": img, "area": img.width * img.height}]

    for idx, entry in enumerate(targets):
        ocr_img = _normalize_image(entry["image"], branch="text", for_ocr=True)
        text, conf, engine = _run_ocr_once(ocr_img, tier)
        if text:
            label = f"[roi_{idx}]\n" if len(targets) > 1 else ""
            text_parts.append(label + text.strip())
        confidences.append(conf)
        regions.append(
            {
                "bbox": entry.get("bbox"),
                "confidence": round(float(conf), 3),
                "chars": len(text.strip()),
                "engine": engine,
            }
        )

    combined = "\n\n".join(p for p in text_parts if p)
    avg_conf = float(sum(confidences) / max(1, len(confidences)))
    engine_name = regions[0]["engine"] if regions else "none"
    return {
        "text": combined.strip(),
        "confidence": round(avg_conf, 4),
        "engine": engine_name,
        "regions": regions,
    }


def _run_ocr_once(img, tier: str) -> Tuple[str, float, str]:
    try:
        if tier in ("performance", "high"):
            try:
                import easyocr

                reader = easyocr.Reader(["en"], gpu=True)
                lines = reader.readtext(_pil_to_cv_bgr(img))
                text = "\n".join(str(row[1]).strip() for row in lines if len(row) >= 2).strip()
                confs = [float(row[2]) for row in lines if len(row) >= 3 and isinstance(row[2], (float, int))]
                conf = float(sum(confs) / max(1, len(confs))) if confs else 0.0
                if text:
                    return text, conf, "easyocr"
            except ImportError:
                pass
            except Exception as e:
                logger.warning("easyocr failed, falling back to pytesseract: %s", e)

        import pytesseract
        from pytesseract import Output

        data = pytesseract.image_to_data(img, output_type=Output.DICT)
        confs = []
        for val in data.get("conf", []):
            try:
                conf_v = float(val)
            except Exception:
                continue
            if conf_v >= 0:
                confs.append(conf_v / 100.0)

        text = pytesseract.image_to_string(img).strip()
        conf = float(sum(confs) / max(1, len(confs))) if confs else 0.0
        return text, conf, "pytesseract"
    except Exception as e:
        logger.warning("OCR failed: %s", e)
        return "", 0.0, "none"


def _should_use_visual_tokens(mode: str, text_centric: bool, ocr_confidence: float) -> bool:
    if mode == "ocr_only" and text_centric and ocr_confidence >= 0.75:
        return False
    return True


def _build_frame_specs(
    mode: str,
    text_heavy: bool,
    rois: List[Dict[str, Any]],
    hard_visual_cap: int,
    profile: ModelProfile,
) -> List[Dict[str, Any]]:
    specs: List[Dict[str, Any]] = []

    if mode == "ocr_only":
        # Low-cost fallback visual frame when OCR confidence is weak.
        target = max(profile.min_visual_tokens, min(256, hard_visual_cap))
        if rois:
            specs.append({"kind": "roi", "bbox": rois[0]["bbox"], "image": rois[0]["image"], "target_tokens": target, "priority": 10})
        else:
            specs.append({"kind": "global", "bbox": None, "image": None, "target_tokens": target, "priority": 50})
        return specs

    if mode == "full_frame":
        target = 512 if text_heavy else min(640, hard_visual_cap)
        target = max(profile.min_visual_tokens, min(target, hard_visual_cap))
        # Placeholder image will be set by caller.
        # Caller may overwrite image with full-frame source.
        return [{"kind": "global", "bbox": None, "image": None, "target_tokens": target, "priority": 50}]

    # ROI hybrid mode
    global_target = min(96, hard_visual_cap)
    remaining = max(0, hard_visual_cap - global_target)
    usable_rois = rois[: profile.max_images_per_turn]
    total_area = float(sum(max(1, r["area"]) for r in usable_rois))

    for idx, roi in enumerate(usable_rois):
        weight = (float(roi["area"]) / total_area) if total_area > 0 else (1.0 / max(1, len(usable_rois)))
        tgt = int(max(profile.min_visual_tokens, min(192, remaining * weight)))
        specs.append(
            {
                "kind": "roi",
                "bbox": roi["bbox"],
                "image": roi["image"],
                "target_tokens": tgt,
                "priority": 10 + idx,
            }
        )

    specs.append(
        {
            "kind": "global",
            "bbox": None,
            "image": None,
            "target_tokens": global_target,
            "priority": 80,
        }
    )
    return specs


def _normalize_image(img, branch: str, for_ocr: bool):
    import cv2
    import numpy as np
    from PIL import Image

    arr = np.array(img.convert("RGB"))

    if branch == "text":
        gray = cv2.cvtColor(arr, cv2.COLOR_RGB2GRAY)
        mean_luma = float(np.mean(gray))
        gamma = 1.0
        if mean_luma < 110:
            gamma = 0.78
        elif mean_luma > 190:
            gamma = 1.18

        table = np.array([((i / 255.0) ** gamma) * 255 for i in np.arange(256)]).astype("uint8")
        gray = cv2.LUT(gray, table)

        clahe = cv2.createCLAHE(clipLimit=2.0, tileGridSize=(8, 8))
        gray = clahe.apply(gray)
        gray = cv2.medianBlur(gray, 3)

        blur = cv2.GaussianBlur(gray, (0, 0), 1.0)
        gray = cv2.addWeighted(gray, 1.35, blur, -0.35, 0)

        if for_ocr:
            gray = cv2.adaptiveThreshold(
                gray,
                255,
                cv2.ADAPTIVE_THRESH_GAUSSIAN_C,
                cv2.THRESH_BINARY,
                31,
                11,
            )
            return Image.fromarray(gray)

        rgb = cv2.cvtColor(gray, cv2.COLOR_GRAY2RGB)
        return Image.fromarray(rgb)

    # Scene branch
    lab = cv2.cvtColor(arr, cv2.COLOR_RGB2LAB)
    l, a, b = cv2.split(lab)
    clahe = cv2.createCLAHE(clipLimit=1.6, tileGridSize=(8, 8))
    l = clahe.apply(l)
    merged = cv2.merge([l, a, b])
    rgb = cv2.cvtColor(merged, cv2.COLOR_LAB2RGB)
    rgb = cv2.bilateralFilter(rgb, d=5, sigmaColor=65, sigmaSpace=65)
    blur = cv2.GaussianBlur(rgb, (0, 0), 0.8)
    rgb = cv2.addWeighted(rgb, 1.1, blur, -0.1, 0)
    return Image.fromarray(rgb)


def _resize_to_target_tokens(
    img,
    target_tokens: int,
    effective_patch: int,
    min_side: int,
    max_side: int,
):
    from PIL import Image

    w0, h0 = img.size
    target_tokens = max(1, int(target_tokens))
    target_area = float(target_tokens * effective_patch * effective_patch)
    scale = math.sqrt(target_area / max(1.0, float(w0 * h0)))

    w = max(min_side, int(round(w0 * scale)))
    h = max(min_side, int(round(h0 * scale)))

    if max(w, h) > max_side:
        down = max_side / float(max(w, h))
        w = int(round(w * down))
        h = int(round(h * down))

    w = _round_to_multiple(max(min_side, w), effective_patch)
    h = _round_to_multiple(max(min_side, h), effective_patch)

    if max_side > 0 and max(w, h) > max_side:
        clamp_scale = max_side / float(max(w, h))
        w = _round_to_multiple(max(min_side, int(round(w * clamp_scale))), effective_patch)
        h = _round_to_multiple(max(min_side, int(round(h * clamp_scale))), effective_patch)

    resized = img.resize((max(1, w), max(1, h)), Image.LANCZOS)
    return resized, {"resized_width": resized.width, "resized_height": resized.height}


def _apply_visual_fallbacks(
    frames: List[Dict[str, Any]],
    hard_visual_cap: int,
    effective_patch: int,
    mode: str,
) -> Tuple[List[Dict[str, Any]], int]:
    fallback = 0

    def total_tokens(items: List[Dict[str, Any]]) -> int:
        return int(sum(int(it["visual_tokens"]) for it in items))

    if total_tokens(frames) <= hard_visual_cap:
        return frames, fallback

    # Fallback 1: uniform downscale
    fallback = 1
    scaled = []
    for frame in frames:
        resized, info = _resize_to_target_tokens(
            frame["image"],
            target_tokens=max(1, int(frame["target_tokens"] * 0.75)),
            effective_patch=effective_patch,
            min_side=224,
            max_side=_max_side_for_frame(mode, frame["kind"]),
        )
        scaled.append(
            {
                **frame,
                "image": resized,
                "resized_width": info["resized_width"],
                "resized_height": info["resized_height"],
                "visual_tokens": _visual_tokens(resized.width, resized.height, effective_patch),
            }
        )
    frames = scaled
    if total_tokens(frames) <= hard_visual_cap:
        return frames, fallback

    # Fallback 2: keep highest priority frame(s)
    fallback = 2
    frames = sorted(frames, key=lambda it: int(it.get("priority", 100)))
    frames = frames[:1]
    if total_tokens(frames) <= hard_visual_cap:
        return frames, fallback

    # Fallback 3: drop all visual payloads (OCR-only path)
    fallback = 3
    return [], fallback


def _encode_selected_images(
    frames: List[Dict[str, Any]],
    max_images: int,
) -> List[Dict[str, Any]]:
    selected = []
    for frame in frames[: max(1, max_images)]:
        encoded = _encode_image(frame["image"])
        selected.append(
            {
                "kind": frame["kind"],
                "bbox": frame.get("bbox"),
                "width": frame["image"].width,
                "height": frame["image"].height,
                "visual_tokens": int(frame["visual_tokens"]),
                "mime_type": encoded["mime_type"],
                "data_base64": encoded["data_base64"],
            }
        )
    return selected


def _encode_image(img) -> Dict[str, str]:
    buf = io.BytesIO()
    if img.mode == "RGBA":
        img.save(buf, format="PNG")
        mime = "image/png"
    else:
        img.save(buf, format="JPEG", quality=85)
        mime = "image/jpeg"
    return {
        "mime_type": mime,
        "data_base64": base64.b64encode(buf.getvalue()).decode("ascii"),
    }


def _make_compat_thumbnail(img, tier: str) -> Dict[str, Any]:
    from PIL import Image

    thumb = img.copy()
    max_dim = _THUMB_SIZE.get(tier, 1024)
    if max_dim > 0 and max(thumb.width, thumb.height) > max_dim:
        thumb.thumbnail((max_dim, max_dim), Image.LANCZOS)
    encoded = _encode_image(thumb)
    return {
        "data_base64": encoded["data_base64"],
        "mime_type": encoded["mime_type"],
        "width": thumb.width,
        "height": thumb.height,
    }


def _cap_ocr_text(text: str, token_cap: int) -> Tuple[str, int, int]:
    raw = text.strip()
    raw_tokens = _estimate_tokens(raw)
    if raw_tokens <= token_cap:
        return raw, raw_tokens, raw_tokens

    max_chars = token_cap * 4
    if max_chars <= 0:
        return "", raw_tokens, 0

    marker = "\n\n...[truncated]...\n\n"
    marker_len = len(marker)

    if max_chars <= marker_len + 8:
        shortened = raw[:max_chars]
    else:
        content_budget = max_chars - marker_len
        head = int(content_budget * 0.7)
        tail = content_budget - head
        shortened = f"{raw[:head]}{marker}{raw[-tail:]}"

    used = _estimate_tokens(shortened)
    if used > token_cap:
        shortened = shortened[:max_chars]
        used = _estimate_tokens(shortened)

    return shortened, raw_tokens, used


def _estimate_tokens(text: str) -> int:
    if not text:
        return 0
    return int(math.ceil(len(text) / 4.0))


def _visual_tokens(width: int, height: int, effective_patch: int) -> int:
    return int(math.ceil(width / float(effective_patch)) * math.ceil(height / float(effective_patch)))


def _round_to_multiple(value: int, multiple: int) -> int:
    if multiple <= 1:
        return max(1, value)
    return int(max(multiple, round(value / multiple) * multiple))


def _max_side_for_frame(mode: str, kind: str) -> int:
    if mode == "roi_hybrid":
        return 1024 if kind == "roi" else 448
    if mode == "ocr_only":
        return 768
    return 896


def _pil_to_cv_bgr(img):
    import cv2
    import numpy as np

    return cv2.cvtColor(np.array(img.convert("RGB")), cv2.COLOR_RGB2BGR)
