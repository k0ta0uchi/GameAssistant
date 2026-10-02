# GameAssistant

[English](#english) | [日本語](#日本語)

---

<a id="english"></a>
## English

GameAssistant is a desktop application designed to analyze PC gameplay video and audio in real time, providing voice interaction, automated commentary, and live streaming chat integration.
Built on Tauri 2.0, it separates native processing in Rust from the user interface in React, integrating multimodal AI models while minimizing CPU and GPU overhead during gameplay.

### Key Features

#### Voice Recognition and Interaction
Captures microphone input and voice communications (such as Discord via WASAPI loopback), performing transcription on a local GPU with Faster-Whisper (CUDA).
Upon detecting a configured wake word, the application immediately plays an acknowledgment sound and delivers responses synthesized via the VOICEVOX Engine using the Gemini API.

#### Gameplay Screen Analysis and Autonomous Commentary
Combines captured gameplay frames with recent conversational context for multimodal analysis using Gemini 2.0.
When the player remains silent for a set duration, the system detects visual activity and autonomously generates commentary or reactions.

#### Persistent Memory and Semantic Search
Session dialogue and game events are vectorized using an embedding model (GLuCoSE-base-ja) and stored in an embedded LanceDB database.
Semantic search retrieves past context and player preferences across sessions, dynamically injecting them into AI prompts to enable persistent, long-term awareness.

#### Live Streaming Integration
Includes a native Rust WebSocket IRC client to monitor and respond to viewer chat during Twitch streaming sessions.
Authentication is handled through an OAuth authorization code flow for secure token acquisition.

#### Secure Credential Storage
Sensitive tokens such as the Gemini API key and Twitch OAuth secrets are never stored in plaintext configuration files. They are encrypted using Windows DPAPI (Data Protection API) and persisted in `credentials.enc`.
Read-modify-write operations are guarded by a two-layer exclusive lock combining an intra-process mutex and a Windows Named Mutex, preventing lost updates and file corruption during concurrent operations.

#### Automated Blog Article Generation
Generates Markdown articles suitable for publication on note at the end of a gaming session, summarizing highlights and conversational logs.
Writing personas and style guidelines placed in the `skills/` directory can be dynamically injected into the generation prompt.

### System Requirements

- **OS**: Windows 10 / 11 (64-bit)
- **GPU**: NVIDIA GeForce (CUDA 12.x support; 6 GB or more VRAM recommended)
- **External Services & Software**:
  - Google Gemini API Key
  - VOICEVOX Engine (running locally; default port: `50021`)

### Getting Started

#### Using the Portable Release
Place the standalone executable (`GameAssistant-v<version>-portable.exe`) in any writable directory and launch it.
On initial launch, a setup wizard automatically configures the runtime environment and downloads the following required models into `models/` and the local virtual environment within the same directory:

- **Python Runtime and Virtual Environment**: Managed Python 3.12 runtime (via `uv`) and an application-dedicated virtual environment
- **Speech Recognition Models (ASR)**:
  - `kotoba-whisper-v2.0-faster` (GPU inference with CUDA, Japanese-specialized)
  - `faster-whisper-small` (CPU fallback model)
- **Text Embedding Model (Embedding)**:
  - `GLuCoSE-base-ja` (768-dimensional model for long-term memory and vector search)
- **Local Summarization Model (LLM)**:
  - `gemma-3-1b-it-Q4_K_S.gguf` (GGUF format for llama-server, downloaded after accepting the bundled terms of service)

Downloading dependencies and models requires an active internet connection and several gigabytes of free disk space.

#### Building and Developing from Source

1. **Prerequisites**:
   - Node.js 20 or higher
   - Rust toolchain (nightly version specified in `rust-toolchain.toml`)
   - Python 3.12 and `uv` (or standard `python` / `pip`)
   - Git

2. **Clone the Repository and Install Dependencies**:
   ```bash
   git clone https://github.com/k0ta0uchi/GameAssistant.git
   cd GameAssistant

   # Install Node.js packages
   npm install

   # Create a Python virtual environment and install ASR/embedding dependencies
   # Using uv (recommended):
   uv venv
   uv pip install -r requirements.txt

   # Or using standard Python venv:
   python -m venv venv
   .\venv\Scripts\activate
   pip install -r requirements.txt
   ```

3. **Launch Development Mode**:
   ```bash
   npm run tauri dev
   ```

4. **Package the Portable Release**:
   ```powershell
   .\scripts\build-portable.ps1
   ```
   Upon completion, the standalone executable is generated in `dist_release/GameAssistant-v<version>-portable.exe`.

### Testing and CI

Automated tests are maintained for both Rust and frontend code to ensure reliability.

- **Rust Test Suite**:
  ```powershell
  # Fast unit tests
  .\scripts\test-rust.ps1 -Suite Fast

  # Full test suite (~360 tests, including storage and platform integrations)
  .\scripts\test-rust.ps1 -Suite Full
  ```
- **Frontend Contract Tests & Build**:
  ```bash
  # Component and hook behavioral contract tests
  npm test

  # TypeScript typecheck and Vite production build
  npm run build
  ```
- **Continuous Integration (GitHub Actions)**:
  On pull requests and pushes to `main`, GitHub Actions runs the frontend pipeline (`npm ci`, `npm test`, `npm run build`) on Ubuntu in parallel with the Rust pipeline (`cargo fmt`, `cargo check`, Fast/Full tests) on Windows.

### Technology Stack

| Domain | Technologies & Libraries |
| :--- | :--- |
| **Frontend** | React 18, TypeScript, Tailwind CSS, Lucide Icons, Vite |
| **Backend Core** | Tauri 2.0, Rust (Tokio, cpal, hound, rodio, reqwest) |
| **Speech Recognition (ASR)** | Faster-Whisper (CUDA INT8), cpal (WASAPI Loopback) |
| **Vector Search & Memory** | LanceDB (Pure Rust SDK), Apache Arrow, GLuCoSE-base-ja |
| **Multimodal & Inference** | Google Gemini API (2.0 Flash / Pro), llama-server (Gemma 3 GGUF) |
| **Speech Synthesis (TTS)** | VOICEVOX Engine (Local HTTP REST) |
| **Streaming Integration** | Twitch WebSocket IRC Client (tokio-tungstenite) |
| **Security & Storage** | Windows DPAPI (CryptProtectData / CryptUnprotectData), Windows Named Mutex |

---

<a id="日本語"></a>
## 日本語

GameAssistant は、PC ゲームのプレイ映像と音声をリアルタイムに解析し、音声対話、自動実況、配信チャット連携を行うデスクトップアプリケーションである。
Tauri 2.0 を基盤に採用し、Rust によるネイティブ処理と React による操作画面を分離することで、ゲーム動作への負荷を抑えつつマルチモーダル AI を統合している。

### 主な機能

#### 音声認識と音声対話
マイクロフォン入力および Discord などのボイスチャット音声（WASAPI ループバック）を取得し、ローカル環境の GPU 上で動作する Faster-Whisper（CUDA）によって文字起こしを行う。
設定したウェイクワードの検知時には確認音（相槌）を即時再生し、Gemini API による回答テキストを VOICEVOX Engine と連携して音声出力する。

#### ゲーム画面解析と自動実況
キャプチャしたゲーム画面のフレームと直近の会話文脈を組み合わせ、マルチモーダルモデル（Gemini 2.0）で状況を判定する。
プレイヤーの発話がない状態が一定時間継続した場合は、画面の動きや変化を検知して自動的に実況コメントを生成する。

#### 長期記憶とセマンティック検索
セッション中の発話や認識結果は、埋め込みモデル（GLuCoSE-base-ja）によりベクトル化され、組み込みの LanceDB に保存される。
過去のセッションで蓄積された文脈やプレイヤーの好みをセマンティック検索で抽出し、AI へのプロンプトに動的に注入することで、長期的な記憶に基づく対話を実現する。

#### 配信プラットフォーム連携
Twitch の WebSocket IRC クライアントを内蔵しており、配信セッション中に視聴者チャットを受信して応答対象に含めることができる。
認証には OAuth 認可コードフローを採用し、トークンを安全に取得する。

#### 安全な資格情報管理
Gemini API キーや Twitch OAuth トークンなどの機密情報は、平文の設定ファイルには保存せず、Windows DPAPI（Data Protection API）で暗号化して `credentials.enc` に保管する。
ファイル操作にはプロセス内ミューテックスと Windows 名前付きミューテックスを組み合わせた排他制御を適用し、並行更新によるデータの巻き戻りや破損を防ぐ。

#### プレイログからのブログ記事生成
ゲームセッションの終了時に、会話ログと状況要約を基に note 形式の Markdown 記事を自動生成する。
`skills/` ディレクトリ配下に定義した文体ガイドラインや執筆ペルソナをプロンプトへ注入できる。

### 動作要件

- **OS**: Windows 10 / 11 (64-bit)
- **GPU**: NVIDIA GeForce（CUDA 12.x 対応、VRAM 6GB 以上を推奨）
- **外部サービス・依存ソフトウェア**:
  - Google Gemini API キー
  - VOICEVOX Engine（ローカル実行、デフォルトポート: `50021`）

### 利用手順

#### ポータブル版の実行
スタンドアロン配布ファイル（`GameAssistant-v<version>-portable.exe`）を書き込み権限のあるディレクトリに配置して実行する。
初回起動時にセットアップ画面が表示され、以下のランタイムおよび必須モデル群が同一ディレクトリ配下の `models/` や仮想環境へ自動的に構築・取得される。

- **Python ランタイムと仮想環境**: `uv` により管理される Python 3.12 およびアプリ専用仮想環境
- **音声認識モデル (ASR)**:
  - `kotoba-whisper-v2.0-faster`（CUDA 推論用、日本語特化モデル）
  - `faster-whisper-small`（CPU フォールバック用モデル）
- **テキスト埋め込みモデル (Embedding)**:
  - `GLuCoSE-base-ja`（長期記憶・ベクトル検索用、768次元）
- **ローカル要約言語モデル (LLM)**:
  - `gemma-3-1b-it-Q4_K_S.gguf`（llama-server 用 GGUF モデル、同梱の利用規約同意確認後にダウンロード）

依存パッケージやモデルの取得には、ネットワーク接続および数 GB の空き容量が必要となる。

#### ソースコードからのビルドと開発

1. **前提ツールの準備**:
   - Node.js 20 以上
   - Rust ツールチェーン（`rust-toolchain.toml` で指定された nightly バージョン）
   - Python 3.12 および `uv`（または標準 `python` / `pip`）
   - Git

2. **リポジトリの取得と依存関係の導入**:
   ```bash
   git clone https://github.com/k0ta0uchi/GameAssistant.git
   cd GameAssistant

   # Node.js パッケージのインストール
   npm install

   # Python 仮想環境の作成と音声認識・埋め込み依存ライブラリのインストール
   # uv を使用する場合（推奨）:
   uv venv
   uv pip install -r requirements.txt

   # または標準 Python を使用する場合:
   python -m venv venv
   .\venv\Scripts\activate
   pip install -r requirements.txt
   ```

3. **開発モードでの起動**:
   ```bash
   npm run tauri dev
   ```

4. **ポータブル版バイナリのパッケージング**:
   ```powershell
   .\scripts\build-portable.ps1
   ```
   ビルド完了後、`dist_release/GameAssistant-v<version>-portable.exe` に単一実行可能ファイルが出力される。

### テストと CI

本リポジトリでは、品質維持のために Rust およびフロントエンド双方の自動テストを実施している。

- **Rust テストスイート**:
  ```powershell
  # 高速ユニットテスト
  .\scripts\test-rust.ps1 -Suite Fast

  # 全テスト（約360件、ストレージ統合・プラットフォームテストを含む）
  .\scripts\test-rust.ps1 -Suite Full
  ```
- **フロントエンド契約テストとビルド**:
  ```bash
  # コンポーネントおよびフックの振る舞い検証
  npm test

  # TypeScript 型検査および Vite プロダクションビルド
  npm run build
  ```
- **継続的インテグレーション (GitHub Actions)**:
  プルリクエストおよび `main` ブランチへのプッシュ時に、Ubuntu 環境でのフロントエンド検証（`npm ci`, `npm test`, `npm run build`）と Windows 環境での Rust 検証（`cargo fmt`, `cargo check`, Fast/Full テスト）が並列で実行される。

### 技術スタック

| 分野 | 採用技術・ライブラリ |
| :--- | :--- |
| **フロントエンド** | React 18, TypeScript, Tailwind CSS, Lucide Icons, Vite |
| **バックエンド基盤** | Tauri 2.0, Rust (Tokio, cpal, hound, rodio, reqwest) |
| **音声認識 (ASR)** | Faster-Whisper (CUDA INT8), cpal (WASAPI Loopback) |
| **ベクトル検索・記憶** | LanceDB (Pure Rust SDK), Apache Arrow, GLuCoSE-base-ja |
| **マルチモーダル・推論** | Google Gemini API (2.0 Flash / Pro), llama-server (Gemma 3 GGUF) |
| **音声合成 (TTS)** | VOICEVOX Engine (ローカル HTTP REST) |
| **配信連携** | Twitch WebSocket IRC クライアント (tokio-tungstenite) |
| **セキュリティ** | Windows DPAPI (CryptProtectData / CryptUnprotectData), Windows Named Mutex |
