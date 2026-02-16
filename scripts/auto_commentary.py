# -*- coding: utf-8 -*-
import threading
import time
import random
import logging
import asyncio
import os
import re
from datetime import datetime
from scripts.prompts import AUTO_COMMENTARY_PROMPT
import scripts.voice as voice

class AutoCommentaryService:
    """
    自立型ツッコミサービス（AutoCommentaryService）
    
    セッション中、ユーザーの発話がない沈黙時間を監視し、定期的に画面（スクリーンショット）
    や会話履歴をもとに、AIが自発的にコメント（ツッコミや独り言）を生成・発話します。
    """
    def __init__(self, app, session_manager):
        self.app = app
        self.session_manager = session_manager
        self.is_running = False
        self.timer_thread = None
        self._stop_event = threading.Event()
        
        # 進行管理用
        self.current_interval = 0
        self.start_time = 0
        
        # リトライ管理（ビジー状態でスキップされた場合用）
        self.retry_count = 0
        self.max_retries = 3

    def start(self):
        """サービスの開始：設定を確認し、タイマーを始動します"""
        if self.is_running:
            return
        
        # GUIの設定（AppState）から有効無効を確認
        if not self.app.state.enable_auto_commentary.get():
            logging.info("AutoCommentaryService は設定で無効化されています。")
            return

        logging.info("AutoCommentaryService を開始します。")
        self.is_running = True
        self._stop_event = threading.Event()
        self._schedule_next_commentary()

    def stop(self):
        """サービスの停止：実行中のタイマーを破棄します"""
        if not self.is_running:
            return
            
        logging.info("AutoCommentaryService を停止します。")
        self.is_running = False
        self._stop_event.set()
        self.timer_thread = None
        self.current_interval = 0

    def get_remaining_time(self):
        """GUIの進捗バー表示用に、次の実行までの残り時間と全待機時間を返します"""
        if not self.is_running or self.current_interval == 0:
            return 0, 0
        elapsed = time.time() - self.start_time
        remaining = max(0, self.current_interval - elapsed)
        return remaining, self.current_interval

    def notify_activity(self):
        """
        アクティビティ通知：ユーザーの発話やAIの応答があった時に呼び出します。
        これにより、会話中にAIがいきなり割り込むのを防ぐためタイマーをリセットします。
        """
        if self.is_running:
            logging.debug("ユーザーのアクティビティを検知。自動ツッコミタイマーをリセットします。")
            self._schedule_next_commentary()

    def _schedule_next_commentary(self, interval=None):
        """次の自動ツッコミをスケジュールします（ランダムな間隔を設定）"""
        if not self.is_running:
            return

        # 既存の待機スレッドがあれば確実に停止（リセット処理）
        self._stop_event.set()
        self._stop_event = threading.Event()

        if interval is None:
            # AppState から設定値を取得
            try:
                min_val = int(self.app.state.auto_commentary_min.get())
                max_val = int(self.app.state.auto_commentary_max.get())
            except (ValueError, TypeError, AttributeError):
                min_val, max_val = 300, 600 # フォールバック
                
            if min_val > max_val: min_val = max_val
            if min_val < 10: min_val = 10 # 最低10秒
            
            interval = random.randint(min_val, max_val)
            logging.info(f"📅 次の自動ツッコミを {interval} 秒後にスケジュールしました。")
        else:
            logging.info(f"🔄 自動ツッコミを {interval} 秒後に再試行します...")
        
        self.current_interval = interval
        self.start_time = time.time()
        
        # 非同期待機スレッドを開始
        self.timer_thread = threading.Thread(
            target=self._wait_and_execute, 
            args=(interval, self._stop_event),
            daemon=True
        )
        self.timer_thread.start()

    def _wait_and_execute(self, interval, stop_event):
        """指定された秒数待機し、中断されなければ実行フェーズへ移行します"""
        if stop_event.wait(timeout=interval):
            # 待機中に stop() または notify_activity() が呼ばれた場合
            return

        if not self.is_running:
            return

        self._try_execute_commentary()

    def _try_execute_commentary(self):
        """実行直前の割り込み防止チェックを行い、問題なければ生成を開始します"""
        if not self.is_running: 
            return

        # ユーザーが話している、またはTTSが再生中なら延期する
        if self._is_busy():
            logging.info("✋ ユーザーまたはAIが発話中のため、自動ツッコミを延期します。")
            self._retry_later()
            return
        
        self._generate_and_speak()

    def _is_busy(self):
        """システムが「使用中」かどうかを判定します（発話の重なり防止）"""
        # 1. ユーザーが話しているか（ASRの途中結果があるか）
        if self._is_user_speaking():
            return True
        # 2. AIが合成中、または再生待ちキューに何か入っているか
        if not self.app.tts_manager.playback_queue.empty() or not self.app.tts_manager.tts_queue.empty():
            return True
        # 3. 物理的に再生中か（音声ライブラリのフラグ）
        if getattr(voice, 'is_playing', False):
            return True
        return False

    def _is_user_speaking(self):
        """Whisperの認識状況から、ユーザーが現在発話中かどうかを判定します"""
        if hasattr(self.session_manager, 'transcriber') and self.session_manager.transcriber:
            if getattr(self.session_manager.transcriber, 'last_partial_text', ""):
                return True
        return False 

    def _retry_later(self):
        """ビジー状態だった場合に、少し時間を置いて再試行します"""
        self.retry_count += 1
        if self.retry_count > self.max_retries:
            logging.info("❌ 再試行回数の上限に達しました。このサイクルはスキップします。")
            self.retry_count = 0
            self._schedule_next_commentary()
        else:
            delay = 30 # 30秒後に再チェック
            self._schedule_next_commentary(interval=delay)

    def _generate_and_speak(self):
        """Geminiにリクエストを送り、独り言を生成・再生します"""
        logging.info("🎬 自動ツッコミ（独り言）を生成中...")
        
        # ウィンドウキャプチャ
        screenshot_path = None
        if self.app.state.current_window:
            try:
                screenshot_path = self.app.capture_service.capture_window()
            except Exception as e:
                logging.warning(f"スクリーンショットの取得に失敗しました: {e}")
        
        # 会話履歴の取得（直近1000文字程度）
        history = self.session_manager.get_session_history()
        prompt = AUTO_COMMENTARY_PROMPT
        if history:
            prompt += f"\n\n(直近の会話履歴):\n{history[-1000:]}"
        else:
            prompt += "\n\n(会話履歴: まだありません)"

        try:
            # AIへの問い合わせ
            response = self.app.gemini_service.ask(
                prompt=prompt,
                image_path=screenshot_path,
                is_private=self.app.state.is_private.get(),
                memory_type='auto_commentary',
                session_history=None
            )

            # 生成完了後の最終チェック（生成中に状況が変わっていないか）
            if not self.is_running or self._is_busy():
                logging.info("✋ 生成中に状況が変化（発話開始など）したため、出力を中止して延期します。")
                self._retry_later()
                return

            if response and "申し訳ありません、エラーが発生しました" not in response:
                self.retry_count = 0
                logging.info(f"🗣️ 自動ツッコミを生成しました: {response}")
                
                # 長期メモリに保存（RAG用）
                self.app.memory_manager.enqueue_save({
                    'type': 'auto_commentary',
                    'source': 'AI_Auto',
                    'content': response,
                    'timestamp': datetime.now().isoformat()
                })
                
                # 再生停止イベントをクリアして発話開始
                voice.stop_playback_event.clear()
                
                # 1. 音声再生（文分割してTTSキューへ）
                sentences = [s.strip() for s in re.split(r'[。！？\n]', response) if s.strip()]
                if sentences:
                    for sentence in sentences:
                        self.app.tts_manager.put_text(sentence)
                    self.app.tts_manager.put_text("END_MARKER")
                    
                    # 2. GUIのポップアップ表示
                    self.app.root.after(0, lambda: self.app.show_gemini_response(response, auto_close=False))
                    
                    # 3. ログエリアへの表示（復旧ポイント）
                    # 「新しいウィンドウで表示」がオフの場合、または履歴として残したい場合に出力
                    self.app.append_log_text(f"(Auto): {response}")
                else:
                    logging.warning("⚠️ 生成されたテキストが空でした。")
            else:
                logging.warning(f"⚠️ AIからの応答が空、またはエラーが含まれています: {response}")
                
        except Exception as e:
            logging.error(f"自動ツッコミの生成中に例外が発生しました: {e}")

        # 通常のサイクルに戻る
        self._schedule_next_commentary()
