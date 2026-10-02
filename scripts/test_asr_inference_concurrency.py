"""Exercise the real ASR inference coroutine without loading GPU/ML packages."""

import ast
import asyncio
import logging
import threading
import time
import unittest
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from types import SimpleNamespace
from typing import Any


class Model:
    def __init__(self, action=None, text="recognized"):
        self.action = action
        self.text = text
        self.calls = 0

    def transcribe(self, *args, **kwargs):
        self.calls += 1
        if self.action:
            self.action()
        return iter([SimpleNamespace(text=self.text)]), None


class InferenceConcurrencyTests(unittest.IsolatedAsyncioTestCase):
    def make_server(self, gpu=None, cpu_factory=None, workers=2):
        tree = ast.parse(
            Path(__file__).with_name("asr_server.py").read_text(encoding="utf-8")
        )
        pool = ThreadPoolExecutor(max_workers=workers)
        self.addCleanup(pool.shutdown)
        loop = asyncio.get_running_loop()
        jobs = []

        def submit(_, fn):
            job = pool.submit(fn)
            jobs.append(job)
            return asyncio.wrap_future(job)

        def unexpected_exit(code):
            raise AssertionError(f"Unexpected process exit: {code}")

        ns: dict[str, Any] = dict(
            asyncio=asyncio,
            time=time,
            threading=threading,
            sys=SimpleNamespace(argv=[]),
            os=SimpleNamespace(environ={}, _exit=unexpected_exit),
            torch=SimpleNamespace(
                cuda=SimpleNamespace(
                    is_available=lambda: False,
                    empty_cache=lambda: None,
                    OutOfMemoryError=type("TorchOOM", (Exception,), {}),
                )
            ),
            logger=logging.getLogger("asr-concurrency-test"),
            loop=SimpleNamespace(time=loop.time, run_in_executor=submit),
            sample_rate=1,
            MIN_AUDIO_SECONDS=0.25,
            send_queue=asyncio.Queue(),
            current_device="cuda",
            whisper_model=gpu or Model(),
            create_cpu_whisper_model=cpu_factory or (lambda: Model(text="cpu")),
            in_flight_since=None,
            inference_stall_streak=0,
            _in_flight_skip_log_armed=True,
            last_switch_reason=None,
            _device_switch_lock=threading.Lock(),
            INFERENCE_IN_FLIGHT_EXIT_SECONDS=10.0,
            INFERENCE_STALL_EXIT_THRESHOLD=3,
            REASON_GPU_OOM="gpu_oom",
            REASON_CUDA_ERROR="cuda_error",
            REASON_INFERENCE_TIMEOUT="inference_timeout",
            FATAL_CUDA_PATTERNS=("illegal memory access",),
            record_switch_diagnostic=lambda *args, **kwargs: None,
            np=SimpleNamespace(zeros=lambda size, dtype=None: [0] * size, float32=float),
            SAMPLE_RATE=16000,
            DEFAULT_INFERENCE_TIMEOUT_SECONDS=3.0,
            FIRST_INFERENCE_TIMEOUT_SECONDS=10.0,
            _is_first_inference=True,
            _cold_start_inference_ms=None,
            _warmup_inference_ms=None,
            current_model_name="kotoba-whisper-v2.0-faster",
            current_compute_type="float16",
        )
        names = {
            "is_cuda_oom_error",
            "classify_inference_error",
            "is_fatal_cuda_error",
            "vram_snapshot",
            "vram_status_text",
            "record_inference_stall",
            "reset_inference_stall_streak",
            "note_in_flight_gate",
            "clear_in_flight_gate",
            "note_in_flight_skip_log",
            "_CpuFallbackRequest",
            "_perform_cpu_switch",
            "warmup_whisper_model",
            "transcribe_buffer",
        }
        nodes: list[ast.stmt] = []
        for node in ast.walk(tree):
            if (
                isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef))
                and node.name in names
            ):
                nodes.append(node)
        exec(
            compile(ast.Module(body=nodes, type_ignores=[]), "<ASR functions>", "exec"),
            ns,
        )
        return ns, jobs, pool

    def gate(self):
        entered, release = threading.Event(), threading.Event()
        self.addCleanup(release.set)

        def block():
            entered.set()
            if not release.wait(5):
                raise AssertionError("Test did not release native operation")

        return entered, release, block

    async def entered(self, event):
        self.assertTrue(await asyncio.to_thread(event.wait, 1))

    async def test_oom_switch_excludes_new_inference_until_cpu_retry_finishes(self):
        entered, release, block = self.gate()
        retry_entered, retry_release, retry_block = self.gate()
        cpu = Model(action=retry_block, text="cpu")
        error = RuntimeError("CUDA out of memory")

        def oom():
            raise error

        def load_cpu():
            block()
            return cpu

        gpu = Model(action=oom)
        ns, jobs, _ = self.make_server(gpu, load_cpu)
        diagnostics = []
        ns["record_switch_diagnostic"] = lambda *args, **kw: diagnostics.append(
            (args, kw)
        )
        first = asyncio.create_task(ns["transcribe_buffer"]([1]))
        try:
            await self.entered(entered)
            second = await asyncio.gather(
                ns["transcribe_buffer"]([2]), return_exceptions=True
            )
            release.set()
            await self.entered(retry_entered)
            during_retry = await ns["transcribe_buffer"]([3])
        finally:
            release.set()
            retry_release.set()
        self.assertEqual((await first)[0], "cpu")
        self.assertEqual(second, [("", 0.0)])
        self.assertEqual(during_retry, ("", 0.0))
        self.assertEqual(len(jobs), 1)
        self.assertEqual((gpu.calls, cpu.calls), (1, 1))
        self.assertIs(diagnostics[0][1]["exc"], error)
        self.assertEqual(ns["send_queue"].get_nowait()["reason"], "gpu_oom")

    async def test_submission_reserves_inference_before_executor_starts(self):
        ns, jobs, pool = self.make_server(workers=1)
        entered, release, block = self.gate()
        pool.submit(block)
        await self.entered(entered)
        first = asyncio.create_task(ns["transcribe_buffer"]([1]))
        await asyncio.sleep(0)
        second = asyncio.create_task(ns["transcribe_buffer"]([2]))
        await asyncio.sleep(0)
        submitted = len(jobs)
        release.set()
        await asyncio.gather(first, second)
        self.assertEqual(
            submitted, 1, "A queued inference must exclude a second submission"
        )

    async def test_timeout_keeps_native_job_exclusive_until_it_finishes(self):
        entered, release, block = self.gate()
        ns, jobs, _ = self.make_server(Model(action=block))
        ns["_is_first_inference"] = False
        try:
            result = await ns["transcribe_buffer"]([1])
            self.assertEqual(result[0], "")
            self.assertEqual(await ns["transcribe_buffer"]([2]), ("", 0.0))
            self.assertEqual(len(jobs), 1)
            self.assertEqual(ns["current_device"], "cuda")
        finally:
            release.set()
        await asyncio.wrap_future(jobs[0])
        self.assertEqual((await ns["transcribe_buffer"]([3]))[0], "recognized")

    async def test_first_inference_transitions_to_steady_state(self):
        ns, _, _ = self.make_server()
        self.assertTrue(ns["_is_first_inference"])
        self.assertIsNone(ns["_cold_start_inference_ms"])

        text, lat = await ns["transcribe_buffer"]([1])
        self.assertEqual(text, "recognized")
        self.assertFalse(ns["_is_first_inference"])
        self.assertIsNotNone(ns["_cold_start_inference_ms"])
        self.assertGreaterEqual(ns["_cold_start_inference_ms"], 0.0)

    async def test_first_inference_timeout_records_diagnostic_extra(self):
        entered, release, block = self.gate()
        ns, jobs, _ = self.make_server(Model(action=block))
        ns["FIRST_INFERENCE_TIMEOUT_SECONDS"] = 0.05
        diagnostics = []
        ns["record_switch_diagnostic"] = lambda *args, **kw: diagnostics.append(
            (args, kw)
        )
        try:
            result = await ns["transcribe_buffer"]([1])
            self.assertEqual(result[0], "")
        finally:
            release.set()
        await asyncio.wrap_future(jobs[0])

        self.assertEqual(len(diagnostics), 1)
        self.assertEqual(diagnostics[0][0][0], "inference_timeout")
        extra = diagnostics[0][1].get("extra", {})
        self.assertTrue(extra.get("is_first_inference"))
        self.assertEqual(extra.get("timeout_budget"), 0.05)

    async def test_cpu_switch_resets_first_inference_flag(self):
        ns, _, _ = self.make_server()
        ns["_is_first_inference"] = False

        ns["_perform_cpu_switch"]("gpu_oom")
        self.assertTrue(ns["_is_first_inference"])
        self.assertEqual(ns["current_device"], "cpu")

    async def test_cancelled_waiter_does_not_cancel_queued_native_job(self):
        ns, jobs, pool = self.make_server(workers=1)
        entered, release, block = self.gate()
        pool.submit(block)
        await self.entered(entered)
        first = asyncio.create_task(ns["transcribe_buffer"]([1]))
        await asyncio.sleep(0)
        first.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await first
        release.set()
        await asyncio.sleep(0)
        self.assertFalse(
            jobs[0].cancelled(), "Queued owner must run and release its reservation"
        )
        await asyncio.wrap_future(jobs[0])
        self.assertEqual((await ns["transcribe_buffer"]([2]))[0], "recognized")

    async def test_submission_failure_releases_admission(self):
        ns, _, _ = self.make_server()
        submit = ns["loop"].run_in_executor

        def fail(*args):
            raise RuntimeError("executor unavailable")

        ns["loop"].run_in_executor = fail
        with self.assertRaisesRegex(RuntimeError, "executor unavailable"):
            await ns["transcribe_buffer"]([1])
        ns["loop"].run_in_executor = submit
        self.assertEqual((await ns["transcribe_buffer"]([2]))[0], "recognized")

    async def test_input_error_releases_admission_without_cpu_switch(self):
        def fail():
            raise ValueError("invalid audio")

        model = Model(action=fail)
        ns, _, _ = self.make_server(model)
        with self.assertLogs("asr-concurrency-test", level="ERROR"):
            with self.assertRaisesRegex(ValueError, "invalid audio"):
                await ns["transcribe_buffer"]([1])
        self.assertEqual(ns["current_device"], "cuda")
        model.action = None
        self.assertEqual((await ns["transcribe_buffer"]([2]))[0], "recognized")


if __name__ == "__main__":
    unittest.main()
