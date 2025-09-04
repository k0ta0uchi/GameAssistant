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


class OutputRedirector:
    """print文をテキストボックスにリダイレクトするクラス"""
    def __init__(self, widget):
        self.widget = widget

    def write(self, str):
        self.widget.insert(END, str)
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
        self.show_response_in_new_window = ttk.BooleanVar(value=self.settings.get("show_response_in_new_window", True)) # デフォルト値を設定ファイルから読み込む
        self.response_display_duration = ttk.IntVar(value=self.settings.get("response_display_duration", 10000))  # デフォルト値を設定ファイルから読み込む
        self.session = gemini.GeminiSession(self.custom_instruction)

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
        self.settings["show_response_in_new_window"] = self.show_response_in_new_window.get() # 設定を保存
        self.settings["response_display_duration"] = self.response_display_duration.get()  # 設定を保存

        with open(self.settings_file, "w", encoding="utf-8") as f:
            json.dump(self.settings, f, ensure_ascii=False, indent=4)

    def get_device_index_from_name(self, device_name):
        """デバイス名からデバイスインデックスを取得する"""
        return record.get_device_index_from_name(device_name)

    def create_widgets(self):
        self.audio_container = ttk.LabelFrame(self.root, text="インプットデバイス", padding=10)
        self.audio_container.pack(fill=X)

        # オーディオデバイス選択ドロップダウンメニュー
        self.audio_dropdown = ttk.Combobox(
            master=self.audio_container,
            textvariable=self.selected_device,
            values=self.audio_devices,
            state=READONLY,
            width=48
        )
        self.audio_dropdown.pack(pady=(0, 10))
        self.audio_dropdown.bind("<<ComboboxSelected>>", self.update_device_index)

        # ラベル (デバイスインデックスを表示)
        self.device_index_label = ttk.Label(
            master=self.audio_container,
            text="Device index: "
        )
        self.device_index_label.pack(pady=5)

        self.window_container = ttk.LabelFrame(self.root, text="ウィンドウ", padding=10)
        self.window_container.pack(fill=X)

        # ウィンドウ選択ドロップダウンメニュー
        self.window_dropdown = ttk.Combobox(
            master=self.window_container,
            textvariable=self.selected_window_title,
            values=self.windows,
            state=READONLY,
            width=48
        )
        self.window_dropdown.pack(pady=(0, 10))
        self.window_dropdown.bind("<<ComboboxSelected>>", self.update_window)

        # 選択されたウィンドウタイトルを表示するラベル
        self.selected_window_label = ttk.Label(
            master=self.window_container,
            text="Selected window: "
        )
        self.selected_window_label.pack(pady=5)

        self.meter_container = ttk.LabelFrame(self.root, text="レベルメーター", padding=10)
        self.meter_container.pack(fill=X)

        # レベルメーター
        self.level_meter = ttk.Progressbar(
            self.meter_container,
            length=300,
            maximum=100,  # 音量レベルの最大値
            value=0,  # 初期値
            style="danger.Horizontal.TProgressbar",
        )
        self.level_meter.pack(pady=10)

        # 設定コンテナ
        self.config_container = ttk.LabelFrame(self.root, text="設定", padding=10)
        self.config_container.pack(fill=X)

        # 画像使用チェックボックス
        self.use_image_check = ttk.Checkbutton(
            self.config_container,
            text="画像を使用する",
            variable=self.use_image,
            style="success-square-toggle",
            command=self.save_settings  # チェックボックスの状態が変わったときに設定を保存
        )
        self.use_image_check.pack(side=LEFT, pady=5)

        # プライベートチェックボックス
        self.is_private_check = ttk.Checkbutton(
            self.config_container,
            text="プライベート",
            variable=self.is_private,
            style="success-square-toggle",
            command=self.save_settings  # チェックボックスの状態が変わったときに設定を保存
        )
        self.is_private_check.pack(side=LEFT, pady=10)
        
        # レスポンスを別ウィンドウに表示するかのチェックボックス
        self.show_response_in_new_window_check = ttk.Checkbutton(
            self.config_container,
            text="レスポンスを別ウィンドウに表示する",
            variable=self.show_response_in_new_window,
            style="success-square-toggle",
            command=self.save_settings  # チェックボックスの状態が変わったときに設定を保存
        )
        self.show_response_in_new_window_check.pack(side=LEFT, pady=10)

        # レスポンス表示時間設定
        self.response_duration_label = ttk.Label(self.config_container, text="レスポンス表示時間 (ミリ秒):")
        self.response_duration_label.pack(side=LEFT, pady=5)
        self.response_duration_entry = ttk.Entry(self.config_container, textvariable=self.response_display_duration, width=10)
        self.response_duration_entry.pack(side=LEFT, pady=5)
        self.response_duration_entry.bind("<FocusOut>", lambda e: self.save_settings())

        # 画像を表示するラベル
        self.image_label = ttk.Label(self.root)
        self.image_label.pack(pady=10)

        # ログ表示コンテナ
        self.text_container = ttk.Frame(self.root, padding=10)
        self.text_container.pack(fill=X)

        # テキストボックスとスクロールバーの追加
        self.output_textbox = ttk.Text(master=self.text_container, height=10, width=50, wrap=WORD)
        self.output_textbox.pack(side=LEFT, fill=BOTH, expand=True, padx=10, pady=10)

        self.scrollbar = ttk.Scrollbar(self.text_container, orient=VERTICAL, command=self.output_textbox.yview)
        self.scrollbar.pack(side=RIGHT, fill=Y, padx=(0, 10), pady=10)

        self.output_textbox['yscrollcommand'] = self.scrollbar.set

        self.record_container = ttk.Frame(self.root, padding=10)
        self.record_container.pack(fill=X)

        # 録音ボタン
        self.record_button = ttk.Button(self.record_container, text="録音開始", style="success.TButton", command=self.toggle_recording)
        self.record_button.pack(side=LEFT, pady=10)

        # 録音ボタン
        self.record_wait_button = ttk.Button(self.record_container, text="録音待機", style="success.TButton", command=self.toggle_record_waiting)
        self.record_wait_button.pack(side=LEFT, pady=10)

        # デフォルトでデバイスインデックスを取得
        if self.audio_devices:
            self.update_device_index()

        # デフォルトで最初のウィンドウタイトルを取得
        if self.windows:
            self.update_window()

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
        self.recording = False
        self.record_button.config(text="録音開始", style="success.TButton")

        # ウィンドウをキャプチャする
        if self.selected_window:
            self.capture_window()
        else:
            print("ウィンドウが選択されていません")
            return
        
        # ランダムな相槌を打つ
        self.play_random_nod_thread = threading.Thread(target=voice.play_random_nod)
        self.play_random_nod_thread.start()

        # 録音停止後にテキスト変換を実行
        if self.recording_complete:
            self.prompt = self.transcribe_audio()
        else:
            print("録音が停止されていません")
            return
        
        # "検索" または "けんさく" が含まれているか確認
        if self.prompt and ("検索" in self.prompt or "けんさく" in self.prompt):
            # 検索キーワードを抽出
            search_keyword = self.prompt
            search_results = asyncio.run(self.run_ai_search(search_keyword))
            
            # 検索結果をpromptに追加
            if search_results:
                self.prompt += "\n\n検索結果:\n" + "\n".join(search_results)
    
        if self.prompt:
            thread = threading.Thread(target=self.process_response)
            thread.start()
        
    def start_record_waiting(self):
        """録音待機を開始する"""
        self.record_waiting = True
        self.recording_complete = False
        self.record_wait_button.config(text="録音待機中", style="danger.TButton")
        self.stop_event.clear()  # イベントをクリア

        # 録音をバックグラウンドスレッドで実行
        self.record_waiting_thread = threading.Thread(target=self.record_audio_with_keyword_thread)
        self.record_waiting_thread.start()

    def stop_record_temporary(self):
        # ウィンドウをキャプチャする
        if self.selected_window:
            self.capture_window()
        else:
            print("ウィンドウが選択されていません")
            return
        
        # ランダムな相槌を打つ
        self.play_random_nod_thread = threading.Thread(target=voice.play_random_nod)
        self.play_random_nod_thread.start()

        # 録音停止後にテキスト変換を実行
        if self.recording_complete:
            self.prompt = self.transcribe_audio()
        else:
            print("録音が停止されていません")
            return

        if self.prompt != "":
            thread = threading.Thread(target=self.process_response)
            thread.start()
        else:
            self.record_waiting_thread = threading.Thread(target=self.record_audio_with_keyword_thread)
            self.record_waiting_thread.start()


    def stop_record_waiting(self):
        """録音待機を停止する""" 
        self.record_waiting = False
        self.record_wait_button.config(text="録音待機", style="success.TButton")
        self.stop_event.set()  # スレッド停止イベントをセット
        
        
    def process_response(self):
        self.response = self.ask_gemini()

        # レスポンスを別ウィンドウに表示するか、テキストボックスに表示するかを切り替える
        if self.show_response_in_new_window.get():
            if self.response:
                self.show_gemini_response(self.response)
        else:
            if self.response:
                self.output_textbox.insert(END, "Geminiの回答: " + self.response + "\n")
                self.output_textbox.see(END)  # スクロールして常に一番下を表示

        voice.text_to_speech(self.response)
        # 一時ファイルを削除
        if os.path.exists(self.audio_file_path):
            os.remove(self.audio_file_path)
        if os.path.exists(self.screenshot_file_path):
            os.remove(self.screenshot_file_path)
        
        if self.record_waiting:
            self.record_waiting_thread = threading.Thread(target=self.record_audio_with_keyword_thread)
            self.record_waiting_thread.start()

    async def run_ai_search(self, query: str):
        """ai_searchを非同期で実行する"""
        return await ai_search(query)
    
    def show_gemini_response(self, response_text):
        """Geminiのレスポンスを別ウィンドウで表示する"""
        GeminiResponseWindow(self.root, response_text, self.response_display_duration.get())


    def record_audio_thread(self):
        """別スレッドで録音処理を実行する（エコーキャンセリング付き）"""
        if self.device_index is None:
            print("マイクが選択されていません。")
            return
        
        record.record_audio_with_echo_cancellation(
            mic_device_index=self.device_index,
            loopback_device_index=None, # ループバックを無効化
            update_callback=self.update_level_meter,
            audio_file_path=self.audio_file_path,
            stop_event=self.stop_event # 通常録音でも停止イベントを渡すように変更
        )
        print("録音完了")
        self.recording_complete = True
        if self.recording: # ユーザーが手動で停止した場合のみ後処理を行う
            self.root.after(0, self.stop_recording)
    
    def record_audio_with_keyword_thread(self):
        """キーワード検出で録音を待機するスレッド（エコーキャンセリング付き）"""
        if self.device_index is None:
            print("マイクが選択されていません。")
            return

        record.record_audio_with_echo_cancellation(
            mic_device_index=self.device_index,
            loopback_device_index=None, # ループバックを無効化
            update_callback=self.update_level_meter,
            audio_file_path=self.audio_file_path,
            stop_event=self.stop_event
        )
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
            print("Geminiの回答:", response)
            return response
        return "プロンプトがありません。"

def on_closing():
    print("アプリケーションを終了します...")
    if record.p:
        record.p.terminate()
    root.destroy()

if __name__ == "__main__":
    root = ttk.Window(themename="darkly")
    app = GameAssistantApp(root)
    root.protocol("WM_DELETE_WINDOW", on_closing)
    root.mainloop()