"""Dependency-free contract checks for the portable ASR WebSocket server."""

import ast
from pathlib import Path
import unittest


SERVER_SOURCE = Path(__file__).with_name("asr_server.py")


class AsrServerContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.source = SERVER_SOURCE.read_text(encoding="utf-8")
        cls.tree = ast.parse(cls.source, filename=str(SERVER_SOURCE))
        function = next(
            node
            for node in cls.tree.body
            if isinstance(node, ast.FunctionDef)
            and node.name == "remember_transcript"
        )
        namespace = {}
        exec(
            compile(
                ast.Module(body=[function], type_ignores=[]),
                str(SERVER_SOURCE),
                "exec",
            ),
            namespace,
        )
        cls.remember_transcript = staticmethod(namespace["remember_transcript"])

    def test_short_vad_threshold_and_final_timeout_are_explicit(self):
        constants = {}
        for node in ast.walk(self.tree):
            if isinstance(node, ast.Assign) and len(node.targets) == 1:
                target = node.targets[0]
                if isinstance(target, ast.Name) and isinstance(node.value, ast.Constant):
                    constants[target.id] = node.value.value

        self.assertLessEqual(constants["MIN_AUDIO_SECONDS"], 0.25)
        self.assertEqual(constants["PARTIAL_POLL_INTERVAL_SECONDS"], 0.08)
        self.assertGreaterEqual(constants["SHORT_UTTERANCE_FINAL_TIMEOUT_SECONDS"], 0.7)

    def test_partial_and_final_messages_preserve_stream_and_is_final_contract(self):
        self.assertIn('"is_final": False', self.source)
        self.assertIn('"is_final": True', self.source)
        self.assertGreaterEqual(self.source.count('"stream": stream_name'), 2)
        self.assertIn("stream_states =", self.source)
        self.assertIn("for stream_name, state in list(stream_states.items())", self.source)
        self.assertIn("stable_text = remember_transcript(", self.source)

    def test_stream_switch_and_reset_keep_buffers_isolated(self):
        self.assertIn('cmd == "audio_stream"', self.source)
        self.assertIn('stream_states.setdefault(active_stream, new_stream_state())', self.source)
        self.assertIn('for state in stream_states.values():', self.source)
        self.assertIn('active_stream = "mic"', self.source)

    def test_flush_has_a_final_path_when_no_partial_was_emitted(self):
        """A short VAD buffer must not be dropped just because partial was absent."""
        flush_start = self.source.index('cmd == "flush"')
        flush_end = self.source.index('cmd == "ping"', flush_start)
        flush_source = self.source[flush_start:flush_end]

        self.assertIn('state["audio_buffer"]', flush_source)
        self.assertIn('await transcribe_buffer(', flush_source)
        self.assertIn('state["audio_buffer"], allow_short=True', flush_source)
        self.assertIn('state["last_partial_text"]', flush_source)
        self.assertIn('len(state["audio_buffer"]) > 0', flush_source)
        self.assertIn('"is_final": True', flush_source)

    def test_vad_timeout_logs_why_an_unfinalized_buffer_was_reset(self):
        self.assertIn('"reason=no_transcript samples=%d"', self.source)
        self.assertIn('"audio_started_at": None', self.source)
        self.assertIn('reset_stream_state(state)', self.source)

    def test_short_buffer_uses_silence_age_for_final_only_fallback(self):
        self.assertIn('"last_audio_at": None', self.source)
        self.assertIn('state["last_audio_at"] = loop.time()', self.source)
        self.assertIn('allow_short=True', self.source)
        self.assertIn('SHORT_UTTERANCE_FINAL_TIMEOUT_SECONDS', self.source)

    def test_final_keeps_complete_partial_when_sliding_window_regresses(self):
        """A later tail-only Whisper revision must not erase the utterance."""
        candidates = [
            "ごめん",
            "というわけでね",
            "というわけでねやっていきたいと思う",
            "というわけでねやっていきたいと思うんですけれども",
            "ですけれども",
            "ごちそう",
        ]
        best = ""
        for candidate in candidates:
            best = self.remember_transcript(best, candidate)

        self.assertEqual(
            best,
            "というわけでねやっていきたいと思うんですけれども",
        )


if __name__ == "__main__":
    unittest.main()
