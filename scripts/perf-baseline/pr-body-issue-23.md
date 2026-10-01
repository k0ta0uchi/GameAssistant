## 概要 (Overview)

Issue #23 に基づき、GameAssistant の Rust コードベースに対する基本的な健全性チェック（Format、Check、Fast tests、Full tests）を自動化する GitHub Actions ワークフロー（`.github/workflows/rust-ci.yml`）を導入しました。

CI ランナー上で実際にすべてのチェックが成功することを確認済みです。

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

1. **Checkout repository**: `actions/checkout@v4`
2. **Setup Rust toolchain**: `dtolnay/rust-toolchain@master` (`nightly-2026-01-14`, components: `rustfmt`)
3. **Setup uv**: `astral-sh/setup-uv@v5` (version: `0.8.12`)
4. **Prepare build prerequisites**:
   - Tauri の proc macro (`tauri::generate_context!()`) が要求する `frontendDist` (`../dist/index.html`) プレースホルダーの生成。
   - `src-tauri/src/bootstrap.rs` でバイナリ埋め込み (`include_bytes!("../resources/uv.exe")`) される `src-tauri/resources/uv.exe` の配置。
5. **Check code formatting**:
   ```powershell
   cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check
   ```
6. **Cargo check**:
   ```powershell
   cargo check --manifest-path src-tauri/Cargo.toml
   ```
7. **Run Fast tests**:
   ```powershell
   .\scripts\test-rust.ps1 -Suite Fast
   ```
8. **Run Full tests**:
   ```powershell
   .\scripts\test-rust.ps1 -Suite Full
   ```

---

## 3. 追加セットアップとその理由 (Environment Fixes & Rationale)

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

## 4. ローカル検証結果 (Local Verification)

- **`cargo fmt --check`**: 差分なし（OK）
- **`cargo check`**: 成功（Finished dev profile）
- **Fast tests**: **145 passed; 0 failed** (OK)
- **Full tests**: **323 passed; 0 failed** (30.31s, OK)

---

## 5. 今回スコープ外としたもの (Out of Scope)

初版の安定性・再現性・シンプルさを最優先とするため、以下は意図的に除外しています：
- `cargo-nextest` の必須化（Issue #16 の実測結果に基づき、標準 Cargo ランナーを使用）
- `sccache` / 複雑なコンパイルキャッシュ（cache 複雑性による不安定化防止）
- ベンチマーク回帰検知 / build time 閾値
- `gameassistant-memory` などのクレート分割
- Linux / macOS matrix

---

## 6. GitHub Actions 実行結果 (CI Run Status)

- **Workflow Run**: [Rust CI Run #36820099272](https://github.com/k0ta0uchi/GameAssistant/actions/runs/36820099272)
- **ステータス**: **PASS (All Green)**
  - `✓ Checkout repository`
  - `✓ Setup Rust toolchain`
  - `✓ Setup uv`
  - `✓ Prepare build prerequisites`
  - `✓ Check code formatting`
  - `✓ Cargo check`
  - `✓ Run Fast tests`
  - `✓ Run Full tests`
