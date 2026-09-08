# GameAssistant

ゲームプレイや配信をリアルタイムで支援するために作られた、超高速・低遅延な **Tauri 2.0 + Pure Rust Native** 製 AI デスクトップアシスタントです。
音声対話（Kotoba-Whisper CUDA INT8）、画面認識、Twitch連携、LanceDB ベクトル長期記憶、自立型実況（Auto Commentary）、noteブログ自動執筆など、様々な機能であなたのゲーミング体験を向上させます。

---

## ⚡ 主な機能 (Features)

- **🎮 AI によるゲームアシスト & 実況 (Gemini 2.0 Flash / Pro)**:
  - 画面認識・過去の会話文脈・ゲーム内履歴を総合的に考慮して応答。
  - 沈黙時間が続いた際に画面を見て自律的にコメント・ツッコミを入れる「Auto Commentary」機能。
  - Thinking モードの動的切り替え（会話時は高速、ブログ執筆時は熟考）。

- **🎤 超低遅延・高精度 音声対話 (Kotoba-Whisper CUDA INT8 + cpal)**:
  - 「ねえぐり」「アシスタント」等のウェイクワードで即座に起動し、相槌（Nod Sound）を即時再生。
  - Faster-Whisper CUDA INT8 によるミリ秒単位のストリーミング音声認識。
  - WASAPI Loopback による Discord 通話音声のゼロ遅延キャプチャ。
  - VRAM 蓄積による推論遅延を自動検知して自律的にワーカーを再起動するウォッチドッグ。

- **🧠 ローカルベクトル長期記憶 (Pure Rust LanceDB + GLuCoSE-base-ja)**:
  - プレイヤーの発話・AI応答・Discord会話を 768 次元の埋め込みベクトルとして LanceDB に自動蓄積。
  - 過去の出来事やプレイ記憶を高速セマンティック検索してプロンプトに動的注入。

- **📝 note ブログ記事の自動執筆 & スキル注入 (Skills Engine)**:
  - セッション終了時に配信ログから note ブログ記事（Markdown）を自動生成。
  - `skills/` 配下の執筆ペルソナ・文体ガイドライン（YAMLフロントマター付き）を動的に注入。

- **🤖 Pure Rust Twitch 連携 (WebSocket IRC)**:
  - 配信セッション開始時に自動接続し、視聴者チャットに応答。
  - OAuth Authorization Code フローによる安全なワンクリック認証。

- **🖥️ リッチ & モダンな UI (React 18 + Tailwind CSS + Tauri 2.0)**:
  - MIC, GEMINI, VOICE, TWITCH のリアルタイム・アクティビティ演出（Pulse / Glow）。
  - GPU VRAM / システム RAM のリアルタイムモニターと VRAM 1GB 事前確保オプション。

---

## 🚀 必要なもの & セットアップ (Setup)

### 必要な環境
- **OS**: Windows 10 / 11 (64-bit)
- **GPU**: NVIDIA GeForce (CUDA 12.x / VRAM 6GB以上推奨)
- **Node.js**: v18 以上
- **Rust**: 1.75 以上 (`rustup`)
- **VOICEVOX Engine**: ローカル音声合成用に起動（デフォルト: `http://127.0.0.1:50021`）
- **Google Gemini API Key**: [Google AI Studio](https://aistudio.google.com/) から取得

### インストール手順

#### ポータブル版（推奨）

`GameAssistant-v0.3.0-portable.exe` を書き込み可能なフォルダへ置いて起動してください。初回起動時にセットアップ画面が表示され、同じフォルダへ次のランタイムを自動準備します。

- `.python/` — `uv python install 3.12` で取得した管理対象Python
- `venv/` — アプリ専用仮想環境
- `models/` — 必須モデル
- `.uv-cache/`、`logs/`、`scripts/`、`setup-state.json`、`uv.lock`

Gemma 3 1B IT (GGUF Q4_K_S) は必須モデルです。初回セットアップでは、同梱の `NOTICE-GEMMA.txt` と [Gemma Terms](https://ai.google.dev/gemma/terms) を確認して同意すると、固定リビジョンから取得します。

依存パッケージとモデルの取得にはネットワーク接続と数GBの空き容量が必要です。`Program Files` など書き込み禁止の場所では、画面の「管理者としてセットアップ」からUACを承認するか、書き込み可能なフォルダへEXEを移動してください。途中でキャンセルしても、次回起動時に完了済みの段階から再開します。

モデルはすべて EXE 隣の `models/` に保存されます。旧バージョンの `settings.json` に `models_dir` が残っていても、セットアップ、状態表示、ダウンロード、実行時のモデル探索からは無視されます（`LOCALAPPDATA` へはリダイレクトしません）。

要約用 llama-server は空きポートを一時確保して起動します。Phase 3 のワーカー分離では、ポート割り当てと子プロセスのライフサイクルを同一ワーカー内で管理する設計へ移行する予定ですが、現行版では「確保直後に別プロセスがポートを取得する」小さな競合窓を完全にはなくせません。

1. **リポジトリをクローン:**
   ```bash
   git clone https://github.com/k0ta0uchi/GameAssistant.git
   cd GameAssistant
   ```

2. **Python 仮想環境の作成と ASR 依存ライブラリのインストール:**
   ```bash
   python -m venv venv
   .\venv\Scripts\activate
   pip install -r requirements.txt
   ```

3. **Node.js パッケージのインストール:**
   ```bash
   npm install
   ```

4. **アプリケーションの起動 (開発モード):**
   ```bash
   npm run tauri dev
   ```

---

## 🛠️ 技術スタック (Technology Stack)

- **Frontend**: React 18, TypeScript, Tailwind CSS, Lucide Icons, Vite
- **Core Backend**: Tauri 2.0, Pure Rust (`tokio`, `cpal`, `hound`, `rodio`, `reqwest`, `image`)
- **Vector Database**: LanceDB (Pure Rust SDK) + Apache Arrow 53
- **ASR & Embedding**: Kotoba-Whisper-v2.0-faster (CUDA INT8), pkshatech/GLuCoSE-base-ja
- **AI & Multimodal**: Google Gemini 2.0 Flash / Pro API, Brave Search API
- **TTS Engine**: VOICEVOX Engine (Local HTTP REST) / Style-Bert-VITS2
- **Twitch Integration**: Pure Rust WebSocket IRC Client (`tokio-tungstenite`)
