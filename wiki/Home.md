# GameAssistant Wiki

Welcome to the **GameAssistant** documentation wiki.
GameAssistant は、AI（Google Gemini 2.0 Flash / Pro）と高速音声認識（Kotoba-Whisper-v2.0-faster CUDA INT8）を活用し、ゲーム配信やプレイをリアルタイムにアシストするデスクトップアプリケーションです。

---

## 📚 ドキュメント一覧

- [🏛️ Rust への完全移植とアーキテクチャ知見 (Architecture Migration to Rust)](Architecture-Migration-to-Rust)
- [🎙️ 音声認識 & ASR エンジン仕様](Architecture-Migration-to-Rust#2-音声入力認識パイプライン)
- [🧠 LanceDB ローカルベクトル記憶](Architecture-Migration-to-Rust#3-ローカルベクトル記憶-lancedb--semantic-memory)
- [📝 note ブログ自動執筆 & スキル注入](Architecture-Migration-to-Rust#52-note-ブログ記事の自動生成--スキル注入-skills-engine)
- [⚡ VRAM 最適化 & プロセス管理](Architecture-Migration-to-Rust#6-gpu--vram-リソース最適化とプロセス管理)

---

## 🚀 クイックスタート

```powershell
# リポジトリのクローン & セットアップ
git clone https://github.com/k0ta0uchi/GameAssistant.git
cd GameAssistant

# 依存パッケージのインストール
npm install

# 開発モード起動 (Tauri 2.0 + Vite)
npm run tauri dev
```
