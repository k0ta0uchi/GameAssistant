# GameAssistant

ゲームプレイをリアルタイムで支援するために作られた、多機能AIアシスタントです。
音声対話、画面認識、Twitch連携など、様々な機能であなたのゲーミング体験を向上させます。

## 主な機能 (Features)

- **🎮 AIによるゲームアシスト**:
  - ゲームに関する質問に、過去の会話や文脈を考慮して応答します。
  - キャラクター（優しい犬の女の子）として、親しみやすく対話します。

- **🎤 高度な音声対話**:
  - 「ねえぐり」というウェイクワードでアシスタントを起動。
  - 高性能な `Whisper` モデルによる正確な音声認識。
  - `VOICEVOX` または `Google Gemini` TTSによる自然な音声応答。
  - AIの応答が長い時に「ストップ」と言うことで、再生をいつでも中断できます。

- **🖼️ スクリーンショット解析**:
  - 指定したゲーム画面のスクリーンショットをAIが解析し、状況に基づいたアドバイスを提供します。
  - `mss` ライブラリを使用し、ゲーム画面も安定してキャプチャできます。

- **🤖 Twitch連携**:
  - あなたのTwitchチャンネルのボットとして動作します。
  - チャットでメンションが送られると、AIが内容を理解して応答します。

- **🧠 永続的な記憶**:
  - 会話の履歴やゲーム内のイベントを `ChromaDB` に自動で保存・記憶します。
  - 記憶した内容を基に、より的確な応答が可能です。
  - セッション終了時に、その日の会話内容をまとめたブログ記事（マークダウン形式）を自動生成できます。

- **🖥️ 使いやすいGUI**:
  - `ttkbootstrap` を利用した、モダンで分かりやすいデスクトップアプリケーション。
  - 使用するマイクやキャプチャするウィンドウをGUIから簡単に選択・変更できます。

## 必要なもの (Requirements)

- Python 3.9 以上
- `requirements.txt` に記載されたPythonライブラリ
- **VOICEVOX Engine**: ローカルでの音声合成に必要です。事前に起動しておく必要があります。
- **各種APIキー**:
  - `PICOVOICE_ACCESS_KEY` (Porcupineウェイクワードエンジン用)
  - `GOOGLE_API_KEY` (Google Gemini用)

## セットアップ (Setup)

1. **リポジトリをクローン:**
   ```bash
   git clone https://github.com/k0ta0uchi/GameAssistant.git
   cd GameAssistant
   ```

2. **Python仮想環境の作成と有効化:**
   ```bash
   python -m venv venv
   .\venv\Scripts\activate
   ```

3. **必要なライブラリをインストール:**
   ```bash
   pip install -r requirements.txt
   ```
   
4. **環境変数の設定:**
   プロジェクトのルートに `.env` という名前のファイルを作成し、以下のようにAPIキーを記述します。
   ```
   PICOVOICE_ACCESS_KEY="YOUR_PICOVOICE_ACCESS_KEY"
   GOOGLE_API_KEY="YOUR_GOOGLE_API_KEY"
   ```

5. **VOICEVOXの準備:**
   ローカル環境に[VOICEVOX Engine](https://voicevox.hiroshiba.jp/)をインストールし、本アプリケーションを使用する前に起動しておいてください。

## 使い方 (Usage)

1. **アプリケーションの起動:**
   ```bash
   python main.py
   ```

2. **初期設定:**
   - **インプットデバイス**: 音声入力に使用するマイクを選択します。
   - **ウィンドウ**: スクリーンショットの対象となるゲームウィンドウを選択します。

3. **基本的な使い方:**
   - **音声で起動**: 「ねえぐり」と話しかけると録音が開始されます。無音が続くと自動で録音が終了します。
   - **ボタンで録音**: 「録音開始」ボタンで手動録音も可能です。
   - **応答の中断**: AIが話している途中で「ストップ」と言うと、いつでも応答を中断できます。

4. **Twitch連携:**
   - GUIの「Twitch Bot」セクションで、ボットのユーザー名や認証情報を設定します。
   - 「承認URLコピー」ボタンから認証を行い、取得したコードを「認証コード」欄に入力して「トークン登録」ボタンを押します。
   - 「接続」ボタンを押すと、あなたのチャンネルでボットが動作を開始します。

## 技術スタック (Technology Stack)

- **AI & ML**: Google Gemini, OpenAI Whisper, Picovoice Porcupine
- **音声処理**: `pyaudio`, `sounddevice`, VOICEVOX
- **データベース**: `ChromaDB` (Vector DB)
- **GUI**: `ttkbootstrap` (Tkinter)
- **スクリーンショット**: `mss`
- **Twitch連携**: `twitchio`
- **その他**: `Python 3`, `threading`