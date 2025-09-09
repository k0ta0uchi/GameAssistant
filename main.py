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
from scripts.twitch_bot import TwitchBot, setup_database
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

class OutputRedirector:
    """print文をテキストボックスにリダイレクトするクラス"""
    def __init__(self, widget):
        self.widget = widget
        self.widget.tag_config("error", foreground="red")
        self.widget.tag_config("warning", foreground="yellow")
        self.widget.tag_config("success", foreground="green")
        self.widget.tag_config("info", foreground="cyan")

    def write(self, str):
        tag = None
        if "エラー" in str or "error" in str.lower():
            tag = "error"
        elif "警告" in str or "warning" in str.lower():
            tag = "warning"
        elif "成功" in str or "success" in str.lower() or "完了" in str:
            tag = "success"
        elif "***" in str:
            tag = "info"

        self.widget.insert(END, str, tag)
        self.widget.see(END)

    def flush(self):
        pass

class GeminiResponseWindow(ttk.Toplevel):
    def __init__(self, parent, response_text, duration=10000):
        super().__init__(parent)
        self.title("Gemini Response")
        self.geometry("600x400")
        self.label = None
        self.create_widgets()
        self.configure(background="green")
        self.after(duration, self.dim_text)

    def create_widgets(self):
        my_font = font.Font(family='Arial', size=20)

        self.label = ttk.Label(
            self,
            text="",
            wraplength=600,
            justify=LEFT,
            background="green",
            foreground="white",
            padding=10,
            font=my_font,
            borderwidth=2,
        )
        self.label.pack(expand=True, fill=X)

    def set_response_text(self, response_text):
        if self.label:
            self.label.configure(text=response_text)

    def close_window(self):
        self.destroy()

    def dim_text(self):
        if self.label:
            self.label.configure(text="")

class MemoryWindow(ttk.Toplevel):
    def __init__(self, parent, memory_manager):
        super().__init__(parent)
        self.parent = parent
        self.memory_manager = memory_manager
        self.title("メモリー管理")
        self.geometry("500x400")

        self.create_widgets()
        self.load_memories_to_listbox()

    def create_widgets(self):
        main_frame = ttk.Frame(self, padding=10)
        main_frame.pack(fill=BOTH, expand=True)

        left_frame = ttk.Frame(main_frame)
        left_frame.pack(side=LEFT, fill=BOTH, expand=True, padx=(0, 10))

        self.memory_listbox = ttk.Treeview(left_frame, columns=("key", "value"), show="headings")
        self.memory_listbox.heading("key", text="キー")
        self.memory_listbox.heading("value", text="値")
        self.memory_listbox.pack(fill=BOTH, expand=True)
        self.memory_listbox.bind("<<TreeviewSelect>>", self.on_memory_select)

        right_frame = ttk.Frame(main_frame, width=200)
        right_frame.pack(side=RIGHT, fill=Y)
        right_frame.pack_propagate(False)

        key_label = ttk.Label(right_frame, text="キー:")
        key_label.pack(fill=X, pady=(0, 5))
        self.key_entry = ttk.Entry(right_frame)
        self.key_entry.pack(fill=X, pady=(0, 10))

        value_label = ttk.Label(right_frame, text="値:")
        value_label.pack(fill=X, pady=(0, 5))
        self.value_text = ttk.Text(right_frame, height=5)
        self.value_text.pack(fill=BOTH, expand=True, pady=(0, 10))

        button_frame = ttk.Frame(right_frame)
        button_frame.pack(fill=X)

        save_button = ttk.Button(button_frame, text="保存", command=self.save_memory, style="success.TButton")
        save_button.pack(side=LEFT, expand=True, fill=X, padx=(0, 5))

        delete_button = ttk.Button(button_frame, text="削除", command=self.delete_memory, style="danger.TButton")
        delete_button.pack(side=LEFT, expand=True, fill=X)

    def load_memories_to_listbox(self):
        for item in self.memory_listbox.get_children():
            self.memory_listbox.delete(item)
        memories = self.memory_manager.get_all_memories()
        for key, value in memories.items():
            self.memory_listbox.insert("", "end", values=(key, value))

    def on_memory_select(self, event):
        selected_items = self.memory_listbox.selection()
        if not selected_items:
            return
        selected_item = selected_items[0]
        item = self.memory_listbox.item(selected_item)
        key, value = item['values']
        self.key_entry.delete(0, END)
        self.key_entry.insert(0, key)
        self.value_text.delete("1.0", END)
        self.value_text.insert("1.0", value)

    def save_memory(self):
        key = self.key_entry.get()
        value = self.value_text.get("1.0", END).strip()
        if not key:
            print("キーは必須です。")
            return
        self.memory_manager.add_or_update_memory(key, value)
        self.load_memories_to_listbox()
        self.clear_entries()

    def delete_memory(self):
        key = self.key_entry.get()
        if not key:
            print("削除するキーを指定してください。")
            return
        if self.memory_manager.delete_memory(key):
            self.load_memories_to_listbox()
            self.clear_entries()
        else:
            print("指定されたキーのメモリーが見つかりません。")

    def clear_entries(self):
        self.key_entry.delete(0, END)
        self.value_text.delete("1.0", END)
        self.memory_listbox.selection_remove(self.memory_listbox.selection())

