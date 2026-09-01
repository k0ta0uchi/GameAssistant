# -*- coding: utf-8 -*-
import sys
import os
import io
import time
import json
import logging
import asyncio
import base64
import threading
from typing import List, Dict, Any, Optional
from datetime import datetime
from contextlib import asynccontextmanager

# ChromaDB テレメトリの無効化
os.environ["ANONYMIZED_TELEMETRY"] = "False"
os.environ["CHROMA_TELEMETRY"] = "False"

import uvicorn
from fastapi import FastAPI, WebSocket, WebSocketDisconnect, HTTPException
from fastapi.middleware.cors import CORSMiddleware
from pydantic import BaseModel

# 内部モジュール
from scripts.settings import SettingsManager
from scripts.record import get_audio_device_names, get_discord_audio_device_names, AudioService, DiscordAudioService
from scripts.visual_capture import CaptureService, list_available_windows, get_window_by_title
from scripts.streaming_whisper import StreamTranscriber
from scripts.memory import MemoryManager
from scripts.gemini import GeminiService
from scripts.twitch_bot import TwitchService
from scripts.session_manager import SessionManager
from scripts.resource_monitor import ResourceMonitor, VRAMPreallocator
from scripts.skills import get_available_skills
from scripts.prompts import SYSTEM_INSTRUCTION_CHARACTER, get_all_prompts_data, PROMPT_DEFINITIONS, get_prompt
from scripts.tts_player import TTSManager
import scripts.voice as voice

import collections

# -------------------------------------------------------------
# 1. リアルタイム・ログインターセプター & ブロードキャスター
# -------------------------------------------------------------
class Broadcaster:
    def __init__(self):
        self.active_connections: List[WebSocket] = []
        self.loop: Optional[asyncio.AbstractEventLoop] = None
        self.log_history: collections.deque = collections.deque(maxlen=1000)

    def set_loop(self, loop: asyncio.AbstractEventLoop):
        self.loop = loop

    async def connect(self, websocket: WebSocket):
        await websocket.accept()
        self.active_connections.append(websocket)
        if not self.loop or self.loop.is_closed():
            try:
                self.loop = asyncio.get_running_loop()
            except RuntimeError:
                pass

    def disconnect(self, websocket: WebSocket):
        if websocket in self.active_connections:
            self.active_connections.remove(websocket)

    def queue_message(self, message: Dict[str, Any]):
        # ログメッセージの場合は履歴バッファに必ず追加
        if message.get("type") == "log":
            self.log_history.append(message)

        if not self.active_connections:
            return

        loop = self.loop
        if not loop or loop.is_closed():
            try:
                loop = asyncio.get_event_loop()
                self.loop = loop
            except RuntimeError:
                return

        try:
            if loop.is_running():
                asyncio.run_coroutine_threadsafe(self.broadcast(message), loop)
            else:
                loop.run_until_complete(self.broadcast(message))
        except Exception:
            pass

    async def broadcast(self, message: Dict[str, Any]):
        if not self.active_connections:
            return
        dead = []
        text = json.dumps(message, ensure_ascii=False)
        for ws in self.active_connections:
            try:
                await ws.send_text(text)
            except Exception:
                dead.append(ws)
        for ws in dead:
            self.disconnect(ws)

broadcaster = Broadcaster()

class WebSocketLogHandler(logging.Handler):
    def __init__(self, broadcaster_inst: Broadcaster):
        super().__init__()
        self.broadcaster = broadcaster_inst
        self.seen_messages = collections.deque(maxlen=100)

    def emit(self, record):
        try:
            logger_name = record.name

            # 1. 不要なテレメトリログ・アクセスログの除外
            if "telemetry" in logger_name or logger_name.startswith("uvicorn.access"):
                return

            msg = self.format(record).strip()
            if not msg:
                return

            # 2. uvicorn.error の WebSocket 接続ライフサイクルログ等を除外
            ignored_patterns = (
                "WebSocket /ws",
                "connection open",
                "connection closed",
                "Started server process",
                "Waiting for application startup",
                "Application startup complete",
                "Uvicorn running on",
                "Shutting down",
                "Finished server process",
            )
            if any(pat in msg for pat in ignored_patterns):
                return

            # 3. 重複ログの完全除外 (直近 100 件のハッシュキャッシュ)
            timestamp_sec = int(record.created)
            log_key = (timestamp_sec, logger_name, msg)
            if log_key in self.seen_messages:
                return
            self.seen_messages.append(log_key)

            entry = {
                "type": "log",
                "timestamp": datetime.fromtimestamp(record.created).strftime("%H:%M:%S.%f")[:-3],
                "level": record.levelname,
                "logger": logger_name,
                "message": msg
            }
            self.broadcaster.queue_message(entry)
        except Exception:
            pass

