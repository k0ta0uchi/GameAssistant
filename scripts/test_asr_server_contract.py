"""Dependency-free contract checks for the portable ASR WebSocket server."""

import ast
import unittest
from pathlib import Path
from typing import Callable

SERVER_SOURCE = Path(__file__).with_name("asr_server.py")


class AsrServerContractTests(unittest.TestCase):
    # AST-extracted server functions, bound in setUpClass. Declared here so
    # static checkers can see the dynamically attached attributes.
    remember_transcript: Callable[..., str]
    is_cuda_oom_error: Callable[[BaseException], bool]
    classify_inference_error: Callable[[BaseException], str]
    torch_oom_stub: Callable[..., Exception]

    @classmethod
    def setUpClass(cls):
        cls.source = SERVER_SOURCE.read_text(encoding="utf-8")
        cls.tree = ast.parse(cls.source, filename=str(SERVER_SOURCE))

        # remember_transcript is self-contained.
        cls.remember_transcript = staticmethod(
            cls._exec_module_function("remember_transcript")
        )
        # The classifier delegates to is_cuda_oom_error and references the
        # REASON_* constants, so both functions and the constants run in one
        # shared namespace.
        classifier_ns = {}
        for node in cls.tree.body:
            if (
                isinstance(node, ast.Assign)
                and len(node.targets) == 1
                and isinstance(node.targets[0], ast.Name)
                and node.targets[0].id.startswith("REASON_")
            ):
                exec(
                    compile(
                        ast.Module(body=[node], type_ignores=[]),
                        str(SERVER_SOURCE),
                        "exec",
                    ),
                    classifier_ns,
                )
        for name in ("is_cuda_oom_error", "classify_inference_error"):
            exec(
                compile(
                    ast.Module(body=[cls._find_function(name)], type_ignores=[]),
                    str(SERVER_SOURCE),
                    "exec",
                ),
                classifier_ns,
            )
        # classify_inference_error references torch.cuda.OutOfMemoryError; a
        # minimal stub keeps these checks dependency-free while still exercising
        # the isinstance branch.
        cls.torch_oom_stub = type("StubTorchOutOfMemoryError", (Exception,), {})
        classifier_ns["torch"] = type(
            "torch",
            (),
            {"cuda": type("cuda", (), {"OutOfMemoryError": cls.torch_oom_stub})},
        )
        cls.is_cuda_oom_error = staticmethod(classifier_ns["is_cuda_oom_error"])
        cls.classify_inference_error = staticmethod(
            classifier_ns["classify_inference_error"]
        )

    @classmethod
    def _find_function(cls, name):
        return next(
            node
            for node in cls.tree.body
            if isinstance(node, ast.FunctionDef) and node.name == name
        )

    @classmethod
    def _exec_module_function(cls, name):
        namespace = {}
        exec(
            compile(
                ast.Module(body=[cls._find_function(name)], type_ignores=[]),
                str(SERVER_SOURCE),
                "exec",
            ),
            namespace,
        )
        return namespace[name]

    def test_oom_detection_ignores_incidental_cuda_wording(self):
        # True: 真のVRAM枯渇 (torchのOOM文言 / CTranslate2の確保失敗) のみ。
        self.assertTrue(
            self.is_cuda_oom_error(
                RuntimeError("CUDA memory allocation failed: out of memory")
            )
        )
        self.assertTrue(
            self.is_cuda_oom_error(RuntimeError("CUDA error: out of memory"))
        )
        self.assertTrue(
            self.is_cuda_oom_error(RuntimeError("allocator: out_of_memory"))
        )
        # False: 'cuda' を含む一時的エラーでGPUセッションを失わせてはならない。
        # 以前は "cuda" in err_str の部分一致でVRAM空きありでも恒久CPUに落ちていた。
        self.assertFalse(
            self.is_cuda_oom_error(
                RuntimeError(
                    "cuDNN error: CUDNN_STATUS_NOT_SUPPORTED. This probably means..."
                )
            )
        )
        self.assertFalse(
            self.is_cuda_oom_error(
                RuntimeError("CUDA error: an illegal instruction was encountered")
            )
        )
        # API名だけではOOMと判定しない (確認済みメモリ不足メッセージに限定)。
        self.assertFalse(self.is_cuda_oom_error(RuntimeError("cudamalloc returned 2")))
        self.assertFalse(self.is_cuda_oom_error(ValueError("unrelated failure")))

    def test_inference_path_retries_gpu_before_cpu_fallback(self):
        # 旧実装の部分一致ルールの回帰ガード: 推論パスは真のOOMのみでCPUへ
        # フォールバックし、一時的CUDAエラーはキャッシュ解放後にGPUで再試行する。
        self.assertNotIn('"cuda" in err_str', self.source)
        self.assertIn("is_cuda_oom_error(exc)", self.source)
        self.assertIn("Transient CUDA error during inference", self.source)
        self.assertIn("retrying once on GPU", self.source)
        self.assertIn("def is_cuda_oom_error", self.source)
        self.assertIn("def vram_status_text", self.source)

    def test_error_classification_separates_oom_cuda_and_input(self):
        # OOM / 一時的CUDA障害 / 入力不正を別理由として分離する。
        self.assertEqual(
            self.classify_inference_error(
                RuntimeError("CUDA memory allocation failed: out of memory")
            ),
            "gpu_oom",
        )
        self.assertEqual(
            self.classify_inference_error(
                RuntimeError("cuDNN error: CUDNN_STATUS_INTERNAL_ERROR")
            ),
            "cuda_error",
        )
        self.assertEqual(
            self.classify_inference_error(ValueError("invalid audio buffer")),
            "input_error",
        )
        # torch.cuda.OutOfMemoryError インスタンスは文言に依らず gpu_oom。
        self.assertEqual(
            self.classify_inference_error(self.torch_oom_stub("CUDA out of memory")),
            "gpu_oom",
        )

    def test_timeout_is_not_treated_as_oom_and_never_switches_models(self):
        # 2秒タイムアウトは inference_timeout として記録され、OOM扱いしない。
        # モデル切替も行わない (実行中スレッドが旧モデルを使い続けるため)。
        self.assertNotIn("oom_or_timeout", self.source)
        self.assertIn('REASON_GPU_OOM = "gpu_oom"', self.source)
        self.assertIn('REASON_INFERENCE_TIMEOUT = "inference_timeout"', self.source)
        self.assertIn('REASON_CUDA_ERROR = "cuda_error"', self.source)
        self.assertIn("record_inference_stall(", self.source)
        self.assertIn("INFERENCE_STALL_EXIT_THRESHOLD", self.source)
        # 旧タイムアウト経路の恒久CPU切替 (モデル破棄) が戻っていないこと。
        self.assertNotIn("Attempting dynamic fallback to CPU", self.source)
        # 復旧不能なスタールは外側の監督処理に委ねる (プロセス終了で依頼)。
        self.assertIn("os._exit(87)", self.source)

    def test_switch_diagnostics_record_original_exception(self):
        # CPUへ切る前に例外の型・全文・スタック・処理時間・VRAM実測を記録する。
        self.assertIn("def record_switch_diagnostic", self.source)
        self.assertIn('"exception_type"', self.source)
        self.assertIn("traceback.format_exc", self.source)
        self.assertIn("device_switch_diagnostic", self.source)
        self.assertIn("is_fatal_cuda_error", self.source)

    def test_short_vad_threshold_and_final_timeout_are_explicit(self):
        constants = {}
        for node in ast.walk(self.tree):
            if isinstance(node, ast.Assign) and len(node.targets) == 1:
                target = node.targets[0]
                if isinstance(target, ast.Name) and isinstance(
                    node.value, ast.Constant
                ):
                    constants[target.id] = node.value.value

        self.assertLessEqual(constants["MIN_AUDIO_SECONDS"], 0.25)
        self.assertEqual(constants["PARTIAL_POLL_INTERVAL_SECONDS"], 0.08)
        self.assertGreaterEqual(constants["SHORT_UTTERANCE_FINAL_TIMEOUT_SECONDS"], 0.7)

    def test_partial_and_final_messages_preserve_stream_and_is_final_contract(self):
        self.assertIn('"is_final": False', self.source)
        self.assertIn('"is_final": True', self.source)
        self.assertGreaterEqual(self.source.count('"stream": stream_name'), 2)
        self.assertIn("stream_states =", self.source)
        self.assertIn(
            "for stream_name, state in list(stream_states.items())", self.source
        )
        self.assertIn("stable_text = remember_transcript(", self.source)

    def test_stream_switch_and_reset_keep_buffers_isolated(self):
        self.assertIn('cmd == "audio_stream"', self.source)
        self.assertIn(
            "stream_states.setdefault(active_stream, new_stream_state())", self.source
        )
        self.assertIn("for state in stream_states.values():", self.source)
        self.assertIn('active_stream = "mic"', self.source)

    def test_flush_has_a_final_path_when_no_partial_was_emitted(self):
        """A short VAD buffer must not be dropped just because partial was absent."""
        flush_start = self.source.index('cmd == "flush"')
        flush_end = self.source.index('cmd == "ping"', flush_start)
        flush_source = self.source[flush_start:flush_end]

        self.assertIn('state["audio_buffer"]', flush_source)
        self.assertIn("await transcribe_buffer(", flush_source)
        self.assertIn('state["audio_buffer"], allow_short=True', flush_source)
        self.assertIn('state["last_partial_text"]', flush_source)
        self.assertIn('len(state["audio_buffer"]) > 0', flush_source)
        self.assertIn('"is_final": True', flush_source)

    def test_vad_timeout_logs_why_an_unfinalized_buffer_was_reset(self):
        self.assertIn('"reason=no_transcript samples=%d"', self.source)
        self.assertIn('"audio_started_at": None', self.source)
        self.assertIn("reset_stream_state(state)", self.source)

    def test_short_buffer_uses_silence_age_for_final_only_fallback(self):
        self.assertIn('"last_audio_at": None', self.source)
        self.assertIn('state["last_audio_at"] = loop.time()', self.source)
        self.assertIn("allow_short=True", self.source)
        self.assertIn("SHORT_UTTERANCE_FINAL_TIMEOUT_SECONDS", self.source)

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
