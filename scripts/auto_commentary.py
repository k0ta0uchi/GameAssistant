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
        
        # アクティビティ管理
        self.last_activity_time = time.time()
        
        # リトライ管理
        self.retry_count = 0
        self.max_retries = 3

    def notify_activity(self):
        """
        アクティビティ（ユーザーの発話、TTS再生終了など）を通知し、タイマーをリセットする。
        """
        self.last_activity_time = time.time()
        logging.debug("AutoCommentary timer reset due to activity.")

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
        self.last_activity_time = time.time()
        self._schedule_next_commentary()

    def stop(self):
        """サービスの停止"""
        if not self.is_running:
            return
            
        logging.info("Stopping AutoCommentaryService...")
        self.is_running = False
        self._stop_event.set()
        
        # タイマースレッドの終了待機は行わず、フラグチェックで自然消滅させる
        self.timer_thread = None

    def _schedule_next_commentary(self, interval=None):
        """次のコメント実行をスケジュールする"""
        if not self.is_running or self._stop_event.is_set():
            logging.info("AutoCommentaryService is stopping, scheduling cancelled.")
            return

        if interval is None:
            interval = random.randint(self.min_interval, self.max_interval)
        
        logging.info(f"📅 Next auto-commentary scheduled in {interval} seconds of silence.")
        
        self.timer_thread = threading.Thread(
            target=self._wait_and_execute, 
            args=(interval,),
            daemon=True
        )
        self.timer_thread.start()

    def _wait_and_execute(self, interval):
        """指定された『沈黙時間』が経過するまで待機して実行を試みる"""
        logging.debug(f"AutoCommentary silence timer started: {interval}s")
        
        while self.is_running and not self._stop_event.is_set():
            now = time.time()
            elapsed = now - self.last_activity_time
            
            if elapsed >= interval:
                # 規定の沈黙時間が経過
                logging.debug(f"Silence interval ({interval}s) reached. Trying to execute...")
                break
            
            # 残り時間を計算して待機（最大1秒間隔でチェック）
            remaining = interval - elapsed
            wait_time = min(1.0, remaining)
            
            if self._stop_event.wait(timeout=wait_time):
                logging.debug("AutoCommentary timer cancelled.")
                return

        if not self.is_running or self._stop_event.is_set():
            return

        self._try_execute_commentary()

    def _try_execute_commentary(self):
        """コメント生成と再生の実行を試みる（割り込み防止チェック付き）"""
        if not self.is_running: 
            return

        logging.info("🤖 Trying to execute auto-commentary...")

        # 1. ユーザー発話中チェック
        if self._is_user_speaking():
            logging.info("✋ User is speaking. Delaying commentary...")
            self._retry_later()
            return

        # 2. AI発話中チェック（キューが空でない場合を含む）
        if not self.app.playback_queue.empty() or not self.app.tts_queue.empty():
             logging.info("✋ AI is currently speaking or queue is not empty. Delaying commentary...")
             self._retry_later()
             return
        
        # 実行
        self._generate_and_speak()

    def _is_user_speaking(self):
        """
        ユーザーが現在話している最中（確定前のテキストがある状態）か判定。
        """
        if hasattr(self.session_manager, 'transcriber') and self.session_manager.transcriber:
            # 確定前のテキストがある = ユーザーが話している最中
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
            logging.info(f"🔄 Retrying in {delay} seconds (Attempt {self.retry_count}/{self.max_retries})...")
            self._schedule_next_commentary(interval=delay)

    def _generate_and_speak(self):
        """Geminiにリクエストしてツッコミを生成・再生する"""
        self.retry_count = 0 # リセット
        
        logging.info("🎬 Generating auto-commentary...")
        
        # スクリーンショット撮影
        screenshot_path = None
        if self.app.selected_window:
            try:
                screenshot_path = self.app.capture_service.capture_window()
                logging.debug(f"Screenshot taken for auto-commentary: {screenshot_path}")
            except Exception as e:
                logging.warning(f"Failed to take screenshot for auto-commentary: {e}")
        
        # 会話履歴取得
        history = self.session_manager.get_session_history()
        
        # プロンプト作成
        prompt = AUTO_COMMENTARY_PROMPT
        if history:
            # 直近の履歴を一部含める
            prompt += f"\n\n(直近の会話履歴):\n{history[-500:]}"
        else:
            prompt += "\n\n(会話履歴: なし)"

        # Geminiリクエスト
        try:
            logging.debug("Sending auto-commentary request to Gemini...")
            
            # メインのGeminiServiceを使用して生成
            response = self.app.gemini_service.ask(
                prompt=prompt,
                image_path=screenshot_path,
                is_private=self.app.is_private.get(),
                memory_type='auto_commentary',
                session_history=None # プロンプトに埋め込み済み
            )

            if response:
                logging.info(f"🗣️ Auto-Commentary generated: {response}")
                
                # TTSキューへ投入して発話させる
                # 読点などで分割して投入（長い文対策）
                sentences = [s.strip() for s in re.split(r'[。！？\n]', response) if s.strip()]
                for sentence in sentences:
                    self.app.tts_queue.put(sentence)
                self.app.tts_queue.put("END_MARKER")
                
                # GUIに表示する（メインスレッドで実行）
                # auto_close=False にし、TTS終了時に App 側でタイマーを開始させる
                self.app.root.after(0, lambda: self.app.show_gemini_response(response, auto_close=False))
                
                # チャットログにも追記（メインスレッドで実行）
                if not self.app.show_response_in_new_window.get():
                    self.app.root.after(0, lambda: self.app._update_log_with_partial_response(f"\n(Auto): {response}", is_start=True))
            else:
                logging.warning("⚠️ Auto-Commentary response was empty.")
                
        except Exception as e:
            logging.error(f"Error in auto-commentary generation: {e}", exc_info=True)
            # エラー発生時は次回までの間隔を長めにとる
            self._schedule_next_commentary(interval=self.min_interval * 2)
            return

        # 次回スケジュール（通常間隔）
        self._schedule_next_commentary()