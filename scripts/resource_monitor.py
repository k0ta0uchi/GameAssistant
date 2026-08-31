import os
import sys
import ctypes
from contextlib import contextmanager
from typing import Dict, Any

class ResourceMonitor:
    """
    リアルタイムのシステムRAMおよびGPU VRAM使用率を取得するモニタークラス
    (Win32 API / PyTorch / NVML 対応)
    """
    def __init__(self):
        self.pid = os.getpid()

    def get_memory_info(self) -> Dict[str, Any]:
        """
        アプリ単体およびシステム全体のRAM/VRAM情報を返す
        """
        # --- 1. RAM (システムメモリ) ---
        app_ram_bytes = 0
        sys_ram_used_bytes = 0
        sys_ram_total_bytes = 0
        sys_ram_percent = 0.0

        # psutil があれば最優先使用
        try:
            import psutil
            process = psutil.Process(self.pid)
            app_ram_bytes = process.memory_info().rss
            for child in process.children(recursive=True):
                try:
                    app_ram_bytes += child.memory_info().rss
                except (psutil.NoSuchProcess, psutil.AccessDenied):
                    pass

            sys_mem = psutil.virtual_memory()
            sys_ram_used_bytes = sys_mem.used
            sys_ram_total_bytes = sys_mem.total
            sys_ram_percent = sys_mem.percent
        except Exception:
            # Win32 API (ctypes) でのフォールバック
            try:
                # システム全体 RAM
                class MEMORYSTATUSEX(ctypes.Structure):
                    _fields_ = [
                        ('dwLength', ctypes.c_ulong),
                        ('dwMemoryLoad', ctypes.c_ulong),
                        ('ullTotalPhys', ctypes.c_ulonglong),
                        ('ullAvailPhys', ctypes.c_ulonglong),
                        ('ullTotalPageFile', ctypes.c_ulonglong),
                        ('ullAvailPageFile', ctypes.c_ulonglong),
                        ('ullTotalVirtual', ctypes.c_ulonglong),
                        ('ullAvailVirtual', ctypes.c_ulonglong),
                        ('ullAvailExtendedVirtual', ctypes.c_ulonglong),
                    ]
                stat = MEMORYSTATUSEX()
                stat.dwLength = ctypes.sizeof(MEMORYSTATUSEX)
                if ctypes.windll.kernel32.GlobalMemoryStatusEx(ctypes.byref(stat)):
                    sys_ram_total_bytes = stat.ullTotalPhys
                    sys_ram_used_bytes = stat.ullTotalPhys - stat.ullAvailPhys
                    sys_ram_percent = float(stat.dwMemoryLoad)

                # プロセス単体 RAM
                class PROCESS_MEMORY_COUNTERS(ctypes.Structure):
                    _fields_ = [
                        ('cb', ctypes.c_ulong),
                        ('PageFaultCount', ctypes.c_ulong),
                        ('PeakWorkingSetSize', ctypes.c_size_t),
                        ('WorkingSetSize', ctypes.c_size_t),
                        ('QuotaPeakPagedPoolUsage', ctypes.c_size_t),
                        ('QuotaPagedPoolUsage', ctypes.c_size_t),
                        ('QuotaPeakNonPagedPoolUsage', ctypes.c_size_t),
                        ('QuotaNonPagedPoolUsage', ctypes.c_size_t),
                        ('PagefileUsage', ctypes.c_size_t),
                        ('PeakPagefileUsage', ctypes.c_size_t),
                    ]
                counters = PROCESS_MEMORY_COUNTERS()
                counters.cb = ctypes.sizeof(PROCESS_MEMORY_COUNTERS)
                handle = ctypes.windll.kernel32.OpenProcess(0x0400 | 0x0010, False, self.pid)
                if handle:
                    if ctypes.windll.psapi.GetProcessMemoryInfo(handle, ctypes.byref(counters), counters.cb):
                        app_ram_bytes = counters.WorkingSetSize
                    ctypes.windll.kernel32.CloseHandle(handle)
            except Exception:
                pass

        # --- 2. VRAM (GPUメモリ) ---
        app_vram_bytes = 0
        sys_vram_used_bytes = 0
        sys_vram_total_bytes = 0
        sys_vram_percent = 0.0
        has_vram = False

        # 方式A: PyTorch (torch.cuda)
        try:
            import torch
            if torch.cuda.is_available():
                has_vram = True
                app_vram_bytes = torch.cuda.memory_allocated()
                free_b, total_b = torch.cuda.mem_get_info()
                sys_vram_total_bytes = total_b
                sys_vram_used_bytes = total_b - free_b
                if sys_vram_total_bytes > 0:
                    sys_vram_percent = (sys_vram_used_bytes / sys_vram_total_bytes) * 100.0
        except Exception:
            pass

        # 方式B: NVML (pynvml) での精度向上
        try:
            import pynvml
            pynvml.nvmlInit()
            device_count = pynvml.nvmlDeviceGetCount()
            if device_count > 0:
                handle = pynvml.nvmlDeviceGetHandleByIndex(0)
                mem_info = pynvml.nvmlDeviceGetMemoryInfo(handle)
                sys_vram_total_bytes = mem_info.total
                sys_vram_used_bytes = mem_info.used
                if sys_vram_total_bytes > 0:
                    sys_vram_percent = (sys_vram_used_bytes / sys_vram_total_bytes) * 100.0
                has_vram = True

                nvml_app_vram = 0
                procs = []
                try:
                    procs.extend(pynvml.nvmlDeviceGetComputeRunningProcesses(handle))
                except Exception:
                    pass
                try:
                    procs.extend(pynvml.nvmlDeviceGetGraphicsRunningProcesses(handle))
                except Exception:
                    pass

                for p in procs:
                    if p.pid == self.pid:
                        nvml_app_vram += getattr(p, 'usedGpuMemory', 0) or 0

                if nvml_app_vram > 0:
                    app_vram_bytes = nvml_app_vram
        except Exception:
            pass

        return {
            "app_ram": self._format_bytes(app_ram_bytes),
            "sys_ram_used": self._format_bytes(sys_ram_used_bytes),
            "sys_ram_total": self._format_bytes(sys_ram_total_bytes),
            "sys_ram_percent": f"{sys_ram_percent:.1f}%",
            "sys_ram_percent_val": round(sys_ram_percent, 1),
            "has_vram": has_vram,
            "app_vram": self._format_bytes(app_vram_bytes) if has_vram else "N/A",
            "sys_vram_used": self._format_bytes(sys_vram_used_bytes) if has_vram else "N/A",
            "sys_vram_total": self._format_bytes(sys_vram_total_bytes) if has_vram else "N/A",
            "sys_vram_percent": f"{sys_vram_percent:.1f}%" if has_vram else "N/A",
            "sys_vram_percent_val": round(sys_vram_percent, 1) if has_vram else 0.0,
        }

    @staticmethod
    def _format_bytes(size_bytes: int) -> str:
        if size_bytes <= 0:
            return "0 B"
        if size_bytes >= 1024 ** 3:
            return f"{size_bytes / (1024 ** 3):.2f} GB"
        if size_bytes >= 1024 ** 2:
            return f"{size_bytes / (1024 ** 2):.1f} MB"
        return f"{size_bytes / 1024:.1f} KB"