class StreamToLog:
    """stdout / stderr をキャプチャして改行ごとにログとして WebSocket 配信するストリーム"""
    def __init__(self, level: str, broadcaster_inst: Broadcaster, original_stream):
        self.level = level
        self.broadcaster = broadcaster_inst
        self.original_stream = original_stream
        self.buffer = ""
        self._in_write = False
        self.seen_lines = collections.deque(maxlen=50)

    def write(self, buf):
        # 元のコンソールストリームへ書き込み
        if self.original_stream:
            try:
                self.original_stream.write(buf)
                self.original_stream.flush()
            except UnicodeEncodeError:
                try:
                    encoded = buf.encode('cp932', errors='replace').decode('cp932')
                    self.original_stream.write(encoded)
                    self.original_stream.flush()
                except Exception:
                    pass
            except Exception:
                pass

        # 再帰呼び出し防止
        if self._in_write:
            return

        self._in_write = True
        try:
            text = str(buf)
            self.buffer += text
            if '\n' in self.buffer:
                lines = self.buffer.split('\n')
                for line in lines[:-1]:
                    trimmed = line.strip()
                    if not trimmed:
                        continue

                    # 1. 不要なログの除外
                    if any(pat in trimmed for pat in ('" HTTP/1.1" ', '" HTTP/1.0" ', 'WebSocket /ws', 'connection open', 'connection closed', 'telemetry')):
                        continue

                    # 2. 重複防止
                    if trimmed in self.seen_lines:
                        continue
                    self.seen_lines.append(trimmed)

                    is_diagnostic = (
                        trimmed.startswith("llama_") 
                        or trimmed.startswith("load:") 
                        or trimmed.startswith("llm_load_") 
                        or "special_eos_id" in trimmed 
                        or "full-size SWA cache" in trimmed
                    )
                    is_err = self.level == "ERROR" and not is_diagnostic and (
                        "Traceback" in trimmed 
                        or "Error:" in trimmed 
                        or trimmed.startswith("CRITICAL")
                        or "ERROR" in trimmed
                    )

                    entry = {
                        "type": "log",
                        "timestamp": datetime.now().strftime("%H:%M:%S.%f")[:-3],
                        "level": "ERROR" if is_err else ("WARN" if "Warning" in trimmed else "INFO"),
                        "logger": "llama.cpp" if is_diagnostic else ("stdout" if self.level == "INFO" else "stderr"),
                        "message": trimmed
                    }
                    self.broadcaster.queue_message(entry)
                self.buffer = lines[-1]
        except Exception:
            pass
        finally:
            self._in_write = False

    def flush(self):
        if self.original_stream:
            try:
                self.original_stream.flush()
            except Exception:
                pass

    def isatty(self):
        if self.original_stream and hasattr(self.original_stream, 'isatty'):
            return self.original_stream.isatty()
        return False

# ログハンドラーの設定 (root_logger のみに追加し、伝播による重複を防止)
root_logger = logging.getLogger()
root_logger.setLevel(logging.INFO)
ws_handler = WebSocketLogHandler(broadcaster)
ws_handler.setFormatter(logging.Formatter('%(message)s'))
root_logger.addHandler(ws_handler)

# 外部ライブラリのアクセスログ抑制
logging.getLogger("uvicorn.access").setLevel(logging.WARNING)
logging.getLogger("uvicorn.error").setLevel(logging.INFO)
logging.getLogger("chromadb").setLevel(logging.WARNING)

def setup_stream_interceptors():
    if not isinstance(sys.stdout, StreamToLog):
        sys.stdout = StreamToLog("INFO", broadcaster, sys.__stdout__)
    if not isinstance(sys.stderr, StreamToLog):
        sys.stderr = StreamToLog("ERROR", broadcaster, sys.__stderr__)

# 起動直後に即座にインターセプトを開始
setup_stream_interceptors()

# -------------------------------------------------------------
# 2. アプリケーション状態 & バックエンドサービス管理
# -------------------------------------------------------------
class MockVar:
    def __init__(self, val):
        self._val = val
    def get(self):
        return self._val
    def set(self, val):
        self._val = val

