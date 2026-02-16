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
import scripts.visual_capture as visual_capture
from scripts.settings import SettingsManager
from scripts.record import AudioService
from scripts.visual_capture import CaptureService
from scripts.session_manager import SessionManager, GeminiResponse
from scripts.tts_player import TTSManager
from .components import GeminiResponseWindow, MemoryWindow, SettingsWindow
import subprocess
import glob

import win32job
import win32api
import win32con

class LoggingStream:
    def __init__(self, level):
        self.level = level
        self.buffer = ""

    def write(self, message):
        if message:
            self.buffer += message
            if "\n" in self.buffer:
                lines = self.buffer.split("\n")
                for line in lines[:-1]:
                    if line.strip(): logging.log(self.level, line.rstrip())
                self.buffer = lines[-1]

    def flush(self):
        if self.buffer.strip(): logging.log(self.level, self.buffer.rstrip()); self.buffer = ""

class AppState:
    """アプリケーションの設定と動的状態を一括管理するクラス"""
    def __init__(self, root, settings_manager):
        self.root = root
        self.settings = settings_manager
        
        # --- 永続設定 (Persistent Settings) ---
        self.audio_device = ttk.StringVar(value=self.settings.get("audio_device", ""))
        self.window_title = ttk.StringVar(value=self.settings.get("window", ""))
        self.use_image = ttk.BooleanVar(value=self.settings.get("use_image", True))
        self.is_private = ttk.BooleanVar(value=self.settings.get("is_private", True))
        self.show_response_in_new_window = ttk.BooleanVar(value=self.settings.get("show_response_in_new_window", True))
        self.response_display_duration = ttk.IntVar(value=self.settings.get("response_display_duration", 10000))
        self.tts_engine = ttk.StringVar(value=self.settings.get("tts_engine", "voicevox"))
        self.vits2_speaker_id = ttk.IntVar(value=self.settings.get("vits2_speaker_id", 0))
        self.disable_thinking_mode = ttk.BooleanVar(value=self.settings.get("disable_thinking_mode", False))
        self.asr_engine = ttk.StringVar(value=self.settings.get("asr_engine", "large"))
        self.user_name = ttk.StringVar(value=self.settings.get("user_name", "User"))
        self.create_blog_post = ttk.BooleanVar(value=self.settings.get("create_blog_post", False))
        self.enable_auto_commentary = ttk.BooleanVar(value=self.settings.get("enable_auto_commentary", False))
        
        # Twitch関連
        self.twitch_bot_username = ttk.StringVar(value=self.settings.get("twitch_bot_username", ""))
        # 互換性のため bot_id もチェック
        initial_bot_id = self.settings.get("twitch_bot_id") or self.settings.get("bot_id", "")
        self.twitch_bot_id = ttk.StringVar(value=initial_bot_id)
        self.twitch_client_id = ttk.StringVar(value=self.settings.get("twitch_client_id", ""))
        self.twitch_client_secret = ttk.StringVar(value=self.settings.get("twitch_client_secret", ""))
        self.twitch_auth_code = ttk.StringVar()

        # --- 動的状態 (Volatile State) ---
        self.is_vits2_ready = False
        self.current_window = None
        self.device_index = None
        self.last_engine = self.tts_engine.get()
        
        # パスとデータ
        self.audio_file_path = os.path.abspath("temp_recording.wav")
        self.screenshot_file_path = os.path.abspath("temp_screenshot.png")
        self.cached_screenshot = None
        self.image = None # PhotoImage object

    def save(self, key, value):
        """設定を保存"""
        self.settings.set(key, value)
        self.settings.save(self.settings.settings)

