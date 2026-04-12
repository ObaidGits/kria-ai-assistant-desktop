"""
VRAM Orchestrator
=================
Monitors GPU VRAM and calculates safe GPU-layer offload counts for llama.cpp.

Because the RTX on the Predator Helios Neo 16 comfortably holds both
Whisper-medium (~1.5 GB) and Qwen3-8B Q4_K_M (~5.2 GB) + draft model
(~0.6 GB) simultaneously, the default path never triggers a swap.

The orchestrator is still implemented so that on tighter GPUs (6 GB cards)
it can automatically reduce GPU layers at runtime via the llama.cpp API.
"""
import logging
from dataclasses import dataclass
from typing import Optional

logger = logging.getLogger("kria.vram")

# Approximate VRAM requirements (MB)
MODEL_VRAM_MB = {
    "qwen3_8b_q4": 5_200,
    "qwen3_0_6b_q8": 600,
    "whisper_medium": 1_500,
}
CUDA_OVERHEAD_MB = 512
SAFETY_MARGIN_MB = 512
QWEN3_TOTAL_LAYERS = 36   # transformer block count for Qwen3-8B


@dataclass
class VRAMState:
    total_mb: int
    used_mb: int

    @property
    def free_mb(self) -> int:
        return self.total_mb - self.used_mb


class VRAMOrchestrator:
    def __init__(self, vram_total_mb: int = 8_192) -> None:
        self._total = vram_total_mb
        self._budget = vram_total_mb - CUDA_OVERHEAD_MB - SAFETY_MARGIN_MB

    def calculate_gpu_layers(
        self,
        model_key: str,
        other_loaded_mb: int = 0,
    ) -> int:
        """
        Return the number of layers to offload to GPU.
        99 means "all layers" (llama.cpp convention).
        """
        model_mb = MODEL_VRAM_MB.get(model_key, 5_000)
        available = self._budget - other_loaded_mb

        if available >= model_mb:
            return 99  # Full GPU offload

        if available <= 0:
            logger.warning("[vram] No VRAM available for %s — CPU only", model_key)
            return 0

        ratio = available / model_mb
        layers = max(1, int(ratio * QWEN3_TOTAL_LAYERS))
        logger.info(
            "[vram] Partial offload for %s: %d/%d layers (%.0f MB available)",
            model_key, layers, QWEN3_TOTAL_LAYERS, available,
        )
        return layers

    def query_nvidia_smi(self) -> Optional[VRAMState]:
        """Try to read live VRAM stats via nvidia-smi."""
        try:
            import subprocess
            out = subprocess.check_output(
                ["nvidia-smi", "--query-gpu=memory.total,memory.used",
                 "--format=csv,noheader,nounits"],
                timeout=5, text=True,
            ).strip().split(",")
            return VRAMState(total_mb=int(out[0].strip()), used_mb=int(out[1].strip()))
        except Exception:
            return None


vram_orchestrator = VRAMOrchestrator()