class ServerAppState:
    """Tkinter AppState と同等のインターフェースを FastAPI サーバー側で提供"""
    def __init__(self, settings_mgr: SettingsManager):
        self.settings_mgr = settings_mgr
        self.settings = settings_mgr.settings

        # 永続設定
        self.audio_device = MockVar(self.settings.get("audio_device", "Default (System Default)"))
        self.window_title = MockVar(self.settings.get("window", ""))
        self.use_image = MockVar(self.settings.get("use_image", True))
        self.is_private = MockVar(self.settings.get("is_private", True))
        self.show_response_in_new_window = MockVar(self.settings.get("show_response_in_new_window", True))
        self.response_display_duration = MockVar(self.settings.get("response_display_duration", 10000))
        self.tts_engine = MockVar(self.settings.get("tts_engine", "voicevox"))
        self.vits2_speaker_id = MockVar(self.settings.get("vits2_speaker_id", 0))
        self.disable_thinking_mode = MockVar(self.settings.get("disable_thinking_mode", False))
        self.asr_engine = MockVar(self.settings.get("asr_engine", "large"))
        self.user_name = MockVar(self.settings.get("user_name", "User"))
        self.create_blog_post = MockVar(self.settings.get("create_blog_post", False))
        self.blog_use_thinking = MockVar(self.settings.get("blog_use_thinking", False))
        self.enable_blog_skills = MockVar(self.settings.get("enable_blog_skills", True))
        self.enabled_blog_skills = list(self.settings.get("enabled_blog_skills", ["k0ta-writing-style"]))
        self.enable_auto_commentary = MockVar(self.settings.get("enable_auto_commentary", False))
        self.auto_commentary_min = MockVar(self.settings.get("auto_commentary_min", 300))
        self.auto_commentary_max = MockVar(self.settings.get("auto_commentary_max", 600))
        self.auto_commentary_avoid_overlap = MockVar(self.settings.get("auto_commentary_avoid_overlap", True))
        self.auto_commentary_avoid_duration = MockVar(self.settings.get("auto_commentary_avoid_duration", 15))
        self.preallocate_vram = MockVar(self.settings.get("preallocate_vram", False))
        self.wake_word_engine = MockVar(self.settings.get("wake_word_engine", "whisper_vad"))
        self.wake_word_threshold = MockVar(self.settings.get("wake_word_threshold", 0.25))
        self.custom_wake_words = MockVar(self.settings.get("custom_wake_words", "ねえぐり, ねぐり, ネグリ, ねーぐり, ねぇぐり, ね〜ぐり, neguri"))
        self.enable_discord_capture = MockVar(self.settings.get("enable_discord_capture", False))
        self.discord_audio_device = MockVar(self.settings.get("discord_audio_device", "Auto (Discord App / System Loopback)"))

        # Twitch
        self.twitch_bot_username = MockVar(self.settings.get("twitch_bot_username", ""))
        self.twitch_bot_id = MockVar(self.settings.get("twitch_bot_id") or self.settings.get("bot_id", ""))
        self.twitch_client_id = MockVar(self.settings.get("twitch_client_id", ""))
        self.twitch_client_secret = MockVar(self.settings.get("twitch_client_secret", ""))
        self.twitch_auth_code = MockVar("")

        # 動的状態
        self.is_vits2_ready = False
        self.current_window = None
        self.device_index = None
        self.cached_screenshot = None
        self.screenshot_file_path = os.path.abspath("temp_screenshot.png")
        self.audio_file_path = os.path.abspath("temp_recording.wav")
        self.image = None

    def is_skill_enabled(self, skill_id: str) -> bool:
        return skill_id in self.enabled_blog_skills

    def set_skill_enabled(self, skill_id: str, enabled: bool):
        if enabled and skill_id not in self.enabled_blog_skills:
            self.enabled_blog_skills.append(skill_id)
        elif not enabled and skill_id in self.enabled_blog_skills:
            self.enabled_blog_skills.remove(skill_id)
        self.save("enabled_blog_skills", self.enabled_blog_skills)

    def get(self, key, default=None):
        if hasattr(self, key):
            attr = getattr(self, key)
            if hasattr(attr, 'get'):
                return attr.get()
            return attr
        return self.settings.get(key, default)

    def save(self, key, value):
        self.settings[key] = value
        self.settings_mgr.save(self.settings)

class MockRoot:
    """Tkinter root.after をシミュレート"""
    def after(self, delay_ms, func):
        if delay_ms <= 0:
            func()
        else:
            threading.Timer(delay_ms / 1000.0, func).start()

