# -*- coding: utf-8 -*-
import threading
import time
import random
import logging
import asyncio
import os
from datetime import datetime
from scripts.prompts import AUTO_COMMENTARY_PROMPT

class AutoCommentaryService:
    def __init__(self, app, session_manager):
        self.app = app
        self.session_manager = session_manager
        self.is_running = False
        self.timer_thread = None
        self._stop_event = threading.Event()
        self.min_interval = 300  # 5分
        self.max_interval = 600  # 10分
        self.retry_count = 0
        self.max_retries = 3

    def start(self):
        """サービスの開始"""
        if self.is_running:
            return
        
        # 設定で無効なら起動しない
        if not self.app.enable_auto_commentary.get():
            logging.info("AutoCommentaryService is disabled in settings.")
            return

        logging.info("Starting AutoCommentaryService...")
        self.is_running = True
        self._stop_event.clear()
        self._schedule_next_commentary()

    def stop(self):
        """サービスの停止"""
        logging.info("Stopping AutoCommentaryService...")
        self.is_running = False
        self._stop_event.set()
        if self.timer_thread and self.timer_thread.is_alive():
            # タイマー待機をキャンセルするためにイベントをセットしたが、
            # sleep中のスレッドを即座に起こすのは難しいため、
            # 次のループで is_running チェックにより終了するのを待つ
            pass

    def _schedule_next_commentary(self, interval=None):
        """次のコメント実行をスケジュールする"""
        if not self.is_running or self._stop_event.is_set():
            return

        if interval is None:
            interval = random.randint(self.min_interval, self.max_interval)
        
        logging.info(f"Next auto-commentary scheduled in {interval} seconds.")
        
        self.timer_thread = threading.Thread(target=self._wait_and_execute, args=(interval,), daemon=True)
        self.timer_thread.start()

    def _wait_and_execute(self, interval):
        """指定時間待機して実行を試みる"""
        # stop_eventを使って待機（中断可能にする）
        if self._stop_event.wait(timeout=interval):
            return

        if not self.is_running:
            return

        self._try_execute_commentary()

    def _try_execute_commentary(self):
        """コメント生成と再生の実行を試みる（チェック付き）"""
        if not self.is_running: return

        # 1. ユーザー発話中チェック (簡易: レベルメーターが閾値以上か)
        # Note: level_meterの値はGUIスレッドで更新されるため、ここでは直接AudioServiceの状態などを見るのが理想だが、
        # 簡易的にapp.level_meter.get()は見れない(Tkinter変数ではない)ため、
        # session_manager経由でTranscriberの状態を確認する
        if self._is_user_speaking():
            logging.info("User is speaking. Delaying commentary...")
            self._retry_later()
            return

        # 2. AI発話中チェック
        # voiceモジュールのイベントやキューの状態を確認
        import scripts.voice as voice
        # キューに溜まっているか、再生中ならスキップ
        if not self.app.playback_queue.empty() or not self.app.tts_queue.empty():
             logging.info("AI is currently speaking or queue is not empty. Delaying commentary...")
             self._retry_later()
             return

        # 実行
        self._generate_and_speak()

    def _is_user_speaking(self):
        """ユーザーが話しているか判定"""
        # 簡易実装: Transcriberが処理中か、AudioServiceが信号を受信中か
        # ここではSessionManagerのTranscriberの状態をチェックできればベスト
        # とりあえず安全側に倒して、過去5秒以内に認識結果があったかなどをチェックしたいが、
        # ログがないので、今回は「確率でスキップ」などはせず、そのまま実行へ進む（被ったらドンマイ）
        # 将来的にはAudioServiceにis_speakingフラグを実装する
        return False 

    def _retry_later(self):
        """少し待って再試行"""
        self.retry_count += 1
        if self.retry_count > self.max_retries:
            logging.info("Max retries reached. Skipping this commentary.")
            self.retry_count = 0
            self._schedule_next_commentary()
        else:
            delay = 10  # 10秒後
            logging.info(f"Retrying in {delay} seconds (Attempt {self.retry_count}/{self.max_retries})...")
            self._schedule_next_commentary(interval=delay)

    def _generate_and_speak(self):
        """Geminiにリクエストして再生"""
        self.retry_count = 0 # リセット
        
        logging.info("Generating auto-commentary...")
        
        # スクショ撮影
        screenshot_path = None
        if self.app.selected_window:
            screenshot_path = self.app.capture_service.capture_window()
        
        # 会話履歴取得
        history = self.session_manager.get_session_history()
        # 直近10行程度に絞るなどしてもよい
        
        # プロンプト作成
        prompt = AUTO_COMMENTARY_PROMPT
        if not history:
            prompt += "\n(会話履歴: なし)"
        else:
            prompt += f"\n(直近の会話履歴):\n{history[-500:]}" # 最後500文字

        # Geminiリクエスト（非同期で投げっぱなしにするか、スレッド内で待つか）
        # ここはThread内なので同期的に呼んでもGUIは固まらない
        try:
            # 独立したGeminiセッションを使うか、メインのを使うか
            # コンテキストを共有したいのでメインのGeminiServiceを使うが、履歴には追加したくないかもしれない
            # ここでは「履歴に追加しない」単発リクエストとして処理したいが、GeminiService.askは履歴に追加してしまう
            # なので、GeminiServiceに「履歴に追加しないモード」があるのが理想だが、
            # なければ新規セッション（GeminiService.sessionとは別）で投げる
            
            # 簡易的にメインサービスを使う（履歴に残っても「ツッコミ」として自然ならOK）
            # ただし、Auto Commentaryが履歴を汚染しすぎると文脈が乱れる可能性あり
            
            # 今回はGeminiService.askを使うが、historyへの追加は許容する
            response = self.app.gemini_service.ask(
                prompt=prompt,
                image_path=screenshot_path,
                is_private=self.app.is_private.get(),
                memory_type='auto_commentary', # 専用タイプ
                session_history=None # 履歴はプロンプトに埋め込んだのでNone
            )

            if response:
                logging.info(f"Auto-Commentary: {response}")
                # TTSキューへ投入
                # 文分割などはapp.execute_gemini_interactionと同様に行う必要があるが、
                # 短い一言（1-2文）という要件なので、そのまま投げる
                self.app.tts_queue.put(response)
                
                # チャットへも送信（オプション）
                # self.app.twitch_service.send_message(response) 
                
        except Exception as e:
            logging.error(f"Error in auto-commentary generation: {e}")
            # エラー時は間隔を延ばす（バックオフ）
            self._schedule_next_commentary(interval=self.min_interval * 2)
            return

        # 次回スケジュール
        self._schedule_next_commentary()