class GameAssistantApp:
    def __init__(self, root):
        self.root = root
        self.style = Style(theme="superhero") 
        self.root.title("GameAssistant - AI Companion")
        self.root.geometry("1100x850")
        self._setup_custom_styles()
        self.log_history = []
        self._setup_logging()
        
        self.settings_manager = SettingsManager()
        self.state = AppState(self.root, self.settings_manager)
        self.cleanup_temp_files()
        
        self._init_services()
        self.create_widgets()

        keyboard.add_hotkey("ctrl+shift+f2", self.toggle_session)
        self._process_log_queue()
        
        self.sync_initial_state()
        self.root.protocol("WM_DELETE_WINDOW", self.on_closing)
        self._update_auto_commentary_bar_loop()

    def _init_services(self):
        self.audio_service = AudioService(self)
        self.capture_service = CaptureService(self)
        self.memory_manager = MemoryManager()
        self.gemini_service = gemini.GeminiService(self, SYSTEM_INSTRUCTION_CHARACTER, self.settings_manager)
        
        self.tts_manager = TTSManager(
            on_playback_start=lambda: self.root.after(0, lambda: self.update_status('tts', True)),
            on_playback_end=self._on_tts_playback_finished
        )
        self.tts_manager.start()

        self.twitch_service = TwitchService(self, mention_callback=self.schedule_twitch_mention)
        self.session_manager = SessionManager(self, self.twitch_service)
        
        self.twitch_connect_button = None 
        self.vits2_server_process = None
        
        if self.state.tts_engine.get() == "style_bert_vits2": self.start_vits2_server()

    def sync_initial_state(self):
        audio_names = record.get_audio_device_names()
        if audio_names:
            if not self.state.audio_device.get(): self.state.audio_device.set(audio_names[0])
            self.update_device_index()
            
        win_titles = visual_capture.list_available_windows()
        if win_titles:
            if not self.state.window_title.get(): self.state.window_title.set(win_titles[0])
            self.update_window()

    def _on_tts_playback_finished(self, is_final):
        if is_final:
            self.root.after(0, lambda: (
                self.show_gemini_response(None, auto_close=True, only_timer=True),
                self.update_status('tts', False)
            ))

    def _setup_custom_styles(self):
        self.style.configure("TFrame", background="#0F0F23")
        self.style.configure("TLabel", background="#0F0F23")
        self.style.configure("Card.TLabelframe", background="#1a1a3a", bordercolor="#7C3AED")
        self.style.configure("Card.TLabelframe.Label", font=("Chakra Petch", 10, "bold"), foreground="#A78BFA", background="#1a1a3a")
        self.style.configure("Status.TLabel", font=("Chakra Petch", 10, "bold"), foreground="#475569", background="#0F0F23")
        self.style.configure("Status.Asr.TLabel", foreground="#00d2ff")
        self.style.configure("Status.Gemini.TLabel", foreground="#A78BFA")
        self.style.configure("Status.Tts.TLabel", foreground="#F43F5E")
        self.style.configure("Header.TLabel", font=("Russo One", 14), foreground="#E2E8F0", background="#0F0F23")
        self.style.configure("Asr.Horizontal.TProgressbar", thickness=10, troughcolor="#1a1a3a", background="#00d2ff")
        self.style.configure("Commentary.Horizontal.TProgressbar", thickness=4, troughcolor="#0F0F23", background="#7C3AED")

    def create_widgets(self):
        self.main_container = ttk.Frame(self.root, padding=4); self.main_container.pack(fill=BOTH, expand=True)
        self.sidebar = ttk.Frame(self.main_container, width=320); self.sidebar.pack(side=LEFT, fill=Y, padx=(2, 2))
        self.sidebar_content = ttk.Frame(self.sidebar); self.sidebar_content.pack(fill=BOTH, expand=True)
        self._create_audio_card(self.sidebar_content); self._create_target_card(self.sidebar_content)
        
        self.btn_container = ttk.Frame(self.sidebar_content, padding=2); self.btn_container.pack(fill=X, pady=4)
        self.start_session_button = ttk.Button(self.btn_container, text="🚀 Start Session", style="success.TButton", command=self.start_session); self.start_session_button.pack(fill=X, pady=2)
        self.stop_session_button = ttk.Button(self.btn_container, text="🛑 Stop Session", style="danger.TButton", command=self.stop_session); self.stop_session_button.pack(fill=X, pady=2); self.stop_session_button.pack_forget()
        
        self.settings_btn = ttk.Button(self.btn_container, text="⚙️ Settings", command=self.open_settings_window, style="secondary.TButton"); self.settings_btn.pack(fill=X, pady=2)
        ttk.Button(self.btn_container, text="📂 Memory", command=self.open_memory_window, style="info.TButton").pack(fill=X, pady=2)
        
        self.content_area = ttk.Frame(self.main_container); self.content_area.pack(side=LEFT, fill=BOTH, expand=True, padx=(0, 2))
        self._create_status_dashboard(self.content_area)
        self.response_frame = ttk.Labelframe(self.content_area, text="Geminiの回答", style="Card.TLabelframe"); self.response_frame.pack(fill=X, pady=(0, 4))
        self.response_text_area = ttk.ScrolledText(self.response_frame, height=5, font=("Arial", 12), wrap=WORD, state="disabled"); self.response_text_area.pack(fill=X, padx=4, pady=4)
        self.asr_container = ttk.Frame(self.content_area); self.asr_container.pack(fill=X, pady=(0, 4))
        self.asr_frame = ttk.Labelframe(self.asr_container, text="認識されたテキスト", style="Card.TLabelframe"); self.asr_frame.pack(fill=X)
        self.asr_text_area = ttk.ScrolledText(self.asr_frame, height=3, font=("Arial", 11), wrap=WORD, state="disabled"); self.asr_text_area.pack(fill=X, padx=4, pady=4)
        self.auto_commentary_bar = ttk.Progressbar(self.asr_container, length=100, mode='determinate', style="Commentary.Horizontal.TProgressbar"); self.auto_commentary_bar.pack(fill=X, pady=(1, 0))
        self.auto_commentary_label = ttk.Label(self.asr_container, text="Silence Timer", font=("Chakra Petch", 7), foreground="#475569"); self.auto_commentary_label.pack(anchor="e")
        self.image_frame = ttk.Frame(self.content_area, style="TFrame"); self.image_frame.pack(fill=X, pady=(0, 4))
        self.image_label = ttk.Label(self.image_frame, style="TLabel"); self.image_label.pack(expand=True)
        self._create_log_area(self.content_area)

    def _create_audio_card(self, parent):
        card = ttk.Labelframe(parent, text="AUDIO", style="Card.TLabelframe", padding=8); card.pack(fill=X, pady=4)
        ttk.Label(card, text="インプットデバイス:").pack(anchor="w")
        self.audio_dropdown = ttk.Combobox(card, textvariable=self.state.audio_device, values=record.get_audio_device_names(), state=READONLY); self.audio_dropdown.pack(fill=X, pady=4); self.audio_dropdown.bind("<<ComboboxSelected>>", self.update_device_index)
        self.device_index_label = ttk.Label(card, text="Index: -", font=("TkDefaultFont", 8)); self.device_index_label.pack(anchor="w")
        self.level_meter = ttk.Progressbar(card, length=200, maximum=100, value=0, style="Asr.Horizontal.TProgressbar"); self.level_meter.pack(fill=X, pady=(8, 0))

    def _create_target_card(self, parent):
        card = ttk.Labelframe(parent, text="TARGET WINDOW", style="Card.TLabelframe", padding=8); card.pack(fill=X, pady=4)
        ttk.Label(card, text="対象ウィンドウ:").pack(anchor="w")
        win_frame = ttk.Frame(card); win_frame.pack(fill=X)
        self.window_dropdown = ttk.Combobox(win_frame, textvariable=self.state.window_title, values=visual_capture.list_available_windows(), state=READONLY); self.window_dropdown.pack(side=LEFT, fill=X, expand=True); self.window_dropdown.bind("<<ComboboxSelected>>", self.update_window)
        ttk.Button(win_frame, text="🔄", command=self.refresh_window_list, width=3).pack(side=LEFT, padx=2)
        self.selected_window_label = ttk.Label(card, text="Selected: -", font=("TkDefaultFont", 8), wraplength=250); self.selected_window_label.pack(anchor="w", pady=2)

    def _create_status_dashboard(self, parent):
        self.status_frame = ttk.Frame(parent, padding=2); self.status_frame.pack(fill=X, pady=(0, 4))
        ttk.Label(self.status_frame, text="SYSTEM STATUS", style="Header.TLabel").pack(side=LEFT)
        self.indicator_container = ttk.Frame(self.status_frame); self.indicator_container.pack(side=RIGHT)
        self.asr_status = ttk.Label(self.indicator_container, text="● MIC", style="Status.TLabel"); self.asr_status.pack(side=LEFT, padx=8)
        self.gemini_status = ttk.Label(self.indicator_container, text="● BRAIN", style="Status.TLabel"); self.gemini_status.pack(side=LEFT, padx=8)
        self.tts_status = ttk.Label(self.indicator_container, text="● VOICE", style="Status.TLabel"); self.tts_status.pack(side=LEFT, padx=8)

    def _create_log_area(self, parent):
        log_container = ttk.Labelframe(parent, text="LOGS", style="Card.TLabelframe"); log_container.pack(fill=BOTH, expand=True)
        filter_frame = ttk.Frame(log_container); filter_frame.pack(fill=X, padx=4, pady=2)
        log_levels = {"DEBUG": "secondary", "INFO": "info", "WARNING": "warning", "ERROR": "danger"}
        self.log_filters = {}
        for level, bstyle in log_levels.items():
            var = ttk.BooleanVar(value=True); cb = ttk.Checkbutton(filter_frame, text=level, variable=var, style=f"{bstyle}.TCheckbutton", command=self._refilter_logs); cb.pack(side=LEFT, padx=4); self.log_filters[level] = var
        self.log_textbox = ttk.ScrolledText(log_container, height=5, wrap=WORD, font=("Consolas", 9)); self.log_textbox.pack(fill=BOTH, expand=True, padx=4, pady=4); self.log_textbox.config(state="disabled")
        self.log_textbox.tag_config("DEBUG", foreground="gray"); self.log_textbox.tag_config("INFO", foreground="#007bff"); self.log_textbox.tag_config("WARNING", foreground="#ffc107"); self.log_textbox.tag_config("ERROR", foreground="#dc3545"); self.log_textbox.tag_config("CRITICAL", foreground="#dc3545", font=("TkDefaultFont", 10, "bold"))

    def update_status(self, key, active):
        if key == 'asr': self.asr_status.config(style="Status.Asr.TLabel" if active else "Status.TLabel")
        elif key == 'gemini': self.gemini_status.config(style="Status.Gemini.TLabel" if active else "Status.TLabel")
        elif key == 'tts': self.tts_status.config(style="Status.Tts.TLabel" if active else "Status.TLabel")

    def _update_auto_commentary_bar_loop(self):
        try:
            if hasattr(self, 'session_manager') and self.session_manager.session_running and self.state.enable_auto_commentary.get():
                service = self.session_manager.auto_commentary_service; rem, tot = service.get_remaining_time()
                if tot > 0: self.auto_commentary_bar['value'] = ((tot-rem)/tot)*100; self.auto_commentary_label.config(text=f"Next Commentary: {int(rem)}s")
                else: self.auto_commentary_bar['value'] = 0; self.auto_commentary_label.config(text="Waiting for silence...")
            else: self.auto_commentary_bar['value'] = 0; self.auto_commentary_label.config(text="Inactive")
        except Exception: pass
        self.root.after(200, self._update_auto_commentary_bar_loop)

    def open_settings_window(self): SettingsWindow(self.root, self)
    def open_memory_window(self): MemoryWindow(self.root, self, self.memory_manager, self.gemini_service)

    def on_closing(self):
        self.cleanup_temp_files(); self.stop_vits2_server()
        self.memory_manager.stop(); self.tts_manager.stop(); self.root.destroy()

    def cleanup_temp_files(self):
        for f in glob.glob("temp_recording_*.wav"):
            try: os.remove(f)
            except: pass

    def toggle_session(self): 
        if self.session_manager.is_session_active(): self.stop_session()
        else: self.start_session()
    def start_session(self): 
        self.session_manager.start_session(); self.start_session_button.pack_forget(); self.stop_session_button.pack(fill=X, pady=2, before=self.settings_btn)
    def stop_session(self): 
        self.session_manager.stop_session(); self.stop_session_button.pack_forget(); self.start_session_button.pack(fill=X, pady=2, before=self.settings_btn)
        if self.state.create_blog_post.get(): threading.Thread(target=self.generate_and_save_blog_post).start()

    def generate_and_save_blog_post(self, c=None):
        try:
            if c is None: c = self.session_manager.get_session_conversation()
            if not c: return
            bp = self.gemini_service.generate_blog_post(c)
            if bp:
                if not os.path.exists("blogs"): os.makedirs("blogs")
                with open(os.path.join("blogs", f"{datetime.now().strftime('%Y-%m-%d')}.md"), "w", encoding="utf-8") as f: f.write(bp)
        except Exception as e: logging.error(f"Blog Error: {e}")

    def update_device_index(self, e=None):
        n = self.state.audio_device.get(); self.state.device_index = record.get_device_index_from_name(n)
        logging.info(f"オーディオ入力デバイスを選択しました: {n} (Index: {self.state.device_index})")
        self.device_index_label.config(text=f"Index: {self.state.device_index} - {n}")
        self.state.save("audio_device", n)

    def update_window(self, e=None):
        t = self.state.window_title.get(); self.state.current_window = visual_capture.get_window_by_title(t)
        if self.state.current_window: logging.info(f"対象ウィンドウを選択しました: {t}")
        self.selected_window_label.config(text=f"Selected: {t if self.state.current_window else '(Not Found)'}")
        self.state.save("window", t)

    def refresh_window_list(self): 
        self.window_dropdown['values'] = visual_capture.list_available_windows(); self.update_window()

    def update_level_meter(self, v):
        lvl = int(v / 100); self.root.after(0, lambda: self.level_meter.config(value=lvl))
        if lvl > 5: self.root.after(0, lambda: self.update_status('asr', True)); self.root.after(500, lambda: self.update_status('asr', False))

    def execute_gemini_interaction(self, p, i, h):
        self.root.after(0, lambda: self.update_status('gemini', True))
        try:
            self.memory_manager.enqueue_save({'type': 'user_prompt', 'source': self.state.user_name.get(), 'content': p, 'timestamp': datetime.now().isoformat()})
            s, full = self.gemini_service.ask_stream(p, i, self.state.is_private.get(), session_history=h), ""
            for sent in gemini.split_into_sentences(s):
                if voice.stop_playback_event.is_set(): break
                full += sent; self.root.after(0, self.show_gemini_response, full); self.tts_manager.put_text(sent)
            if full:
                self.memory_manager.enqueue_save({'type': 'ai_response', 'source': 'AI', 'content': full, 'timestamp': datetime.now().isoformat()})
                if self.session_manager.session_memory: self.session_manager.session_memory.events.append(GeminiResponse(content=full))
                self.tts_manager.put_text("END_MARKER")
        except: pass
        finally: self.root.after(0, lambda: (self.update_status('gemini', False), self.finalize_response_processing()))

    def finalize_response_processing(self): 
        if os.path.exists(self.state.audio_file_path): os.remove(self.state.audio_file_path)
        if os.path.exists(self.state.screenshot_file_path): os.remove(self.state.screenshot_file_path)
    def update_asr_display(self, t, f=False):
        self.asr_text_area.config(state="normal")
        if f:
            if not hasattr(self, 'asr_history'): self.asr_history = []
            self.asr_history.append(t); self.asr_history = self.asr_history[-10:]; self.asr_text_area.delete("1.0", END)
            for l in self.asr_history: self.asr_text_area.insert(END, l + "\n")
            self.update_status('asr', False)
        else:
            self.asr_text_area.delete("1.0", END)
            for l in getattr(self, 'asr_history', []): self.asr_text_area.insert(END, l + "\n")
            self.asr_text_area.insert(END, ">>> " + t); self.update_status('asr', True)
        self.asr_text_area.see(END); self.asr_text_area.config(state="disabled")
    def show_gemini_response(self, t, auto=False, timer=False):
        if self.state.show_response_in_new_window.get():
            if self.current_response_window and self.current_response_window.winfo_exists():
                if not timer: self.current_response_window.set_response_text(t, auto_close=auto)
                else: self.current_response_window.start_close_timer()
            elif not timer: self.current_response_window = GeminiResponseWindow(self.root, t, self.state.response_display_duration.get()); (self.current_response_window.start_close_timer() if auto else None)
        else:
            if not timer: self.response_text_area.config(state="normal"); self.response_text_area.delete("1.0", END); self.response_text_area.insert(END, t); self.response_text_area.see(END); self.response_text_area.config(state="disabled")
            if auto: self.root.after(self.state.response_display_duration.get(), self._clear_response_area)
    def _clear_response_area(self): self.response_text_area.config(state="normal"); self.response_text_area.delete("1.0", END); self.response_text_area.config(state="disabled")
    def schedule_twitch_mention(self, a, p, c): (asyncio.run_coroutine_threadsafe(self.handle_twitch_mention(a, p, c), self.twitch_service.twitch_bot_loop) if self.twitch_service.twitch_bot_loop else None)
    async def handle_twitch_mention(self, a, p, c):
        try:
            r = await asyncio.to_thread(self.gemini_service.ask, p, None, self.state.is_private.get(), session_history=self.session_manager.get_session_history())
            if r and self.twitch_service.twitch_bot: await self.twitch_service.twitch_bot.send_chat_message(c, r)
        except: pass
    def process_prompt(self, p, h, s=None): threading.Thread(target=self.process_prompt_thread, args=(p, h, s)).start()
    def process_prompt_thread(self, p, h, s=None):
        if p and ("まて" in p or "待て" in p): voice.play_wav_file("wav/nod/5.wav"); return
        if p: self.execute_gemini_interaction(p, s, h)
    def _setup_logging(self):
        if not os.path.exists("logs"): os.makedirs("logs")
        self.log_queue = queue.Queue(); root = logging.getLogger(); [root.removeHandler(h) for h in root.handlers[:]]
        fmt = logging.Formatter('%(asctime)s - %(levelname)s - %(message)s')
        sh, fh, qh = logging.StreamHandler(), logging.FileHandler("logs/app.log", encoding='utf-8'), QueueHandler(self.log_queue)
        [h.setFormatter(fmt) for h in [sh, fh]]; [root.addHandler(h) for h in [sh, fh, qh]]; root.setLevel(logging.INFO)
        sys.stdout, sys.stderr = LoggingStream(logging.INFO), LoggingStream(logging.ERROR)
    def _process_log_queue(self):
        try:
            while True: self._write_log(self.log_queue.get_nowait())
        except queue.Empty: pass
        self.root.after(100, self._process_log_queue)
    def _refilter_logs(self):
        self.log_textbox.config(state="normal"); self.log_textbox.delete("1.0", END); self.log_textbox.config(state="disabled"); [self._write_log(r, from_history=True) for r in self.log_history]
    def on_tts_engine_change(self):
        eng = self.state.tts_engine.get()
        if eng == "style_bert_vits2" and self.vits2_server_process is None:
            if messagebox.askokcancel("VITS2", "VITS2サーバーを起動しますか？"): self.start_vits2_server()
            else: self.state.tts_engine.set(self.state.last_engine); return
        self.state.last_engine = eng; self.state.save('tts_engine', eng)
        if eng == "style_bert_vits2" and hasattr(self, 'vits2_config_frame') and self.vits2_config_frame.winfo_exists(): self.vits2_config_frame.pack(fill=X, pady=4); self.refresh_vits2_models()
        elif hasattr(self, 'vits2_config_frame') and self.vits2_config_frame.winfo_exists(): self.vits2_config_frame.pack_forget()

    def on_vits2_model_change(self, e=None):
        if not hasattr(self, 'vits2_model_dropdown') or not self.vits2_model_dropdown.winfo_exists(): return
        n = self.vits2_model_dropdown.get() 
        for s in getattr(self, 'vits2_speakers', []):
            if s['name'] == n:
                sid = s['styles'][0]['id']; self.state.vits2_speaker_id.set(sid); self.state.save('vits2_speaker_id', sid); self.pre_load_vits2_model(sid); break
    def pre_load_vits2_model(self, sid):
        def _req():
            try: import requests; requests.post(f"http://localhost:50021/initialize?speaker={sid}", timeout=300)
            except: pass
        threading.Thread(target=_req, daemon=True).start()
    def refresh_vits2_models(self):
        def _f():
            import requests
            for _ in range(10):
                try:
                    r = requests.get("http://localhost:50021/speakers", timeout=2)
                    if r.status_code == 200:
                        self.vits2_speakers = r.json(); names = [s['name'] for s in self.vits2_speakers]
                        self.root.after(0, lambda: self._update_vits2_dropdown(names)); self.pre_load_vits2_model(self.state.vits2_speaker_id.get()); return
                except: pass
                time.sleep(1)
        threading.Thread(target=_f, daemon=True).start()
    def _update_vits2_dropdown(self, n):
        if hasattr(self, 'vits2_model_dropdown') and self.vits2_model_dropdown.winfo_exists(): self.vits2_model_dropdown.config(values=n); self.vits2_model_dropdown.set(n[0] if n else "")
    def start_vits2_server(self):
        if self.vits2_server_process is None:
            try:
                self.vits2_job = win32job.CreateJobObject(None, ""); info = win32job.QueryInformationJobObject(self.vits2_job, win32job.JobObjectExtendedLimitInformation)
                info['BasicLimitInformation']['LimitFlags'] = win32job.JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE; win32job.SetInformationJobObject(self.vits2_job, win32job.JobObjectExtendedLimitInformation, info)
                self.vits2_server_process = subprocess.Popen([sys.executable, "scripts/vits2_server.py"], creationflags=subprocess.CREATE_NO_WINDOW | win32con.HIGH_PRIORITY_CLASS); win32job.AssignProcessToJobObject(self.vits2_job, self.vits2_server_process._handle)
            except: pass
    def stop_vits2_server(self): (self.vits2_server_process.terminate() if self.vits2_server_process else None); self.vits2_server_process = None
    def _write_log(self, record, from_history=False):
        if not from_history: self.log_history.append(record)
        if not self.log_filters.get(record.levelname, ttk.BooleanVar(value=True)).get(): return
        log_level_emojis = {'DEBUG': '⚙️', 'INFO': '🔵', 'WARNING': '🟡', 'ERROR': '🔴', 'CRITICAL': '🔥'}
        msg_content = record.getMessage(); levelname = record.levelname
        noise_keywords = ['Embedding', 'Batch', 'onnx', 'cudnn', 'Batches:', 'llama_', 'n_ctx', 'SWA cache']
        if levelname == 'ERROR' and any(k in msg_content for k in noise_keywords): levelname = 'INFO'
        tag_name = levelname
        if levelname == 'INFO' and any(k in msg_content for k in noise_keywords): tag_name = 'DEFAULT'
        self.log_textbox.config(state="normal")
        msg = f"{datetime.fromtimestamp(record.created).strftime('%H:%M:%S')} {log_level_emojis.get(record.levelname, ' ')} [{record.levelname}] {msg_content}\n"
        if tag_name == 'DEFAULT': self.log_textbox.insert(END, msg)
        else: self.log_textbox.insert(END, msg, tag_name)
        self.log_textbox.see(END); self.log_textbox.config(state="disabled")