class BackendApp:
    """Tkinter App の代わりに SessionManager や各サービスを統括するバックエンドロジック"""
    def __init__(self):
        self.root = MockRoot()
        self.settings_manager = SettingsManager()
        self.state = ServerAppState(self.settings_manager)

        # サービス群の初期化
        self.capture_service = CaptureService(self)
        self.audio_service = AudioService(self)
        self.discord_audio_service = DiscordAudioService(self)
        self.memory_manager = MemoryManager()
        self.gemini_service = GeminiService(self, SYSTEM_INSTRUCTION_CHARACTER, self.settings_manager)
        self.tts_manager = TTSManager(
            on_playback_start=lambda: self.update_status('tts', True),
            on_playback_end=self._on_tts_playback_finished
        )
        self.tts_manager.start()
        self.twitch_service = TwitchService(self)
        self.session_manager = SessionManager(self, self.twitch_service)
        self.twitch_connect_button = None

        # ステータス
        self.status = {
            "asr": False,
            "gemini": False,
            "tts": False,
            "twitch": False,
            "session": False
        }

    def update_status(self, key, is_active):
        self.status[key] = is_active
        broadcaster.queue_message({
            "type": "status",
            "status": self.status
        })

    def update_asr_display(self, text, is_final=False):
        broadcaster.queue_message({
            "type": "asr",
            "text": text,
            "is_final": is_final
        })

    def update_level_meter(self, level):
        broadcaster.queue_message({
            "type": "level_meter",
            "level": level
        })

    def update_commentary_timer(self, progress, remaining_sec):
        broadcaster.queue_message({
            "type": "commentary_timer",
            "progress": progress,
            "remaining": remaining_sec
        })

    def show_gemini_response(self, text, auto_close=False, only_timer=False):
        if text:
            broadcaster.queue_message({
                "type": "gemini_response",
                "text": text
            })

    def process_prompt(self, text, session_history, screenshot_path):
        """プロンプトを Gemini に送信してストリーミング生成 & TTS 再生"""
        self.update_status('gemini', True)

        def _worker():
            try:
                self.memory_manager.enqueue_save({
                    'type': 'user_prompt',
                    'source': self.state.user_name.get(),
                    'content': text,
                    'timestamp': datetime.now().isoformat()
                })
                
                stream = self.gemini_service.ask_stream(
                    text,
                    screenshot_path if self.state.use_image.get() else None,
                    self.state.is_private.get(),
                    session_history=session_history
                )
                
                full_response = ""
                from scripts.gemini import split_into_sentences
                for sentence in split_into_sentences(stream):
                    if voice.stop_playback_event.is_set():
                        break
                    full_response += sentence
                    self.show_gemini_response(full_response)
                    self.tts_manager.put_text(sentence)

                if full_response:
                    self.memory_manager.enqueue_save({
                        'type': 'ai_response',
                        'source': 'AI',
                        'content': full_response,
                        'timestamp': datetime.now().isoformat()
                    })
                    if self.session_manager.session_memory:
                        from scripts.session_manager import GeminiResponse
                        self.session_manager.session_memory.events.append(GeminiResponse(content=full_response))
                    self.tts_manager.put_text("END_MARKER")

            except Exception as e:
                logging.error(f"Error processing prompt: {e}", exc_info=True)
            finally:
                self.update_status('gemini', False)
                if os.path.exists(self.state.audio_file_path):
                    try: os.remove(self.state.audio_file_path)
                    except: pass
                if os.path.exists(self.state.screenshot_file_path):
                    try: os.remove(self.state.screenshot_file_path)
                    except: pass

        threading.Thread(target=_worker, daemon=True).start()

    def _on_tts_playback_finished(self, is_final):
        if is_final:
            self.update_status('tts', False)
            if hasattr(self.session_manager, 'auto_commentary_service'):
                self.session_manager.auto_commentary_service.start_next_cycle()

backend = BackendApp()

# -------------------------------------------------------------
# 3. FastAPI アプリケーション & REST API
# -------------------------------------------------------------
@asynccontextmanager
async def lifespan(app_inst: FastAPI):
    setup_stream_interceptors()
    try:
        loop = asyncio.get_running_loop()
    except RuntimeError:
        loop = asyncio.get_event_loop()
    broadcaster.set_loop(loop)
    logging.info("🚀 GameAssistant Backend Server started on port 18080.")
    # リソース監視バックグラウンドタスク
    monitor_task = asyncio.create_task(resource_monitor_loop())
    yield
    monitor_task.cancel()
    try:
        await monitor_task
    except asyncio.CancelledError:
        pass

app = FastAPI(title="GameAssistant Backend Server", lifespan=lifespan)

app.add_middleware(
    CORSMiddleware,
    allow_origins=["*"],
    allow_credentials=True,
    allow_methods=["*"],
    allow_headers=["*"],
)

resource_monitor = ResourceMonitor()

