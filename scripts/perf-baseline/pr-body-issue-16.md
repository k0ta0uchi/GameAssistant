## 概要 (Overview)

Issue #16 の要件に基づき、Windows/MSVC 環境における `cargo-nextest` およびコンパイルキャッシュ（`sccache`）の実測ベンチマークを実施しました。ツールを無目的に導入するのではなく、実測数値とトレードオフを厳密に評価し、真に効果のあるツールのみを採用／統合しました。

Closes #16

---

## 1. Phase 1: `cargo-nextest` 実測結果と評価

### 実測値比較 (Wall Time, 複数回計測平均)
- 測定環境: Windows 11 (64-bit), MSVC toolchain (`x86_64-pc-windows-msvc`), `rustc 1.94.0-nightly`, `cargo-nextest 0.9.146`
- 計測記録: `scripts/perf-baseline/issue-16-nextest-sccache.json`

| Suite | テスト件数 | `cargo test` (mean) | `cargo nextest` (mean) | 差分 / 変化 | 備考 |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **Fast** | 145 | 0.86s (0.80 - 0.93) | 2.20s (2.19 - 2.20) | +1.34s | テスト実行自体は0.7s未満だが、145プロセスの起動・監視オーバーヘッドによる微増 |
| **Memory** | 156 | 26.11s (26.05 - 26.17) | **13.75s (13.67 - 13.83)** | **-12.36s (約47%短縮 / 約1.9倍高速)** | LanceDB / ストレージ統合テスト群で絶大な並列高速化を達成 |
| **Platform** | 22 | 22.20s (21.81 - 22.58) | 23.60s (23.25 - 23.96) | +1.40s | ASR 外部プロセス・ライブ接続待機が律速のため同等 |
| **Full** | 323 | **26.08s (25.84 - 26.33)** | 29.01s (28.85 - 29.17) | +2.93s | 全件一括の高並列実行により、ASRサーバー通信とNTFSストレージ競合が発生 |

### テスト結果の等価性 (Equivalence)
- `cargo test`: 323 passed, 0 failed
- `cargo nextest`: 323 passed, 0 failed, 0 skipped
- **テストの欠落・スキップ・誤判定ゼロを確認**

### 採否判断: **条件付き採用 / オプション統合 (Adopted: true)**
- **採用理由**:
  - **Memory suite が 26.1s から 13.8s へと約半減（約1.9倍高速化）**。
  - プロセス分離によるテストクラッシュ時の障害特定（Isolation）に優れる。
- **トレードオフと設計**:
  - Fast スイートおよび Full スイートではスレッドプール型の標準 Cargo ランナーが最速かつ安定しているため、デフォルトの強制にはせず、`scripts/test-rust.ps1 -Runner Nextest` によるオプトイン方式を採用。
  - デフォルトは安定して最速な `Cargo` を維持。
  - 未インストール環境では分かりやすい警告とインストール案内を表示し、自動的に `Cargo` へフォールバック。

---

## 2. Phase 2: `sccache` 実測結果と評価

### 実測値比較 (Wall Time & Cache Stats)
- 測定環境: Windows MSVC, `sccache 0.18.0`
- 計測記録・provenance: `scripts/perf-baseline/issue-16-sccache-stats.json`

| ビルド条件 | 説明 | Wall Time | Cache Hit 率 | 備考 |
| :--- | :--- | :--- | :--- | :--- |
| **Cold / Cache Populate** | キャッシュ空からの初回 clean build | 316.0s (5m 16s) | 0.00% (miss 691) | 302 MiB のキャッシュを生成 |
| **Cache Hit Rebuild** | `target` clean 後、キャッシュ活用 rebuild | 309.98s (5m 09s) | **76.88% (hit 572)** | **短縮幅: わずか 6.02s (約 1.9%)** |

### C/C++ (`process_loopback.cpp`) の評価
- MSVC の `cl.exe` は標準 PowerShell セッションの PATH に含まれておらず、Visual Studio 開発者環境（`vcvars64.bat`）外では sccache C++ ラッパーは起動不能。
- そもそも `process_loopback.cpp` は単一ファイルであり、`cc` クレートによる直接コンパイル時間は **0.3 秒未満** のため、キャッシュの恩恵自体が皆無。

### 非キャッシュ要因とボトルネックの分析
- **Non-cacheable 呼び出し**: 147件が `crate-type`（cdylib / proc-macro）制限によりキャッシュ不可。
- **MSVC の律速点**: Windows/MSVC におけるコンパイル時間の大部分は、リンクフェーズ（`link.exe` による PDB 生成・シンボル解決）および重い proc-macro 展開に費やされているため、76.88% の Rust rlib がキャッシュヒットしても全体の Wall Time は約1.9%しか短縮されない。

