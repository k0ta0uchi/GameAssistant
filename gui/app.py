from tkinter import font, messagebox
import ttkbootstrap as ttk
import glob
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
        self.root.title("ゲームアシスタント")
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
        self.user_name = ttk.StringVar(value=self.settings_manager.get("user_name", "User"))
        self.create_blog_post = ttk.BooleanVar(value=self.settings_manager.get("create_blog_post", False))

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
        
        # 2. 初期表示の更新（ダイアログなしでUIを構成）
        self.on_tts_engine_change()

    def _tts_synthesis_worker(self):
        """文を音声データに変換する（先行合成）スレッド"""
        while True:
            item = self.tts_queue.get()
            if item is None:
                break
            
            if item == "END_MARKER":
                self.playback_queue.put("END_MARKER")
                self.tts_queue.task_done()
                continue

            # 長すぎる文はさらに分割して VOICEVOX の負荷を減らす
            sentences = []
            if len(item) > 100:
                # 読点などで分割
                sentences = [s.strip() for s in re.split(r'([、,])', item) if s.strip()]
                # 分割記号を前の文に結合
                merged_sentences = []
                for i in range(0, len(sentences)-1, 2):
                    merged_sentences.append(sentences[i] + sentences[i+1])
                if len(sentences) % 2 == 1:
                    merged_sentences.append(sentences[-1])
                sentences = merged_sentences if merged_sentences else [item]
            else:
                sentences = [item]

            for sub_sentence in sentences:
                try:
                    if voice.stop_playback_event.is_set():
                        break

                    logging.info(f"TTS先行合成開始: {sub_sentence}")
                    wav_data = voice.generate_speech_data(sub_sentence)
                    if wav_data:
                        self.playback_queue.put(wav_data)
                except Exception as e:
                    logging.error(f"TTS合成ワーカーでエラー: {e}")
            
            self.tts_queue.task_done()

    def _tts_playback_worker(self):
        """合成済み音声を順次再生するスレッド"""
        while True:
            item = self.playback_queue.get()
            if item is None:
                break
            
            if item == "END_MARKER":
                logging.info("すべての再生が完了しました。")
                self.root.after(0, lambda: self.show_gemini_response(None, auto_close=True, only_timer=True))
                self.playback_queue.task_done()
                continue

            wav_data = item
            try:
                if not voice.stop_playback_event.is_set():
                    voice.play_wav_data(wav_data)
            except Exception as e:
                logging.error(f"TTS再生ワーカーでエラー: {e}")
            finally:
                self.playback_queue.task_done()

    def _process_db_save_queue(self):
        """DB関連の全タスクを処理する単一のワーカースレッド"""
        while True:
            try:
                task = self.db_save_queue.get()
                if task is None:
                    logging.info("DBワーカースレッドを終了します。")
                    break

                task_type = task.get('type')
                future = task.get('future')
                data = task.get('data')

                # 後方互換性：'data'キーがない場合はtask全体をデータとみなす
                if data is None and task_type is not None:
                    data = task

                try:
                    if task_type == 'query':
                        if not data:
                            raise ValueError("Query data is missing")
                        result = self.memory_manager.query_collection(**data)
                        if future:
                            future.set_result(result)
                    
                    elif task_type == 'summarize_and_save':
                        if not data:
                            raise ValueError("Summarize data is missing")
                        self.memory_manager.summarize_and_add_memory(**data)
                        if future:
                            future.set_result(True)
                    
                    elif task_type == 'save' or task_type is not None:
                        # 'save'タイプ、または直接データが投げ込まれた場合
                        self.memory_manager.save_event_to_chroma_sync(data)
                        if future:
                            future.set_result(True)
                    
                    else:
                        logging.warning(f"未知のDBタスクタイプです: {task_type}")
                
                except Exception as e:
                    logging.error(f"DBタスク '{task_type}' の処理中にエラー: {e}", exc_info=True)
                    if future:
                        future.set_exception(e)

            except Exception as e:
                logging.error(f"DB保存キューのループで予期せぬエラー: {e}", exc_info=True)

    def on_closing(self):
        self.cleanup_temp_files()
        # VITS2サーバーを停止
        self.stop_vits2_server()
        # DB保存スレッドを終了
        self.db_save_queue.put(None)
        self.db_worker_thread.join()
        self.root.destroy()

    def cleanup_temp_files(self):
        temp_files = glob.glob("temp_recording_*.wav")
        for f in temp_files:
            try:
                os.remove(f)
                logging.info(f"一時ファイルを削除しました: {f}")
            except OSError as e:
                logging.error(f"一時ファイルの削除に失敗しました: {f} - {e}")

    def get_device_index_from_name(self, device_name):
        return record.get_device_index_from_name(device_name)

    def create_widgets(self):
        main_frame = ttk.Frame(self.root, padding=20)
        main_frame.pack(fill=BOTH, expand=True)
        main_frame.pack_propagate(False)

        left_frame = ttk.Frame(main_frame, width=250)
        left_frame.pack(side=LEFT, fill=Y, padx=(0, 20))
        left_frame.pack_propagate(False)

        right_frame = ttk.Frame(main_frame)
        right_frame.pack(side=RIGHT, fill=BOTH, expand=True)

        # --- Left Frame Widgets ---
        device_frame = ttk.Frame(left_frame)
        device_frame.pack(fill=X, pady=(0, 15))
        ttk.Label(device_frame, text="インプットデバイス", style="inverse-primary").pack(fill=X, pady=(0, 8))
        self.audio_dropdown = ttk.Combobox(
            master=device_frame, textvariable=self.selected_device, values=self.audio_devices, state=READONLY, width=30
        )
        self.audio_dropdown.pack(fill=X, pady=(0, 5))
        self.audio_dropdown.bind("<<ComboboxSelected>>", self.update_device_index)
        self.device_index_label = ttk.Label(master=device_frame, text="Device index: ", wraplength=230)
        self.device_index_label.pack(fill=X)

        window_frame = ttk.Frame(left_frame)
        window_frame.pack(fill=X, pady=(0, 15))
        ttk.Label(window_frame, text="ウィンドウ", style="inverse-primary").pack(fill=X, pady=(0, 8))
        combo_button_frame = ttk.Frame(window_frame)
        combo_button_frame.pack(fill=X)

        self.window_dropdown = ttk.Combobox(
            master=combo_button_frame, textvariable=self.selected_window_title, values=self.windows, state=READONLY
        )
        self.window_dropdown.pack(side=LEFT, fill=X, expand=True)
        self.window_dropdown.bind("<<ComboboxSelected>>", self.update_window)

        refresh_button = ttk.Button(combo_button_frame, text="🔄", command=self.refresh_window_list, style="info.TButton", width=2)
        refresh_button.pack(side=LEFT, padx=(5, 0))
 
        self.selected_window_label = ttk.Label(master=window_frame, text="Selected window: ", wraplength=230)
        self.selected_window_label.pack(fill=X)

        memory_button = ttk.Button(left_frame, text="メモリー管理", command=self.open_memory_window, style="info.TButton")
        memory_button.pack(fill=X, pady=(15, 0))

        config_frame = ttk.Frame(left_frame)
        config_frame.pack(fill=X, pady=(15, 15))
        ttk.Label(config_frame, text="設定", style="inverse-primary").pack(fill=X, pady=(0, 8))

        self.use_image_check = ttk.Checkbutton(
            config_frame, text="画像を使用する", variable=self.use_image, style="success-square-toggle",
            command=lambda: (self.settings_manager.set('use_image', self.use_image.get()), self.settings_manager.save(self.settings_manager.settings), self.update_record_buttons_state())
        )
        self.use_image_check.pack(fill=X, pady=5)

        self.is_private_check = ttk.Checkbutton(
            config_frame, text="プライベート", variable=self.is_private, style="success-square-toggle", 
            command=lambda: (self.settings_manager.set('is_private', self.is_private.get()), self.settings_manager.save(self.settings_manager.settings))
        )
        self.is_private_check.pack(fill=X, pady=5)

        self.show_response_in_new_window_check = ttk.Checkbutton(
            config_frame, text="レスポンスを別ウィンドウに表示", variable=self.show_response_in_new_window,
            style="success-square-toggle", 
            command=lambda: (self.settings_manager.set('show_response_in_new_window', self.show_response_in_new_window.get()), self.settings_manager.save(self.settings_manager.settings))
        )
        self.show_response_in_new_window_check.pack(fill=X, pady=5)
        
        duration_frame = ttk.Frame(config_frame)
        duration_frame.pack(fill=X, pady=5)
        ttk.Label(duration_frame, text="表示時間(ms):").pack(side=LEFT)
        self.response_duration_entry = ttk.Entry(duration_frame, textvariable=self.response_display_duration, width=8)
        self.response_duration_entry.pack(side=LEFT)
        self.response_duration_entry.bind("<FocusOut>", lambda e: (self.settings_manager.set('response_display_duration', self.response_display_duration.get()), self.settings_manager.save(self.settings_manager.settings)))

        tts_frame = ttk.Frame(config_frame)
        tts_frame.pack(fill=X, pady=5)
        ttk.Label(tts_frame, text="TTSエンジン:").pack(side=LEFT)
        voicevox_radio = ttk.Radiobutton(tts_frame, text="VOICEVOX", variable=self.tts_engine, value="voicevox", command=self.on_tts_engine_change)
        voicevox_radio.pack(side=LEFT, padx=5)
        gemini_radio = ttk.Radiobutton(tts_frame, text="Gemini", variable=self.tts_engine, value="gemini", command=self.on_tts_engine_change)
        gemini_radio.pack(side=LEFT, padx=5)
        vits2_radio = ttk.Radiobutton(tts_frame, text="VITS2", variable=self.tts_engine, value="style_bert_vits2", command=self.on_tts_engine_change)
        vits2_radio.pack(side=LEFT, padx=5)

        # 先に thinking_mode 等を定義・pack しておく
        self.disable_thinking_mode_check = ttk.Checkbutton(
            config_frame, text="Thinkingモードをオフにする", variable=self.disable_thinking_mode,
            style="success-square-toggle",
            command=lambda: (self.settings_manager.set('disable_thinking_mode', self.disable_thinking_mode.get()), self.settings_manager.save(self.settings_manager.settings))
        )
        self.disable_thinking_mode_check.pack(fill=X, pady=5)

        # その後で vits2_config_frame を作成（pack は後で before 指定で行う）
        self.vits2_config_frame = ttk.Frame(config_frame)
        ttk.Label(self.vits2_config_frame, text="VITS2モデル:").pack(side=LEFT)
        self.vits2_model_dropdown = ttk.Combobox(self.vits2_config_frame, state=READONLY, width=20)
        self.vits2_model_dropdown.pack(side=LEFT, padx=5)
        self.vits2_model_dropdown.bind("<<ComboboxSelected>>", self.on_vits2_model_change)

        user_name_frame = ttk.Frame(config_frame)
        user_name_frame.pack(fill=X, pady=5)
        ttk.Label(user_name_frame, text="ユーザー名:").pack(side=LEFT)
        user_name_entry = ttk.Entry(user_name_frame, textvariable=self.user_name)
        user_name_entry.pack(side=LEFT, fill=X, expand=True)
        user_name_entry.bind("<FocusOut>", lambda e: (self.settings_manager.set('user_name', self.user_name.get()), self.settings_manager.save(self.settings_manager.settings)))

        self.create_blog_post_check = ttk.Checkbutton(
            config_frame, text="セッション終了時にブログ記事を作成する", variable=self.create_blog_post,
            style="success-square-toggle",
            command=lambda: (self.settings_manager.set('create_blog_post', self.create_blog_post.get()), self.settings_manager.save(self.settings_manager.settings))
        )
        self.create_blog_post_check.pack(fill=X, pady=5)

        twitch_frame = ttk.Frame(left_frame)
        twitch_frame.pack(fill=X, pady=(0, 15))
        ttk.Label(twitch_frame, text="Twitch Bot", style="inverse-primary").pack(fill=X, pady=(0, 8))

        bot_username_frame = ttk.Frame(twitch_frame)
        bot_username_frame.pack(fill=X, pady=2)
        ttk.Label(bot_username_frame, text="Bot Username:", width=12).pack(side=LEFT)
        bot_username_entry = ttk.Entry(bot_username_frame, textvariable=self.twitch_bot_username)
        bot_username_entry.pack(side=LEFT, fill=X, expand=True)
        bot_username_entry.bind("<FocusOut>", lambda e: (self.settings_manager.set('twitch_bot_username', self.twitch_bot_username.get()), self.settings_manager.save(self.settings_manager.settings)))

        bot_id_frame = ttk.Frame(twitch_frame)
        bot_id_frame.pack(fill=X, pady=2)
        ttk.Label(bot_id_frame, text="Bot ID:", width=12).pack(side=LEFT)
        bot_id_entry = ttk.Entry(bot_id_frame, textvariable=self.twitch_bot_id)
        bot_id_entry.pack(side=LEFT, fill=X, expand=True)
        bot_id_entry.bind("<FocusOut>", lambda e: (self.settings_manager.set('bot_id', self.twitch_bot_id.get()), self.settings_manager.save(self.settings_manager.settings)))

        client_id_frame = ttk.Frame(twitch_frame)
        client_id_frame.pack(fill=X, pady=2)
        ttk.Label(client_id_frame, text="Client ID:", width=12).pack(side=LEFT)
        client_id_entry = ttk.Entry(client_id_frame, textvariable=self.twitch_client_id)
        client_id_entry.pack(side=LEFT, fill=X, expand=True)
        client_id_entry.bind("<FocusOut>", lambda e: (self.settings_manager.set('twitch_client_id', self.twitch_client_id.get()), self.settings_manager.save(self.settings_manager.settings)))

        client_secret_frame = ttk.Frame(twitch_frame)
        client_secret_frame.pack(fill=X, pady=2)
        ttk.Label(client_secret_frame, text="Client Secret:", width=12).pack(side=LEFT)
        client_secret_entry = ttk.Entry(client_secret_frame, textvariable=self.twitch_client_secret, show="*")
        client_secret_entry.pack(side=LEFT, fill=X, expand=True)
        client_secret_entry.bind("<FocusOut>", lambda e: (self.settings_manager.set('twitch_client_secret', self.twitch_client_secret.get()), self.settings_manager.save(self.settings_manager.settings)))

        auth_code_frame = ttk.Frame(twitch_frame)
        auth_code_frame.pack(fill=X, pady=5)
        ttk.Label(auth_code_frame, text="認証コード:", width=12).pack(side=LEFT)
        auth_code_entry = ttk.Entry(auth_code_frame, textvariable=self.twitch_auth_code)
        auth_code_entry.pack(side=LEFT, fill=X, expand=True)
        
        auth_button_frame = ttk.Frame(twitch_frame)
        auth_button_frame.pack(fill=X, pady=5)
        self.register_token_button = ttk.Button(auth_button_frame, text="トークン登録", command=self.twitch_service.register_auth_code, style="success.TButton")
        self.register_token_button.pack(side=LEFT, fill=X, expand=True, padx=(0, 5))
        self.copy_auth_url_button = ttk.Button(auth_button_frame, text="承認URLコピー", command=self.twitch_service.copy_auth_url, style="info.TButton")
        self.copy_auth_url_button.pack(side=LEFT, fill=X, expand=True)
        
        self.twitch_connect_button = ttk.Button(twitch_frame, text="接続", command=self.twitch_service.toggle_twitch_connection, style="primary.TButton")
        self.twitch_connect_button.pack(fill=X, pady=5)

        # --- Right Frame Widgets ---
        self.response_frame = ttk.Labelframe(right_frame, text="Geminiの回答", style="info.TLabelframe")
        self.response_frame.pack(fill=X, pady=(0, 10))
        
        self.response_text_area = ttk.ScrolledText(
            self.response_frame, height=6, font=("Arial", 12), wrap=WORD, state="disabled"
        )
        self.response_text_area.pack(fill=X, padx=5, pady=5)

        # --- ASR (音声認識結果) 表示エリア ---
        self.asr_frame = ttk.Labelframe(right_frame, text="認識されたテキスト", style="info.TLabelframe")
        self.asr_frame.pack(fill=X, pady=(0, 10))
        
        self.asr_text_area = ttk.ScrolledText(
            self.asr_frame, height=4, font=("Arial", 11), wrap=WORD, state="disabled"
        )
        self.asr_text_area.pack(fill=X, padx=5, pady=5)

        self.meter_container = ttk.Frame(right_frame)
        self.meter_container.pack(fill=X, pady=(0, 10))
        self.level_meter = ttk.Progressbar(
            self.meter_container, length=300, maximum=100, value=0, style="danger.Horizontal.TProgressbar"
        )
        self.level_meter.pack(pady=10)

        self.image_frame = ttk.Frame(right_frame, height=300)
        self.image_frame.pack(fill=X, pady=10)
        self.image_frame.pack_propagate(False)
        self.image_label = ttk.Label(self.image_frame)
        self.image_label.pack(pady=10)

        # New Log Frame
        log_container = ttk.Labelframe(right_frame, text="ログ", style="info.TLabelframe")
        log_container.pack(fill=BOTH, expand=True, pady=(10, 0))
        
        filter_frame = ttk.Frame(log_container)
        filter_frame.pack(fill=X, padx=5, pady=5)
        
        self.log_filters = {}
        log_levels = {"DEBUG": "secondary", "INFO": "info", "WARNING": "warning", "ERROR": "danger", "CRITICAL": "danger"}
        for level, style in log_levels.items():
            var = ttk.BooleanVar(value=True)
            cb = ttk.Checkbutton(filter_frame, text=level, variable=var, style=f"{style}.TCheckbutton", command=self._refilter_logs)
            cb.pack(side=LEFT, padx=5)
            self.log_filters[level] = var

        log_text_frame = ttk.Frame(log_container)
        log_text_frame.pack(fill=BOTH, expand=True, padx=5, pady=(0, 5))

        self.log_textbox = ttk.ScrolledText(master=log_text_frame, height=5, width=50, wrap=WORD)
        self.log_textbox.pack(fill=BOTH, expand=True)
        self.log_textbox.config(state="disabled")

        # Log level colors
        self.log_textbox.tag_config("DEBUG", foreground="gray")
        self.log_textbox.tag_config("INFO", foreground="#007bff") # Blue
        self.log_textbox.tag_config("WARNING", foreground="#ffc107") # Yellow
        self.log_textbox.tag_config("ERROR", foreground="#dc3545") # Red
        self.log_textbox.tag_config("CRITICAL", foreground="#dc3545", font=("TkDefaultFont", 10, "bold"))

        self.record_container = ttk.Frame(right_frame)
        self.record_container.pack(fill=X, padx=10, pady=10)

        self.start_session_button = ttk.Button(self.record_container, text="セッションを開始", style="success.TButton", command=self.start_session)
        self.start_session_button.pack(side=LEFT, padx=5)

        self.stop_session_button = ttk.Button(self.record_container, text="セッションを停止", style="danger.TButton", command=self.stop_session)
        self.stop_session_button.pack(side=LEFT, padx=5)
        self.stop_session_button.pack_forget()

        # ストリーミング版では個別の録音ボタンは不要なため削除
        # self.record_button = ...
        # self.record_wait_button = ...


    def toggle_session(self):
        """セッションの開始/停止を切り替える"""
        if self.session_manager.is_session_active():
            self.stop_session()
        else:
            self.start_session()

    def start_session(self):
        self.session_manager.start_session()
        self.start_session_button.pack_forget()
        self.stop_session_button.pack(side=LEFT, padx=5)

    def stop_session(self):
        summary = self.session_manager.stop_session()
        self.stop_session_button.pack_forget()
        self.start_session_button.pack(side=LEFT, padx=5)

        if self.create_blog_post.get():
            threading.Thread(target=self.generate_and_save_blog_post).start()

    def generate_and_save_blog_post(self, conversation=None):
        logging.info("ブログ記事の生成を開始します...")
        try:
            if conversation is None:
                conversation = self.session_manager.get_session_conversation()
            
            if not conversation:
                logging.warning("ブログ記事の生成をスキップしました。会話がありません。")
                return

            blog_post = self.gemini_service.generate_blog_post(conversation)
            if blog_post:
                if not os.path.exists("blogs"):
                    os.makedirs("blogs")
                
                today_str = datetime.now().strftime("%Y-%m-%d")
                filepath = os.path.join("blogs", f"{today_str}.md")
                
                counter = 1
                while os.path.exists(filepath):
                    filepath = os.path.join("blogs", f"{today_str}_{counter}.md")
                    counter += 1

                with open(filepath, "w", encoding="utf-8") as f:
                    f.write(blog_post)
                logging.info(f"ブログ記事を保存しました: {filepath}")
            else:
                logging.error("ブログ記事の生成に失敗しました。")

        except Exception as e:
            logging.error(f"ブログ記事の生成または保存中にエラーが発生しました: {e}", exc_info=True)

    def update_device_index(self, event=None):
        selected_device_name = self.selected_device.get()
        self.device_index = self.get_device_index_from_name(selected_device_name)
        self.device_index_label.config(text=f"選択されたデバイス: {self.device_index}-{selected_device_name}")
        self.settings_manager.set("audio_device", selected_device_name)
        self.settings_manager.save(self.settings_manager.settings)

    def update_window(self, event=None):
        selected_window_title = self.selected_window_title.get()
        self.selected_window = capture.get_window_by_title(selected_window_title)
        if self.selected_window:
            logging.info(f"選択されたウィンドウ: {self.selected_window.title}")
            self.selected_window_label.config(text=f"選択されたウィンドウ: {self.selected_window.title}")
        else:
            logging.warning("ウィンドウが見つかりませんでした")
            self.selected_window_label.config(text="選択されたウィンドウ: (見つかりません)")
        self.settings_manager.set("window", selected_window_title)
        self.settings_manager.save(self.settings_manager.settings)
        self.update_record_buttons_state()

    def refresh_window_list(self):
        logging.info("ウィンドウリストを更新します...")
        self.windows = capture.list_available_windows()
        self.window_dropdown['values'] = self.windows
        current_selection = self.selected_window_title.get()

        if self.windows:
            if current_selection not in self.windows:
                self.selected_window_title.set(self.windows[0])
        else:
            self.selected_window_title.set("")
        
        self.update_window()
        logging.info("ウィンドウリストを更新しました。")

    def update_record_buttons_state(self, event=None):
        pass # ストリーミング版ではボタン状態の管理は不要

    def update_level_meter(self, volume):
        level = int(volume / 100)
        self.root.after(0, self.set_level_meter_value, level)

    def set_level_meter_value(self, level):
        self.level_meter['value'] = level

    def transcribe_audio(self):
        logging.info("音声認識を開始します...")
        try:
            text = whisper.recognize_speech(self.audio_file_path)
            if text:
                logging.info(f"*** 認識されたテキスト: '{text}' ***")
            else:
                logging.warning("*** 音声は検出されましたが、テキストとして認識されませんでした。***")
            return text
        except Exception as e:
            logging.error(f"音声認識エラー: {e}", exc_info=True)
            return None

    def execute_gemini_interaction(self, prompt, image_path, session_history):
        """Geminiとの対話をストリーミングで実行し、表示・音声・保存を行う。"""
        logging.info(f"Gemini対話開始: {prompt}")
        
        # ユーザープロンプトをDBに保存
        user_event_data = {
            'type': 'user_prompt',
            'source': self.user_name.get(),
            'content': prompt,
            'timestamp': datetime.now().isoformat()
        }
        self.db_save_queue.put({'type': 'save', 'data': user_event_data, 'future': None})

        # 応答表示の準備
        full_response = ""
        voice.stop_playback_event.clear()
        
        # キューをクリアして古い発話を破棄
        while not self.tts_queue.empty():
            try: self.tts_queue.get_nowait()
            except queue.Empty: break
        while not self.playback_queue.empty():
            try: self.playback_queue.get_nowait()
            except queue.Empty: break

        # チャットログへの表示（初期空文字）
        if not self.show_response_in_new_window.get():
            self.root.after(0, lambda: self._update_log_with_partial_response("Gemini: ", is_start=True))

        try:
            # ストリーミング開始
            stream = self.gemini_service.ask_stream(prompt, image_path, self.is_private.get(), session_history=session_history)
            
            # 文割ジェネレータ
            for sentence in gemini.split_into_sentences(stream):
                if voice.stop_playback_event.is_set():
                    logging.info("ユーザーによる中断を検知しました。")
                    break
                
                full_response += sentence
                
                # GUI更新
                self.root.after(0, self.show_gemini_response, full_response)
                if not self.show_response_in_new_window.get():
                    self.root.after(0, lambda s=sentence: self._update_log_with_partial_response(s))
                
                # TTSキューへ投入
                self.tts_queue.put(sentence)

            # 最終的な応答をDBに保存
            if full_response:
                ai_event_data = {
                    'type': 'ai_response',
                    'source': 'AI',
                    'content': full_response,
                    'timestamp': datetime.now().isoformat()
                }
                self.db_save_queue.put({'type': 'save', 'data': ai_event_data, 'future': None})
                
                if self.session_manager.session_memory:
                    event = GeminiResponse(content=full_response)
                    self.session_manager.session_memory.events.append(event)

                # 全ての文を投げ終えたらマーカーを投入
                self.tts_queue.put("END_MARKER")

        except Exception as e:
            logging.error(f"Gemini対話中にエラー: {e}", exc_info=True)
        finally:
            self.root.after(0, self.finalize_response_processing)

    def _update_log_with_partial_response(self, text, is_start=False):
        self.log_textbox.config(state="normal")
        if is_start:
            self.log_textbox.insert(END, "\n" + text)
        else:
            self.log_textbox.insert(END, text)
        self.log_textbox.see(END)
        self.log_textbox.config(state="disabled")

    def process_and_respond(self, from_temporary_stop=False):
        prompt = self.transcribe_audio()

        if prompt and ("まて" in prompt or "待て" in prompt):
            logging.info("キャンセルワードを検出しました。処理を中断し、待機モードに戻ります。")
            voice.play_wav_file("wav/nod/5.wav")
            self.root.after(0, self.reset_buttons_after_cancel)
            return

        if not prompt:
            logging.info("プロンプトが空のため、処理を中断します。")
            self.root.after(0, self.reset_buttons_after_cancel)
            return

        if any(k in prompt for k in ["検索", "調べて", "教えて", "wiki"]):
            # フィードバック音声
            msg = "ウェブで調べてみるだわん！少々お待ちくださいだわん。"
            self.tts_queue.put(msg)
            
            search_results = asyncio.run(ai_search(prompt))
            if search_results:
                prompt += "\n\nWeb検索結果:\n" + "\n".join(search_results)

        image_path = self.screenshot_file_path if self.use_image.get() and os.path.exists(self.screenshot_file_path) else None
        session_history = self.session_manager.get_session_history() if self.session_manager.is_session_active() else None

        threading.Thread(target=self.execute_gemini_interaction, args=(prompt, image_path, session_history)).start()

    def reset_buttons_after_cancel(self):
        self.record_button.config(text="録音開始", style="success.TButton", state="normal")
        self.record_wait_button.config(text="録音待機", style="success.TButton", state="normal")
        if self.audio_service.record_waiting:
            self.record_wait_button.config(text="録音待機中", style="danger.TButton")
            self.audio_service.record_waiting_thread = threading.Thread(target=self.audio_service.wait_for_keyword_thread)
            self.audio_service.record_waiting_thread.start()

    def process_prompt_thread(self, prompt, session_history, screenshot_path=None):
        if prompt and ("まて" in prompt or "待て" in prompt):
            logging.info("キャンセルワードを検出しました。処理を中断します。")
            voice.play_wav_file("wav/nod/5.wav")
            return

        if not prompt:
            logging.info("プロンプトが空のため、処理を中断します。")
            return

        if any(k in prompt for k in ["検索", "調べて", "教えて", "wiki"]):
            # フィードバック音声
            msg = "ウェブで調べてみるだわん！少々お待ちくださいだわん。"
            self.tts_queue.put(msg)
            
            search_results = asyncio.run(ai_search(prompt))
            if search_results:
                prompt += "\n\nWeb検索結果:\n" + "\n".join(search_results)

        self.execute_gemini_interaction(prompt, screenshot_path, session_history)

    def finalize_response_processing(self):
        if os.path.exists(self.audio_file_path):
            os.remove(self.audio_file_path)
        if os.path.exists(self.screenshot_file_path):
            os.remove(self.screenshot_file_path)
        
        # ストリーミング版ではボタン復帰処理は不要

    def update_asr_display(self, text, is_final=False):
        """音声認識結果をGUIに表示する"""
        self.asr_text_area.config(state="normal")
        
        if is_final:
            # 1. 確定時: 前回の Partial 表示（>>> で始まる行など）があれば消す
            # 面倒なので、一度全削除して「確定済みリスト」を再描画する方式が最も確実
            if not hasattr(self, 'asr_history'):
                self.asr_history = []
            
            self.asr_history.append(text)
            # 履歴が増えすぎたら古いものを消す（直近10件など）
            if len(self.asr_history) > 10:
                self.asr_history.pop(0)
            
            # 再描画
            self.asr_text_area.delete("1.0", END)
            for line in self.asr_history:
                self.asr_text_area.insert(END, line + "\n")
        else:
            # 2. 認識中: 確定済みテキストの後に、一時的に現在のテキストを表示
            self.asr_text_area.delete("1.0", END)
            if hasattr(self, 'asr_history'):
                for line in self.asr_history:
                    self.asr_text_area.insert(END, line + "\n")
            self.asr_text_area.insert(END, ">>> " + text)
        
        self.asr_text_area.see(END)
        self.asr_text_area.config(state="disabled")

    def open_memory_window(self):
        """メモリー管理ウィンドウを開く"""
        MemoryWindow(self.root, self, self.memory_manager, self.gemini_service)

    def show_gemini_response(self, response_text, auto_close=False, only_timer=False):
        if self.show_response_in_new_window.get():
            if self.current_response_window and self.current_response_window.winfo_exists():
                if not only_timer:
                    self.current_response_window.set_response_text(response_text, auto_close=auto_close)
                else:
                    self.current_response_window.start_close_timer()
            elif not only_timer:
                self.current_response_window = GeminiResponseWindow(self.root, response_text, self.response_display_duration.get())
                if auto_close:
                    self.current_response_window.start_close_timer()
        else:
            if not only_timer:
                self.response_text_area.config(state="normal")
                self.response_text_area.delete("1.0", END)
                self.response_text_area.insert(END, response_text)
                self.response_text_area.see(END)
                self.response_text_area.config(state="disabled")
            
            if auto_close:
                self.root.after(self.response_display_duration.get(), self._clear_response_area)

    def _clear_response_area(self):
        self.response_text_area.config(state="normal")
        self.response_text_area.delete("1.0", END)
        self.response_text_area.config(state="disabled")

    def schedule_twitch_mention(self, author_name, prompt, channel):
        """Twitchのメンション処理をスレッドセーフにスケジュールする"""
        if self.twitch_service.twitch_bot_loop:
            future = asyncio.run_coroutine_threadsafe(
                self.handle_twitch_mention(author_name, prompt, channel),
                self.twitch_service.twitch_bot_loop
            )
            def callback(future):
                try:
                    future.result()
                except Exception as e:
                    logging.error(f"handle_twitch_mentionで予期せぬエラーが発生しました: {e}", exc_info=True)
            future.add_done_callback(callback)

    async def handle_twitch_mention(self, author_name, prompt, channel):
        """Twitchのメンションを処理する"""
        try:
            logging.debug(f"handle_twitch_mention called by {author_name}: {prompt}")

            event_data = {
                'type': 'twitch_mention',
                'source': author_name,
                'content': prompt,
                'timestamp': datetime.now().isoformat()
            }
            self.db_save_queue.put({'type': 'save', 'data': event_data, 'future': None})

            session_history = None
            if self.session_manager.is_session_active():
                logging.debug("Session is active.")
                session_history = self.session_manager.get_session_history()
            else:
                logging.debug("Session is not active.")

            response = await asyncio.to_thread(self.gemini_service.ask, prompt, None, self.is_private.get(), session_history=session_history)
            logging.debug(f"Gemini response: {response}")

            if response:
                if self.twitch_service.twitch_bot:
                    logging.debug(f"Sending message to Twitch channel {channel.name}")
                    await self.twitch_service.twitch_bot.send_chat_message(channel, response)
                    logging.debug("Message sent to Twitch.")
                else:
                    logging.warning("twitch_bot is not available.")
            else:
                logging.info("Gemini response is empty.")
        except Exception as e:
            logging.error(f"handle_twitch_mentionでエラーが発生しました: {e}", exc_info=True)

    def process_prompt(self, prompt, session_history, screenshot_path=None):
        thread = threading.Thread(target=self.process_prompt_thread, args=(prompt, session_history, screenshot_path))
        thread.start()

    def _setup_logging(self):
        log_dir = "logs"
        if not os.path.exists(log_dir):
            os.makedirs(log_dir)

        self.log_queue = queue.Queue()
        queue_handler = QueueHandler(self.log_queue)
        
        root_logger = logging.getLogger()
        
        # 既存のハンドラをすべて削除
        for handler in root_logger.handlers[:]:
            root_logger.removeHandler(handler)
            
        # 新しいハンドラを設定
        formatter = logging.Formatter('%(asctime)s - %(name)s - %(levelname)s - %(threadName)s - %(message)s')
        
        # StreamHandler (コンソール出力)
        stream_handler = logging.StreamHandler()
        stream_handler.setFormatter(formatter)

        # FileHandler (ファイル出力)
        file_handler = logging.FileHandler(os.path.join(log_dir, "app.log"), encoding='utf-8')
        file_handler.setFormatter(formatter)

        root_logger.addHandler(queue_handler)
        root_logger.addHandler(stream_handler)
        root_logger.addHandler(file_handler)
        root_logger.setLevel(logging.DEBUG)

        # 標準出力・標準エラーを logging にリダイレクト
        sys.stdout = LoggingStream(logging.INFO)
        sys.stderr = LoggingStream(logging.ERROR)

    def _process_log_queue(self):
        try:
            while True:
                record = self.log_queue.get_nowait()
                self._write_log(record)
        except queue.Empty:
            pass
        self.root.after(100, self._process_log_queue)

    def _refilter_logs(self):
        self.log_textbox.config(state="normal")
        self.log_textbox.delete("1.0", END)
        self.log_textbox.config(state="disabled")

        for record in self.log_history:
            self._write_log(record, from_history=True)

    def on_tts_engine_change(self):
        engine = self.tts_engine.get()
        
        # 1. VITS2を選択した場合の処理（サーバー起動確認）
        if engine == "style_bert_vits2" and self.vits2_server_process is None:
            if messagebox.askokcancel("VITS2サーバーの起動", "Style-Bert-VITS2サーバーを起動しますか？\n(既にポート50021を使用しているアプリがある場合は競合します)"):
                self.start_vits2_server()
            else:
                self.tts_engine.set(self.last_engine)
                return

        # 2. VITS2から他のエンジンに切り替える場合の処理（サーバー終了確認）
        if self.last_engine == "style_bert_vits2" and engine != "style_bert_vits2" and self.vits2_server_process is not None:
            if messagebox.askokcancel("VITS2サーバーの終了", "VITS2サーバーを終了して、他のTTSエンジンに切り替えますか？"):
                self.stop_vits2_server()
            else:
                # キャンセルの場合は選択をVITS2に戻す
                self.tts_engine.set("style_bert_vits2")
                return

        self.last_engine = engine
        self.settings_manager.set('tts_engine', engine)
        self.settings_manager.save(self.settings_manager.settings)
        
        if engine == "style_bert_vits2":
            # 他の設定項目（Thinkingモード等）より上に表示されるよう
            # tts_frame の直後に配置を維持。
            self.vits2_config_frame.pack(fill=X, pady=5, before=self.disable_thinking_mode_check)
            self.refresh_vits2_models()
        else:
            self.vits2_config_frame.pack_forget()

    def on_vits2_model_change(self, event=None):
        selected_name = self.vits2_model_dropdown.get()
        if hasattr(self, 'vits2_speakers'):
            for speaker in self.vits2_speakers:
                if speaker['name'] == selected_name:
                    speaker_id = speaker['styles'][0]['id']
                    self.vits2_speaker_id.set(speaker_id)
                    self.settings_manager.set('vits2_speaker_id', speaker_id)
                    self.settings_manager.save(self.settings_manager.settings)
                    logging.info(f"VITS2モデルを切り替えました: {selected_name} (ID: {speaker_id})")
                    
                    # サーバーにモデルのロードを事前リクエスト
                    self.pre_load_vits2_model(speaker_id)
                    break

    def pre_load_vits2_model(self, speaker_id):
        """サーバーに対してモデルの事前ロードをリクエストする"""
        def _request():
            try:
                import requests
                logging.info(f"VITS2モデルの事前ロードをリクエスト中 (ID: {speaker_id})...")
                # 大型モデル対応のためタイムアウトを5分に延長
                response = requests.post(f"http://localhost:50021/initialize?speaker={speaker_id}", timeout=300)
                if response.status_code == 200:
                    logging.info(f"VITS2モデルの事前ロードが完了しました (ID: {speaker_id})")
                else:
                    logging.error(f"VITS2事前ロードがエラーを返しました: {response.status_code}")
            except Exception as e:
                logging.error(f"VITS2事前ロードリクエストに失敗: {e}")
        
        threading.Thread(target=_request, daemon=True).start()

    def refresh_vits2_models(self):
        """VITS2サーバーからモデルリストを取得してドロップダウンを更新する（リトライ付き）"""
        def _fetch():
            import requests
            max_retries = 15
            for i in range(max_retries):
                try:
                    response = requests.get("http://localhost:50021/speakers", timeout=2)
                    if response.status_code == 200:
                        self.vits2_speakers = response.json()
                        names = [s['name'] for s in self.vits2_speakers]
                        logging.info(f"VITS2モデルリストを取得しました: {names}")
                        self.root.after(0, lambda: self._update_vits2_dropdown(names))
                        
                        # 取得できたら、現在選択されているモデルの事前ロードをリクエスト
                        self.pre_load_vits2_model(self.vits2_speaker_id.get())
                        return
                except Exception:
                    pass
                logging.debug(f"VITS2サーバーの待機中... ({i+1}/{max_retries})")
                time.sleep(1)
            logging.error("VITS2モデルリストの取得に失敗しました（タイムアウト）")
        
        threading.Thread(target=_fetch, daemon=True).start()

    def _update_vits2_dropdown(self, names):
        self.vits2_model_dropdown['values'] = names
        if names:
            # 現在のspeaker_idに対応する名前を選択状態にする
            current_id = self.vits2_speaker_id.get()
            selected_name = names[0]
            if hasattr(self, 'vits2_speakers'):
                for s in self.vits2_speakers:
                    if s['styles'][0]['id'] == current_id:
                        selected_name = s['name']
                        break
            self.vits2_model_dropdown.set(selected_name)

    def start_vits2_server(self):
        """Style-Bert-VITS2 ブリッジサーバーを起動する"""
        if self.vits2_server_process is None:
            logging.info("VITS2サーバーを起動します...")
            try:
                # ジョブオブジェクトの作成（強制終了時の道連れ用）
                self.vits2_job = win32job.CreateJobObject(None, "")
                extended_info = win32job.QueryInformationJobObject(self.vits2_job, win32job.JobObjectExtendedLimitInformation)
                extended_info['BasicLimitInformation']['LimitFlags'] = win32job.JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                win32job.SetInformationJobObject(self.vits2_job, win32job.JobObjectExtendedLimitInformation, extended_info)

                # scripts/vits2_server.py を実行
                # CREATE_BREAKAWAY_FROM_JOB を防ぐため flags=0 (デフォルト)
                self.vits2_server_process = subprocess.Popen(
                    [sys.executable, "scripts/vits2_server.py"],
                    stdout=subprocess.PIPE,
                    stderr=subprocess.STDOUT,
                    text=True,
                    bufsize=1,
                    creationflags=subprocess.CREATE_NO_WINDOW # コンソールウィンドウを表示しない
                )
                
                # プロセスをジョブに割り当て
                win32job.AssignProcessToJobObject(self.vits2_job, self.vits2_server_process._handle)

                # サーバーログを読み取るスレッドを開始
                def log_reader(pipe):
                    try:
                        for line in iter(pipe.readline, ''):
                            if line:
                                logging.info(f"[VITS2 Server] {line.strip()}")
                    except Exception: pass
                    finally:
                        try: pipe.close()
                        except: pass
                
                threading.Thread(target=log_reader, args=(self.vits2_server_process.stdout,), daemon=True).start()
                
                # 起動待ち
                time.sleep(3) 
            except Exception as e:
                logging.error(f"VITS2サーバーの起動に失敗: {e}")

    def stop_vits2_server(self):
        """VITS2サーバーを停止する"""
        if self.vits2_server_process:
            logging.info("VITS2サーバーを停止します...")
            try:
                # ジョブを閉じることでプロセスを確実に終了させる
                self.vits2_server_process.terminate()
                self.vits2_server_process.wait(timeout=2)
            except Exception:
                try: self.vits2_server_process.kill()
                except: pass
            finally:
                self.vits2_server_process = None
                if hasattr(self, 'vits2_job'):
                    # ジョブオブジェクトのハンドルを閉じる（これでKILL_ON_JOB_CLOSEが発動）
                    # 本来は CloseHandle ですが win32job ではオブジェクト削除でOK
                    del self.vits2_job

    def _write_log(self, record, from_history=False):
        if not from_history:
            self.log_history.append(record)

        if not self.log_filters.get(record.levelname, ttk.BooleanVar(value=True)).get():
            return

        log_level_emojis = {
            'DEBUG': '⚙️',
            'INFO': '🔵',
            'WARNING': '🟡',
            'ERROR': '🔴',
            'CRITICAL': '🔥'
        }
        self.log_textbox.config(state="normal")
        
        msg = f"{datetime.fromtimestamp(record.created).strftime('%H:%M:%S')} {log_level_emojis.get(record.levelname, ' ')} [{record.levelname}] {record.getMessage()}\n"
        
        self.log_textbox.insert(END, msg, record.levelname)
        self.log_textbox.see(END)
        self.log_textbox.config(state="disabled")