async def resource_monitor_loop():
    while True:
        try:
            status = resource_monitor.get_memory_info()
            broadcaster.queue_message({
                "type": "resource_status",
                "vram": {
                    "used": status.get("vram_used_bytes", 0) / (1024 * 1024),
                    "total": status.get("vram_total_bytes", 0) / (1024 * 1024),
                    "percent": status.get("vram_percent", 0.0)
                },
                "ram": {
                    "used": status.get("ram_used_bytes", 0) / (1024 * 1024),
                    "total": status.get("ram_total_bytes", 0) / (1024 * 1024),
                    "percent": status.get("ram_percent", 0.0)
                }
            })
        except Exception:
            pass
        await asyncio.sleep(2.0)

# WebSocket エンドポイント
@app.websocket("/ws")
async def websocket_endpoint(websocket: WebSocket):
    await broadcaster.connect(websocket)
    try:
        # 1. 初期状態を即時送信
        await websocket.send_text(json.dumps({
            "type": "status",
            "status": backend.status
        }))
        # 2. 直近のログ履歴（最新100件）をバッチ送信
        recent_logs = list(broadcaster.log_history)[-100:]
        if recent_logs:
            await websocket.send_text(json.dumps({
                "type": "log_history",
                "logs": recent_logs
            }, ensure_ascii=False))

        while True:
            data = await websocket.receive_text()
            # 必要に応じたPing/Pongやコマンド受信
    except WebSocketDisconnect:
        broadcaster.disconnect(websocket)
    except Exception:
        broadcaster.disconnect(websocket)

# -------------------------------------------------------------
# REST エンドポイント
# -------------------------------------------------------------
@app.get("/api/status")
def get_status():
    return {
        "status": backend.status,
        "session_running": backend.session_manager.session_running,
        "current_window": backend.state.current_window
    }

@app.get("/api/logs")
def get_logs_history(limit: int = 500):
    """直近のサーバーログ履歴を取得"""
    recent = list(broadcaster.log_history)[-limit:]
    return {
        "success": True,
        "logs": recent
    }

@app.post("/api/session/start")
def start_session():
    if backend.session_manager.session_running:
        return {"success": False, "message": "Session already running."}
    try:
        backend.session_manager.start_session()
        backend.update_status("session", True)
        return {"success": True}
    except Exception as e:
        logging.error(f"Failed to start session: {e}", exc_info=True)
        return {"success": False, "error": str(e)}

@app.post("/api/session/stop")
def stop_session():
    try:
        backend.session_manager.stop_session()
        backend.update_status("session", False)
        backend.update_status("gemini", False)
        backend.update_status("tts", False)
        return {"success": True}
    except Exception as e:
        logging.error(f"Failed to stop session: {e}", exc_info=True)
        return {"success": False, "error": str(e)}

@app.post("/api/session/restart-whisper")
def restart_whisper():
    backend.session_manager.restart_whisper()
    return {"success": True}

@app.get("/api/devices")
def get_devices():
    input_devs = get_audio_device_names()
    discord_devs = get_discord_audio_device_names()
    sel_input = backend.state.audio_device.get()
    sel_discord = backend.state.discord_audio_device.get()

    if not sel_input and len(input_devs) > 0:
        sel_input = input_devs[0]
        backend.state.audio_device.set(sel_input)
    if not sel_discord and len(discord_devs) > 0:
        sel_discord = discord_devs[0]
        backend.state.discord_audio_device.set(sel_discord)

    return {
        "input_devices": input_devs,
        "discord_devices": discord_devs,
        "selected_device": sel_input,
        "selected_discord_device": sel_discord
    }

@app.get("/api/windows")
def get_windows():
    win_list = list_available_windows()
    sel_win = backend.state.window_title.get()

    if not sel_win and len(win_list) > 0:
        sel_win = win_list[0]
        backend.state.window_title.set(sel_win)
        backend.state.current_window = get_window_by_title(sel_win)

    return {
        "windows": win_list,
        "selected_window": sel_win
    }

class PreviewPayload(BaseModel):
    window: Optional[str] = None

@app.post("/api/capture/preview")
def get_preview(payload: Optional[PreviewPayload] = None):
    target_win = payload.window if payload and payload.window else None
    if target_win:
        backend.state.window_title.set(target_win)
        backend.state.current_window = get_window_by_title(target_win)

    img_path = backend.capture_service.capture_window(target_win)
    if img_path and os.path.exists(img_path):
        with open(img_path, "rb") as f:
            b64 = base64.b64encode(f.read()).decode("utf-8")
        return {"success": True, "image": f"data:image/png;base64,{b64}"}
    return {"success": False, "image": None}

@app.get("/api/settings")
def get_settings():
    return backend.state.settings

class SettingUpdate(BaseModel):
    key: str
    value: Any

