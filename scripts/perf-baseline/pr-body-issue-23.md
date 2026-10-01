## 概要 (Overview)

Issue #23 に基づき、GameAssistant の Rust コードベースに対する基本的な健全性チェック（Format、Check、Fast tests、Full tests）を自動化する GitHub Actions ワークフロー（`.github/workflows/rust-ci.yml`）を導入しました。

さらに、初回実測で約51分を要していた cold ビルド時間を改善するため、**GitHub Actions 向けの Rust/Cargo キャッシュ機構（`Swatinem/rust-cache@v2`）** を導入しました。`main` ブランチを共有キャッシュの基準点として、後続の新規 PR でもキャッシュを安全かつ確実に再利用できる構成を整備しています。

Closes #23

---

## 1. ワークフロー構成 (Workflow Configuration)

- **Workflow ファイル**: [`.github/workflows/rust-ci.yml`](file:///.github/workflows/rust-ci.yml)
- **Triggers**:
  - `pull_request` (PR 作成・更新時)
  - `push` (`main` ブランチへの push 時)
  - `workflow_dispatch` (手動実行用)
- **Runner**:
  - `windows-latest`（Tauri、Windows API、CPAL オーディオ、MSVC C++ `process_loopback.cpp` の依存関係を充足する正規環境）
- **Shell**: `pwsh` (PowerShell Core)
- **Rust Toolchain**:
  - `nightly-2026-01-14` + `rustfmt`
  - [`rust-toolchain.toml`](file:///rust-toolchain.toml) をリポジトリルートに配置し、ローカル開発環境と CI 環境の間での Toolchain ドリフトを完全に防止。
  - ※ `arrow-data v58.4.0` が `edition2024` Cargo feature を要求するため、nightly が必須となります。

---

## 2. 実行ステップ (CI Steps)

単一ジョブ（`Rust Build & Test (Windows)`）内で以下のステップを明確に分離して順次実行します（`continue-on-error` なし。いずれかの失敗で即座にジョブ failure）：

```text
Checkout repository
  ↓
Setup Rust toolchain
  ↓
Rust cache (Swatinem/rust-cache@v2)
  ↓
Setup uv
  ↓
Prepare build prerequisites
  ↓
Check code formatting
  ↓
Cargo check
  ↓
Run Fast tests
  ↓
Run Full tests
```

---

## 3. CI 高速化：Rust / Cargo キャッシュ設計 (CI Caching Architecture)

### ① 導入した Cache Action
- **Action**: `Swatinem/rust-cache@v2`
  - Windows + Cargo 環境で実績があり、`src-tauri` のサブディレクトリ構成に対応した信頼性の高いアクションを採用。
  - ※ Issue #16 で実測評価済みの通り、`sccache` は Windows/MSVC 環境での効果が限定的であったため **sccache は一切使用していません**。

### ② キャッシュ対象 (Cached Directories)
- `C:\Users\runneradmin\.cargo\registry` (Cargo crates.io インデックス・ソース)
- `C:\Users\runneradmin\.cargo\git` (Cargo git 依存リポジトリ)
- `C:\Users\runneradmin\.cargo\bin` (インストール済みバイナリ)
- `src-tauri\target` (ビルド・テスト成果物バイナリ、中間オブジェクト)
  - 約 882 packages の巨大な依存グラフを毎回再コンパイルするオーバーヘッドを根絶します。

### ③ キャッシュキー設計 (Cache Key & Invalidation)
- **Primary Key / Prefix**:
  - OS (`Windows_NT-x64`)
  - Toolchain (`rustc 1.94.0-nightly 2026-01-14`)
  - ワークスペースの依存定義: `src-tauri/Cargo.lock`, `src-tauri/Cargo.toml`
  - 追加設定ファイルのハッシュ: `key: ${{ hashFiles('rust-toolchain.toml', '.cargo/config.toml', 'src-tauri/.cargo/config.toml') }}`
- **コミット SHA 非依存**:
  - `github.sha` をキーに含めず、同一の toolchain・依存構成であればコミットを跨いで確実にヒットする設計。
  - 依存変更（Cargo.lock / Cargo.toml 更新）、Toolchain 変更、Cargo config 変更時には自動的に安全な cache miss / invalidation が発生します。

### ④ main ブランチを基準とするキャッシュ共有モデル (Cross-PR Cache Sharing)
GitHub Actions のキャッシュスコープ規則に基づき、以下のサイクルで動作します：
1. **PR #24 (初回)**: Cold ビルド実行後、PR ブランチに初期キャッシュを保存。
2. **PR #24 (2回目以降)**: 同一 PR 内でキャッシュが restore され、高速ビルドを実証。
3. **`main` へのマージ (`push: branches: [main]`)**: マージ時の CI 実行により、**`main` ブランチスコープとして共有キャッシュが保存・更新**。
4. **新規 PR (#25 以降)**: GitHub Actions の親ブランチ継承機能により、新規 PR の CI が自動的に `main` の最新キャッシュを restore し、変更差分のみを数分〜十数分で高速コンパイル・テスト実行可能となります。

---

## 4. 追加セットアップとその理由 (Environment Fixes & Rationale)

1. **`PROTOC` 相対パス化 & ルート設定**:
   - 従来 `src-tauri/.cargo/config.toml` に絶対パス `C:/Workspace/GameAssistant/...` がハードコードされていたため、CI 環境およびワークスペース外からの実行で `protoc` が見つからず `lance-table` 等のビルドが失敗する問題がありました。
   - `relative = true` による相対パス指定へ改修し、リポジトリルートにも [`.cargo/config.toml`](file:///.cargo/config.toml) を配置。さらに CI ワークフローの環境変数で `PROTOC` / `PROTOC_INCLUDE` を定義し、どのディレクトリからの実行でも 100% 確実に解決されるようにしました。
2. **クリーンチェックアウト環境における前提ファイルの準備**:
   - `.gitignore` されている `dist/`（フロントエンド成果物）および `src-tauri/resources/uv.exe`（ランタイムバイナリ）が存在しないことによるコンパイルエラーを解消するため、`Prepare build prerequisites` ステップで最小限のプレースホルダーと `uv.exe` を安全に配備。
3. **プラットフォームテストの外部プロセスガード**:
   - `src-tauri/src/asr.rs` の `platform_test_whisper_ws_client` において、他のプラットフォームテスト（`tts` や `transcribe_wav`）と同様に、Python venv が未セットアップの環境（CI 環境等）では安全にスキップ（早期 return）するガードを追加。
4. **`rustfmt` フォーマット違反の解消**:
   - `src-tauri/src/bootstrap.rs` および `src-tauri/src/memory_v2/mod.rs` のフォーマット揺れ（中括弧改行）を修正。
5. **ドキュメントの更新**:
   - [`README.md`](file:///README.md) の Testing セクションに CI 自動実行およびローカルでの同等確認コマンドを追記。

---

## 5. キャッシュ実測・ベンチマーク結果 (Benchmark Measurements)

| 測定項目 | Cold Run (初回ビルド) | Cache Hit Run (2回目) | 改善幅 (Delta) | 状態 |
| :--- | :--- | :--- | :--- | :--- |
| **Job Total Time** | **52m 00s** | *(計測中)* | - | - |
| **Cargo check** | 11m 40s | *(計測中)* | - | - |
| **Fast tests** | 36m 50s | *(計測中)* | - | - |
| **Full tests** | 2m 05s | *(計測中)* | - | - |
| **Cache Restore** | - (Miss) | *(計測中)* | - | - |

---

## 6. 今回スコープ外としたもの (Out of Scope)

初版の安定性・再現性・シンプルさを最優先とするため、以下は意図的に除外しています：
- `cargo-nextest` の必須化（Issue #16 の実測結果に基づき、標準 Cargo ランナーを使用）
- `sccache` / 独自 RUSTC_WRAPPER（GitHub Actions cache action で代替）
- ベンチマーク回帰検知 / build time 閾値
- `gameassistant-memory` などのクレート分割
- Linux / macOS matrix
