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
    セッション中、定期的に自動でコメント（ツッコミ）を生成・発話するサービス。
    """
    def __init__(self, app, session_manager):
        self.app = app
        self.session_manager = session_manager
        self.is_running = False
        self.timer_thread = None
        self._stop_event = threading.Event()
        
        # 実行間隔の設定（秒）
        self.min_interval = 300  # 5分
        self.max_interval = 600  # 10分
        
        # リトライ管理
        self.retry_count = 0
        self.max_retries = 3

    def start(self):
        """サービスの開始"""
        if self.is_running:
            return
        
        # 設定で無効なら起動しない
        if not hasattr(self.app, 'enable_auto_commentary') or not self.app.enable_auto_commentary.get():
            logging.info("AutoCommentaryService is disabled in settings.")
            return

        logging.info("Starting AutoCommentaryService...")
        self.is_running = True
        self._stop_event.clear()
        self._schedule_next_commentary()

    def stop(self):
        """サービスの停止"""
        if not self.is_running:
            return
            
        logging.info("Stopping AutoCommentaryService...")
        self.is_running = False
        self._stop_event.set()
        self.timer_thread = None

    def _schedule_next_commentary(self, interval=None):
        """次のコメント実行をスケジュールする"""
        if not self.is_running or self._stop_event.is_set():
            return

        if interval is None:
            interval = random.randint(self.min_interval, self.max_interval)
            logging.info(f"📅 Next auto-commentary scheduled in {interval} seconds.")
        else:
            logging.info(f"🔄 Retrying auto-commentary in {interval} seconds...")
        
        self.timer_thread = threading.Thread(
            target=self._wait_and_execute, 
            args=(interval,),
            daemon=True
        )
        self.timer_thread.start()

    def _wait_and_execute(self, interval):
        """指定時間待機して実行を試みる"""
        if self._stop_event.wait(timeout=interval):
            return

        if not self.is_running:
            return

        self._try_execute_commentary()

    def _try_execute_commentary(self):
        """コメント生成と再生の実行を試みる（割り込み防止チェック付き）"""
        if not self.is_running: 
            return

        # 実行前の初期チェック
        if self._is_busy():
            logging.info("✋ System is busy. Delaying commentary...")
            self._retry_later()
            return
        
        # 実行
        self._generate_and_speak()

    def _is_busy(self):
        """ユーザーが話しているか、AIが話しているか判定。"""
        # ユーザー発話中チェック
        if self._is_user_speaking():
            return True
        # AI発話中チェック（キューが空でない場合を含む）
        if not self.app.playback_queue.empty() or not self.app.tts_queue.empty():
            return True
        return False

    def _is_user_speaking(self):
        """
        ユーザーが現在話している最中か判定。
        """
        if hasattr(self.session_manager, 'transcriber') and self.session_manager.transcriber:
            if getattr(self.session_manager.transcriber, 'last_partial_text', ""):
                return True
        return False 

    def _retry_later(self):
        """少し待って再試行（最大リトライ数まで）"""
        self.retry_count += 1
        if self.retry_count > self.max_retries:
            logging.info("❌ Max retries reached. Skipping this commentary cycle.")
            self.retry_count = 0
            self._schedule_next_commentary()
        else:
            delay = 15  # 15秒後に再試行
            self._schedule_next_commentary(interval=delay)

    def _generate_and_speak(self):
        """Geminiにリクエストしてツッコミを生成・再生する"""
        logging.info("🎬 Generating auto-commentary...")
        
        screenshot_path = None
        if self.app.selected_window:
            try:
                screenshot_path = self.app.capture_service.capture_window()
            except Exception as e:
                logging.warning(f"Failed to take screenshot: {e}")
        
        history = self.session_manager.get_session_history()
        prompt = AUTO_COMMENTARY_PROMPT
        if history:
            prompt += f"\n\n(直近の会話履歴):\n{history[-500:]}"
        else:
            prompt += "\n\n(会話履歴: なし)"

        try:
            # Geminiリクエスト（ここでの待機中に状況が変わる可能性がある）
            response = self.app.gemini_service.ask(
                prompt=prompt,
                image_path=screenshot_path,
                is_private=self.app.is_private.get(),
                memory_type='auto_commentary',
                session_history=None
            )

            # 生成後の最終チェック
            if not self.is_running or self._is_busy():
                logging.info("✋ System became busy during generation. Delaying commentary...")
                self._retry_later()
                return

            if response and "申し訳ありません、エラーが発生しました" not in response:
                self.retry_count = 0 # 成功したのでリセット
                logging.info(f"🗣️ Auto-Commentary generated: {response}")
                
                # 割り込みフラグをクリア
                voice.stop_playback_event.clear()
                
                # TTSキューへ投入（文分割）
                sentences = [s.strip() for s in re.split(r'[。！？\n]', response) if s.strip()]
                if sentences:
                    for sentence in sentences:
                        self.app.tts_queue.put(sentence)
                    self.app.tts_queue.put("END_MARKER")
                
                    # GUI表示
                    self.app.root.after(0, lambda: self.app.show_gemini_response(response, auto_close=False))
                    
                    if not self.app.show_response_in_new_window.get():
                        self.app.root.after(0, lambda: self.app._update_log_with_partial_response(f"\n(Auto): {response}", is_start=True))
                else:
                    logging.warning("⚠️ Auto-Commentary sentences were empty.")
            else:
                logging.warning(f"⚠️ Auto-Commentary response was empty or error: {response}")
                
        except Exception as e:
            logging.error(f"Error in auto-commentary generation: {e}")

        # 次回スケジュール（通常サイクル）
        self._schedule_next_commentary()

    def notify_activity(self):
        """
        互換性のために残すが、現在は何もしない。
        """
        pass