@app.post("/api/settings")
def update_setting(payload: SettingUpdate):
    backend.state.save(payload.key, payload.value)
    # 動的属性の更新
    if payload.key == "window":
        backend.state.window_title.set(payload.value)
        backend.state.current_window = get_window_by_title(payload.value)
    elif hasattr(backend.state, payload.key):
        attr = getattr(backend.state, payload.key)
        if hasattr(attr, 'set'):
            attr.set(payload.value)
    return {"success": True, "key": payload.key, "value": payload.value}

@app.get("/api/skills")
def get_skills():
    available = get_available_skills()
    enabled = backend.state.enabled_blog_skills
    return {
        "skills": available,
        "enabled_skills": enabled,
        "master_enabled": backend.state.enable_blog_skills.get()
    }

# =====================================================================
# Twitch 連携 API
# =====================================================================
@app.get("/api/twitch/status")
def get_twitch_status():
    is_connected = bool(
        backend.twitch_service.twitch_bot and
        backend.twitch_service.twitch_thread and
        backend.twitch_service.twitch_thread.is_alive()
    )
    return {
        "success": True,
        "connected": is_connected,
        "bot_username": backend.state.twitch_bot_username.get(),
        "bot_id": backend.state.twitch_bot_id.get(),
        "has_client_id": bool(backend.state.twitch_client_id.get()),
        "has_client_secret": bool(backend.state.twitch_client_secret.get())
    }

@app.get("/api/twitch/auth-url")
def get_twitch_auth_url(client_id: Optional[str] = None):
    cid = (client_id or backend.state.twitch_client_id.get() or backend.state.settings.get("twitch_client_id") or "").strip()
    if not cid:
        return {"success": False, "error": "Twitch Client ID が設定されていません。先に Client ID を入力してください。"}
    
    # 渡された client_id を即時保存
    if client_id and client_id.strip():
        backend.state.twitch_client_id.set(cid)
        backend.state.save("twitch_client_id", cid)

    import scripts.twitch_auth as twitch_auth
    auth_url = twitch_auth.generate_auth_url(cid)
    return {"success": True, "auth_url": auth_url}

class TwitchRegisterCodePayload(BaseModel):
    code: str
    client_id: Optional[str] = None
    client_secret: Optional[str] = None

@app.post("/api/twitch/register-code")
async def register_twitch_auth_code(payload: TwitchRegisterCodePayload):
    code = payload.code.strip()
    if not code:
        return {"success": False, "error": "認証コードが入力されていません。"}

    cid = (payload.client_id or backend.state.twitch_client_id.get() or backend.state.settings.get("twitch_client_id") or "").strip()
    csecret = (payload.client_secret or backend.state.twitch_client_secret.get() or backend.state.settings.get("twitch_client_secret") or "").strip()

    if not cid or not csecret:
        return {"success": False, "error": "Twitch Client ID または Client Secret が未設定です。"}

    # 最新の Client ID / Secret を即時保存
    backend.state.twitch_client_id.set(cid)
    backend.state.save("twitch_client_id", cid)
    backend.state.twitch_client_secret.set(csecret)
    backend.state.save("twitch_client_secret", csecret)

    import scripts.twitch_auth as twitch_auth
    try:
        result = await twitch_auth.exchange_code_for_token(cid, csecret, code)
        if result and result.get("user_id"):
            user_id = result["user_id"]
            backend.state.twitch_bot_id.set(user_id)
            backend.state.save("twitch_bot_id", user_id)
            logging.info(f"Bot IDを {user_id} に設定し、保存しました。")
            return {
                "success": True,
                "user_id": user_id,
                "message": f"ユーザーID {user_id} のトークンを正常に登録しました。"
            }
        else:
            return {"success": False, "error": "トークンの交換に失敗しました。認証コードが正しいか確認してください。"}
    except Exception as e:
        return {"success": False, "error": str(e)}

@app.post("/api/twitch/connect")
def connect_twitch():
    bot_id = backend.state.twitch_bot_id.get()
    client_id = backend.state.twitch_client_id.get()
    if not bot_id or not client_id:
        return {
            "success": False,
            "error": "Twitch Bot ID または Client ID が未設定です。認証コードを登録してください。"
        }
    backend.twitch_service.connect_twitch_bot()
    return {"success": True}

@app.post("/api/twitch/disconnect")
def disconnect_twitch():
    backend.twitch_service.disconnect_twitch_bot()
    backend.update_status("twitch", False)
    return {"success": True}