### 採否判断: **不採用 (Adopted: false)**
- **不採用理由**:
  - キャッシュヒット率 76.88% にもかかわらず、Wall Time の改善はわずか 1.9%（6秒）。
  - バックグラウンドデーモンの常駐、数GB〜10GBのディスク消費、開発環境依存性（PATH/MSVC）の複雑さに対して、得られる効果が極めて薄いため。

---

## 3. ベンチマーク再現性と検証基盤 (Verification & Reliability)

レビュー指摘事項を受け、ベンチマーク基盤を大幅に強化しました：
1. **非ゼロ Exit Code の厳格検証**:
   - `scripts/measure-issue-16.ps1` 内の各テストステップ実行直後に exit code を検証。テスト失敗やランナー異常終了を「計測成功」として不正記録しないようフェイルファストに改善。
2. **sccache 実測スクリプトと Provenance の分離**:
   - `scripts/measure-sccache.ps1` を新設し、sccache の実測および統計パース（Cold/Hit, Hit率, Non-cacheable理由, 生ログ）を独立して再実行・保存可能に整理。
   - `scripts/perf-baseline/issue-16-sccache-stats.json` に provenance を永続化し、`measure-issue-16.ps1` からも参照・更新可能に統合。
3. **成果物 JSON の記述整合性**:
   - `nextest_evaluation.reasons` の評価記述を、PR 本文および実測データ（Memory 2倍高速、Fast はプロセスオーバーヘッドで Cargo が優勢）と完全に整合。
4. **差分 PID 追跡による安全なプロセス終了 (Blocker対応)**:
   - ベンチマーク開始前に既存の Python プロセス PID を記録（`$baselinePythonPids`）し、テスト後に新規 spawn された孤立子プロセスのみを特定して終了する差分 PID 方式に改修。別ウィンドウで稼働中の GameAssistant 開発セッションや手動起動した Python プロセスを一切巻き込まない安全性を担保。
5. **Committed JSON と PR 本文の完全な Provenance 一致 (Medium対応)**:
   - 全 suite を 2 サンプルで再計測し、`scripts/perf-baseline/issue-16-nextest-sccache.json` の `samples`, `mean`, `min`, `max` と PR 本文の数値を完全に同期。

---

## 4. 将来の CI キャッシュ戦略 (Cache Key 設計)

GitHub Actions 等で CI を導入する際は、コンパイララッパー型（sccache）よりも、ディレクトリベースのキャッシュ（`actions/cache` または `Swatinem/rust-cache`）を採用することを推奨します。

### キャッシュキーの設計方針
- **Rust Toolchain**: `${{ runner.os }}-rust-${{ hashFiles('rust-toolchain*', 'src-tauri/Cargo.lock') }}`
- **Cargo Registry & Git Dependencies**: `${{ runner.os }}-cargo-registry-${{ hashFiles('src-tauri/Cargo.lock') }}`
- **Cargo Target Artifacts (Incremental)**:
  - Cache Key: `${{ runner.os }}-cargo-target-${{ hashFiles('src-tauri/Cargo.lock', 'src-tauri/Cargo.toml', 'src-tauri/.cargo/config.toml', 'src-tauri/build.rs', 'src-tauri/src/process_loopback.cpp') }}`
  - Restore Keys: `${{ runner.os }}-cargo-target-`

---

## 5. 開発者向けセットアップ手順 (Setup & Fallback)

### `cargo-nextest` のインストール（推奨）
```powershell
# winget を使用する場合
winget install nextest.cargo-nextest

# または cargo を使用する場合
cargo install cargo-nextest --locked
```

### テストの実行例
```powershell
# デフォルト (Cargo runner):
scripts\test-rust.ps1 -Suite Fast
scripts\test-rust.ps1 -Suite Full

# Nextest runner の活用 (Memory suite で2倍高速):
scripts\test-rust.ps1 -Suite Memory -Runner Nextest   # ~13.8s で完了
```

### フォールバック動作 (Fallback)
`cargo-nextest` がインストールされていない環境で `-Runner Nextest` を指定した場合、以下のように分かりやすいインストール案内を表示し、自動的に標準 Cargo ランナーへフォールバックします：
```
WARNING: cargo-nextest is not installed or not in PATH.
To install cargo-nextest on Windows:
  winget install nextest.cargo-nextest
  or: cargo install cargo-nextest --locked
Falling back to standard 'Cargo' test runner...
```
