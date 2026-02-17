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
    
    セッション中、AIの最後の発話が終わってからの時間を監視し、定期的に
    自発的なコメントを生成します。実行時にビジー（誰かが発話中）であれば、
    数秒待ってから再試行します。
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
        
        # リトライ管理
        self.retry_count = 0
        self.max_retries = 5 # 待機回数を少し多めに設定

    def start(self):
        """サービスの開始：最初のカウントダウンを開始します"""
        if self.is_running:
            return
        
        if not self.app.state.enable_auto_commentary.get():
            return

        logging.info("AutoCommentaryService を開始します。")
        self.is_running = True
        self.start_next_cycle()

    def stop(self):
        """サービスの停止"""
        if not self.is_running:
            return
            
        logging.info("AutoCommentaryService を停止します。")
        self.is_running = False
        self._stop_event.set()
        self.timer_thread = None
        self.current_interval = 0

    def start_next_cycle(self):
        """次の長期間カウントダウンを開始します（TTS終了時などに呼ばれる）"""
        if not self.is_running:
            return
        # 前のタイマーがあればキャンセル
        self._stop_event.set()
        self._schedule_next_commentary()

    def get_remaining_time(self):
        """GUI表示用"""
        if not self.is_running or self.current_interval == 0:
            return 0, 0
        elapsed = time.time() - self.start_time
        remaining = max(0, self.current_interval - elapsed)
        return remaining, self.current_interval

    def notify_activity(self):
        """
        アクティビティ通知：
        ユーザーからは「リセットするのは間違い」との指摘があったため、
        現在はログ出力のみ行い、タイマーのリセットは行いません。
        """
        # logging.debug("Activity detected (No reset per user requirement)")
        pass

    def _schedule_next_commentary(self, interval=None):
        """待機スレッドを起動します"""
        if not self.is_running:
            return

        self._stop_event = threading.Event()

        if interval is None:
            # 新規サイクル（長期間待機）
            try:
                min_val = int(self.app.state.auto_commentary_min.get())
                max_val = int(self.app.state.auto_commentary_max.get())
            except:
                min_val, max_val = 300, 600
                
            if min_val > max_val: min_val = max_val
            interval = random.randint(min_val, max_val)
            logging.info(f"📅 次の自動ツッコミまで {interval} 秒カウントダウンを開始します。")
        else:
            # 回避待機（短期間待機）
            logging.info(f"🔄 割り込み回避のため {interval} 秒待機します...")
        
        self.current_interval = interval
        self.start_time = time.time()
        
        self.timer_thread = threading.Thread(
            target=self._wait_and_execute, 
            args=(interval, self._stop_event),
            daemon=True
        )
        self.timer_thread.start()

    def _wait_and_execute(self, interval, stop_event):
        if stop_event.wait(timeout=interval):
            return # キャンセルされた

        if not self.is_running:
            return

        self._try_execute_commentary()

    def _try_execute_commentary(self):
        if not self.is_running: 
            return

        # 誰かが喋っていたら、タイマーをリセットせず「数秒待って回避」する
        if self._is_busy():
            logging.info("✋ ユーザーまたはAIが発話中のため、タイミングをずらします。")
            self._avoid_and_retry()
            return
        
        self._generate_and_speak()

    def _is_busy(self):
        """システムが使用中か判定"""
        if self._is_user_speaking():
            return True
        if not self.app.tts_manager.playback_queue.empty() or not self.app.tts_manager.tts_queue.empty():
            return True
        if getattr(voice, 'is_playing', False):
            return True
        return False

    def _is_user_speaking(self):
        if hasattr(self.session_manager, 'transcriber') and self.session_manager.transcriber:
            if getattr(self.session_manager.transcriber, 'last_partial_text', ""):
                return True
        return False 

    def _avoid_and_retry(self):
        """数秒（15秒）待って再試行する（メインタイマーはリセットしない）"""
        self.retry_count += 1
        if self.retry_count > self.max_retries:
            logging.info("❌ 再試行回数の上限に達しました。このサイクルは一旦終了します。")
            self.retry_count = 0
            # 諦めて次の通常サイクルへ（TTS終了を待たないのでここでスケジュール）
            self._schedule_next_commentary()
        else:
            delay = 15 # 15秒ずらす
            self._schedule_next_commentary(interval=delay)

    def _generate_and_speak(self):
        logging.info("🎬 自動ツッコミを生成中...")
        self.retry_count = 0 
        
        screenshot_path = None
        if self.app.state.current_window:
            try:
                screenshot_path = self.app.capture_service.capture_window()
            except Exception as e:
                logging.warning(f"Screenshot Error: {e}")
        
        history = self.session_manager.get_session_history()
        prompt = AUTO_COMMENTARY_PROMPT
        if history:
            prompt += f"\n\n(直近の会話履歴):\n{history[-1000:]}"

        try:
            response = self.app.gemini_service.ask(
                prompt=prompt,
                image_path=screenshot_path,
                is_private=self.app.state.is_private.get(),
                memory_type='auto_commentary',
                session_history=None
            )

            # 生成後の最終チェック
            if not self.is_running or self._is_busy():
                logging.info("✋ 生成中に状況が変化したため、タイミングをずらして再試行します。")
                self._avoid_and_retry()
                return

            if response and "申し訳ありません" not in response:
                logging.info(f"🗣️ 自動ツッコミ: {response}")
                
                self.app.memory_manager.enqueue_save({
                    'type': 'auto_commentary',
                    'source': 'AI_Auto',
                    'content': response,
                    'timestamp': datetime.now().isoformat()
                })
                
                voice.stop_playback_event.clear()
                sentences = [s.strip() for s in re.split(r'[。！？\n]', response) if s.strip()]
                if sentences:
                    for sentence in sentences:
                        self.app.tts_manager.put_text(sentence)
                    self.app.tts_manager.put_text("END_MARKER")
                    self.app.root.after(0, lambda: self.app.show_gemini_response(response, auto_close=False))
                    self.app.append_log_text(f"(Auto): {response}")
                    
                    # ここでは次をスケジュールしない。TTS終了時に App から呼ばれる。
                else:
                    self._schedule_next_commentary()
            else:
                self._schedule_next_commentary()
                
        except Exception as e:
            logging.error(f"AutoCommentary Error: {e}")
            self._schedule_next_commentary()