@app.get("/api/memories")
def get_memories(query: Optional[str] = None, limit: int = 200):
    try:
        raw_dict = backend.memory_manager.get_all_memories()
        items = []
        for mem_id, val_str in raw_dict.items():
            doc = ""
            meta = {}
            try:
                data = json.loads(val_str)
                if isinstance(data, dict):
                    doc = data.get("document", "")
                    meta = data.get("metadata", {})
                else:
                    doc = str(data)
            except Exception:
                doc = val_str

            ts_raw = meta.get("timestamp") or meta.get("created_at", "")
            display_ts = ts_raw
            if ts_raw:
                try:
                    dt_obj = datetime.fromisoformat(ts_raw)
                    display_ts = dt_obj.strftime('%Y-%m-%d %H:%M:%S')
                except Exception:
                    display_ts = ts_raw
            else:
                display_ts = "N/A"

            user_val = meta.get("user") or meta.get("source") or "Unknown"
            type_val = meta.get("type", "memory")

            items.append({
                "id": mem_id,
                "key": mem_id,
                "content": doc,
                "source": user_val,
                "user": user_val,
                "type": type_val,
                "timestamp": ts_raw,
                "display_ts": display_ts
            })

        # 最新順にソート
        items.sort(key=lambda x: x["timestamp"] or "", reverse=True)

        if query:
            q = query.lower()
            items = [
                m for m in items
                if q in m["content"].lower()
                or q in m["user"].lower()
                or q in m["type"].lower()
                or q in m["id"].lower()
            ]
        return {"success": True, "memories": items[:limit]}
    except Exception as e:
        return {"success": False, "error": str(e), "memories": []}

class MemorySavePayload(BaseModel):
    id: str
    content: str
    type: Optional[str] = "memory"
    user: Optional[str] = "User"

@app.post("/api/memories/save")
def save_memory_item(payload: MemorySavePayload):
    try:
        memories = backend.memory_manager.get_all_memories()
        original_json = memories.get(payload.id)
        created_at = None
        if original_json:
            try:
                orig_data = json.loads(original_json)
                created_at = orig_data.get("metadata", {}).get("created_at") or orig_data.get("metadata", {}).get("timestamp")
            except Exception:
                pass
        if not created_at:
            created_at = datetime.now().isoformat()

        new_obj = {
            "document": payload.content,
            "metadata": {
                "type": payload.type or "memory",
                "user": payload.user or "User",
                "created_at": created_at,
                "timestamp": created_at
            }
        }
        backend.memory_manager.add_or_update_memory(payload.id, json.dumps(new_obj, ensure_ascii=False, indent=2))
        return {"success": True}
    except Exception as e:
        return {"success": False, "error": str(e)}

class MemoryBulkDeletePayload(BaseModel):
    ids: List[str]

@app.post("/api/memories/bulk-delete")
def bulk_delete_memories(payload: MemoryBulkDeletePayload):
    try:
        deleted = 0
        for mem_id in payload.ids:
            if backend.memory_manager.delete_memory(mem_id):
                deleted += 1
        return {"success": True, "deleted_count": deleted}
    except Exception as e:
        return {"success": False, "error": str(e)}

class MemoryBulkUpdatePayload(BaseModel):
    ids: List[str]
    type: Optional[str] = None
    user: Optional[str] = None

@app.post("/api/memories/bulk-update")
def bulk_update_memories(payload: MemoryBulkUpdatePayload):
    try:
        memories = backend.memory_manager.get_all_memories()
        updated_count = 0
        for mem_id in payload.ids:
            val_str = memories.get(mem_id)
            if not val_str:
                continue
            try:
                data = json.loads(val_str)
                doc = data.get("document", "")
                meta = data.get("metadata", {})
            except Exception:
                doc = val_str
                meta = {}

            if payload.type is not None and payload.type.strip():
                meta["type"] = payload.type.strip()
            if payload.user is not None and payload.user.strip():
                meta["user"] = payload.user.strip()

            new_obj = {"document": doc, "metadata": meta}
            backend.memory_manager.add_or_update_memory(mem_id, json.dumps(new_obj, ensure_ascii=False, indent=2))
            updated_count += 1
        return {"success": True, "updated_count": updated_count}
    except Exception as e:
        return {"success": False, "error": str(e)}

class BlogGeneratePayload(BaseModel):
    ids: Optional[List[str]] = None
    items: Optional[List[Dict[str, Any]]] = None

