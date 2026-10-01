"""
Regression tests and pipeline validation for Issue #30:
ASR Streaming Pipeline Accuracy, Buffering, VAD Endpointing, and Latency Optimization.
"""

import ast
import json
import os
import sys
import unittest
from pathlib import Path

import numpy as np

SERVER_SOURCE_PATH = Path(__file__).with_name("asr_server.py")


class AsrPipelineRegressionTests(unittest.TestCase):
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
        # Compile get_model_spec in a sandboxed namespace
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
        # Quality preset ensures identical model comparison between CPU and GPU
        self.assertEqual(path, path_cpu)

    def test_speech_end_based_on_silence_not_transcript_stillness(self):
        """Verify speech-end endpointing is based on real audio silence time, not transcript stillness."""
        # In asr_server.py:
        # Check that silence_timeout uses last_voice_at and SPEECH_END_SILENCE_SECONDS
        self.assertIn('now - state["last_voice_at"]) >= SPEECH_END_SILENCE_SECONDS', self.source)
        # Ensure we do not use transcript stillness (e.g. silence_start_time from text match) for finalization
        self.assertNotIn('now - silence_start_time) >= timeout', self.source)

    def test_full_utterance_preservation_for_short_medium_long(self):
        """Simulate VAD pipeline with short (<1s), medium (4s), and long (12s) utterances
        and verify PCM is never clipped to 3 seconds."""
        sample_rate = 16000
        frame_size = 320  # 20ms
        energy_threshold = 0.012

        for duration_s in [0.8, 4.0, 12.0]:
            total_samples = int(sample_rate * duration_s)
            t = np.linspace(0, duration_s, total_samples, dtype=np.float32)
            # Simulated speech: sine wave with 0.1 amplitude (well above 0.012 threshold)
            speech_pcm = 0.1 * np.sin(2 * np.pi * 300 * t)

            # Pre-roll silence (300ms)
            pre_silence = np.zeros(int(sample_rate * 0.3), dtype=np.float32)
            # Post speech silence (800ms to trigger endpointing)
            post_silence = np.zeros(int(sample_rate * 0.8), dtype=np.float32)

            full_audio = np.concatenate([pre_silence, speech_pcm, post_silence])

            # Simulate state machine
            state = {
                "full_utterance_pcm": np.array([], dtype=np.float32),
                "pre_roll_pcm": np.array([], dtype=np.float32),
                "is_speaking": False,
                "speech_frame_count": 0,
                "silence_frame_count": 0,
                "last_voice_at": None,
                "force_endpoint": False,
            }

            pre_roll_max = int(sample_rate * 0.20)
            now = 0.0
            chunk_size = 640  # 40ms packets

            finalized_pcm = None

            for i in range(0, len(full_audio), chunk_size):
                samples = full_audio[i : i + chunk_size]
                now += len(samples) / sample_rate

                for f_idx in range(0, len(samples), frame_size):
                    frame = samples[f_idx : f_idx + frame_size]
                    if len(frame) < frame_size:
                        break
                    rms = float(np.sqrt(np.mean(frame**2)))

                    if rms >= energy_threshold:
                        state["speech_frame_count"] += 1
                        state["silence_frame_count"] = 0
                        state["last_voice_at"] = now

                        if not state["is_speaking"]:
                            if state["speech_frame_count"] >= 3:
                                state["is_speaking"] = True
                                state["full_utterance_pcm"] = state["pre_roll_pcm"].copy()
                    else:
                        state["silence_frame_count"] += 1
                        state["speech_frame_count"] = 0

                if state["is_speaking"]:
                    state["full_utterance_pcm"] = np.concatenate(
                        [state["full_utterance_pcm"], samples]
                    )
                else:
                    state["pre_roll_pcm"] = np.concatenate(
                        [state["pre_roll_pcm"], samples]
                    )
                    if len(state["pre_roll_pcm"]) > pre_roll_max:
                        state["pre_roll_pcm"] = state["pre_roll_pcm"][-pre_roll_max:]

                # Check endpoint
                if state["is_speaking"] and state["last_voice_at"] is not None:
                    if (now - state["last_voice_at"]) >= 0.65:
                        finalized_pcm = state["full_utterance_pcm"].copy()
                        state["is_speaking"] = False
                        break

            self.assertIsNotNone(
                finalized_pcm,
                f"Failed to finalize utterance of duration {duration_s}s",
            )
            # The finalized PCM must include the full speech AND the pre-roll (no 3s cutoff!)
            finalized_duration = len(finalized_pcm) / sample_rate
            self.assertGreaterEqual(
                finalized_duration,
                duration_s,
                f"Utterance of {duration_s}s was truncated to {finalized_duration}s!",
            )
            # Check that it did NOT truncate to 3.0 seconds
            if duration_s > 3.0:
                self.assertGreater(
                    finalized_duration,
                    3.5,
                    f"Utterance was incorrectly capped at 3s: {finalized_duration}s",
                )

    def test_intra_utterance_pause_does_not_prematurely_finalize(self):
        """A 350ms pause during speaking must not finalize or reset the buffer."""
        sample_rate = 16000
        # Speech part 1 (1.5s)
        s1 = 0.1 * np.sin(2 * np.pi * 300 * np.linspace(0, 1.5, int(sample_rate * 1.5), dtype=np.float32))
        # Short pause (350ms, less than 650ms endpoint)
        pause = np.zeros(int(sample_rate * 0.35), dtype=np.float32)
        # Speech part 2 (1.5s)
        s2 = 0.1 * np.sin(2 * np.pi * 300 * np.linspace(0, 1.5, int(sample_rate * 1.5), dtype=np.float32))
        # Final silence (800ms to end)
        post = np.zeros(int(sample_rate * 0.8), dtype=np.float32)

        full_audio = np.concatenate([s1, pause, s2, post])

        state = {
            "full_utterance_pcm": np.array([], dtype=np.float32),
            "pre_roll_pcm": np.array([], dtype=np.float32),
            "is_speaking": False,
            "speech_frame_count": 0,
            "silence_frame_count": 0,
            "last_voice_at": None,
        }

        now = 0.0
        chunk_size = 640
        frame_size = 320
        finalized_count = 0

        for i in range(0, len(full_audio), chunk_size):
            samples = full_audio[i : i + chunk_size]
            now += len(samples) / sample_rate

            for f_idx in range(0, len(samples), frame_size):
                frame = samples[f_idx : f_idx + frame_size]
                if len(frame) < frame_size:
                    break
                rms = float(np.sqrt(np.mean(frame**2)))
                if rms >= 0.012:
                    state["speech_frame_count"] += 1
                    state["silence_frame_count"] = 0
                    state["last_voice_at"] = now
                    if not state["is_speaking"] and state["speech_frame_count"] >= 3:
                        state["is_speaking"] = True
                        state["full_utterance_pcm"] = state["pre_roll_pcm"].copy()
                else:
                    state["silence_frame_count"] += 1
                    state["speech_frame_count"] = 0

            if state["is_speaking"]:
                state["full_utterance_pcm"] = np.concatenate(
                    [state["full_utterance_pcm"], samples]
                )

            # Endpoint check
            if state["is_speaking"] and state["last_voice_at"] is not None:
                if (now - state["last_voice_at"]) >= 0.65:
                    finalized_count += 1
                    state["is_speaking"] = False
                    state["full_utterance_pcm"] = np.array([], dtype=np.float32)

        # Must finalize exactly ONCE at the end, not during the 350ms pause!
        self.assertEqual(
            finalized_count,
            1,
            f"Expected exactly 1 finalization, but got {finalized_count} (prematurely finalized during pause)",
        )


if __name__ == "__main__":
    unittest.main()