class GameAssistantApp:
    def __init__(self, root):
        self.root = root
        self.root.title("ゲームアシスタント")

        self.settings_file = "settings.json"
        self.load_settings()

        self.audio_devices = record.get_audio_device_names()
        default_audio_device = self.settings.get("audio_device", self.audio_devices[0] if self.audio_devices else "")
        self.selected_device = ttk.StringVar(value=default_audio_device)
        self.device_index = None
        
        self.loopback_device_index = None
        self.recording = False
        self.recording_complete = False
        self.record_waiting = False
        self.stop_event = threading.Event()

        self.windows = capture.list_available_windows()
        default_window = self.settings.get("window", self.windows[0] if self.windows else "")
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

        self.use_image = ttk.BooleanVar(value=self.settings.get("use_image", True))
        self.is_private = ttk.BooleanVar(value=self.settings.get("is_private", True))
        self.show_response_in_new_window = ttk.BooleanVar(value=self.settings.get("show_response_in_new_window", True))
        self.response_display_duration = ttk.IntVar(value=self.settings.get("response_display_duration", 10000))
        self.tts_engine = ttk.StringVar(value=self.settings.get("tts_engine", "voicevox"))

        self.twitch_bot_username = ttk.StringVar(value=self.settings.get("twitch_bot_username", ""))
        self.twitch_client_id = ttk.StringVar(value=self.settings.get("twitch_client_id", ""))
        self.twitch_client_secret = ttk.StringVar(value=self.settings.get("twitch_client_secret", ""))
        self.twitch_bot_id = ttk.StringVar(value=self.settings.get("twitch_bot_id", ""))
        self.twitch_auth_code = ttk.StringVar() # 認証コード入力用
        self.twitch_is_bot_auth = ttk.BooleanVar(value=False) # ボット自身の認証かどうかのフラグ

        self.session = gemini.GeminiSession(self.custom_instruction)
        self.memory_manager = MemoryManager()
        self.twitch_bot = None
        self.twitch_thread = None
        self.twitch_bot_loop = None
        self.twitch_last_mention_time = {}
        self.twitch_mention_cooldown = 30 # クールダウンタイム（秒）

        self.create_widgets()

        self.redirector = OutputRedirector(self.output_textbox)
        sys.stdout = self.redirector

        self.audio_file_path = "temp_recording.wav"
        self.screenshot_file_path = "temp_screenshot.png"
        self.image = None

        keyboard.add_hotkey("ctrl+shift+f2", self.toggle_recording)
        print("ホットキー (Ctrl+Shift+F2) が登録されました。")

    def load_settings(self):
        try:
            with open(self.settings_file, "r", encoding="utf-8") as f:
                self.settings = json.load(f)
        except (FileNotFoundError, json.JSONDecodeError):
            self.settings = {}

    def save_settings(self):
        self.settings = {
            "audio_device": self.selected_device.get(),
            "window": self.selected_window_title.get(),
            "use_image": self.use_image.get(),
            "is_private": self.is_private.get(),
            "show_response_in_new_window": self.show_response_in_new_window.get(),
            "response_display_duration": self.response_display_duration.get(),
            "tts_engine": self.tts_engine.get(),
            "twitch_bot_username": self.twitch_bot_username.get(),
            "twitch_client_id": self.twitch_client_id.get(),
            "twitch_client_secret": self.twitch_client_secret.get(),
            "twitch_bot_id": self.twitch_bot_id.get(),
        }
        with open(self.settings_file, "w", encoding="utf-8") as f:
            json.dump(self.settings, f, ensure_ascii=False, indent=4)

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
            command=lambda: (self.save_settings(), self.update_record_buttons_state())
        )
        self.use_image_check.pack(fill=X, pady=5)

        self.is_private_check = ttk.Checkbutton(
            config_frame, text="プライベート", variable=self.is_private, style="success-square-toggle", command=self.save_settings
        )
        self.is_private_check.pack(fill=X, pady=5)

        self.show_response_in_new_window_check = ttk.Checkbutton(
            config_frame, text="レスポンスを別ウィンドウに表示", variable=self.show_response_in_new_window,
            style="success-square-toggle", command=self.save_settings
        )
        self.show_response_in_new_window_check.pack(fill=X, pady=5)
        
        duration_frame = ttk.Frame(config_frame)
        duration_frame.pack(fill=X, pady=5)
        ttk.Label(duration_frame, text="表示時間(ms):").pack(side=LEFT)
        self.response_duration_entry = ttk.Entry(duration_frame, textvariable=self.response_display_duration, width=8)
        self.response_duration_entry.pack(side=LEFT)
        self.response_duration_entry.bind("<FocusOut>", lambda e: self.save_settings())

        tts_frame = ttk.Frame(config_frame)
        tts_frame.pack(fill=X, pady=5)
        ttk.Label(tts_frame, text="TTSエンジン:").pack(side=LEFT)
        voicevox_radio = ttk.Radiobutton(tts_frame, text="VOICEVOX", variable=self.tts_engine, value="voicevox", command=self.save_settings)
        voicevox_radio.pack(side=LEFT, padx=5)
        gemini_radio = ttk.Radiobutton(tts_frame, text="Gemini", variable=self.tts_engine, value="gemini", command=self.save_settings)
        gemini_radio.pack(side=LEFT, padx=5)

        twitch_frame = ttk.Frame(left_frame)
        twitch_frame.pack(fill=X, pady=(0, 15))
        ttk.Label(twitch_frame, text="Twitch Bot", style="inverse-primary").pack(fill=X, pady=(0, 8))

        bot_username_frame = ttk.Frame(twitch_frame)
        bot_username_frame.pack(fill=X, pady=2)
        ttk.Label(bot_username_frame, text="Bot Username:", width=12).pack(side=LEFT)
        bot_username_entry = ttk.Entry(bot_username_frame, textvariable=self.twitch_bot_username)
        bot_username_entry.pack(side=LEFT, fill=X, expand=True)
        bot_username_entry.bind("<FocusOut>", lambda e: self.save_settings())

        client_id_frame = ttk.Frame(twitch_frame)
        client_id_frame.pack(fill=X, pady=2)
        ttk.Label(client_id_frame, text="Client ID:", width=12).pack(side=LEFT)
        client_id_entry = ttk.Entry(client_id_frame, textvariable=self.twitch_client_id)
        client_id_entry.pack(side=LEFT, fill=X, expand=True)
        client_id_entry.bind("<FocusOut>", lambda e: self.save_settings())

        client_secret_frame = ttk.Frame(twitch_frame)
        client_secret_frame.pack(fill=X, pady=2)
        ttk.Label(client_secret_frame, text="Client Secret:", width=12).pack(side=LEFT)
        client_secret_entry = ttk.Entry(client_secret_frame, textvariable=self.twitch_client_secret, show="*")
        client_secret_entry.pack(side=LEFT, fill=X, expand=True)
        client_secret_entry.bind("<FocusOut>", lambda e: self.save_settings())

        # --- 新しい認証UI ---
        auth_code_frame = ttk.Frame(twitch_frame)
        auth_code_frame.pack(fill=X, pady=5)
        ttk.Label(auth_code_frame, text="認証コード:", width=12).pack(side=LEFT)
        auth_code_entry = ttk.Entry(auth_code_frame, textvariable=self.twitch_auth_code)
        auth_code_entry.pack(side=LEFT, fill=X, expand=True)
        
        is_bot_auth_check = ttk.Checkbutton(
            twitch_frame, text="ボット自身の認証として登録する", variable=self.twitch_is_bot_auth, style="success-square-toggle"
        )
        is_bot_auth_check.pack(fill=X, pady=5)

        auth_button_frame = ttk.Frame(twitch_frame)
        auth_button_frame.pack(fill=X, pady=5)
        self.register_token_button = ttk.Button(auth_button_frame, text="トークン登録", command=self.register_auth_code, style="success.TButton")
        self.register_token_button.pack(side=LEFT, fill=X, expand=True, padx=(0, 5))
        self.copy_auth_url_button = ttk.Button(auth_button_frame, text="承認URLコピー", command=self.copy_auth_url, style="info.TButton")
        self.copy_auth_url_button.pack(side=LEFT, fill=X, expand=True)
        # --- ここまで ---
        
        self.twitch_connect_button = ttk.Button(twitch_frame, text="接続", command=self.toggle_twitch_connection, style="primary.TButton")
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

        self.record_button = ttk.Button(self.record_container, text="録音開始", style="success.TButton", command=self.toggle_recording)
        self.record_button.pack(side=LEFT, padx=5)

        self.record_wait_button = ttk.Button(self.record_container, text="録音待機", style="success.TButton", command=self.toggle_record_waiting)
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
        self.save_settings()

    def update_window(self, event=None):
        selected_window_title = self.selected_window_title.get()
        self.selected_window = capture.get_window_by_title(selected_window_title)
        if self.selected_window:
            print(f"選択されたウィンドウ: {self.selected_window.title}")
            self.selected_window_label.config(text=f"選択されたウィンドウ: {self.selected_window.title}")
        else:
            print("ウィンドウが見つかりませんでした")
            self.selected_window_label.config(text="選択されたウィンドウ: (見つかりません)")
        self.save_settings()
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

    def toggle_recording(self, event=None):
        if self.device_index is None:
            print("デバイスが選択されていません")
            return
        if not self.recording:
            self.start_recording()
        else:
            self.stop_recording()

    def toggle_record_waiting(self, event=None):
        if self.device_index is None:
            print("デバイスが選択されていません")
            return
        if not self.record_waiting:
            self.start_record_waiting()
        else:
            self.stop_record_waiting()

    def start_recording(self):
        self.recording = True
        self.recording_complete = False
        self.record_button.config(text="録音停止", style="danger.TButton")
        self.recording_thread = threading.Thread(target=self.record_audio_thread)
        self.recording_thread.start()

    def stop_recording(self):
        self.recording = False
        self.record_button.config(text="処理中...", style="success.TButton", state="disabled")
        self.record_wait_button.config(state="disabled")
        if self.selected_window:
            self.capture_window()
        else:
            print("ウィンドウが選択されていません")
            return
        self.play_random_nod_thread = threading.Thread(target=voice.play_random_nod)
        self.play_random_nod_thread.start()
        if self.recording_complete:
            thread = threading.Thread(target=self.process_audio_and_generate_response)
            thread.start()
        else:
            print("録音が停止されていません")
        
    def start_record_waiting(self):
        self.record_waiting = True
        self.recording_complete = False
        self.record_wait_button.config(text="録音待機中", style="danger.TButton")
        self.stop_event.clear()
        self.record_waiting_thread = threading.Thread(target=self.wait_for_keyword_thread)
        self.record_waiting_thread.start()

    def stop_record_temporary(self):
        self.record_wait_button.config(text="処理中...", style="danger.TButton", state="disabled")
        self.record_button.config(state="disabled")
        if self.selected_window:
            self.capture_window()
        else:
            print("ウィンドウが選択されていません")
            return
        self.play_random_nod_thread = threading.Thread(target=voice.play_random_nod)
        self.play_random_nod_thread.start()
        if self.recording_complete:
            thread = threading.Thread(target=self.process_audio_and_generate_response, args=(True,))
            thread.start()
        else:
            print("録音が停止されていません")

    def stop_record_waiting(self):
        self.record_waiting = False
        self.record_wait_button.config(text="録音待機", style="success.TButton")
        self.stop_event.set()

    def update_record_buttons_state(self, event=None):
        if self.use_image.get() and self.selected_window is None:
            self.record_button.config(state="disabled")
            self.record_wait_button.config(state="disabled")
            print("画像利用がオンですが、ウィンドウが選択されていないため録音ボタンを無効化しました。")
        else:
            self.record_button.config(state="normal")
            self.record_wait_button.config(state="normal")
        
    def process_audio_and_generate_response(self, from_temporary_stop=False):
        prompt = self.transcribe_audio()
        if not prompt:
            print("プロンプトが空です。")
            def enable_buttons():
                self.record_button.config(text="録音開始", style="success.TButton", state="normal")
                self.record_wait_button.config(text="録音待機", style="success.TButton", state="normal")
                if self.record_waiting:
                    self.record_wait_button.config(text="録音待機中", style="danger.TButton")
                    self.record_waiting_thread = threading.Thread(target=self.wait_for_keyword_thread)
                    self.record_waiting_thread.start()
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
        response = self.ask_gemini()
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
            if self.record_waiting:
                self.record_wait_button.config(text="録音待機中", style="danger.TButton")
                self.record_waiting_thread = threading.Thread(target=self.wait_for_keyword_thread)
                self.record_waiting_thread.start()
            self.record_button.config(text="録音開始", style="success.TButton", state="normal")
            self.record_wait_button.config(state="normal")
            if not self.record_waiting:
                self.record_wait_button.config(text="録音待機", style="success.TButton")

        self.root.after(0, update_gui_and_speak)

    async def run_ai_search(self, query: str):
        return await ai_search(query)
    
    def show_gemini_response(self, response_text):
        if self.show_response_in_new_window.get():
            GeminiResponseWindow(self.root, response_text, self.response_display_duration.get())
        else:
            self.response_label.config(text=response_text)
            self.root.after(self.response_display_duration.get(), lambda: self.response_label.config(text=""))

    def record_audio_thread(self):
        if self.device_index is None:
            print("マイクが選択されていません。")
            return
        record.record_audio(
            device_index=self.device_index,
            update_callback=self.update_level_meter,
            audio_file_path=self.audio_file_path,
            stop_event=self.stop_event
        )
        print("録音完了")
        self.recording_complete = True
        if self.recording:
            self.root.after(0, self.stop_recording)
    
    def wait_for_keyword_thread(self):
        if self.device_index is None:
            print("マイクが選択されていません。")
            return
        result = record.wait_for_keyword(
            device_index=self.device_index,
            update_callback=self.update_level_meter,
            audio_file_path=self.audio_file_path,
            stop_event=self.stop_event
        )
        if result:
            print("録音完了")
            self.recording_complete = True
            if not self.stop_event.is_set():
                self.root.after(0, self.stop_record_temporary)

    def update_level_meter(self, volume):
        level = int(volume / 100)
        self.root.after(0, self.set_level_meter_value, level)

    def set_level_meter_value(self, level):
        self.level_meter['value'] = level

    def capture_window(self):
        print("ウィンドウをキャプチャします…")
        try:
            capture.capture_screen(self.selected_window, self.screenshot_file_path)
            self.load_and_display_image(self.screenshot_file_path)
        except Exception as e:
            print(f"キャプチャできませんでした： {e}")

    def load_and_display_image(self, image_path):
        threading.Thread(target=self.process_image, args=(image_path,)).start()

    def process_image(self, image_path):
        try:
            image = Image.open(image_path)
            max_size = (400, 300)
            image.thumbnail(max_size)
            self.image = ImageTk.PhotoImage(image)
            self.root.after(0, self.update_image_label)
        except Exception as e:
            print(f"画像処理エラー: {e}")

    def update_image_label(self):
        if self.image:
            self.image_label.config(image=self.image)

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

    def ask_gemini(self):
        image_path = self.screenshot_file_path if self.use_image.get() and os.path.exists(self.screenshot_file_path) else None
        if self.prompt:
            response = self.session.generate_content(self.prompt, image_path, self.is_private.get())
            return response
        return "プロンプトがありません。"

    def open_memory_window(self):
        """メモリー管理ウィンドウを開く"""
        MemoryWindow(self.root, self.memory_manager)

    def copy_auth_url(self):
        """Twitch認証URLを生成してクリップボードにコピーする"""
        client_id = self.twitch_client_id.get()
        if not client_id:
            print("エラー: Client IDが設定されていません。")
            return
        
        auth_url = twitch_auth.generate_auth_url(client_id)
        
        try:
            import pyperclip
            pyperclip.copy(auth_url)
            print("成功: 認証URLをクリップボードにコピーしました。")
        except ImportError:
            print("エラー: pyperclipモジュールが見つかりません。`pip install pyperclip`でインストールしてください。")
            print(f"認証URL: {auth_url}")
        except Exception as e:
            print(f"クリップボードへのコピーに失敗しました: {e}")
            print(f"認証URL: {auth_url}")

    def register_auth_code(self):
        """認証コードを使ってトークンを登録する"""
        threading.Thread(target=self.run_register_auth_code, daemon=True).start()

    def run_register_auth_code(self):
        loop = asyncio.new_event_loop()
        asyncio.set_event_loop(loop)
        loop.run_until_complete(self.async_register_auth_code())

    async def async_register_auth_code(self):
        """認証コードの登録処理"""
        code = self.twitch_auth_code.get()
        if not code:
            print("エラー: 認証コードが入力されていません。")
            return

        client_id = self.twitch_client_id.get()
        client_secret = self.twitch_client_secret.get()

        if not all([client_id, client_secret]):
            print("エラー: Client IDまたはClient Secretが設定されていません。")
            return
        
        print(f"認証コード '{code[:10]}...' を使ってトークンを交換しています...")
        try:
            is_bot = self.twitch_is_bot_auth.get()
            result = await twitch_auth.exchange_code_for_token(client_id, client_secret, code, is_bot_auth=is_bot)
            if result and result.get("user_id"):
                user_id = result["user_id"]
                print(f"成功: ユーザーID {user_id} のトークンを登録しました。")
                
                # ボットとして登録した場合、bot_idを更新
                if is_bot:
                    self.twitch_bot_id.set(user_id)
                    self.save_settings()
                    print(f"Bot IDを {user_id} に設定し、保存しました。")

                self.twitch_auth_code.set("") # 入力欄をクリア
                self.twitch_is_bot_auth.set(False) # チェックボックスをリセット
            else:
                print("エラー: トークンの登録に失敗しました。")
        except Exception as e:
            print(f"トークン登録中にエラーが発生しました: {e}")

    def toggle_twitch_connection(self):
        if self.twitch_bot and self.twitch_thread and self.twitch_thread.is_alive():
            self.disconnect_twitch_bot()
        else:
            self.connect_twitch_bot()

    def connect_twitch_bot(self):
        threading.Thread(target=self.run_connect_twitch_bot, daemon=True).start()

    def run_connect_twitch_bot(self):
        loop = asyncio.new_event_loop()
        asyncio.set_event_loop(loop)
        loop.run_until_complete(self.async_connect_twitch_bot())

    async def async_connect_twitch_bot(self):
        client_id = self.twitch_client_id.get()
        client_secret = self.twitch_client_secret.get()
        
        # DBからbot_idを取得し、なければ設定ファイルの値を使用
        bot_id = await twitch_auth.get_bot_id_from_db()
        if not bot_id:
            bot_id = self.twitch_bot_id.get()
            if bot_id:
                print(f"DBにbot_idがなかったため、設定ファイルのID: {bot_id} を使用します。")
            else:
                print("エラー: ボットのIDがDBにも設定ファイルにも見つかりません。認証コードでボットのトークンを登録してください。")
                return
        else:
            print(f"DBからボットID: {bot_id} を取得しました。")
            self.twitch_bot_id.set(bot_id) # UIにも反映

        if not await twitch_auth.ensure_bot_token_valid(client_id, client_secret, bot_id):
            return

        print("TwitchボTットに接続しています...")
        try:
            # token_collectionはTwitchBotの初期化に渡すだけで良い
            token_collection = chromadb.PersistentClient(path="./chroma_tokens_data").get_or_create_collection(name="user_tokens")
            
            self.twitch_bot = TwitchBot(
                client_id=client_id,
                client_secret=client_secret,
                bot_id=bot_id,
                owner_id=bot_id,
                nick=self.twitch_bot_username.get(),
                token_collection=token_collection,
                mention_callback=self.handle_twitch_mention,
            )

            self.twitch_bot_loop = asyncio.new_event_loop()
            # run_bot_in_threadにtokensを渡す必要はなくなる
            self.twitch_thread = threading.Thread(target=self.run_bot_in_thread, args=(self.twitch_bot_loop,), daemon=True)
            self.twitch_thread.start()
            self.twitch_connect_button.config(text="切断", style="danger.TButton")
        except Exception as e:
            print(f"Twitchへの接続に失敗しました: {e}")
            self.twitch_bot = None

    def disconnect_twitch_bot(self):
        print("Twitchボットの切断を試みます（アプリケーション終了時にスレッドは閉じられます）...")
        self.twitch_thread = None
        self.twitch_connect_button.config(text="接続", style="primary.TButton")
        print("Twitchボットを切断しました。")

    def run_bot_in_thread(self, loop):
        asyncio.set_event_loop(loop)
        if self.twitch_bot:
            # トークンのロードはTwitchBotのsetup_hookに任せる
            loop.run_until_complete(self.twitch_bot.start()) # type: ignore

    async def handle_twitch_mention(self, author, prompt, channel):
        print(f"Twitchのメンションを処理中: {author} in {channel.name} - {prompt}")

        # クールダウン処理
        current_time = time.time()
        last_mention_time = self.twitch_last_mention_time.get(author, 0)
        if current_time - last_mention_time < self.twitch_mention_cooldown:
            cooldown_remaining = round(self.twitch_mention_cooldown - (current_time - last_mention_time))
            reply_message = f"@{author} ちょっと待ってだわん！ あと{cooldown_remaining}秒待ってから話しかけてほしいわん。"
            if self.twitch_bot and self.twitch_bot_loop:
                coro = self.twitch_bot.send_chat_message(channel, reply_message)
                asyncio.run_coroutine_threadsafe(coro, self.twitch_bot_loop)
            return

        self.twitch_last_mention_time[author] = current_time

        # Geminiに応答を生成させる
        response = self.session.generate_content(prompt, image_path=None, is_private=False)
        
        if response:
            # # TTSで読み上げ
            # voice.text_to_speech(response)
            
            # Twitchに応答を送信
            if self.twitch_bot and self.twitch_bot_loop:
                reply_message = f"@{author} {response}"
                coro = self.twitch_bot.send_chat_message(channel, reply_message)
                asyncio.run_coroutine_threadsafe(coro, self.twitch_bot_loop)

def on_closing(app_instance):
    print("アプリケーションを終了します...")
    if app_instance.twitch_bot:
        app_instance.disconnect_twitch_bot()
    if record.p:
        record.p.terminate()
    app_instance.root.destroy()

if __name__ == "__main__":
    setup_logging(level=logging.DEBUG)
    root = ttk.Window(themename="superhero")
    root.geometry("1280x960")
    app = GameAssistantApp(root)
    root.protocol("WM_DELETE_WINDOW", lambda: on_closing(app))
    root.mainloop()