class VRAMPreallocator:
    """
    VRAM 事前確保 (1GB) / 解放およびやりくり (pool_context) を管理するクラス
    """
    _buffer = None
    _is_enabled = False

    @classmethod
    def set_preallocation(cls, enable: bool, size_mb: int = 1024) -> bool:
        """
        VRAM のパディング割り当て (1GB) または解放を行う
        """
        cls._is_enabled = enable
        if enable:
            return cls._allocate()
        else:
            return cls._free()

    @classmethod
    def _allocate(cls) -> bool:
        if cls._buffer is not None:
            return True
        try:
            import torch
            if torch.cuda.is_available():
                # 1024MB = 1024 * 1024 * 1024 bytes = 256 * 1024 * 1024 float32 (4bytes)
                cls._buffer = torch.zeros((1024, 1024, 256), dtype=torch.float32, device='cuda')
                return True
        except Exception:
            cls._buffer = None
            return False
        return False

    @classmethod
    def _free(cls) -> bool:
        cls._buffer = None
        try:
            import torch
            if torch.cuda.is_available():
                torch.cuda.empty_cache()
        except Exception:
            pass
        return True

    @classmethod
    @contextmanager
    def pool_context(cls):
        """
        推論・重い処理の実行時に一時的に1GB予約バッファを解放してその領域を推論アロケータにやりくりさせ、
        処理完了後に不要なメモリキャッシュをクリアして1GB予約状態へ復元するコンテキストマネージャー。
        """
        was_allocated = (cls._buffer is not None)
        if was_allocated:
            cls._free()
        try:
            yield
        finally:
            if cls._is_enabled:
                try:
                    import torch
                    if torch.cuda.is_available():
                        torch.cuda.empty_cache()
                except Exception:
                    pass
                cls._allocate()
