"""
VRAM Orchestrator
=================
Monitors GPU VRAM and calculates safe GPU-layer offload counts for llama.cpp.

The primary brain runs Phi-4-mini-instruct Q4_K_M (~2.5 GB) and the
secondary brain runs Qwen2.5-VL-7B-Instruct Q4_K_M (~4.7 GB).
Whisper-medium (~1.5 GB) runs alongside the primary brain.

The orchestrator is still implemented so that on tighter GPUs (6 GB cards)
it can automatically reduce GPU layers at runtime via the llama.cpp API.
"""
import logging
from dataclasses import dataclass
from typing import Optional

logger = logging.getLogger("kria.vram")

# Approximate VRAM requirements (MB)
MODEL_VRAM_MB = {
    "phi4_mini_q4":     2_500,   # Phi-4-mini-instruct Q4_K_M (primary,  3.8B)
    "qwen25_vl_7b_q4": 4_700,   # Qwen2.5-VL-7B-Instruct Q4_K_M (secondary, 7B)
    "whisper_medium":  1_500,
}
CUDA_OVERHEAD_MB = 512
SAFETY_MARGIN_MB = 512
# Transformer layer counts for partial GPU offload calculation
MODEL_TOTAL_LAYERS = {
    "phi4_mini_q4":     32,
    "qwen25_vl_7b_q4": 28,
}


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
        total_layers = MODEL_TOTAL_LAYERS.get(model_key, 32)
        layers = max(1, int(ratio * total_layers))
        logger.info(
            "[vram] Partial offload for %s: %d/%d layers (%.0f MB available)",
            model_key, layers, total_layers, available,
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