@app.post("/api/memories/generate-blog")
def generate_blog_from_memories(payload: BlogGeneratePayload):
    try:
        # メモリーから会話テキストを整形
        memories = backend.memory_manager.get_all_memories()
        conversation_parts = []

        target_ids = payload.ids or []
        if not target_ids and payload.items:
            target_ids = [it.get("id") for it in payload.items if it.get("id")]

        for mem_id in target_ids:
            val_str = memories.get(mem_id)
            if not val_str:
                continue
            try:
                data = json.loads(val_str)
                doc = data.get("document", "")
                meta = data.get("metadata", {})
            except Exception:
                doc = val_str
                meta = {}

            ts = meta.get("timestamp") or meta.get("created_at") or ""
            t_type = meta.get("type", "memory")
            user = meta.get("user") or meta.get("source") or "User"

            label = user
            if t_type == "twitch_chat":
                label = f"Twitch Viewer: {user}"
            elif t_type == "ai_response":
                label = "AI"
            elif t_type == "auto_commentary":
                label = "AI (Auto)"
            elif t_type in ("user_speech", "user_prompt"):
                label = f"User: {user}"

            conversation_parts.append(f"[{ts}] {label}: {doc}")

        if not conversation_parts:
            return {"success": False, "error": "選択されたメモリーに有効な会話データがありません。"}

        conversation_text = "\n\n".join(conversation_parts)

        # ブログ生成を実行
        from scripts.gemini import generate_blog_post
        enabled_skills = backend.state.enabled_blog_skills if backend.state.enable_blog_skills.get() else []
        use_thinking = backend.state.blog_use_thinking.get()
        user_name = backend.state.user_name.get()

        blog_result = generate_blog_post(
            conversation_history=conversation_text,
            skills=enabled_skills,
            user_name=user_name,
            use_thinking=use_thinking,
            settings_manager=backend.state
        )

        # ファイル保存
        os.makedirs("blog", exist_ok=True)
        now_str = datetime.now().strftime("%Y%m%d_%H%M%S")
        filename = f"blog/blog_selected_{now_str}.md"
        with open(filename, "w", encoding="utf-8") as f:
            f.write(blog_result)

        logging.info(f"Generated blog from selected memories and saved to {filename}")
        return {
            "success": True,
            "filename": filename,
            "content": blog_result
        }
    except Exception as e:
        logging.error(f"Failed to generate blog from memories: {e}", exc_info=True)
        return {"success": False, "error": str(e)}

# =====================================================================
# プロンプト設定 API
# =====================================================================
@app.get("/api/prompts")
def get_prompts():
    """全プロンプトの現在の設定値とメタデータを取得"""
    return {
        "success": True,
        "prompts": get_all_prompts_data(backend.state)
    }

class PromptSaveAction(BaseModel):
    id: str
    value: str

@app.post("/api/prompts")
def save_prompt(action: PromptSaveAction):
    """プロンプトを保存・即時反映"""
    try:
        if action.id not in PROMPT_DEFINITIONS:
            return {"success": False, "error": f"Unknown prompt id: {action.id}"}
        
        # 永続化辞書に保存
        if "prompts" not in backend.state.settings:
            backend.state.settings["prompts"] = {}
        backend.state.settings["prompts"][action.id] = action.value
        backend.state.save("prompts", backend.state.settings["prompts"])
        
        # メインアシスタントのキャラクター指示の場合、GeminiSession のシステム指示も更新
        if action.id == "system_instruction_character":
            try:
                from google.genai import types
                if backend.gemini_service and backend.gemini_service.session:
                    sess = backend.gemini_service.session
                    if sess.history and len(sess.history) > 0:
                        sess.history[0] = types.Content(role="user", parts=[types.Part(text=action.value)])
            except Exception as e:
                logging.warning(f"Failed to dynamically update active Gemini session instruction: {e}")

        logging.info(f"プロンプト '{action.id}' を更新しました。")
        return {
            "success": True,
            "id": action.id,
            "prompts": get_all_prompts_data(backend.state)
        }
    except Exception as e:
        return {"success": False, "error": str(e)}

class PromptResetAction(BaseModel):
    id: str # 'all' または 特定の prompt_id

@app.post("/api/prompts/reset")
def reset_prompt(action: PromptResetAction):
    """プロンプトをデフォルト値にリセット"""
    try:
        prompts_dict = backend.state.settings.get("prompts", {})
        if action.id == "all":
            backend.state.settings["prompts"] = {}
            backend.state.save("prompts", {})
        elif action.id in prompts_dict:
            del prompts_dict[action.id]
            backend.state.settings["prompts"] = prompts_dict
            backend.state.save("prompts", prompts_dict)
            
        logging.info(f"プロンプト '{action.id}' をデフォルトにリセットしました。")
        return {
            "success": True,
            "prompts": get_all_prompts_data(backend.state)
        }
    except Exception as e:
        return {"success": False, "error": str(e)}

if __name__ == "__main__":
    uvicorn.run("server:app", host="127.0.0.1", port=18080, log_config=None, access_log=False)
