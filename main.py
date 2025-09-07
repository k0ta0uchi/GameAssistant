from tkinter import font
import ttkbootstrap as ttk
from ttkbootstrap.constants import *
import scripts.record as record
import scripts.capture as capture
import scripts.whisper as whisper
import scripts.gemini as gemini
import scripts.voice as voice
from scripts.search import ai_search  # ai_searchをインポート
import threading
import sys
import os
from PIL import Image, ImageTk
import keyboard  # keyboardライブラリをインポート
import json  # JSONライブラリをインポート
import asyncio  # asyncioを追加
from scripts.memory import MemoryManager # MemoryManagerをインポート


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
        self.widget.see(END)  # スクロールして常に一番下を表示

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
            text="",  # 初期テキストは空
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
        """レスポンステキストをラベルに設定"""
        if self.label:
            self.label.configure(text=response_text)

    def close_window(self):
        """ウィンドウを閉じる"""
        self.destroy()
        # ウィンドウが閉じられたことをメインアプリに通知する場合、
        # 例えば、メインアプリのウィンドウインスタンス変数をNoneにするなどの処理を追加できます。
        # 例: if hasattr(self.master, 'response_window'): self.master.response_window = None

    def dim_text(self):
        """ラベルのテキストを消去"""
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
        # フレーム
        main_frame = ttk.Frame(self, padding=10)
        main_frame.pack(fill=BOTH, expand=True)

        # 左側（リストボックス）
        left_frame = ttk.Frame(main_frame)
        left_frame.pack(side=LEFT, fill=BOTH, expand=True, padx=(0, 10))

        self.memory_listbox = ttk.Treeview(left_frame, columns=("key", "value"), show="headings")
        self.memory_listbox.heading("key", text="キー")
        self.memory_listbox.heading("value", text="値")
        self.memory_listbox.pack(fill=BOTH, expand=True)
        self.memory_listbox.bind("<<TreeviewSelect>>", self.on_memory_select)

        # 右側（編集エリア）
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

        # ボタン
        button_frame = ttk.Frame(right_frame)
        button_frame.pack(fill=X)

        save_button = ttk.Button(button_frame, text="保存", command=self.save_memory, style="success.TButton")
        save_button.pack(side=LEFT, expand=True, fill=X, padx=(0, 5))

        delete_button = ttk.Button(button_frame, text="削除", command=self.delete_memory, style="danger.TButton")
        delete_button.pack(side=LEFT, expand=True, fill=X)

    def load_memories_to_listbox(self):
        """メモリーをリストボックスに読み込む"""
        # 既存のアイテムをクリア
        for item in self.memory_listbox.get_children():
            self.memory_listbox.delete(item)
        # メモリーを読み込んで追加
        memories = self.memory_manager.get_all_memories()
        for key, value in memories.items():
            self.memory_listbox.insert("", "end", values=(key, value))

    def on_memory_select(self, event):
        """リストボックスでメモリーが選択されたときの処理"""
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
        """メモリーを保存する"""
        key = self.key_entry.get()
        value = self.value_text.get("1.0", END).strip()
        if not key:
            # ttkbootstrap.dialogs.Messagebox.show_error("キーは必須です。", title="エラー")
            print("キーは必須です。")
            return

        self.memory_manager.add_or_update_memory(key, value)
        self.load_memories_to_listbox()
        self.clear_entries()

    def delete_memory(self):
        """メモリーを削除する"""
        key = self.key_entry.get()
        if not key:
            # ttkbootstrap.dialogs.Messagebox.show_error("削除するキーを指定してください。", title="エラー")
            print("削除するキーを指定してください。")
            return

        if self.memory_manager.delete_memory(key):
            self.load_memories_to_listbox()
            self.clear_entries()
        else:
            # ttkbootstrap.dialogs.Messagebox.show_error("指定されたキーのメモリーが見つかりません。", title="エラー")
            print("指定されたキーのメモリーが見つかりません。")

    def clear_entries(self):
        """入力フィールドをクリアする"""
        self.key_entry.delete(0, END)
        self.value_text.delete("1.0", END)
        self.memory_listbox.selection_remove(self.memory_listbox.selection())

