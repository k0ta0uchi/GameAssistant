from tkinter import font, messagebox
import ttkbootstrap as ttk
from ttkbootstrap import Style
from ttkbootstrap.constants import (
    END, BOTH, LEFT, RIGHT, Y, X, VERTICAL, WORD, READONLY
)
import scripts.record as record
import scripts.whisper as whisper
import scripts.gemini as gemini
import scripts.voice as voice
from scripts.prompts import SYSTEM_INSTRUCTION_CHARACTER
from scripts.search import ai_search
import chromadb
from scripts.twitch_bot import TwitchBot, TwitchService
from scripts import twitch_auth
import threading
import sys
import os
from PIL import Image, ImageTk
import keyboard
import json
import asyncio
import time
import re
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime
from scripts.memory import MemoryManager
from twitchio.utils import setup_logging
import logging
from logging.handlers import QueueHandler
import queue
import scripts.capture as capture
from scripts.settings import SettingsManager
from scripts.record import AudioService
from scripts.capture import CaptureService
from scripts.session_manager import SessionManager, GeminiResponse
from .components import GeminiResponseWindow, MemoryWindow
import subprocess
import glob

import win32job
import win32api
import win32con

class LoggingStream:
    """stdout/stderr を logging に変換するストリーム"""
    def __init__(self, level):
        self.level = level
        self.buffer = ""

    def write(self, message):
        if message:
            self.buffer += message
            if "\n" in self.buffer:
                lines = self.buffer.split("\n")
                for line in lines[:-1]:
                    if line.strip():
                        logging.log(self.level, line.rstrip())
                self.buffer = lines[-1]

    def flush(self):
        if self.buffer.strip():
            logging.log(self.level, self.buffer.rstrip())
            self.buffer = ""

