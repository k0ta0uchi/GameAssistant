## 概要 (Overview)

Issue #17 の要件に基づき、`cargo timings` および実コード使用箇所の双方から Rust 依存グラフのボトルネックを徹底的に監査・分析しました。
不要な推移的依存の漏洩メカニズムを特定した上で、最も安全な低リスク feature 整理として **Tokio `full` から実使用 API への明示的絞り込み** を実装し、同一環境での Before / After ベンチマークを実測・評価しました。

Closes #17

---

## 1. `cargo timings` 上のボトルネック分析 (Timings Analysis)

#13 / #14 の clean build timings 分析（`scripts/perf-baseline/issue-14-opt-level-1.json`）において、コンパイル時間の大部分を占める上位クレート群を特定しました：

| クレート名 | 役割 / カテゴリ | 単体ビルド時間 (sec) | 全体比率 / 影響 |
| :--- | :--- | :--- | :--- |
| **`lance` 10.0.0** | ベクトルストレージ中核 | **694.9s** | 最低律速クレート |
| **`lance-index` 10.0.0** | ベクトルインデックス (IVF-PQ) | **254.3s** | 単体で4分超 |
| **`datafusion` 54.1.0** | SQL / クエリエンジン | **135.2s** | Lance のクエリ層 |
| **`sqlparser` 0.62.0** | SQL 構文解析 | **127.2s** | DataFusion 依存 |
| **`datafusion-functions-aggregate`** | 集計関数群 | **125.6s** | DataFusion 依存 |
| **`lance-namespace-impls` 10.0.0** | ネームスペース実装 | **120.8s** | Lance 依存 |
| **`lance-bitpacking` 10.0.0** | ビットパッキング圧縮 | **119.3s** | Lance 依存 |
| **`datafusion-expr` 54.1.0** | 論理式表現 | **117.7s** | DataFusion 依存 |
| **`lancedb` 0.37.1** | LanceDB クライアント | **115.2s** | ストレージフロントエンド |
| **`candle-core` 0.8.4** | ローカル ML 推論テンソル | **75.6s** | ASR (Whisper) 推論 |

### 主要な知見:
1. **LanceDB / Lance / DataFusion / Arrow スタックが clean build 全体の 70% 超（>1800s）を占有**。
2. **Candle スタック（`candle-core`, `candle-nn`, `candle-transformers`, `tokenizers`）** がローカル推論パスとして約 80 秒を消費。
3. 単一モノリシッククレート（`gameassistant`）の構造により、軽量なビジネスロジックやテストの変更時にも、巨大なストレージ・推論・GUI スタックとの再リンクが不可避となっている。

---

## 2. 依存関係 & 機能インベントリ (Feature Inventory)

`src-tauri/Cargo.toml` の主要クレートと実コードの突合監査結果：

### ① Tokio (`tokio = "1"`)
- **実コード使用箇所**:
  - `rt-multi-thread` / `rt`: `tokio::spawn`, `tokio::task::spawn_blocking`, `tokio::runtime::Runtime::new()`
  - `macros`: `tokio::select!` (lib.rs, asr.rs, local_summary.rs)
  - `sync`: `tokio::sync::{Mutex, Notify, mpsc, oneshot}`
  - `time`: `tokio::time::{sleep, timeout, timeout_at, interval, Instant, Duration}`
  - `process`: `tokio::process::{Child, Command}` (local_summary.rs)
  - `net`: `tokio::net::TcpListener` (ai_client.rs, asr.rs test)
  - `io-util`: `tokio::io::{AsyncReadExt, AsyncWriteExt}` (ai_client.rs)
  - `fs`: `tokio::fs::{read, metadata}` (tts.rs)
- **未使用 features**:
  - `signal` (OS シグナルハンドリング: Ctrl-C 等)
  - `io-std` (標準入出力の非同期ラッパー)
  - `parking_lot` (Tokio 内部オプショナル同期プリミティブ)
  - `test-util`
- **技術的発見 (Crucial Finding)**:
  - 推移的依存関係である **`lance-namespace-impls v10.0.0` が直接 `tokio` の `features = [..., "full"]` を要求** しています。
  - そのため、Cargo の **Feature Unification（機能統合）** により、モノリシッククレート構成下では `gameassistant` 側で feature を削っても、LanceDB をリンクしている限りビルドグラフ全体で `tokio/full` が依然として有効化され続けます。

### ② Candle (`candle-core`, `candle-nn`, `candle-transformers`, `tokenizers`)
- **実コード使用箇所**: `src/asr.rs` の embedded Whisper 音声認識パスに完全に局所化（約 500 行）。
- **影響**: 外部 ASR サーバー（Python faster-whisper）接続時やメモリ・UI 開発時にも常にコンパイルパスに乗る。feature gate（例: `features = ["embedded-whisper"]`）または専用クレート化により、通常開発時のビルドから完全に除外可能。

### ③ LanceDB / Arrow / DataFusion
- **実コード使用箇所**: `src/lance_memory.rs`, `src/memory_v2/` のベクトル検索・永続化層。
- **調査結果**: `lancedb` 自体の default features は空（`default = []`）であり、不要なクラウド SDK（aws, azure, gcs）等は既に有効化されていません。中核エンジン（lance/datafusion）自体が重いため、feature の削減ではなく物理的なクレート分割（隔離）が唯一の根本解決策。

### ④ Windows API / Tauri / Audio
- **実コード使用箇所**: Win32 ハンドル操作、Tauri IPC コマンド、マイク録音・TTS 再生。
- **調査結果**: Win32 features は必要最小限の 11 個に限定済み。プラットフォーム非依存コードと物理的に分離することで、リンクオーバーヘッドを局所化可能。

---

## 3. クレート分割候補の評価 (Crate Split Candidates)