class GameAssistantApp:
    def __init__(self, root):
        self.root = root
        self.root.title("ゲームアシスタント")

        # 設定ファイルのパス
        self.settings_file = "settings.json"

        # 設定のロード
        self.load_settings()

        self.audio_devices = record.get_audio_device_names()
        # デフォルト値を設定ファイルから読み込むか、利用可能なデバイスの最初のものを設定
        default_audio_device = self.settings.get("audio_device", self.audio_devices[0] if self.audio_devices else "")
        self.selected_device = ttk.StringVar(value=default_audio_device)
        self.device_index = None  # デバイスインデックスを保存する変数
        
        # self.loopback_devices = self.audio_devices # 同じリストを共有
        # default_loopback_device = self.settings.get("loopback_device", self.loopback_devices[0] if self.loopback_devices else "")
        # self.selected_loopback_device = ttk.StringVar(value=default_loopback_device)
        self.loopback_device_index = None # 無効化のためNoneに設定
        self.recording = False
        self.recording_complete = False  # 録音完了フラグ
        self.record_waiting = False
        self.stop_event = threading.Event()  # スレッド停止用イベント

        self.windows = capture.list_available_windows()
        # デフォルト値を設定ファイルから読み込むか、利用可能なウィンドウの最初のものを設定
        default_window = self.settings.get("window", self.windows[0] if self.windows else "")
        self.selected_window_title = ttk.StringVar(value=default_window)
        self.selected_window = None  # 選択されたウィンドウオブジェクト

        self.custom_instruction = """
あなたは、ユーザーの質問に答える優秀なAIアシスタントです。あなたは優しい女の子の犬のキャラクターとして振る舞います。以下の指示に従って応答してください。

応答を生成する前に、以下の手順に従ってください:

1. 画像が提供されている場合は、画像の内容を分析し、ユーザーの質問と組み合わせて状況を理解してください。
2. 過去の会話が含まれている場合は、それを考慮に入れてください。ただし、明示的に「覚えています」などとは言わず、自然に対応してください。
3. 応答を生成する際は、以下のルールを厳守してください:
   - フレンドリーで親しみやすい口調を使用してください。
   - 文末には「だわん」を使用してください。
   - すべての英単語をカタカナに変換してください。
   - 通常は2文程度の短い応答を心がけてください。ただし、詳細な説明を求められた場合は、より長い応答も可能です。
   - 検索結果のまとめが付与されている場合は、最初に提示されたプロンプトを元にまとめて回答してください。
   
4. 応答が適切な長さと内容になっているか確認し、必要に応じて調整してください。

以下は応答の例です：

例：「はいだわん！その質問面白いだわん！カメラのシャッターはチーズの速さで閉じるんだわん。もっと詳しく知りたいかしら？」

それでは、ユーザーの入力に基づいて応答を生成してください。
        """
        self.prompt = None
        self.response = None

        # デフォルト値を設定ファイルから読み込む
        self.use_image = ttk.BooleanVar(value=self.settings.get("use_image", True)) # 画像を使用するかどうかの変数を追加
        self.is_private = ttk.BooleanVar(value=self.settings.get("is_private", True))
        self.use_streaming = ttk.BooleanVar(value=self.settings.get("use_streaming", False)) # ストリーミングモードを使用するかどうかの変数
        self.show_response_in_new_window = ttk.BooleanVar(value=self.settings.get("show_response_in_new_window", True)) # デフォルト値を設定ファイルから読み込む
        self.response_display_duration = ttk.IntVar(value=self.settings.get("response_display_duration", 10000))  # デフォルト値を設定ファイルから読み込む
        self.tts_engine = ttk.StringVar(value=self.settings.get("tts_engine", "voicevox"))  # TTSエンジン設定
        self.session = gemini.GeminiSession(self.custom_instruction)
        self.memory_manager = MemoryManager()

        self.create_widgets()

        # stdoutのリダイレクト
        self.redirector = OutputRedirector(self.output_textbox)
        sys.stdout = self.redirector

        self.audio_file_path = "temp_recording.wav"
        self.screenshot_file_path = "temp_screenshot.png"
        self.image = None

        # ホットキー登録
        keyboard.add_hotkey("ctrl+shift+f2", self.toggle_recording)
        print("ホットキー (Ctrl+Shift+F2) が登録されました。")

    def load_settings(self):
        """設定ファイルを読み込む"""
        try:
            with open(self.settings_file, "r", encoding="utf-8") as f:
                self.settings = json.load(f)
        except FileNotFoundError:
            # ファイルが存在しない場合は空の辞書で初期化
            self.settings = {}
        except json.JSONDecodeError:
            print("設定ファイルの読み込みに失敗しました。")
            self.settings = {}

    def save_settings(self):
        """設定を保存する"""
        self.settings["audio_device"] = self.selected_device.get()
        # self.settings["loopback_device"] = self.selected_loopback_device.get()
        self.settings["window"] = self.selected_window_title.get()
        self.settings["use_image"] = self.use_image.get()
        self.settings["is_private"] = self.is_private.get()
        self.settings["use_streaming"] = self.use_streaming.get() # ストリーミング設定を保存
        self.settings["show_response_in_new_window"] = self.show_response_in_new_window.get() # 設定を保存
        self.settings["response_display_duration"] = self.response_display_duration.get()  # 設定を保存
        self.settings["tts_engine"] = self.tts_engine.get() # TTSエンジン設定を保存

        with open(self.settings_file, "w", encoding="utf-8") as f:
            json.dump(self.settings, f, ensure_ascii=False, indent=4)

    def get_device_index_from_name(self, device_name):
        """デバイス名からデバイスインデックスを取得する"""
        return record.get_device_index_from_name(device_name)

    def create_widgets(self):
        # メインフレームの作成
        main_frame = ttk.Frame(self.root, padding=20)
        main_frame.pack(fill=BOTH, expand=True)
        main_frame.pack_propagate(False) # ウィジェットのサイズに合わせてフレームがリサイズされないようにする

        # 左カラムの作成
        left_frame = ttk.Frame(main_frame, width=250) # 固定幅を設定
        left_frame.pack(side=LEFT, fill=Y, padx=(0, 20))
        left_frame.pack_propagate(False) # ウィジェットのサイズに合わせてフレームがリサイズされないようにする

        # 右カラムの作成
        right_frame = ttk.Frame(main_frame)
        right_frame.pack(side=RIGHT, fill=BOTH, expand=True)

        # --- 左カラム ---
        # デバイス設定
        device_frame = ttk.Frame(left_frame)
        device_frame.pack(fill=X, pady=(0, 15))
        ttk.Label(device_frame, text="インプットデバイス", style="inverse-primary").pack(fill=X, pady=(0, 8))
        self.audio_dropdown = ttk.Combobox(
            master=device_frame,
            textvariable=self.selected_device,
            values=self.audio_devices,
            state=READONLY,
            width=30, # 固定幅を設定
        )
        self.audio_dropdown.pack(fill=X, pady=(0, 5))
        self.audio_dropdown.bind("<<ComboboxSelected>>", self.update_device_index)
        self.device_index_label = ttk.Label(master=device_frame, text="Device index: ", wraplength=230) # 折り返しを設定
        self.device_index_label.pack(fill=X)

        # ウィンドウ設定
        window_frame = ttk.Frame(left_frame)
        window_frame.pack(fill=X, pady=(0, 15))
        ttk.Label(window_frame, text="ウィンドウ", style="inverse-primary").pack(fill=X, pady=(0, 8))
        # ドロップダウンとボタンを横並びにするためのフレーム
        combo_button_frame = ttk.Frame(window_frame)
        combo_button_frame.pack(fill=X)

        self.window_dropdown = ttk.Combobox(
            master=combo_button_frame,
            textvariable=self.selected_window_title,
            values=self.windows,
            state=READONLY,
        )
        self.window_dropdown.pack(side=LEFT, fill=X, expand=True)
        self.window_dropdown.bind("<<ComboboxSelected>>", self.update_window)

        # ウィンドウリスト更新ボタン (アイコン風)
        refresh_button = ttk.Button(combo_button_frame, text="🔄", command=self.refresh_window_list, style="info.TButton", width=2)
        refresh_button.pack(side=LEFT, padx=(5, 0))
 
        self.selected_window_label = ttk.Label(master=window_frame, text="Selected window: ", wraplength=230) # 折り返しを設定
        self.selected_window_label.pack(fill=X)

        # メモリー管理ボタン
        memory_button = ttk.Button(left_frame, text="メモリー管理", command=self.open_memory_window, style="info.TButton")
        memory_button.pack(fill=X, pady=(15, 0))

        # --- 右カラム ---
        # Geminiレスポンス表示エリア
        self.response_frame = ttk.Frame(right_frame, padding=(0, 0, 0, 10))
        self.response_frame.pack(fill=X)
        self.response_label = ttk.Label(self.response_frame, text="", wraplength=400, justify=LEFT, font=("Arial", 14), style="inverse-info")
        self.response_label.pack(fill=X, ipady=10)

        self.meter_container = ttk.Frame(right_frame)
        self.meter_container.pack(fill=X, pady=(0, 10))

        # レベルメーター
        self.level_meter = ttk.Progressbar(
            self.meter_container,
            length=300,
            maximum=100,  # 音量レベルの最大値
            value=0,  # 初期値
            style="danger.Horizontal.TProgressbar",
        )
        self.level_meter.pack(pady=10)

        # 設定
        config_frame = ttk.Frame(left_frame)
        config_frame.pack(fill=X, pady=(0, 15))
        ttk.Label(config_frame, text="設定", style="inverse-primary").pack(fill=X, pady=(0, 8))

        self.use_image_check = ttk.Checkbutton(
            config_frame,
            text="画像を使用する",
            variable=self.use_image,
            style="success-square-toggle",
            command=lambda: (self.save_settings(), self.update_record_buttons_state())
        )
        self.use_image_check.pack(fill=X, pady=5)

        self.is_private_check = ttk.Checkbutton(
            config_frame,
            text="プライベート",
            variable=self.is_private,
            style="success-square-toggle",
            command=self.save_settings
        )
        self.is_private_check.pack(fill=X, pady=5)

        self.use_streaming_check = ttk.Checkbutton(
            config_frame,
            text="ストリーミングモードを使用する",
            variable=self.use_streaming,
            style="success-square-toggle",
            command=self.save_settings
        )
        self.use_streaming_check.pack(fill=X, pady=5)

        self.show_response_in_new_window_check = ttk.Checkbutton(
            config_frame,
            text="レスポンスを別ウィンドウに表示",
            variable=self.show_response_in_new_window,
            style="success-square-toggle",
            command=self.save_settings
        )
        self.show_response_in_new_window_check.pack(fill=X, pady=5)
        
        duration_frame = ttk.Frame(config_frame)
        duration_frame.pack(fill=X, pady=5)
        ttk.Label(duration_frame, text="表示時間(ms):").pack(side=LEFT)
        self.response_duration_entry = ttk.Entry(duration_frame, textvariable=self.response_display_duration, width=8)
        self.response_duration_entry.pack(side=LEFT)
        self.response_duration_entry.bind("<FocusOut>", lambda e: self.save_settings())

        # TTSエンジン設定
        tts_frame = ttk.Frame(config_frame)
        tts_frame.pack(fill=X, pady=5)
        ttk.Label(tts_frame, text="TTSエンジン:").pack(side=LEFT)
        voicevox_radio = ttk.Radiobutton(tts_frame, text="VOICEVOX", variable=self.tts_engine, value="voicevox", command=self.save_settings)
        voicevox_radio.pack(side=LEFT, padx=5)
        gemini_radio = ttk.Radiobutton(tts_frame, text="Gemini", variable=self.tts_engine, value="gemini", command=self.save_settings)
        gemini_radio.pack(side=LEFT, padx=5)

        # 画像表示エリア
        self.image_frame = ttk.Frame(right_frame, height=300)
        self.image_frame.pack(fill=X, pady=10)
        self.image_frame.pack_propagate(False)

        # 画像を表示するラベル
        self.image_label = ttk.Label(self.image_frame)
        self.image_label.pack(pady=10)

        # ログ表示コンテナ
        self.text_container = ttk.Frame(right_frame)
        self.text_container.pack(fill=BOTH, expand=True)

        # テキストボックスとスクロールバーの追加
        self.output_textbox = ttk.Text(master=self.text_container, height=5, width=50, wrap=WORD)
        self.output_textbox.pack(side=LEFT, fill=BOTH, expand=True, padx=(0, 5), pady=(0, 10))

        self.scrollbar = ttk.Scrollbar(self.text_container, orient=VERTICAL, command=self.output_textbox.yview)
        self.scrollbar.pack(side=RIGHT, fill=Y, pady=(0, 10))

        self.output_textbox['yscrollcommand'] = self.scrollbar.set

        self.record_container = ttk.Frame(right_frame)
        self.record_container.pack(fill=X, padx=10, pady=10)

        # 録音ボタン
        self.record_button = ttk.Button(self.record_container, text="録音開始", style="success.TButton", command=self.toggle_recording)
        self.record_button.pack(side=LEFT, padx=5)

        # 録音ボタン
        self.record_wait_button = ttk.Button(self.record_container, text="録音待機", style="success.TButton", command=self.toggle_record_waiting)
        self.record_wait_button.pack(side=LEFT, padx=5)

        # デフォルトでデバイスインデックスを取得
        if self.audio_devices:
            self.update_device_index()

        # デフォルトで最初のウィンドウタイトルを取得
        if self.windows:
            self.update_window()
        
        # ボタンの初期状態を更新
        self.update_record_buttons_state()

    def update_device_index(self, event=None):
        """選択されたデバイスのインデックスを更新"""
        selected_device_name = self.selected_device.get()
        self.device_index = self.get_device_index_from_name(selected_device_name)
        self.device_index_label.config(text=f"選択されたデバイス: {self.device_index}-{selected_device_name}")
        self.save_settings()  # 設定を保存

    def update_window(self, event=None):
        """選択されたウィンドウを更新"""
        selected_window_title = self.selected_window_title.get()
        self.selected_window = capture.get_window_by_title(selected_window_title)
        if self.selected_window:
            print(f"選択されたウィンドウ: {self.selected_window.title}")
            self.selected_window_label.config(text=f"選択されたウィンドウ: {self.selected_window.title}")  # タイトルを表示
        else:
            print("ウィンドウが見つかりませんでした")
            self.selected_window_label.config(text="選択されたウィンドウ: (見つかりません)")
        self.save_settings()  # 設定を保存
        self.update_record_buttons_state()

    def refresh_window_list(self):
        """ウィンドウリストを更新する"""
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
        """録音の開始/停止を切り替える"""
        if self.device_index is None:
            print("デバイスが選択されていません")
            return

        if not self.recording:
            self.start_recording()
        else:
            self.stop_recording()

    def toggle_record_waiting(self, event=None):
        """録音の開始/停止を切り替える"""
        if self.device_index is None:
            print("デバイスが選択されていません")
            return
        
        if not self.record_waiting:
            self.start_record_waiting()
        else:
            self.stop_record_waiting()

    def start_recording(self):
        """録音を開始する"""
        self.recording = True
        self.recording_complete = False
        self.record_button.config(text="録音停止", style="danger.TButton")

        # 録音をバックグラウンドスレッドで実行
        self.recording_thread = threading.Thread(target=self.record_audio_thread)
        self.recording_thread.start()

    def stop_recording(self):
        """録音を停止する"""
        if not self.recording:
            return

        self.recording = False
        self.record_button.config(text="処理中...", style="success.TButton", state="disabled")
        self.record_wait_button.config(state="disabled")

        if not self.use_streaming.get():
            # 録音スレッドの終了を待つ
            if hasattr(self, 'recording_thread') and self.recording_thread.is_alive():
                self.stop_event.set()
                self.recording_thread.join()
        else:
            # ストリーミングモードの場合はすぐに完了とみなす
            self.recording_complete = True

        # ウィンドウをキャプチャする
        if self.use_image.get():
            if self.selected_window:
                self.capture_window()
            else:
                print("ウィンドウが選択されていません")
        
        # ランダムな相槌を打つ
        self.play_random_nod_thread = threading.Thread(target=voice.play_random_nod)
        self.play_random_nod_thread.start()

        # 録音停止後にテキスト変換と応答生成をバックグラウンドで実行
        if self.recording_complete:
            thread = threading.Thread(target=self.process_audio_and_generate_response)
            thread.start()
        else:
            # ストリーミングでない場合は、スレッドが完了するのを待つ必要がある
            print("録音完了待機中...")
            def check_completion():
                if self.recording_complete:
                    thread = threading.Thread(target=self.process_audio_and_generate_response)
                    thread.start()
                else:
                    self.root.after(100, check_completion)
            self.root.after(100, check_completion)
        
    def start_record_waiting(self):
        """録音待機を開始する"""
        self.record_waiting = True
        self.recording_complete = False
        self.record_wait_button.config(text="録音待機中", style="danger.TButton")
        self.stop_event.clear()  # イベントをクリア

        # 録音をバックグラウンドスレッドで実行
        self.record_waiting_thread = threading.Thread(target=self.wait_for_keyword_thread)
        self.record_waiting_thread.start()

    def stop_record_temporary(self):
        self.record_wait_button.config(text="処理中...", style="danger.TButton", state="disabled")
        self.record_button.config(state="disabled")

        # ウィンドウをキャプチャする
        if self.selected_window:
            self.capture_window()
        else:
            print("ウィンドウが選択されていません")
            return
        
        # ランダムな相槌を打つ
        self.play_random_nod_thread = threading.Thread(target=voice.play_random_nod)
        self.play_random_nod_thread.start()

        # 録音停止後にテキスト変換と応答生成をバックグラウンドで実行
        if self.recording_complete:
            thread = threading.Thread(target=self.process_audio_and_generate_response, args=(True,))
            thread.start()
        else:
            print("録音が停止されていません")

    def stop_record_waiting(self):
        """録音待機を停止する"""
        self.record_waiting = False
        self.record_wait_button.config(text="録音待機", style="success.TButton")
        self.stop_event.set()  # スレッド停止イベントをセット

    def update_record_buttons_state(self, event=None):
        """録音ボタンの状態を更新する"""
        if self.use_image.get() and self.selected_window is None:
            self.record_button.config(state="disabled")
            self.record_wait_button.config(state="disabled")
            print("画像利用がオンですが、ウィンドウが選択されていないため録音ボタンを無効化しました。")
        else:
            self.record_button.config(state="normal")
            self.record_wait_button.config(state="normal")
        
    def process_audio_and_generate_response(self, from_temporary_stop=False):
        """音声認識、応答生成、GUI更新をまとめて行う"""
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

        # "検索" または "けんさく" が含まれているか確認
        if "検索" in prompt or "けんさく" in prompt:
            search_keyword = prompt
            # asyncioのイベントループを管理
            try:
                loop = asyncio.get_running_loop()
            except RuntimeError:  # 'RuntimeError: There is no current event loop...'
                loop = asyncio.new_event_loop()
                asyncio.set_event_loop(loop)
            
            search_results = loop.run_until_complete(self.run_ai_search(search_keyword))
            
            if search_results:
                prompt += "\n\n検索結果:\n" + "\n".join(search_results)

        self.prompt = prompt
        response = self.ask_gemini()
        self.response = response

        # --- GUI更新と音声再生（メインスレッドで実行） ---
        def update_gui_and_speak():
            if self.show_response_in_new_window.get():
                if response:
                    self.show_gemini_response(response)
            else:
                if response:
                    self.output_textbox.insert(END, "Geminiの回答: " + response + "\n")
                    self.output_textbox.see(END)
            
            voice.text_to_speech(response)

            # 一時ファイルを削除
            if os.path.exists(self.audio_file_path):
                os.remove(self.audio_file_path)
            if os.path.exists(self.screenshot_file_path):
                os.remove(self.screenshot_file_path)

            # 待機モードの場合は再度待機を開始
            if self.record_waiting:
                self.record_wait_button.config(text="録音待機中", style="danger.TButton")
                self.record_waiting_thread = threading.Thread(target=self.wait_for_keyword_thread)
                self.record_waiting_thread.start()
            
            # ボタンを再度有効化
            self.record_button.config(text="録音開始", style="success.TButton", state="normal")
            self.record_wait_button.config(state="normal")
            if not self.record_waiting:
                self.record_wait_button.config(text="録音待機", style="success.TButton")


        self.root.after(0, update_gui_and_speak)

    async def run_ai_search(self, query: str):
        """ai_searchを非同期で実行する"""
        return await ai_search(query)
    
    def show_gemini_response(self, response_text):
        """Geminiのレスポンスを表示する"""
        if self.show_response_in_new_window.get():
            GeminiResponseWindow(self.root, response_text, self.response_display_duration.get())
        else:
            self.response_label.config(text=response_text)
            # 一定時間後にテキストを消去
            self.root.after(self.response_display_duration.get(), lambda: self.response_label.config(text=""))


    def record_audio_thread(self):
        """別スレッドで録音処理を実行する"""
        if self.device_index is None:
            print("マイクが選択されていません。")
            return
            
        self.stop_event.clear()
        
        # ストリーミングモードでない場合のみファイルに録音
        if not self.use_streaming.get():
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
        """キーワード検出で録音を待機するスレッド"""
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
            if not self.stop_event.is_set(): # 待機がキャンセルされなかった場合のみ後処理
                self.root.after(0, self.stop_record_temporary)

    def update_level_meter(self, volume):
        """レベルメーターを更新する"""
        level = int(volume / 100)  # ボリュームを0-100の範囲に変換
        self.root.after(0, self.set_level_meter_value, level)

    def set_level_meter_value(self, level):
        self.level_meter['value'] = level

    def capture_window(self):
        """ウィンドウをキャプチャする"""
        print("ウィンドウをキャプチャします…")
        try:
            capture.capture_screen(self.selected_window, self.screenshot_file_path)
            self.load_and_display_image(self.screenshot_file_path)  # ここを変更
        except Exception as e:
            print(f"キャプチャできませんでした： {e}")

    def load_and_display_image(self, image_path):
        """画像を読み込み、別スレッドで表示する"""
        # 画像読み込みとリサイズを別スレッドで実行
        threading.Thread(target=self.process_image, args=(image_path,)).start()

    def process_image(self, image_path):
        """画像処理を行う関数"""
        try:
            image = Image.open(image_path)
            # 最大サイズに合わせてリサイズ
            max_size = (400, 300)  # 例：幅400px、高さ300px
            image.thumbnail(max_size)
            self.image = ImageTk.PhotoImage(image)
            # GUIスレッドで画像を更新
            self.root.after(0, self.update_image_label)
        except Exception as e:
            print(f"画像処理エラー: {e}")

    def update_image_label(self):
        """画像ラベルを更新する"""
        if self.image:
            self.image_label.config(image=self.image)

    def transcribe_audio(self):
        """音声をテキストに変換する"""
        print("音声認識を開始します...")
        try:
            if self.use_streaming.get():
                print("ストリーミングモードで音声認識を実行します。")
                audio_stream = record.stream_audio(
                    device_index=self.device_index,
                    update_callback=self.update_level_meter,
                    stop_event=self.stop_event
                )
                text = whisper.recognize_speech_from_stream(audio_stream)
            else:
                print("ファイルモードで音声認識を実行します。")
                if not os.path.exists(self.audio_file_path):
                    print(f"エラー: 音声ファイルが見つかりません: {self.audio_file_path}")
                    return None
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
        # Gemini APIを呼び出す
        image_path = self.screenshot_file_path if self.use_image.get() and os.path.exists(self.screenshot_file_path) else None
        if self.prompt:
            response = self.session.generate_content(self.prompt, image_path, self.is_private.get())
            return response
        return "プロンプトがありません。"

    def open_memory_window(self):
        """メモリー管理ウィンドウを開く"""
        MemoryWindow(self.root, self.memory_manager)

def on_closing():
    print("アプリケーションを終了します...")
    if record.p:
        record.p.terminate()
    root.destroy()

if __name__ == "__main__":
    root = ttk.Window(themename="superhero")
    root.geometry("800x600") # 初期サイズを設定
    app = GameAssistantApp(root)
    root.protocol("WM_DELETE_WINDOW", on_closing)
    root.mainloop()