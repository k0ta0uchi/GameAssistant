"""
Regression tests and pipeline validation for Issue #30:
ASR Streaming Pipeline Accuracy, Buffering, VAD Endpointing, and Latency Optimization.
"""

import ast
import asyncio
import json
import logging
import os
import sys
import threading
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import numpy as np

SERVER_SOURCE_PATH = Path(__file__).with_name("asr_server.py")


class MockWebSocket:
    """Mock WebSocket supporting async iteration and sending messages."""

    def __init__(self, messages):
        self._messages = list(messages)
        self.sent = []
        self._closed = False

    def __aiter__(self):
        return self

    async def __anext__(self):
        if not self._messages:
            if not self._closed:
                self._closed = True
                await asyncio.sleep(0.05)
            raise StopAsyncIteration
        await asyncio.sleep(0.01)
        return self._messages.pop(0)

    async def send(self, msg):
        self.sent.append(msg)


class AsrPipelineRegressionTests(unittest.IsolatedAsyncioTestCase):
    @classmethod
    def setUpClass(cls):
        cls.source = SERVER_SOURCE_PATH.read_text(encoding="utf-8")
        cls.tree = ast.parse(cls.source, filename=str(SERVER_SOURCE_PATH))

        # Extract constants
        cls.constants = {}
        for node in ast.walk(cls.tree):
            if isinstance(node, ast.Assign) and len(node.targets) == 1:
                target = node.targets[0]
                if isinstance(target, ast.Name) and isinstance(
                    node.value, ast.Constant
                ):
                    cls.constants[target.id] = node.value.value

        # Build sandboxed namespace with shared VAD / state functions
        cls.server_ns: dict[str, Any] = {
            "asyncio": asyncio,
            "json": json,
            "logging": logging,
            "logger": logging.getLogger("test-asr-pipeline"),
            "time": time,
            "np": np,
            "current_device": "cpu",
            "current_model_name": "test-model",
            "current_compute_type": "int8",
            "whisper_model": SimpleNamespace(
                transcribe=lambda *args, **kwargs: ([], None)
            ),
            "_device_switch_lock": threading.Lock(),
            "note_in_flight_gate": lambda: False,
            "clear_in_flight_gate": lambda: None,
            "note_in_flight_skip_log": lambda: False,
            "record_switch_diagnostic": lambda *args, **kwargs: None,
            "record_inference_stall": lambda *args: 0,
            "reset_inference_stall_streak": lambda: None,
            "vram_status_text": lambda: "n/a",
            "last_switch_reason": None,
            "REASON_INFERENCE_TIMEOUT": "inference_timeout",
            "REASON_CUDA_ERROR": "cuda_error",
            "REASON_GPU_OOM": "gpu_oom",
            "INFERENCE_IN_FLIGHT_EXIT_SECONDS": 10.0,
            "INFERENCE_STALL_EXIT_THRESHOLD": 3,
            "_CpuFallbackRequest": type("_CpuFallbackRequest", (Exception,), {}),
            "_perform_cpu_switch": lambda *args, **kwargs: None,
            "_fault_injection_hang": lambda *args: None,
            "SAMPLE_RATE": cls.constants.get("SAMPLE_RATE", 16000),
            "MIN_AUDIO_SECONDS": cls.constants.get("MIN_AUDIO_SECONDS", 0.25),
            "PRE_ROLL_SECONDS": cls.constants.get("PRE_ROLL_SECONDS", 0.20),
            "SPEECH_START_CONSECUTIVE_FRAMES": cls.constants.get(
                "SPEECH_START_CONSECUTIVE_FRAMES", 3
            ),
            "SPEECH_END_SILENCE_SECONDS": cls.constants.get(
                "SPEECH_END_SILENCE_SECONDS", 0.65
            ),
            "MAX_UTTERANCE_SECONDS": cls.constants.get("MAX_UTTERANCE_SECONDS", 30.0),
            "VAD_ENERGY_THRESHOLD": cls.constants.get("VAD_ENERGY_THRESHOLD", 0.012),
            "VAD_FRAME_SIZE": cls.constants.get("VAD_FRAME_SIZE", 320),
            "PARTIAL_POLL_INTERVAL_SECONDS": cls.constants.get(
                "PARTIAL_POLL_INTERVAL_SECONDS", 0.08
            ),
            "PARTIAL_INTERVAL_GPU_SECONDS": cls.constants.get(
                "PARTIAL_INTERVAL_GPU_SECONDS", 0.35
            ),
            "PARTIAL_INTERVAL_CPU_SECONDS": cls.constants.get(
                "PARTIAL_INTERVAL_CPU_SECONDS", 0.65
            ),
            "MIN_PARTIAL_INCREMENT_SECONDS": cls.constants.get(
                "MIN_PARTIAL_INCREMENT_SECONDS", 0.20
            ),
            "SHORT_UTTERANCE_FINAL_TIMEOUT_SECONDS": cls.constants.get(
                "SHORT_UTTERANCE_FINAL_TIMEOUT_SECONDS", 0.75
            ),
            "get_embedding_model": lambda: None,
            "set_vram_preallocation": lambda x: True,
            "websockets": SimpleNamespace(
                exceptions=SimpleNamespace(ConnectionClosed=Exception)
            ),
            "sys": SimpleNamespace(argv=[]),
            "os": SimpleNamespace(environ={}),
        }

        # Extract functions directly from asr_server.py AST
        target_funcs = (
            "get_arg_or_env",
            "resolve_compute_types",
            "remember_transcript",
            "new_stream_state",
            "reset_stream_state",
            "update_vad_and_buffers",
            "asr_handler",
        )
        for node in cls.tree.body:
            if (
                isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
                and node.name in target_funcs
            ):
                exec(
                    compile(
                        ast.Module(body=[node], type_ignores=[]),
                        str(SERVER_SOURCE_PATH),
                        "exec",
                    ),
                    cls.server_ns,
                )

        cls.get_arg_or_env = staticmethod(cls.server_ns["get_arg_or_env"])
        cls.resolve_compute_types = staticmethod(
            cls.server_ns["resolve_compute_types"]
        )
        cls.new_stream_state = staticmethod(cls.server_ns["new_stream_state"])
        cls.reset_stream_state = staticmethod(cls.server_ns["reset_stream_state"])
        cls.update_vad_and_buffers = staticmethod(
            cls.server_ns["update_vad_and_buffers"]
        )
        cls.asr_handler = staticmethod(cls.server_ns["asr_handler"])

    def test_pipeline_constants_satisfy_issue_30_spec(self):
        """Verify that Issue #30 constants are correctly configured."""
        self.assertIn("SAMPLE_RATE", self.constants)
        self.assertEqual(self.constants["SAMPLE_RATE"], 16000)

        # Buffer: max utterance is at least 30s (no 3s rolling truncation)
        self.assertIn("MAX_UTTERANCE_SECONDS", self.constants)
        self.assertGreaterEqual(self.constants["MAX_UTTERANCE_SECONDS"], 30.0)

        # Pre-roll is 150-250ms
        self.assertIn("PRE_ROLL_SECONDS", self.constants)
        self.assertGreaterEqual(self.constants["PRE_ROLL_SECONDS"], 0.15)
        self.assertLessEqual(self.constants["PRE_ROLL_SECONDS"], 0.25)

        # Speech-end silence timeout is 500-800ms (based on real audio silence)
        self.assertIn("SPEECH_END_SILENCE_SECONDS", self.constants)
        self.assertGreaterEqual(self.constants["SPEECH_END_SILENCE_SECONDS"], 0.50)
        self.assertLessEqual(self.constants["SPEECH_END_SILENCE_SECONDS"], 0.80)

        # Partial intervals: GPU 300-400ms, CPU 500-800ms
        self.assertIn("PARTIAL_INTERVAL_GPU_SECONDS", self.constants)
        self.assertGreaterEqual(self.constants["PARTIAL_INTERVAL_GPU_SECONDS"], 0.30)
        self.assertLessEqual(self.constants["PARTIAL_INTERVAL_GPU_SECONDS"], 0.40)

        self.assertIn("PARTIAL_INTERVAL_CPU_SECONDS", self.constants)
        self.assertGreaterEqual(self.constants["PARTIAL_INTERVAL_CPU_SECONDS"], 0.50)
        self.assertLessEqual(self.constants["PARTIAL_INTERVAL_CPU_SECONDS"], 0.80)

    def test_model_preset_resolution(self):
        """Verify quality and fast model preset resolution."""
        ns = {
            "MODELS_DIR": "C:\\fake\\models",
            "whisper_model_source": "C:\\fake\\models\\kotoba-whisper-v2.0-faster",
            "logger": type("FakeLogger", (), {"warning": lambda *args: None})(),
            "os": os,
        }
        for node in self.tree.body:
            if isinstance(node, ast.FunctionDef) and node.name == "get_model_spec":
                exec(
                    compile(
                        ast.Module(body=[node], type_ignores=[]),
                        str(SERVER_SOURCE_PATH),
                        "exec",
                    ),
                    ns,
                )
                break

        get_model_spec = ns.get("get_model_spec")
        self.assertIsNotNone(get_model_spec)

        # Quality preset always selects Kotoba-Whisper
        path, name = get_model_spec("quality", "cuda")
        self.assertEqual(name, "kotoba-whisper-v2.0-faster")

        path_cpu, name_cpu = get_model_spec("quality", "cpu")
        self.assertEqual(name_cpu, "kotoba-whisper-v2.0-faster")
        self.assertEqual(path, path_cpu)

    def test_compute_type_resolution_and_cpu_reflection(self):
        """Verify --compute-type / ASR_COMPUTE_TYPE is respected as common fallback for CPU and GPU,
        and device-specific overrides take precedence."""
        # 1. Default (no args / env) -> GPU: int8, CPU: int8_float32
        gpu, cpu = self.resolve_compute_types(argv=[], environ={})
        self.assertEqual(gpu, "int8")
        self.assertEqual(cpu, "int8_float32")

        # 2. Rust passes --compute-type float32 (or ASR_COMPUTE_TYPE=float32) -> GPU and CPU both float32
        gpu_arg, cpu_arg = self.resolve_compute_types(
            argv=["dummy.py", "--compute-type", "float32"], environ={}
        )
        self.assertEqual(gpu_arg, "float32")
        self.assertEqual(cpu_arg, "float32")

        gpu_env, cpu_env = self.resolve_compute_types(
            argv=[], environ={"ASR_COMPUTE_TYPE": "float32"}
        )
        self.assertEqual(gpu_env, "float32")
        self.assertEqual(cpu_env, "float32")

        # 3. Device-specific override takes precedence
        gpu_ovr, cpu_ovr = self.resolve_compute_types(
            argv=["dummy.py", "--compute-type", "int8", "--cpu-compute-type", "float32"],
            environ={},
        )
        self.assertEqual(gpu_ovr, "int8")
        self.assertEqual(cpu_ovr, "float32")

        # 4. Verify create_cpu_whisper_model reflects the resolved compute_type
        created_models = []
        ns = {
            "MODELS_DIR": "C:\\fake\\models",
            "whisper_model_source": "C:\\fake\\models\\kotoba-whisper-v2.0-faster",
            "logger": type(
                "FakeLogger", (), {"warning": lambda *a: None, "info": lambda *a: None}
            )(),
            "os": os,
            "model_preset": None,
            "current_cpu_threads": 4,
            "cpu_compute_type": cpu_arg,  # "float32" resolved from --compute-type float32
            "current_model_name": "",
            "current_compute_type": "",
            "WhisperModel": lambda *args, **kwargs: created_models.append(kwargs)
            or SimpleNamespace(),
        }
        for node in self.tree.body:
            if isinstance(node, ast.FunctionDef) and node.name in (
                "get_model_spec",
                "create_cpu_whisper_model",
            ):
                exec(
                    compile(
                        ast.Module(body=[node], type_ignores=[]),
                        str(SERVER_SOURCE_PATH),
                        "exec",
                    ),
                    ns,
                )

        ns["create_cpu_whisper_model"]()
        self.assertEqual(len(created_models), 1)
        self.assertEqual(created_models[0]["compute_type"], "float32")
        self.assertEqual(ns["current_compute_type"], "float32")

    def test_speech_end_based_on_silence_not_transcript_stillness(self):
        """Verify speech-end endpointing is based on real audio silence time, not transcript stillness."""
        self.assertIn(
            'now - state["last_voice_at"]) >= SPEECH_END_SILENCE_SECONDS',
            self.source,
        )
        self.assertNotIn('now - silence_start_time) >= timeout', self.source)

    def test_shared_vad_logic_updates_state_correctly(self):
        """Verify update_vad_and_buffers updates speech state, pre-roll, and full PCM accurately."""
        sample_rate = 16000
        state = self.new_stream_state()

        # Send 100ms of silence
        silence = np.zeros(int(sample_rate * 0.1), dtype=np.float32)
        self.update_vad_and_buffers(state, silence, now=0.1, sample_rate=sample_rate)
        self.assertFalse(state["is_speaking"])
        self.assertEqual(len(state["pre_roll_pcm"]), len(silence))
        self.assertEqual(len(state["full_utterance_pcm"]), 0)

        # Send 80ms of loud speech (4 x 20ms frames >= 3 frames required)
        speech = 0.1 * np.sin(
            2 * np.pi * 300 * np.linspace(0, 0.08, int(sample_rate * 0.08), dtype=np.float32)
        )
        self.update_vad_and_buffers(state, speech, now=0.18, sample_rate=sample_rate)
        self.assertTrue(state["is_speaking"])
        # full_utterance_pcm must contain pre-roll plus incoming speech
        self.assertGreaterEqual(
            len(state["full_utterance_pcm"]),
            len(silence) + len(speech),
        )

    def test_full_utterance_preservation_for_short_medium_long(self):
        """Simulate VAD pipeline using real update_vad_and_buffers from asr_server.py
        with short (<1s), medium (4s), and long (12s) utterances and verify PCM is never clipped to 3s."""
        sample_rate = 16000

        for duration_s in [0.8, 4.0, 12.0]:
            total_samples = int(sample_rate * duration_s)
            t = np.linspace(0, duration_s, total_samples, dtype=np.float32)
            speech_pcm = 0.1 * np.sin(2 * np.pi * 300 * t)

            pre_silence = np.zeros(int(sample_rate * 0.3), dtype=np.float32)
            post_silence = np.zeros(int(sample_rate * 0.8), dtype=np.float32)
            full_audio = np.concatenate([pre_silence, speech_pcm, post_silence])

            state = self.new_stream_state()
            now = 0.0
            chunk_size = 640  # 40ms packets
            finalized_pcm = None

            for i in range(0, len(full_audio), chunk_size):
                samples = full_audio[i : i + chunk_size]
                now += len(samples) / sample_rate

                self.update_vad_and_buffers(state, samples, now, sample_rate)

                # Check speech-end silence timeout
                if state["is_speaking"] and state["last_voice_at"] is not None:
                    if (now - state["last_voice_at"]) >= 0.65:
                        finalized_pcm = state["full_utterance_pcm"].copy()
                        state["is_speaking"] = False
                        break

            self.assertIsNotNone(
                finalized_pcm,
                f"Failed to finalize utterance of duration {duration_s}s",
            )
            finalized_duration = len(finalized_pcm) / sample_rate
            self.assertGreaterEqual(
                finalized_duration,
                duration_s,
                f"Utterance of {duration_s}s was truncated to {finalized_duration}s!",
            )
            if duration_s > 3.0:
                self.assertGreater(
                    finalized_duration,
                    3.5,
                    f"Utterance was incorrectly capped at 3s: {finalized_duration}s",
                )

    def test_intra_utterance_pause_does_not_prematurely_finalize(self):
        """A 350ms pause during speaking must not finalize or reset the buffer."""
        sample_rate = 16000
        s1 = 0.1 * np.sin(
            2 * np.pi * 300 * np.linspace(0, 1.5, int(sample_rate * 1.5), dtype=np.float32)
        )
        pause = np.zeros(int(sample_rate * 0.35), dtype=np.float32)
        s2 = 0.1 * np.sin(
            2 * np.pi * 300 * np.linspace(0, 1.5, int(sample_rate * 1.5), dtype=np.float32)
        )
        post = np.zeros(int(sample_rate * 0.8), dtype=np.float32)

        full_audio = np.concatenate([s1, pause, s2, post])
        state = self.new_stream_state()

        now = 0.0
        chunk_size = 640
        finalized_count = 0

        for i in range(0, len(full_audio), chunk_size):
            samples = full_audio[i : i + chunk_size]
            now += len(samples) / sample_rate

            self.update_vad_and_buffers(state, samples, now, sample_rate)

            # Endpoint check
            if state["is_speaking"] and state["last_voice_at"] is not None:
                if (now - state["last_voice_at"]) >= 0.65:
                    finalized_count += 1
                    state["is_speaking"] = False
                    self.reset_stream_state(state)

        # Must finalize exactly ONCE at the end, not during the 350ms pause
        self.assertEqual(
            finalized_count,
            1,
            f"Expected exactly 1 finalization, but got {finalized_count} (prematurely finalized during pause)",
        )

    def test_vad_handles_10ms_sub_frame_chunks_without_dropping(self):
        """Verify 160 samples (10ms) packets carry over in vad_pending_pcm and trigger VAD."""
        sample_rate = 16000
        state = self.new_stream_state()

        # Send 10ms chunks of loud speech (160 samples each)
        speech_10ms = 0.1 * np.sin(
            2 * np.pi * 300 * np.linspace(0, 0.01, 160, dtype=np.float32)
        )

        now = 0.0
        # Chunk 1: 160 samples -> 0 complete frames (needs 320), 160 pending
        self.update_vad_and_buffers(state, speech_10ms, now=0.01, sample_rate=sample_rate)
        self.assertFalse(state["is_speaking"])
        self.assertEqual(len(state["vad_pending_pcm"]), 160)

        # Chunk 2: +160 samples -> 320 total -> 1 frame processed, 0 pending
        self.update_vad_and_buffers(state, speech_10ms, now=0.02, sample_rate=sample_rate)
        self.assertEqual(state["speech_frame_count"], 1)
        self.assertEqual(len(state["vad_pending_pcm"]), 0)

        # Chunks 3-6: 4 more 10ms chunks -> 2 more frames processed -> speech_frame_count reaches 3!
        for i in range(4):
            self.update_vad_and_buffers(
                state, speech_10ms, now=0.03 + i * 0.01, sample_rate=sample_rate
            )

        self.assertTrue(
            state["is_speaking"], "VAD failed to detect speech across 10ms chunk boundaries!"
        )
        self.assertGreaterEqual(state["speech_frame_count"], 3)

    def test_chunk_boundary_invariance_for_speech_detection(self):
        """Verify identical speech audio produces identical VAD start/end detection regardless of chunk boundaries:
        - 640 samples (40ms)
        - 320 samples (20ms)
        - 160 samples (10ms)
        - uneven fractional chunks (simulating 44.1kHz -> 16kHz resampling)
        """
        sample_rate = 16000
        # Audio: 0.3s silence + 1.2s speech + 0.8s silence
        pre = np.zeros(int(sample_rate * 0.3), dtype=np.float32)
        t = np.linspace(0, 1.2, int(sample_rate * 1.2), dtype=np.float32)
        speech = 0.1 * np.sin(2 * np.pi * 300 * t)
        post = np.zeros(int(sample_rate * 0.8), dtype=np.float32)
        full_audio = np.concatenate([pre, speech, post])

        # Define chunk partitioning strategies
        chunk_patterns = {
            "40ms_fixed": [640] * (len(full_audio) // 640),
            "20ms_fixed": [320] * (len(full_audio) // 320),
            "10ms_fixed": [160] * (len(full_audio) // 160),
            "fractional_uneven": [],
        }
        for key in ("40ms_fixed", "20ms_fixed", "10ms_fixed"):
            rem = len(full_audio) - sum(chunk_patterns[key])
            if rem > 0:
                chunk_patterns[key].append(rem)

        # Fractional uneven pattern (simulating variable callback sizes)
        remaining = len(full_audio)
        sizes = [145, 186, 93, 204, 311, 73, 160, 480]
        idx = 0
        while remaining > 0:
            sz = min(remaining, sizes[idx % len(sizes)])
            chunk_patterns["fractional_uneven"].append(sz)
            remaining -= sz
            idx += 1

        results = {}
        for pattern_name, chunks in chunk_patterns.items():
            state = self.new_stream_state()
            now = 0.0
            offset = 0
            finalized_pcm = None

            for sz in chunks:
                samples = full_audio[offset : offset + sz]
                offset += sz
                now += sz / sample_rate

                self.update_vad_and_buffers(state, samples, now, sample_rate)

                if state["is_speaking"] and state["last_voice_at"] is not None:
                    if (now - state["last_voice_at"]) >= 0.65:
                        finalized_pcm = state["full_utterance_pcm"].copy()
                        state["is_speaking"] = False
                        break

            self.assertIsNotNone(
                finalized_pcm,
                f"Pattern {pattern_name} failed to finalize speech utterance",
            )
            results[pattern_name] = len(finalized_pcm)

        base_len = results["20ms_fixed"]
        for pattern_name, pcm_len in results.items():
            # Allow minor packet boundary quantization jitter (<= 2 chunks / 80ms)
            self.assertAlmostEqual(
                pcm_len,
                base_len,
                delta=int(sample_rate * 0.10),
                msg=f"Pattern {pattern_name} produced unexpected PCM length {pcm_len} vs {base_len}",
            )

    async def test_asr_handler_processes_binary_audio_without_name_errors(self):
        """Verify real asr_handler coroutine processes binary PCM without NameError or runtime failure."""
        pcm_chunk = np.zeros(640, dtype=np.float32).tobytes()

        messages = [
            pcm_chunk,
            json.dumps({"cmd": "audio_stream", "stream": "discord"}),
            pcm_chunk,
            json.dumps({"cmd": "ping"}),
        ]

        ws = MockWebSocket(messages)
        # Execute the real asr_handler with mock websocket
        await asyncio.wait_for(self.asr_handler(ws), timeout=3.0)

        # Verify device status and pong were emitted to client
        sent_types = [json.loads(m).get("type") for m in ws.sent if "type" in json.loads(m)]
        sent_statuses = [json.loads(m).get("status") for m in ws.sent if "status" in json.loads(m)]

        self.assertIn("device_status", sent_types)
        self.assertIn("pong", sent_statuses)

    async def test_asr_handler_processes_10ms_sub_frame_chunks(self):
        """Verify real asr_handler coroutine handles 10ms (160 samples) chunks without failure."""
        pcm_10ms = np.zeros(160, dtype=np.float32).tobytes()

        # Stream multiple 10ms packets
        messages = [pcm_10ms for _ in range(8)]
        messages.append(json.dumps({"cmd": "ping"}))

        ws = MockWebSocket(messages)
        await asyncio.wait_for(self.asr_handler(ws), timeout=3.0)

        sent_statuses = [json.loads(m).get("status") for m in ws.sent if "status" in json.loads(m)]
        self.assertIn("pong", sent_statuses)


if __name__ == "__main__":
    unittest.main()