class GameAssistantApp:
    def __init__(self, root):
        self.root = root
        self.style = Style(theme="superhero") # ゲーミング感のあるダークテーマ
        self.root.title("GameAssistant - AI Companion")
        self.root.geometry("1100x850")
        
        # カスタムスタイルの定義
        self._setup_custom_styles()
        
        self.cleanup_temp_files()

        self.settings_manager = SettingsManager()

        self.audio_devices = record.get_audio_device_names()
        default_audio_device = self.settings_manager.get("audio_device", self.audio_devices[0] if self.audio_devices else "")
        self.selected_device = ttk.StringVar(value=default_audio_device)
        self.device_index = None
        
        self.loopback_device_index = None

        self.windows = capture.list_available_windows()
        default_window = self.settings_manager.get("window", self.windows[0] if self.windows else "")
        self.selected_window_title = ttk.StringVar(value=default_window)
        self.selected_window = None

        self.custom_instruction = SYSTEM_INSTRUCTION_CHARACTER
        self.prompt = None
        self.response = None

        self.use_image = ttk.BooleanVar(value=self.settings_manager.get("use_image", True))
        self.is_private = ttk.BooleanVar(value=self.settings_manager.get("is_private", True))
        self.show_response_in_new_window = ttk.BooleanVar(value=self.settings_manager.get("show_response_in_new_window", True))
        self.response_display_duration = ttk.IntVar(value=self.settings_manager.get("response_display_duration", 10000))
        self.tts_engine = ttk.StringVar(value=self.settings_manager.get("tts_engine", "voicevox"))
        self.last_engine = self.tts_engine.get()
        self.vits2_speaker_id = ttk.IntVar(value=self.settings_manager.get("vits2_speaker_id", 0))
        self.vits2_server_process = None
        self.disable_thinking_mode = ttk.BooleanVar(value=self.settings_manager.get("disable_thinking_mode", False))
        # デフォルトは large (高精度)
        self.asr_engine = ttk.StringVar(value=self.settings_manager.get("asr_engine", "large"))
        self.user_name = ttk.StringVar(value=self.settings_manager.get("user_name", "User"))
        self.create_blog_post = ttk.BooleanVar(value=self.settings_manager.get("create_blog_post", False))
        self.enable_auto_commentary = ttk.BooleanVar(value=self.settings_manager.get("enable_auto_commentary", False))

        self.twitch_bot_username = ttk.StringVar(value=self.settings_manager.get("twitch_bot_username", ""))
        self.twitch_client_id = ttk.StringVar(value=self.settings_manager.get("twitch_client_id", ""))
        self.twitch_client_secret = ttk.StringVar(value=self.settings_manager.get("twitch_client_secret", ""))
        self.twitch_bot_id = ttk.StringVar(value=self.settings_manager.get("twitch_bot_id", ""))
        self.twitch_auth_code = ttk.StringVar()

        self.audio_service = AudioService(self)
        self.capture_service = CaptureService(self)
        self.gemini_service = gemini.GeminiService(self, self.custom_instruction, self.settings_manager)
        self.memory_manager = MemoryManager()
        self.twitch_service = TwitchService(self, mention_callback=self.schedule_twitch_mention)
        self.session_manager = SessionManager(self, self.twitch_service)
        self.twitch_last_mention_time = {}
        self.twitch_mention_cooldown = 30
        self.log_history = []

        self.create_widgets()
        self._setup_logging()

        self.audio_file_path = os.path.abspath("temp_recording.wav")
        self.screenshot_file_path = os.path.abspath("temp_screenshot.png")
        self.image = None

        keyboard.add_hotkey("ctrl+shift+f2", self.toggle_session)
        logging.info("ホットキー (Ctrl+Shift+F2) が登録されました。")

        self._process_log_queue()

        # Initial setup after all widgets are created
        if self.audio_devices:
            self.update_device_index()
        if self.windows:
            self.update_window()
        self.update_record_buttons_state()

        self.root.protocol("WM_DELETE_WINDOW", self.on_closing)

        self.db_save_queue = queue.Queue()
        self.db_worker_thread = threading.Thread(target=self._process_db_save_queue, daemon=True)
        self.db_worker_thread.start()

        self.tts_queue = queue.Queue()
        self.tts_worker_thread = threading.Thread(target=self._tts_synthesis_worker, daemon=True)
        self.tts_worker_thread.start()

        self.playback_queue = queue.Queue()
        self.playback_worker_thread = threading.Thread(target=self._tts_playback_worker, daemon=True)
        self.playback_worker_thread.start()

        self.current_response_window = None
        
        # 1. まずVITS2サーバーの起動判定（設定が VITS2 の場合のみ無許可で起動）
        if self.tts_engine.get() == "style_bert_vits2":
            self.start_vits2_server()
        
        # オートコメンタリーバーの更新ループ開始
        self._update_auto_commentary_bar_loop()

    def _setup_custom_styles(self):
        """ゲーミングAIアシスタント風のカスタムスタイルを定義"""
        # メイン背景
        self.style.configure("TFrame", background="#0F0F23")
        # カード（Labelframe）
        self.style.configure("Card.TLabelframe", background="#1a1a3a", bordercolor="#7C3AED")
        self.style.configure("Card.TLabelframe.Label", font=("Chakra Petch", 10, "bold"), foreground="#A78BFA", background="#1a1a3a")
        
        # ステータスラベル用
        self.style.configure("Status.TLabel", font=("Chakra Petch", 10, "bold"), foreground="#475569")
        self.style.configure("Status.Asr.TLabel", foreground="#00d2ff") # Neon Blue
        self.style.configure("Status.Gemini.TLabel", foreground="#A78BFA") # Neon Purple
        self.style.configure("Status.Tts.TLabel", foreground="#F43F5E") # Neon Rose
        
        # タイポグラフィ
        self.style.configure("Header.TLabel", font=("Russo One", 14), foreground="#E2E8F0", background="#0F0F23")
        
        # プログレスバー
        self.style.configure("Asr.Horizontal.TProgressbar", thickness=10, troughcolor="#1a1a3a", background="#00d2ff")
        self.style.configure("Commentary.Horizontal.TProgressbar", thickness=4, troughcolor="#0F0F23", background="#7C3AED")

    def create_widgets(self):
        """メインUIの構築"""
        # メインフレーム
        self.main_container = ttk.Frame(self.root, padding=10)
        self.main_container.pack(fill=BOTH, expand=True)

        # 1. 左サイドバー (Settings)
        self.sidebar = ttk.Frame(self.main_container, width=320)
        self.sidebar.pack(side=LEFT, fill=Y, padx=(0, 10))
        self.sidebar.pack_propagate(False)

        # サイドバー内のスクロール可能なエリア
        self.sidebar_canvas = ttk.Canvas(self.sidebar, background="#0F0F23", highlightthickness=0)
        self.sidebar_scrollbar = ttk.Scrollbar(self.sidebar, orient=VERTICAL, command=self.sidebar_canvas.yview)
        self.sidebar_scrollable = ttk.Frame(self.sidebar_canvas)

        self.sidebar_scrollable.bind(
            "<Configure>",
            lambda e: self.sidebar_canvas.configure(scrollregion=self.sidebar_canvas.bbox("all"))
        )
        self.sidebar_canvas.create_window((0, 0), window=self.sidebar_scrollable, anchor="nw", width=300)
        self.sidebar_canvas.configure(yscrollcommand=self.sidebar_scrollbar.set)

        self.sidebar_canvas.pack(side=LEFT, fill=BOTH, expand=True)
        self.sidebar_scrollbar.pack(side=RIGHT, fill=Y)

        # 各カードの作成
        self._create_audio_card(self.sidebar_scrollable)
        self._create_engine_card(self.sidebar_scrollable)
        self._create_target_card(self.sidebar_scrollable)
        self._create_config_card(self.sidebar_scrollable)
        self._create_twitch_card(self.sidebar_scrollable)
        
        # メモリーボタン
        ttk.Button(self.sidebar_scrollable, text="📂 メモリー管理", command=self.open_memory_window, style="info.TButton").pack(fill=X, pady=20)

        # 2. 右メインエリア (Status + Log)
        self.content_area = ttk.Frame(self.main_container)
        self.content_area.pack(side=RIGHT, fill=BOTH, expand=True)

        # ステータスダッシュボード
        self._create_status_dashboard(self.content_area)

        # 回答エリア
        self.response_frame = ttk.Labelframe(self.content_area, text="Geminiの回答", style="Card.TLabelframe")
        self.response_frame.pack(fill=X, pady=(0, 10))
        self.response_text_area = ttk.ScrolledText(self.response_frame, height=5, font=("Arial", 12), wrap=WORD, state="disabled")
        self.response_text_area.pack(fill=X, padx=5, pady=5)

        # ASRエリア + オートコメンタリーバー
        self.asr_container = ttk.Frame(self.content_area)
        self.asr_container.pack(fill=X, pady=(0, 10))
        
        self.asr_frame = ttk.Labelframe(self.asr_container, text="認識されたテキスト", style="Card.TLabelframe")
        self.asr_frame.pack(fill=X)
        self.asr_text_area = ttk.ScrolledText(self.asr_frame, height=3, font=("Arial", 11), wrap=WORD, state="disabled")
        self.asr_text_area.pack(fill=X, padx=5, pady=5)

        # オートコメンタリープログレスバー
        self.auto_commentary_bar = ttk.Progressbar(self.asr_container, length=100, mode='determinate', style="Commentary.Horizontal.TProgressbar")
        self.auto_commentary_bar.pack(fill=X, pady=(2, 0))
        self.auto_commentary_label = ttk.Label(self.asr_container, text="Silence Timer", font=("Chakra Petch", 7), foreground="#475569")
        self.auto_commentary_label.pack(anchor="e")

        # ログエリア
        self._create_log_area(self.content_area)

        # 下部操作エリア
        self.record_container = ttk.Frame(self.content_area)
        self.record_container.pack(fill=X, pady=10)
        self.start_session_button = ttk.Button(self.record_container, text="🚀 セッションを開始", style="success.TButton", command=self.start_session)
        self.start_session_button.pack(side=LEFT, padx=5)
        self.stop_session_button = ttk.Button(self.record_container, text="🛑 セッションを停止", style="danger.TButton", command=self.stop_session)
        self.stop_session_button.pack(side=LEFT, padx=5)
        self.stop_session_button.pack_forget()

    def _create_audio_card(self, parent):
        card = ttk.Labelframe(parent, text="AUDIO", style="Card.TLabelframe", padding=10)
        card.pack(fill=X, pady=5)
        
        ttk.Label(card, text="インプットデバイス:").pack(anchor="w")
        self.audio_dropdown = ttk.Combobox(card, textvariable=self.selected_device, values=self.audio_devices, state=READONLY)
        self.audio_dropdown.pack(fill=X, pady=5)
        self.audio_dropdown.bind("<<ComboboxSelected>>", self.update_device_index)
        
        self.device_index_label = ttk.Label(card, text="Index: -", font=("TkDefaultFont", 8))
        self.device_index_label.pack(anchor="w")

        # レベルメーター
        self.level_meter = ttk.Progressbar(card, length=200, maximum=100, value=0, style="Asr.Horizontal.TProgressbar")
        self.level_meter.pack(fill=X, pady=(10, 0))

    def _create_engine_card(self, parent):
        card = ttk.Labelframe(parent, text="AI ENGINES", style="Card.TLabelframe", padding=10)
        card.pack(fill=X, pady=5)

        ttk.Label(card, text="TTS Engine:").pack(anchor="w")
        tts_frame = ttk.Frame(card)
        tts_frame.pack(fill=X)
        for engine in ["voicevox", "gemini", "style_bert_vits2"]:
            label_text = "VITS2" if engine == "style_bert_vits2" else engine.upper()
            ttk.Radiobutton(tts_frame, text=label_text, variable=self.tts_engine, value=engine, command=self.on_tts_engine_change).pack(side=LEFT, padx=2)

        # VITS2モデル選択
        self.vits2_config_frame = ttk.Frame(card)
        ttk.Label(self.vits2_config_frame, text="VITS2 Model:").pack(anchor="w", pady=(5, 0))
        self.vits2_model_dropdown = ttk.Combobox(self.vits2_config_frame, state=READONLY)
        self.vits2_model_dropdown.pack(fill=X, pady=2)
        self.vits2_model_dropdown.bind("<<ComboboxSelected>>", self.on_vits2_model_change)

        ttk.Label(card, text="ASR Engine:").pack(anchor="w", pady=(10, 0))
        asr_frame = ttk.Frame(card)
        asr_frame.pack(fill=X)
        ttk.Radiobutton(asr_frame, text="LARGE", variable=self.asr_engine, value="large").pack(side=LEFT, padx=5)
        ttk.Radiobutton(asr_frame, text="TINY", variable=self.asr_engine, value="tiny").pack(side=LEFT, padx=5)

        ttk.Checkbutton(card, text="Thinkingモードをオフ", variable=self.disable_thinking_mode, style="success-square-toggle",
                       command=lambda: (self.settings_manager.set('disable_thinking_mode', self.disable_thinking_mode.get()), self.settings_manager.save(self.settings_manager.settings))).pack(anchor="w", pady=5)

    def _create_target_card(self, parent):
        card = ttk.Labelframe(parent, text="TARGET WINDOW", style="Card.TLabelframe", padding=10)
        card.pack(fill=X, pady=5)

        ttk.Label(card, text="対象ウィンドウ:").pack(anchor="w")
        win_frame = ttk.Frame(card)
        win_frame.pack(fill=X)
        self.window_dropdown = ttk.Combobox(win_frame, textvariable=self.selected_window_title, values=self.windows, state=READONLY)
        self.window_dropdown.pack(side=LEFT, fill=X, expand=True)
        self.window_dropdown.bind("<<ComboboxSelected>>", self.update_window)
        ttk.Button(win_frame, text="🔄", command=self.refresh_window_list, width=3).pack(side=LEFT, padx=2)

        self.selected_window_label = ttk.Label(card, text="Selected: -", font=("TkDefaultFont", 8), wraplength=250)
        self.selected_window_label.pack(anchor="w", pady=2)

    def _create_config_card(self, parent):
        card = ttk.Labelframe(parent, text="CONFIG", style="Card.TLabelframe", padding=10)
        card.pack(fill=X, pady=5)

        ttk.Checkbutton(card, text="画像を使用", variable=self.use_image, style="success-square-toggle",
                       command=lambda: (self.settings_manager.set('use_image', self.use_image.get()), self.settings_manager.save(self.settings_manager.settings), self.update_record_buttons_state())).pack(anchor="w", pady=2)
        ttk.Checkbutton(card, text="プライベート", variable=self.is_private, style="success-square-toggle",
                       command=lambda: (self.settings_manager.set('is_private', self.is_private.get()), self.settings_manager.save(self.settings_manager.settings))).pack(anchor="w", pady=2)
        ttk.Checkbutton(card, text="自発的コメント", variable=self.enable_auto_commentary, style="success-square-toggle",
                       command=lambda: (self.settings_manager.set('enable_auto_commentary', self.enable_auto_commentary.get()), self.settings_manager.save(self.settings_manager.settings))).pack(anchor="w", pady=2)
        ttk.Checkbutton(card, text="別窓で表示", variable=self.show_response_in_new_window, style="success-square-toggle",
                       command=lambda: (self.settings_manager.set('show_response_in_new_window', self.show_response_in_new_window.get()), self.settings_manager.save(self.settings_manager.settings))).pack(anchor="w", pady=2)
        
        ttk.Label(card, text="ユーザー名:").pack(anchor="w", pady=(5, 0))
        user_entry = ttk.Entry(card, textvariable=self.user_name)
        user_entry.pack(fill=X)
        user_entry.bind("<FocusOut>", lambda e: (self.settings_manager.set('user_name', self.user_name.get()), self.settings_manager.save(self.settings_manager.settings)))

    def _create_twitch_card(self, parent):
        card = ttk.Labelframe(parent, text="TWITCH BOT", style="Card.TLabelframe", padding=10)
        card.pack(fill=X, pady=5)

        # 認証
        auth_frame = ttk.Frame(card)
        auth_frame.pack(fill=X)
        ttk.Button(auth_frame, text="承認URL", command=self.twitch_service.copy_auth_url, style="info.TButton", width=8).pack(side=LEFT, padx=2)
        ttk.Entry(auth_frame, textvariable=self.twitch_auth_code, width=15).pack(side=LEFT, padx=2)
        ttk.Button(auth_frame, text="登録", command=self.twitch_service.register_auth_code, style="success.TButton", width=5).pack(side=LEFT, padx=2)
        
        self.twitch_connect_button = ttk.Button(card, text="Connect Twitch", command=self.twitch_service.toggle_twitch_connection, style="primary.TButton")
        self.twitch_connect_button.pack(fill=X, pady=10)

    def _create_status_dashboard(self, parent):
        self.status_frame = ttk.Frame(parent, padding=10)
        self.status_frame.pack(fill=X)
        
        # タイトル
        ttk.Label(self.status_frame, text="SYSTEM STATUS", style="Header.TLabel").pack(side=LEFT)
        
        # インジケーターエリア
        self.indicator_container = ttk.Frame(self.status_frame)
        self.indicator_container.pack(side=RIGHT)
        
        self.asr_status = ttk.Label(self.indicator_container, text="● MIC", style="Status.TLabel")
        self.asr_status.pack(side=LEFT, padx=10)
        
        self.gemini_status = ttk.Label(self.indicator_container, text="● BRAIN", style="Status.TLabel")
        self.gemini_status.pack(side=LEFT, padx=10)
        
        self.tts_status = ttk.Label(self.indicator_container, text="● VOICE", style="Status.TLabel")
        self.tts_status.pack(side=LEFT, padx=10)

    def _create_log_area(self, parent):
        log_container = ttk.Labelframe(parent, text="LOGS", style="Card.TLabelframe")
        log_container.pack(fill=BOTH, expand=True)
        
        filter_frame = ttk.Frame(log_container)
        filter_frame.pack(fill=X, padx=5, pady=2)
        log_levels = {"DEBUG": "secondary", "INFO": "info", "WARNING": "warning", "ERROR": "danger"}
        self.log_filters = {}
        for level, bstyle in log_levels.items():
            var = ttk.BooleanVar(value=True)
            cb = ttk.Checkbutton(filter_frame, text=level, variable=var, style=f"{bstyle}.TCheckbutton", command=self._refilter_logs)
            cb.pack(side=LEFT, padx=5)
            self.log_filters[level] = var

        self.log_textbox = ttk.ScrolledText(log_container, height=5, wrap=WORD, font=("Consolas", 9))
        self.log_textbox.pack(fill=BOTH, expand=True, padx=5, pady=5)
        self.log_textbox.config(state="disabled")
        
        # Log tags
        self.log_textbox.tag_config("INFO", foreground="#00d2ff")
        self.log_textbox.tag_config("ERROR", foreground="#F43F5E")
        self.log_textbox.tag_config("WARNING", foreground="#ffc107")

    def update_status(self, key, active):
        """システム状態インジケーターの更新"""
        if key == 'asr':
            style = "Status.Asr.TLabel" if active else "Status.TLabel"
            self.asr_status.config(style=style)
        elif key == 'gemini':
            style = "Status.Gemini.TLabel" if active else "Status.TLabel"
            self.gemini_status.config(style=style)
        elif key == 'tts':
            style = "Status.Tts.TLabel" if active else "Status.TLabel"
            self.tts_status.config(style=style)

    def _update_auto_commentary_bar_loop(self):
        """オートコメンタリーバーを定期的に更新するループ"""
        try:
            if hasattr(self, 'session_manager') and self.session_manager.session_running:
                if self.enable_auto_commentary.get():
                    service = self.session_manager.auto_commentary_service
                    remaining, total = service.get_remaining_time()
                    
                    if total > 0:
                        progress = ((total - remaining) / total) * 100
                        self.auto_commentary_bar['value'] = progress
                        self.auto_commentary_label.config(text=f"Next Commentary: {int(remaining)}s")
                    else:
                        self.auto_commentary_bar['value'] = 0
                        self.auto_commentary_label.config(text="Waiting for silence...")
                else:
                    self.auto_commentary_bar['value'] = 0
                    self.auto_commentary_label.config(text="Auto-Commentary Disabled")
            else:
                self.auto_commentary_bar['value'] = 0
                self.auto_commentary_label.config(text="Session Inactive")
        except Exception:
            pass
        
        self.root.after(200, self._update_auto_commentary_bar_loop)

    def _tts_synthesis_worker(self):
        """文を音声データに変換する（先行合成）スレッド"""
        while True:
            item = self.tts_queue.get()
            if item is None: break
            if item == "END_MARKER":
                self.playback_queue.put("END_MARKER")
                self.tts_queue.task_done()
                continue

            sentences = [item] # シンプル化
            for sub_sentence in sentences:
                try:
                    if voice.stop_playback_event.is_set(): break
                    wav_data = voice.generate_speech_data(sub_sentence)
                    if wav_data: self.playback_queue.put(wav_data)
                except Exception as e: logging.error(f"TTS合成エラー: {e}")
            self.tts_queue.task_done()

    def _tts_playback_worker(self):
        """合成済み音声を順次再生するスレッド"""
        while True:
            item = self.playback_queue.get()
            if item is None: break
            if item == "END_MARKER":
                self.root.after(0, lambda: self.show_gemini_response(None, auto_close=True, only_timer=True))
                self.root.after(0, lambda: self.update_status('tts', False))
                self.playback_queue.task_done()
                continue

            wav_data = item
            try:
                if not voice.stop_playback_event.is_set():
                    self.root.after(0, lambda: self.update_status('tts', True))
                    voice.play_wav_data(wav_data, volume=0.5)
            except Exception as e: logging.error(f"TTS再生エラー: {e}")
            finally: self.playback_queue.task_done()

    def _process_db_save_queue(self):
        while True:
            try:
                task = self.db_save_queue.get()
                if task is None: break
                task_type = task.get('type')
                future = task.get('future')
                data = task.get('data') or task
                try:
                    if task_type == 'query':
                        result = self.memory_manager.query_collection(**data)
                        if future: future.set_result(result)
                    elif task_type == 'summarize_and_save':
                        self.memory_manager.summarize_and_add_memory(**data)
                        if future: future.set_result(True)
                    else:
                        self.memory_manager.save_event_to_chroma_sync(data)
                        if future: future.set_result(True)
                except Exception as e:
                    if future: future.set_exception(e)
            except Exception: pass

    def on_closing(self):
        self.cleanup_temp_files()
        self.stop_vits2_server()
        self.db_save_queue.put(None)
        self.db_worker_thread.join()
        self.root.destroy()

    def cleanup_temp_files(self):
        for f in glob.glob("temp_recording_*.wav"):
            try: os.remove(f)
            except: pass

    def get_device_index_from_name(self, device_name):
        return record.get_device_index_from_name(device_name)

    def toggle_session(self):
        if self.session_manager.is_session_active(): self.stop_session()
        else: self.start_session()

    def start_session(self):
        self.session_manager.start_session()
        self.start_session_button.pack_forget()
        self.stop_session_button.pack(side=LEFT, padx=5)

    def stop_session(self):
        self.session_manager.stop_session()
        self.stop_session_button.pack_forget()
        self.start_session_button.pack(side=LEFT, padx=5)
        if self.create_blog_post.get():
            threading.Thread(target=self.generate_and_save_blog_post).start()

    def generate_and_save_blog_post(self, conversation=None):
        try:
            if conversation is None: conversation = self.session_manager.get_session_conversation()
            if not conversation: return
            blog_post = self.gemini_service.generate_blog_post(conversation)
            if blog_post:
                if not os.path.exists("blogs"): os.makedirs("blogs")
                filepath = os.path.join("blogs", f"{datetime.now().strftime('%Y-%m-%d')}.md")
                with open(filepath, "w", encoding="utf-8") as f: f.write(blog_post)
        except: pass

    def update_device_index(self, event=None):
        name = self.selected_device.get()
        self.device_index = self.get_device_index_from_name(name)
        self.device_index_label.config(text=f"Index: {self.device_index} - {name}")
        self.settings_manager.set("audio_device", name); self.settings_manager.save(self.settings_manager.settings)

    def update_window(self, event=None):
        title = self.selected_window_title.get()
        self.selected_window = capture.get_window_by_title(title)
        self.selected_window_label.config(text=f"Selected: {title if self.selected_window else '(Not Found)'}")
        self.settings_manager.set("window", title); self.settings_manager.save(self.settings_manager.settings)

    def refresh_window_list(self):
        self.windows = capture.list_available_windows()
        self.window_dropdown['values'] = self.windows
        self.update_window()

    def update_record_buttons_state(self, event=None): pass

    def update_level_meter(self, volume):
        level = int(volume / 100)
        self.root.after(0, lambda: self.level_meter.config(value=level))
        if level > 5:
            self.root.after(0, lambda: self.update_status('asr', True))
            self.root.after(500, lambda: self.update_status('asr', False))

    def execute_gemini_interaction(self, prompt, image_path, session_history):
        self.root.after(0, lambda: self.update_status('gemini', True))
        try:
            stream = self.gemini_service.ask_stream(prompt, image_path, self.is_private.get(), session_history=session_history)
            full = ""
            for s in gemini.split_into_sentences(stream):
                if voice.stop_playback_event.is_set(): break
                full += s
                self.root.after(0, self.show_gemini_response, full)
                self.tts_queue.put(s)
            if full: self.tts_queue.put("END_MARKER")
        except: pass
        finally:
            self.root.after(0, lambda: self.update_status('gemini', False))
            self.root.after(0, self.finalize_response_processing)

    def finalize_response_processing(self):
        if os.path.exists(self.audio_file_path): os.remove(self.audio_file_path)
        if os.path.exists(self.screenshot_file_path): os.remove(self.screenshot_file_path)

    def update_asr_display(self, text, is_final=False):
        self.asr_text_area.config(state="normal")
        if is_final:
            if not hasattr(self, 'asr_history'): self.asr_history = []
            self.asr_history.append(text)
            if len(self.asr_history) > 10: self.asr_history.pop(0)
            self.asr_text_area.delete("1.0", END)
            for line in self.asr_history: self.asr_text_area.insert(END, line + "\n")
            self.update_status('asr', False)
        else:
            self.asr_text_area.delete("1.0", END)
            for line in getattr(self, 'asr_history', []): self.asr_text_area.insert(END, line + "\n")
            self.asr_text_area.insert(END, ">>> " + text)
            self.update_status('asr', True)
        self.asr_text_area.see(END); self.asr_text_area.config(state="disabled")

    def open_memory_window(self): MemoryWindow(self.root, self, self.memory_manager, self.gemini_service)

    def show_gemini_response(self, response_text, auto_close=False, only_timer=False):
        if self.show_response_in_new_window.get():
            if self.current_response_window and self.current_response_window.winfo_exists():
                if not only_timer: self.current_response_window.set_response_text(response_text, auto_close=auto_close)
                else: self.current_response_window.start_close_timer()
            elif not only_timer:
                self.current_response_window = GeminiResponseWindow(self.root, response_text, self.response_display_duration.get())
                if auto_close: self.current_response_window.start_close_timer()
        else:
            if not only_timer:
                self.response_text_area.config(state="normal")
                self.response_text_area.delete("1.0", END)
                self.response_text_area.insert(END, response_text)
                self.response_text_area.see(END)
                self.response_text_area.config(state="disabled")
            if auto_close: self.root.after(self.response_display_duration.get(), self._clear_response_area)

    def _clear_response_area(self):
        self.response_text_area.config(state="normal"); self.response_text_area.delete("1.0", END); self.response_text_area.config(state="disabled")

    def schedule_twitch_mention(self, author_name, prompt, channel):
        if self.twitch_service.twitch_bot_loop:
            asyncio.run_coroutine_threadsafe(self.handle_twitch_mention(author_name, prompt, channel), self.twitch_service.twitch_bot_loop)

    async def handle_twitch_mention(self, author_name, prompt, channel):
        try:
            resp = await asyncio.to_thread(self.gemini_service.ask, prompt, None, self.is_private.get(), session_history=self.session_manager.get_session_history())
            if resp and self.twitch_service.twitch_bot: await self.twitch_service.twitch_bot.send_chat_message(channel, resp)
        except: pass

    def process_prompt(self, prompt, session_history, screenshot_path=None):
        threading.Thread(target=self.process_prompt_thread, args=(prompt, session_history, screenshot_path)).start()

    def process_prompt_thread(self, prompt, session_history, screenshot_path=None):
        if prompt and ("まて" in prompt or "待て" in prompt):
            voice.play_wav_file("wav/nod/5.wav"); return
        if not prompt: return
        self.execute_gemini_interaction(prompt, screenshot_path, session_history)

    def _setup_logging(self):
        if not os.path.exists("logs"): os.makedirs("logs")
        self.log_queue = queue.Queue()
        root_logger = logging.getLogger()
        for h in root_logger.handlers[:]: root_logger.removeHandler(h)
        formatter = logging.Formatter('%(asctime)s - %(levelname)s - %(message)s')
        sh = logging.StreamHandler(); sh.setFormatter(formatter)
        fh = logging.FileHandler("logs/app.log", encoding='utf-8'); fh.setFormatter(formatter)
        qh = QueueHandler(self.log_queue)
        root_logger.addHandler(sh); root_logger.addHandler(fh); root_logger.addHandler(qh)
        root_logger.setLevel(logging.INFO)
        sys.stdout = LoggingStream(logging.INFO); sys.stderr = LoggingStream(logging.ERROR)

    def _process_log_queue(self):
        try:
            while True: self._write_log(self.log_queue.get_nowait())
        except queue.Empty: pass
        self.root.after(100, self._process_log_queue)

    def _refilter_logs(self):
        self.log_textbox.config(state="normal"); self.log_textbox.delete("1.0", END); self.log_textbox.config(state="disabled")
        for r in self.log_history: self._write_log(r, from_history=True)

    def on_tts_engine_change(self):
        engine = self.tts_engine.get()
        if engine == "style_bert_vits2" and self.vits2_server_process is None:
            if messagebox.askokcancel("VITS2", "VITS2サーバーを起動しますか？"): self.start_vits2_server()
            else: self.tts_engine.set(self.last_engine); return
        self.last_engine = engine
        self.settings_manager.set('tts_engine', engine); self.settings_manager.save(self.settings_manager.settings)
        if engine == "style_bert_vits2":
            self.vits2_config_frame.pack(fill=X, pady=5); self.refresh_vits2_models()
        else: self.vits2_config_frame.pack_forget()

    def on_vits2_model_change(self, event=None):
        name = self.vits2_model_dropdown.get()
        for s in getattr(self, 'vits2_speakers', []):
            if s['name'] == name:
                sid = s['styles'][0]['id']
                self.vits2_speaker_id.set(sid); self.settings_manager.set('vits2_speaker_id', sid); self.settings_manager.save(self.settings_manager.settings)
                self.pre_load_vits2_model(sid); break

    def pre_load_vits2_model(self, sid):
        def _req():
            try:
                import requests
                requests.post(f"http://localhost:50021/initialize?speaker={sid}", timeout=300)
            except: pass
        threading.Thread(target=_req, daemon=True).start()

    def refresh_vits2_models(self):
        def _fetch():
            import requests
            for _ in range(10):
                try:
                    r = requests.get("http://localhost:50021/speakers", timeout=2)
                    if r.status_code == 200:
                        self.vits2_speakers = r.json()
                        names = [s['name'] for s in self.vits2_speakers]
                        self.root.after(0, lambda: self._update_vits2_dropdown(names))
                        self.pre_load_vits2_model(self.vits2_speaker_id.get()); return
                except: pass
                time.sleep(1)
        threading.Thread(target=_fetch, daemon=True).start()

    def _update_vits2_dropdown(self, names):
        self.vits2_model_dropdown['values'] = names
        if names: self.vits2_model_dropdown.set(names[0])

    def start_vits2_server(self):
        if self.vits2_server_process is None:
            try:
                self.vits2_job = win32job.CreateJobObject(None, "")
                info = win32job.QueryInformationJobObject(self.vits2_job, win32job.JobObjectExtendedLimitInformation)
                info['BasicLimitInformation']['LimitFlags'] = win32job.JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                win32job.SetInformationJobObject(self.vits2_job, win32job.JobObjectExtendedLimitInformation, info)
                self.vits2_server_process = subprocess.Popen([sys.executable, "scripts/vits2_server.py"], creationflags=subprocess.CREATE_NO_WINDOW | win32con.HIGH_PRIORITY_CLASS)
                win32job.AssignProcessToJobObject(self.vits2_job, self.vits2_server_process._handle)
            except: pass

    def stop_vits2_server(self):
        if self.vits2_server_process:
            self.vits2_server_process.terminate(); self.vits2_server_process = None
            if hasattr(self, 'vits2_job'): del self.vits2_job

    def _write_log(self, record, from_history=False):
        if not from_history: self.log_history.append(record)
        if not self.log_filters.get(record.levelname, ttk.BooleanVar(value=True)).get(): return
        self.log_textbox.config(state="normal")
        msg = f"{datetime.fromtimestamp(record.created).strftime('%H:%M:%S')} [{record.levelname}] {record.getMessage()}\n"
        self.log_textbox.insert(END, msg, record.levelname); self.log_textbox.see(END); self.log_textbox.config(state="disabled")
