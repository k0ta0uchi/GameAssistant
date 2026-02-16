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
from .components import GeminiResponseWindow, MemoryWindow, SettingsWindow
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
        self.style = Style(theme="superhero") 
        self.root.title("GameAssistant - AI Companion")
        self.root.geometry("1100x850")
        self._setup_custom_styles()
        self.cleanup_temp_files()
        self.settings_manager = SettingsManager()
        self.audio_devices = record.get_audio_device_names()
        self.selected_device = ttk.StringVar(value=self.settings_manager.get("audio_device", self.audio_devices[0] if self.audio_devices else ""))
        self.device_index = None
        self.loopback_device_index = None
        self.windows = visual_capture.list_available_windows()
        self.selected_window_title = ttk.StringVar(value=self.settings_manager.get("window", self.windows[0] if self.windows else ""))
        self.selected_window = None
        self.custom_instruction = SYSTEM_INSTRUCTION_CHARACTER
        self.prompt = self.response = None
        self.use_image = ttk.BooleanVar(value=self.settings_manager.get("use_image", True))
        self.is_private = ttk.BooleanVar(value=self.settings_manager.get("is_private", True))
        self.show_response_in_new_window = ttk.BooleanVar(value=self.settings_manager.get("show_response_in_new_window", True))
        self.response_display_duration = ttk.IntVar(value=self.settings_manager.get("response_display_duration", 10000))
        self.tts_engine = ttk.StringVar(value=self.settings_manager.get("tts_engine", "voicevox"))
        self.last_engine = self.tts_engine.get()
        self.vits2_speaker_id = ttk.IntVar(value=self.settings_manager.get("vits2_speaker_id", 0))
        self.vits2_server_process = None
        self.disable_thinking_mode = ttk.BooleanVar(value=self.settings_manager.get("disable_thinking_mode", False))
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
        self.twitch_last_mention_time, self.twitch_mention_cooldown, self.log_history = {}, 30, []
        self.create_widgets(); self._setup_logging()
        self.audio_file_path, self.screenshot_file_path, self.image = os.path.abspath("temp_recording.wav"), os.path.abspath("temp_screenshot.png"), None
        keyboard.add_hotkey("ctrl+shift+f2", self.toggle_session); self._process_log_queue()
        if self.audio_devices: self.update_device_index()
        if self.windows: self.update_window()
        self.root.protocol("WM_DELETE_WINDOW", self.on_closing)
        self.db_save_queue, self.tts_queue, self.playback_queue = queue.Queue(), queue.Queue(), queue.Queue()
        threading.Thread(target=self._process_db_save_queue, daemon=True).start()
        threading.Thread(target=self._tts_synthesis_worker, daemon=True).start()
        threading.Thread(target=self._tts_playback_worker, daemon=True).start()
        self.current_response_window = None
        if self.tts_engine.get() == "style_bert_vits2": self.start_vits2_server()
        self.on_tts_engine_change(); self._update_auto_commentary_bar_loop()

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
        # 1. 左サイドバー (幅を300に微調整)
        self.sidebar = ttk.Frame(self.main_container, width=300); self.sidebar.pack(side=LEFT, fill=Y, padx=(2, 0)); self.sidebar.pack_propagate(False)
        
        # スクロールバーを左(side=LEFT)に配置して、右側の隙間を消す
        canvas = ttk.Canvas(self.sidebar, background="#0F0F23", highlightthickness=0)
        scroll = ttk.Scrollbar(self.sidebar, orient=VERTICAL, command=canvas.yview)
        scroll.pack(side=LEFT, fill=Y) # 左端へ
        
        self.sidebar_scrollable = ttk.Frame(canvas); self.sidebar_scrollable.bind("<Configure>", lambda e: canvas.configure(scrollregion=canvas.bbox("all")))
        canvas.create_window((0, 0), window=self.sidebar_scrollable, anchor="nw", width=280) 
        canvas.configure(yscrollcommand=scroll.set)
        canvas.pack(side=LEFT, fill=BOTH, expand=True)
        
        self._create_audio_card(self.sidebar_scrollable); self._create_target_card(self.sidebar_scrollable)
        
        # アクションボタン
        btns = ttk.Frame(self.sidebar_scrollable, padding=2); btns.pack(fill=X, pady=4)
        self.start_session_button = ttk.Button(btns, text="🚀 Start Session", style="success.TButton", command=self.start_session)
        self.start_session_button.pack(fill=X, pady=2)
        self.stop_session_button = ttk.Button(btns, text="🛑 Stop Session", style="danger.TButton", command=self.stop_session)
        self.stop_session_button.pack(fill=X, pady=2); self.stop_session_button.pack_forget()

        self.sidebar_sep = ttk.Separator(self.sidebar_scrollable, orient="horizontal")
        self.sidebar_sep.pack(fill=X, pady=5)
        
        ttk.Button(btns, text="⚙️ Settings", command=self.open_settings_window, style="secondary.TButton").pack(fill=X, pady=2)
        ttk.Button(btns, text="📂 Memory", command=self.open_memory_window, style="info.TButton").pack(fill=X, pady=2)
        
        # 2. 右メインエリア (padxの左側を0に)
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
        ttk.Label(card, text="インプットデバイス:", background="#1a1a3a").pack(anchor="w")
        self.audio_dropdown = ttk.Combobox(card, textvariable=self.selected_device, values=self.audio_devices, state=READONLY); self.audio_dropdown.pack(fill=X, pady=4)
        self.audio_dropdown.bind("<<ComboboxSelected>>", self.update_device_index)
        self.device_index_label = ttk.Label(card, text="Index: -", font=("TkDefaultFont", 8), background="#1a1a3a"); self.device_index_label.pack(anchor="w")
        self.level_meter = ttk.Progressbar(card, length=200, maximum=100, value=0, style="Asr.Horizontal.TProgressbar"); self.level_meter.pack(fill=X, pady=(8, 0))

    def _create_target_card(self, parent):
        card = ttk.Labelframe(parent, text="TARGET WINDOW", style="Card.TLabelframe", padding=8); card.pack(fill=X, pady=4)
        ttk.Label(card, text="対象ウィンドウ:", background="#1a1a3a").pack(anchor="w")
        win_frame = ttk.Frame(card); win_frame.pack(fill=X) # Fixed: Removed invalid background configure
        self.window_dropdown = ttk.Combobox(win_frame, textvariable=self.selected_window_title, values=self.windows, state=READONLY); self.window_dropdown.pack(side=LEFT, fill=X, expand=True)
        self.window_dropdown.bind("<<ComboboxSelected>>", self.update_window)
        ttk.Button(win_frame, text="🔄", command=self.refresh_window_list, width=3).pack(side=LEFT, padx=2)
        self.selected_window_label = ttk.Label(card, text="Selected: -", font=("TkDefaultFont", 8), background="#1a1a3a", wraplength=250); self.selected_window_label.pack(anchor="w", pady=2)

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
            if hasattr(self, 'session_manager') and self.session_manager.session_running and self.enable_auto_commentary.get():
                service = self.session_manager.auto_commentary_service; rem, tot = service.get_remaining_time()
                if tot > 0: self.auto_commentary_bar['value'] = ((tot-rem)/tot)*100; self.auto_commentary_label.config(text=f"Next Commentary: {int(rem)}s")
                else: self.auto_commentary_bar['value'] = 0; self.auto_commentary_label.config(text="Waiting for silence...")
            else: self.auto_commentary_bar['value'] = 0; self.auto_commentary_label.config(text="Inactive")
        except Exception: pass
        self.root.after(200, self._update_auto_commentary_bar_loop)

    def open_settings_window(self): SettingsWindow(self.root, self)
    def open_memory_window(self): MemoryWindow(self.root, self, self.memory_manager, self.gemini_service)

    def _tts_synthesis_worker(self):
        while True:
            item = self.tts_queue.get()
            if item is None: break
            if item == "END_MARKER": self.playback_queue.put("END_MARKER"); self.tts_queue.task_done(); continue
            sentences = [s.strip() for s in re.split(r'([、,])', item) if s.strip()] if len(item) > 100 else [item]
            for sub in sentences:
                try:
                    if not voice.stop_playback_event.is_set():
                        wav = voice.generate_speech_data(sub)
                        if wav: self.playback_queue.put(wav)
                except Exception as e: logging.error(f"TTS合成エラー: {e}")
            self.tts_queue.task_done()

    def _tts_playback_worker(self):
        while True:
            item = self.playback_queue.get()
            if item is None: break
            if item == "END_MARKER":
                self.root.after(0, lambda: (self.show_gemini_response(None, auto_close=True, only_timer=True), self.update_status('tts', False)))
                self.playback_queue.task_done(); continue
            try:
                if not voice.stop_playback_event.is_set():
                    self.root.after(0, lambda: self.update_status('tts', True)); voice.play_wav_data(item, volume=0.5)
            except Exception as e: logging.error(f"TTS再生エラー: {e}")
            finally: self.playback_queue.task_done()

    def _process_db_save_queue(self):
        while True:
            try:
                task = self.db_save_queue.get()
                if task is None: break
                t, f, d = task.get('type'), task.get('future'), task.get('data') or task
                try:
                    if t == 'query': res = self.memory_manager.query_collection(**d)
                    elif t == 'summarize_and_save': res = self.memory_manager.summarize_and_add_memory(**d)
                    else: res = self.memory_manager.save_event_to_chroma_sync(d)
                    if f: f.set_result(res)
                except Exception as e:
                    if f: f.set_exception(e)
            except Exception: pass

    def on_closing(self): self.cleanup_temp_files(); self.stop_vits2_server(); self.db_save_queue.put(None); self.db_worker_thread.join(); self.root.destroy()
    def cleanup_temp_files(self): 
        for f in glob.glob("temp_recording_*.wav"):
            try: os.remove(f)
            except: pass
    def get_device_index_from_name(self, n): return record.get_device_index_from_name(n)
    def toggle_session(self): 
        if self.session_manager.is_session_active(): self.stop_session()
        else: self.start_session()
    def start_session(self):
        self.session_manager.start_session()
        self.start_session_button.pack_forget()
        self.stop_session_button.pack(fill=X, pady=2, before=self.sidebar_sep)

    def stop_session(self):
        self.session_manager.stop_session()
        self.stop_session_button.pack_forget()
        self.start_session_button.pack(fill=X, pady=2, before=self.sidebar_sep)
        if self.create_blog_post.get():
            threading.Thread(target=self.generate_and_save_blog_post).start()
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
        n = self.selected_device.get(); self.device_index = self.get_device_index_from_name(n); self.device_index_label.config(text=f"Index: {self.device_index} - {n}"); self.settings_manager.set("audio_device", n); self.settings_manager.save(self.settings_manager.settings)
    def update_window(self, e=None):
        t = self.selected_window_title.get(); self.selected_window = visual_capture.get_window_by_title(t); self.selected_window_label.config(text=f"Selected: {t if self.selected_window else '(Not Found)'}"); self.settings_manager.set("window", t); self.settings_manager.save(self.settings_manager.settings)
    def refresh_window_list(self): self.windows = visual_capture.list_available_windows(); self.window_dropdown['values'] = self.windows; self.update_window()
    def update_record_buttons_state(self, e=None): pass
    def update_level_meter(self, v):
        lvl = int(v / 100); self.root.after(0, lambda: self.level_meter.config(value=lvl))
        if lvl > 5: self.root.after(0, lambda: self.update_status('asr', True)); self.root.after(500, lambda: self.update_status('asr', False))
    def execute_gemini_interaction(self, p, i, h):
        self.root.after(0, lambda: self.update_status('gemini', True))
        try:
            self.db_save_queue.put({'type': 'save', 'data': {'type': 'user_prompt', 'source': self.user_name.get(), 'content': p, 'timestamp': datetime.now().isoformat()}})
            s, full = self.gemini_service.ask_stream(p, i, self.is_private.get(), session_history=h), ""
            for sent in gemini.split_into_sentences(s):
                if voice.stop_playback_event.is_set(): break
                full += sent; self.root.after(0, self.show_gemini_response, full); self.tts_queue.put(sent)
            if full:
                self.db_save_queue.put({'type': 'save', 'data': {'type': 'ai_response', 'source': 'AI', 'content': full, 'timestamp': datetime.now().isoformat()}})
                if self.session_manager.session_memory: self.session_manager.session_memory.events.append(GeminiResponse(content=full))
                self.tts_queue.put("END_MARKER")
        except: pass
        finally: self.root.after(0, lambda: (self.update_status('gemini', False), self.finalize_response_processing()))
    def finalize_response_processing(self): 
        if os.path.exists(self.audio_file_path): os.remove(self.audio_file_path)
        if os.path.exists(self.screenshot_file_path): os.remove(self.screenshot_file_path)
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
        if self.show_response_in_new_window.get():
            if self.current_response_window and self.current_response_window.winfo_exists():
                if not timer: self.current_response_window.set_response_text(t, auto_close=auto)
                else: self.current_response_window.start_close_timer()
            elif not timer: self.current_response_window = GeminiResponseWindow(self.root, t, self.response_display_duration.get()); (self.current_response_window.start_close_timer() if auto else None)
        else:
            if not timer: self.response_text_area.config(state="normal"); self.response_text_area.delete("1.0", END); self.response_text_area.insert(END, t); self.response_text_area.see(END); self.response_text_area.config(state="disabled")
            if auto: self.root.after(self.response_display_duration.get(), self._clear_response_area)
    def _clear_response_area(self): self.response_text_area.config(state="normal"); self.response_text_area.delete("1.0", END); self.response_text_area.config(state="disabled")
    def schedule_twitch_mention(self, a, p, c): (asyncio.run_coroutine_threadsafe(self.handle_twitch_mention(a, p, c), self.twitch_service.twitch_bot_loop) if self.twitch_service.twitch_bot_loop else None)
    async def handle_twitch_mention(self, a, p, c):
        try:
            r = await asyncio.to_thread(self.gemini_service.ask, p, None, self.is_private.get(), session_history=self.session_manager.get_session_history())
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
        eng = self.tts_engine.get()
        if eng == "style_bert_vits2" and self.vits2_server_process is None:
            if messagebox.askokcancel("VITS2", "VITS2サーバーを起動しますか？"): self.start_vits2_server()
            else: self.tts_engine.set(self.last_engine); return
        self.last_engine = eng; self.settings_manager.set('tts_engine', eng); self.settings_manager.save(self.settings_manager.settings)
        if eng == "style_bert_vits2" and hasattr(self, 'vits2_config_frame'): self.vits2_config_frame.pack(fill=X, pady=4); self.refresh_vits2_models()
        elif hasattr(self, 'vits2_config_frame'): self.vits2_config_frame.pack_forget()
    def on_vits2_model_change(self, e=None):
        n = self.app.vits2_model_dropdown.get() # Corrected ref
        for s in getattr(self, 'vits2_speakers', []):
            if s['name'] == n:
                sid = s['styles'][0]['id']; self.vits2_speaker_id.set(sid); self.settings_manager.set('vits2_speaker_id', sid); self.settings_manager.save(self.settings_manager.settings); self.pre_load_vits2_model(sid); break
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
                        self.root.after(0, lambda: self._update_vits2_dropdown(names)); self.pre_load_vits2_model(self.vits2_speaker_id.get()); return
                except: pass
                time.sleep(1)
        threading.Thread(target=_f, daemon=True).start()
    def _update_vits2_dropdown(self, n): (self.vits2_model_dropdown.config(values=n) if hasattr(self, 'vits2_model_dropdown') else None); (self.vits2_model_dropdown.set(n[0]) if n and hasattr(self, 'vits2_model_dropdown') else None)
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
        self.log_textbox.config(state="normal"); msg = f"{datetime.fromtimestamp(record.created).strftime('%H:%M:%S')} [{record.levelname}] {record.getMessage()}\n"; self.log_textbox.insert(END, msg, record.levelname); self.log_textbox.see(END); self.log_textbox.config(state="disabled")
