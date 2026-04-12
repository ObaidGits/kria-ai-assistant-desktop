"""
Voice Pipeline
==============
Ties together wake-word detection → VAD recording → STT → agent loop → TTS.

Lifecycle:
  1. ``build_voice_pipeline()`` is called by main.py lifespan startup.
     Returns the singleton ``VoicePipeline`` instance.
  2. ``pipeline.start()`` spawns a supervised background task that loops
     forever: wake → capture → transcribe → respond → speak.
  3. ``pipeline.stop()`` gracefully shuts down the background task.
  4. ``pipeline.process_text(text)`` is the direct (non-voice) entry point
     used by the REST/WebSocket API to bypass the microphone.

Push-to-talk support:
  POST /api/v1/voice/push   — triggers pipeline.push_audio(bytes)
  The WebSocket handler can also stream audio directly via
  pipeline.push_audio_chunk(bytes) which feeds the VAD buffer.
"""
import asyncio
import logging
from typing import Optional

from kria.infra.config import settings
from kria.infra.supervisor import SupervisedTask

logger = logging.getLogger("kria.voice.pipeline")


class VoicePipeline:
    def __init__(self) -> None:
        self._task: Optional[SupervisedTask] = None
        self._running = False
        self._push_queue: asyncio.Queue[bytes] = asyncio.Queue(maxsize=4)

    # ── Lifecycle ─────────────────────────────────────────────────

    async def start(self) -> None:
        if not settings.voice_enabled:
            logger.info("Voice pipeline disabled (KRIA_VOICE_ENABLED=false)")
            return
        self._running = True
        self._task = SupervisedTask(
            name="voice_pipeline",
            coro_factory=self._run_loop,
            max_retries=10,
            base_delay=2.0,
            max_delay=30.0,
        )
        await self._task.start()
        logger.info("Voice pipeline started")

    async def stop(self) -> None:
        self._running = False
        if self._task:
            await self._task.stop()
        logger.info("Voice pipeline stopped")

    # ── Main loop ─────────────────────────────────────────────────

    async def _run_loop(self) -> None:
        from kria.voice.wake_word import wake_word_detector
        from kria.voice.vad import vad_recorder
        from kria.voice.stt_client import stt_client
        from kria.voice.tts_client import tts_client

        await wake_word_detector.start()

        while self._running:
            try:
                logger.debug("Voice loop: waiting for wake word…")
                await wake_word_detector.wait_for_wake()

                if not self._running:
                    break

                logger.debug("Voice loop: wake word detected — recording utterance")
                audio = await vad_recorder.record_utterance()

                if not audio:
                    logger.debug("Voice loop: empty audio — skipping")
                    continue

                transcript = await stt_client.transcribe_bytes(audio)
                if not transcript or not transcript.strip():
                    logger.debug("Voice loop: empty transcript — skipping")
                    continue

                logger.info("Voice loop: transcript=%r", transcript)

                # Route through the agent
                response_text = await self.process_text(transcript)

                if response_text:
                    logger.info("Voice loop: speaking response (%d chars)", len(response_text))
                    await tts_client.speak(response_text)

            except asyncio.CancelledError:
                break
            except Exception as exc:
                logger.error("Voice loop error: %s", exc, exc_info=True)
                await asyncio.sleep(1)

        await wake_word_detector.stop()

    # ── Text processing (also used by REST/WebSocket) ─────────────

    async def process_text(self, text: str, session_id: str = "voice") -> str:
        """
        Send a text message through router → agent loop → return text response.
        This is the shared path for voice input AND text-based API calls.
        """
        try:
            from kria.agent.router import intent_router, IntentType
            from kria.agent.loop import react_loop

            intent = await intent_router.classify(text)

            if intent == IntentType.CONVERSATION:
                # Pure conversational — skip tool loop, streamed LLM
                from kria.agent.llm_client import llm_client
                from kria.agent.prompts import build_system_prompt
                system_content = build_system_prompt() + "\n/no_think"
                result = await llm_client.chat(
                    messages=[
                        {"role": "system", "content": system_content},
                        {"role": "user", "content": text},
                    ],
                    max_tokens=256,
                )
                if result is None:
                    # Circuit breaker blocked us — try direct HTTP as last resort
                    logger.info("CONVERSATION: circuit blocked, trying direct fallback")
                    result = await llm_client.direct_chat(
                        messages=[
                            {"role": "system", "content": system_content},
                            {"role": "user", "content": text},
                        ],
                        max_tokens=256,
                    )
                if result is None:
                    return "I'm having trouble reaching my reasoning engine right now."
                msg = result.get("choices", [{}])[0].get("message", {})
                return msg.get("content") or msg.get("reasoning_content") or ""

            # DIRECT_TOOL or AGENT_LOOP
            from kria.memory.conversation import conversation_memory
            history_rows = await conversation_memory.get_recent(
                session_id, limit=20, roles=["user", "assistant"]
            )
            conversation_history = [
                {"role": t["role"], "content": t["content"]} for t in history_rows
            ]
            response = await react_loop.run(
                user_message=text,
                conversation_history=conversation_history,
                session_id=session_id,
                intent=intent.value,
            )
            return response.text

        except Exception as exc:
            logger.error("process_text failed: %s", exc)
            return "I encountered an error processing your request."

    # ── Push-to-talk ──────────────────────────────────────────────

    async def push_audio(self, audio_bytes: bytes, session_id: str = "voice") -> tuple[str, str]:
        """
        Accepts raw WAV bytes (e.g. from WebSocket upload) and processes them
        without going through the wake-word gate.
        Returns (transcript, response) tuple.
        """
        from kria.voice.stt_client import stt_client
        transcript = await stt_client.transcribe_bytes(audio_bytes)
        if not transcript:
            return "", ""
        response = await self.process_text(transcript, session_id=session_id)
        return transcript, response


def build_voice_pipeline() -> VoicePipeline:
    """Factory called by main.py lifespan. Returns the singleton."""
    return voice_pipeline


voice_pipeline = VoicePipeline()
