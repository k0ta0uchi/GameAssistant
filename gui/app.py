from tkinter import font
import ttkbootstrap as ttk
from ttkbootstrap.constants import (
    END, BOTH, LEFT, RIGHT, Y, X, VERTICAL, WORD, READONLY
)
import scripts.record as record
import scripts.whisper as whisper
import scripts.gemini as gemini
import scripts.voice as voice
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
from scripts.memory import MemoryManager
from twitchio.utils import setup_logging
import logging
import scripts.capture as capture
from scripts.settings import SettingsManager
from scripts.record import AudioService
from scripts.capture import CaptureService
from .components import OutputRedirector, GeminiResponseWindow, MemoryWindow


class GameAssistantApp:
    def __init__(self, root):
        self.root = root
        self.root.title("ゲームアシスタント")

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

        self.custom_instruction = """
あなたは、ユーザーの質問に答える優秀なAIアシスタントです。  
あなたは**優しい女の子の犬のキャラクター**として振る舞います。以下の指示に従って応答してください。
---
## 応答生成手順
1. **画像やスクリーンショットの解析**  
   - 提供されている場合は、画像やスクリーンショットを解析してください。  
   - ゲーム内のUI、キャラクターの状態、アイテム、ステータスなどを特定し、適切なアドバイスや行動案を提供してください。
2. **過去の会話の考慮**  
   - 過去の会話内容を自然に考慮してください。  
   - 明示的に「覚えています」などとは言わないでください。
3. **応答生成ルール**  
   - フレンドリーで親しみやすい口調を使用する  
   - 文末には「だわん」を使用  
   - すべての英単語をカタカナに変換  
   - 通常は2文程度の短い応答を心がける  
   - 詳細な説明や分析を求められた場合は長い応答も可能  
   - 検索結果や画像解析のまとめがある場合は、まとめて提示
4. **ゲームスクリーンショット解析の推奨**  
   - 推論能力をフル活用し、目に見える情報だけでなく、可能性の高い隠れ要素や戦略も含めた提案を行う
5. **応答内容の品質要件**  
   - ユーザーの要望に対する明確かつ直接的な回答  
   - 結論に至った理由の説明  
   - 代替案や高確度の仮説、斬新な視点の提供  
   - 適切な粒度のまとめや具体的行動計画
6. **注意事項**  
   - 事前学習の知識だけでの反射的な回答やWeb検索のみの曖昧回答は避ける  
   - わからない場合は留保や前提条件を明示  
   - 創造的で新たな可能性の提案も積極的に行う
---
## 応答例
> 「はいだわん！その質問面白いだわん！カメラのシャッターはチーズの速さで閉じるんだわん。もっと詳しく知りたいかしら？」
        """
        self.prompt = None
        self.response = None

        self.use_image = ttk.BooleanVar(value=self.settings_manager.get("use_image", True))
        self.is_private = ttk.BooleanVar(value=self.settings_manager.get("is_private", True))
        self.show_response_in_new_window = ttk.BooleanVar(value=self.settings_manager.get("show_response_in_new_window", True))
        self.response_display_duration = ttk.IntVar(value=self.settings_manager.get("response_display_duration", 10000))
        self.tts_engine = ttk.StringVar(value=self.settings_manager.get("tts_engine", "voicevox"))
        self.disable_thinking_mode = ttk.BooleanVar(value=self.settings_manager.get("disable_thinking_mode", False))

        self.twitch_bot_username = ttk.StringVar(value=self.settings_manager.get("twitch_bot_username", ""))
        self.twitch_client_id = ttk.StringVar(value=self.settings_manager.get("twitch_client_id", ""))
        self.twitch_client_secret = ttk.StringVar(value=self.settings_manager.get("twitch_client_secret", ""))
        self.twitch_bot_id = ttk.StringVar(value=self.settings_manager.get("twitch_bot_id", ""))
        self.twitch_auth_code = ttk.StringVar()

        self.audio_service = AudioService(self)
        self.capture_service = CaptureService(self)
        self.gemini_service = gemini.GeminiService(self.custom_instruction, self.settings_manager)
        self.memory_manager = MemoryManager()
        self.twitch_service = TwitchService(self)
        self.twitch_last_mention_time = {}
        self.twitch_mention_cooldown = 30

        self.create_widgets()

        self.redirector = OutputRedirector(self.output_textbox)
        sys.stdout = self.redirector

        self.audio_file_path = "temp_recording.wav"
        self.screenshot_file_path = "temp_screenshot.png"
        self.image = None

        keyboard.add_hotkey("ctrl+shift+f2", self.audio_service.toggle_recording)
        print("ホットキー (Ctrl+Shift+F2) が登録されました。")

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

        self.response_frame = ttk.Frame(right_frame, padding=(0, 0, 0, 10))
        self.response_frame.pack(fill=X)
        self.response_label = ttk.Label(self.response_frame, text="", wraplength=400, justify=LEFT, font=("Arial", 14), style="inverse-info")
        self.response_label.pack(fill=X, ipady=10)

        self.meter_container = ttk.Frame(right_frame)
        self.meter_container.pack(fill=X, pady=(0, 10))

        self.level_meter = ttk.Progressbar(
            self.meter_container, length=300, maximum=100, value=0, style="danger.Horizontal.TProgressbar"
        )
        self.level_meter.pack(pady=10)

        config_frame = ttk.Frame(left_frame)
        config_frame.pack(fill=X, pady=(0, 15))
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
        voicevox_radio = ttk.Radiobutton(tts_frame, text="VOICEVOX", variable=self.tts_engine, value="voicevox", command=lambda: (self.settings_manager.set('tts_engine', self.tts_engine.get()), self.settings_manager.save(self.settings_manager.settings)))
        voicevox_radio.pack(side=LEFT, padx=5)
        gemini_radio = ttk.Radiobutton(tts_frame, text="Gemini", variable=self.tts_engine, value="gemini", command=lambda: (self.settings_manager.set('tts_engine', self.tts_engine.get()), self.settings_manager.save(self.settings_manager.settings)))
        gemini_radio.pack(side=LEFT, padx=5)

        self.disable_thinking_mode_check = ttk.Checkbutton(
            config_frame, text="Thinkingモードをオフにする", variable=self.disable_thinking_mode,
            style="success-square-toggle",
            command=lambda: (self.settings_manager.set('disable_thinking_mode', self.disable_thinking_mode.get()), self.settings_manager.save(self.settings_manager.settings))
        )
        self.disable_thinking_mode_check.pack(fill=X, pady=5)

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
        bot_id_entry.bind("<FocusOut>", lambda e: (self.settings_manager.set('twitch_bot_id', self.twitch_bot_id.get()), self.settings_manager.save(self.settings_manager.settings)))

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

        self.image_frame = ttk.Frame(right_frame, height=300)
        self.image_frame.pack(fill=X, pady=10)
        self.image_frame.pack_propagate(False)

        self.image_label = ttk.Label(self.image_frame)
        self.image_label.pack(pady=10)

        self.text_container = ttk.Frame(right_frame)
        self.text_container.pack(fill=BOTH, expand=True)

        self.output_textbox = ttk.Text(master=self.text_container, height=5, width=50, wrap=WORD)
        self.output_textbox.pack(side=LEFT, fill=BOTH, expand=True, padx=(0, 5), pady=(0, 10))

        self.scrollbar = ttk.Scrollbar(self.text_container, orient=VERTICAL, command=self.output_textbox.yview)
        self.scrollbar.pack(side=RIGHT, fill=Y, pady=(0, 10))

        self.output_textbox['yscrollcommand'] = self.scrollbar.set

        self.record_container = ttk.Frame(right_frame)
        self.record_container.pack(fill=X, padx=10, pady=10)

        self.record_button = ttk.Button(self.record_container, text="録音開始", style="success.TButton", command=self.audio_service.toggle_recording)
        self.record_button.pack(side=LEFT, padx=5)

        self.record_wait_button = ttk.Button(self.record_container, text="録音待機", style="success.TButton", command=self.audio_service.toggle_record_waiting)
        self.record_wait_button.pack(side=LEFT, padx=5)

        if self.audio_devices:
            self.update_device_index()

        if self.windows:
            self.update_window()
        
        self.update_record_buttons_state()

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
            print(f"選択されたウィンドウ: {self.selected_window.title}")
            self.selected_window_label.config(text=f"選択されたウィンドウ: {self.selected_window.title}")
        else:
            print("ウィンドウが見つかりませんでした")
            self.selected_window_label.config(text="選択されたウィンドウ: (見つかりません)")
        self.settings_manager.set("window", selected_window_title)
        self.settings_manager.save(self.settings_manager.settings)
        self.update_record_buttons_state()

    def refresh_window_list(self):
        print("ウィンドウリストを更新します...")
        self.windows = capture.list_available_windows()
        self.window_dropdown['values'] = self.windows
        current_selection = self.selected_window_title.get()

        if self.windows:
            if current_selection not in self.windows:
                self.selected_window_title.set(self.windows[0])
        else:
            self.selected_window_title.set("")
        
        self.update_window()
        print("ウィンドウリストを更新しました。")

    def update_record_buttons_state(self, event=None):
        if self.use_image.get() and self.selected_window is None:
            self.record_button.config(state="disabled")
            self.record_wait_button.config(state="disabled")
            print("画像利用がオンですが、ウィンドウが選択されていないため録音ボタンを無効化しました。")
        else:
            self.record_button.config(state="normal")
            self.record_wait_button.config(state="normal")

    def update_level_meter(self, volume):
        level = int(volume / 100)
        self.root.after(0, self.set_level_meter_value, level)

    def set_level_meter_value(self, level):
        self.level_meter['value'] = level

    def transcribe_audio(self):
        print("音声認識を開始します...")
        try:
            text = whisper.recognize_speech(self.audio_file_path)
            if text:
                print(f"*** 認識されたテキスト: '{text}' ***")
            else:
                print("*** 音声は検出されましたが、テキストとして認識されませんでした。***")
            return text
        except Exception as e:
            print(f"音声認識エラー: {e}")
            return None

    async def process_and_respond_async(self, from_temporary_stop=False):
        # このメソッドは非同期で実行される
        prompt = self.transcribe_audio()

        # promptがNoneまたは空文字列の場合、処理を中断
        if not prompt:
            print("プロンプトが空のため、処理を中断します。")
            def enable_buttons():
                self.record_button.config(text="録音開始", style="success.TButton", state="normal")
                self.record_wait_button.config(text="録音待機", style="success.TButton", state="normal")
                if self.audio_service.record_waiting:
                    self.record_wait_button.config(text="録音待機中", style="danger.TButton")
                    self.audio_service.record_waiting_thread = threading.Thread(target=self.audio_service.wait_for_keyword_thread)
                    self.audio_service.record_waiting_thread.start()
            self.root.after(0, enable_buttons)
            return

        if "検索" in prompt or "けんさく" in prompt:
            search_keyword = prompt
            try:
                loop = asyncio.get_running_loop()
            except RuntimeError:
                loop = asyncio.new_event_loop()
                asyncio.set_event_loop(loop)
            search_results = loop.run_until_complete(self.run_ai_search(search_keyword))
            if search_results:
                prompt += "\n\n検索結果:\n" + "\n".join(search_results)

        self.prompt = prompt
        image_path = self.screenshot_file_path if self.use_image.get() and os.path.exists(self.screenshot_file_path) else None
        
        response = await self.gemini_service.ask(self.prompt, image_path, self.is_private.get()) # type: ignore
        self.response = response

        def update_gui_and_speak():
            if self.show_response_in_new_window.get():
                if response:
                    self.show_gemini_response(response)
            else:
                if response:
                    self.output_textbox.insert(END, "Geminiの回答: " + response + "\n")
                    self.output_textbox.see(END)
            voice.text_to_speech(response)
            if os.path.exists(self.audio_file_path):
                os.remove(self.audio_file_path)
            if os.path.exists(self.screenshot_file_path):
                os.remove(self.screenshot_file_path)
            if self.audio_service.record_waiting:
                self.record_wait_button.config(text="録音待機中", style="danger.TButton")
                self.audio_service.record_waiting_thread = threading.Thread(target=self.audio_service.wait_for_keyword_thread)
                self.audio_service.record_waiting_thread.start()
            self.record_button.config(text="録音開始", style="success.TButton", state="normal")
            self.record_wait_button.config(state="normal")
            if not self.audio_service.record_waiting:
                self.record_wait_button.config(text="録音待機", style="success.TButton")

        self.root.after(0, update_gui_and_speak)

    def process_and_respond(self, from_temporary_stop=False):
        loop = asyncio.new_event_loop()
        asyncio.set_event_loop(loop)
        loop.run_until_complete(self.process_and_respond_async(from_temporary_stop))

    def open_memory_window(self):
        """メモリー管理ウィンドウを開く"""
        MemoryWindow(self.root, self.memory_manager)

    def show_gemini_response(self, response_text):
        if self.show_response_in_new_window.get():
            GeminiResponseWindow(self.root, response_text, self.response_display_duration.get())
        else:
            self.response_label.config(text=response_text)
            self.root.after(self.response_display_duration.get(), lambda: self.response_label.config(text=""))

    async def run_ai_search(self, query: str):
        return await ai_search(query)