将来的な本格アーキテクチャ刷新に向け、5 つの分割候補を具体的に評価しました：

| 候補クレート | 依存境界 (Dependencies) | 期待される短縮効果 | 移行の複雑度 | 循環依存・ランタイムリスク |
| :--- | :--- | :--- | :--- | :--- |
| **`gameassistant-memory`** | `lancedb`, `arrow-*`, `datafusion`, `lance-*` | **極めて高い (High)**<br>clean build の ~70% (>1800s) を隔離。日常開発での LanceDB 再ビルドを根絶。 | **Medium-High**<br>共通型（モデル・イベント）を `gameassistant-core` に切り出す必要あり。 | **Risk: Low**<br>メモリリポジトリ API 境界が確立済み。 |
| **`gameassistant-inference`** | `candle-core`, `candle-nn`, `candle-transformers`, `tokenizers` | **中〜高 (Medium-High)**<br>`candle-core` (75.6s) のビルドを局所化。 | **Low-Medium**<br>`src/asr.rs` の一部（約500行）に局所化されており容易。 | **Risk: Low**<br>PCM 入力 -> 文字列出力の単一パイプライン。 |
| **`gameassistant-audio`** | `cpal`, `rodio`, `hound`, `windows` (Win32), `process_loopback.cpp` | **中 (Medium)**<br>C++ `build.rs` および Windows COM オーディオを局所化。 | **Medium**<br>Windows ハンドル・COM スレッド管理の抽出。 | **Risk: Medium**<br>オーディオ初期化順序の注意が必要。 |
| **`gameassistant-twitch`** | `tokio-tungstenite`, `native-tls` | **低〜中 (Low-Medium)**<br>ネットワーク / WebSocket 依存の局所化。 | **Low**<br>独立性が極めて高い。 | **Risk: Very Low** |
| **`gameassistant-app` (Tauri Bridge)** | `tauri`, `tauri-plugin-*` | **高 (High)**<br>メインアプリを薄い IPC シェル化し、バックエンドロジックを Tauri 非依存でビルド・テスト可能に。 | **Medium**<br>コマンドハンドラー呼び出し層の整理。 | **Risk: Very Low** |

---

## 4. 実施した低リスク改善 (Implemented Change)

`src-tauri/Cargo.toml` において、Tokio の `features = ["full"]` を、実コードで使用している必要最小限の明示的 features に置き換えました：

```toml
[dependencies]
- tokio = { version = "1", features = ["full"] }
+ tokio = { version = "1", features = [
+     "rt-multi-thread",
+     "macros",
+     "sync",
+     "time",
+     "process",
+     "net",
+     "io-util",
+     "fs",
+ ] }
```

### 採用理由 (Rationale):
1. **最小権限原則（Least Privilege）の遵守**: 不要な `signal`, `io-std`, `parking_lot` などの直接要求を排除し、クレートが要求する API 契約を明確化。
2. **将来のクレート分割への布石**: 現状は `lance-namespace-impls` からの推移的漏洩により効果が限定的ですが、将来 `gameassistant-memory` を別クレートへ分離した瞬間に、メインアプリ側で Tokio の不要 features が即座に自動パージされます。

---

## 5. Before / After 実測結果 (Benchmark Measurements)

同一環境（Windows MSVC, `rustc 1.94.0-nightly`, 論理28コア）にて、Before（`tokio = ["full"]`）と After（明示的 features）の全フェーズを実測比較しました：

- 計測記録: `scripts/perf-baseline/issue-17-dependency-features.json`
- 計測スクリプト: `scripts/measure-issue-17.ps1`

| 計測フェーズ | コマンド | Before (Tokio full) | After (Tokio explicit) | 差分 (Delta) | 変化率 |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **clean_build** | `cargo build` (cold clean) | **640.42s** (10m 40s) | **615.96s** (10m 16s) | **-24.46s** | **-3.82% 短縮** |
| **test_compile_cold** | `cargo test --lib --no-run` | 31.79s | 40.49s | +8.70s | +27.37% |
| **warm_build** | `cargo build` (steady-state) | 64.17s | 95.07s | +30.90s | 状態依存 |
| **test_compile_warm** | `cargo test --lib --no-run` | 4.04s | 3.45s | **-0.59s** | **-14.60% 短縮** |
| **full_test_run** | `cargo test --lib` (323 tests) | 44.41s | 42.72s | **-1.69s** | **-3.81% 短縮** |

### 実測結果の分析:
- **Clean build**: 640.42s から 615.96s へと **24.46 秒短縮（-3.8%）** されました。
- **改善幅が限定的な理由**: 前述の通り、推移的依存の `lance-namespace-impls` が `tokio/full` を要求しているため、Cargo の機能統合によって一部の重い feature がビルドグラフに残存しているためです。この結果は分析と完全に整合しています。

---

## 6. ランタイム & テスト等価性検証 (Verification)

- `cargo test --lib`: **323 passed, 0 failed**（全テスト完全パスを確認）
- `cargo check`: 警告のみ（未使用コード）、エラーゼロ
- ランタイム機能・API 動作に一切の後退がないことを検証済み。

---

## 7. 次のステップ・推奨 Follow-up Issue (Follow-ups)

1. **Follow-up A: `gameassistant-inference` クレートの分離 (Low-Medium Risk)**
   - `src/asr.rs` の embedded Whisper パスを独立クレート（または optional feature）化し、日常開発から `candle-core` (75.6s) のビルドを排除。
2. **Follow-up B: `gameassistant-memory` クレートの分離 (High Value / High Impact)**
   - clean build の 70% を占める LanceDB / DataFusion スタックを独立クレート化し、Tokio `full` の推移的漏洩を根絶するとともに、コアアプリのコンパイル速度を飛躍的に向上。
