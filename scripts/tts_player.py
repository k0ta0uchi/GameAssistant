# -*- coding: utf-8 -*-
import threading
import queue
import logging
import re
import time
import scripts.voice as voice

class TTSManager:
    """
    音声合成(TTS)と再生のキュー管理、およびバックグラウンド実行を担うクラス。
    gui/app.py から重いロジックを抽出。
    """
    def __init__(self, on_playback_start=None, on_playback_end=None):
        self.tts_queue = queue.Queue()
        self.playback_queue = queue.Queue()
        
        # コールバック (UI更新用)
        self.on_playback_start = on_playback_start
        self.on_playback_end = on_playback_end
        
        self.is_running = False
        self.threads = []

    def start(self):
        """ワーカー開始"""
        if self.is_running:
            return
        self.is_running = True
        
        t1 = threading.Thread(target=self._synthesis_worker, daemon=True, name="TTS-Synthesis")
        t2 = threading.Thread(target=self._playback_worker, daemon=True, name="TTS-Playback")
        
        self.threads = [t1, t2]
        for t in self.threads:
            t.start()
        logging.info("TTSManager workers started.")

    def stop(self):
        """ワーカー停止"""
        self.is_running = False
        self.tts_queue.put(None)
        self.playback_queue.put(None)
        # 進行中のキューをクリア
        while not self.tts_queue.empty():
            try: self.tts_queue.get_nowait()
            except queue.Empty: break
        while not self.playback_queue.empty():
            try: self.playback_queue.get_nowait()
            except queue.Empty: break
        logging.info("TTSManager workers stopping.")

    def put_text(self, text):
        """合成待ちキューにテキストを投入"""
        if text:
            self.tts_queue.put(text)

    def clear_queues(self):
        """再生待ちを中断しキューを空にする"""
        voice.stop_playback_event.set()
        while not self.tts_queue.empty():
            try: self.tts_queue.get_nowait()
            except queue.Empty: break
        while not self.playback_queue.empty():
            try: self.playback_queue.get_nowait()
            except queue.Empty: break
        # 少し待ってからリセット
        time.sleep(0.1)
        voice.stop_playback_event.clear()

    def _synthesis_worker(self):
        """文を音声データに変換する（先行合成）スレッド"""
        while self.is_running:
            item = self.tts_queue.get()
            if item is None: break
            
            if item == "END_MARKER":
                self.playback_queue.put("END_MARKER")
                self.tts_queue.task_done()
                continue

            # 長文分割ロジック
            sentences = [s.strip() for s in re.split(r'([、,])', item) if s.strip()] if len(item) > 100 else [item]
            
            for sub in sentences:
                try:
                    if voice.stop_playback_event.is_set(): break
                    logging.debug(f"Synthesis starting: {sub[:20]}...")
                    wav_data = voice.generate_speech_data(sub)
                    if wav_data:
                        self.playback_queue.put(wav_data)
                except Exception as e:
                    logging.error(f"TTS Synthesis error: {e}")
            
            self.tts_queue.task_done()

    def _playback_worker(self):
        """合成済み音声を順次再生するスレッド"""
        while self.is_running:
            item = self.playback_queue.get()
            if item is None: break
            
            if item == "END_MARKER":
                if self.on_playback_end:
                    self.on_playback_end(is_final=True)
                self.playback_queue.task_done()
                continue

            wav_data = item
            try:
                if not voice.stop_playback_event.is_set():
                    if self.on_playback_start:
                        self.on_playback_start()
                    
                    # 音量調整込みで再生
                    voice.play_wav_data(wav_data, volume=0.5)
            except Exception as e:
                logging.error(f"TTS Playback error: {e}")
            finally:
                self.playback_queue.task_done()
                # 連続再生中の「文の間」では end(final=False) を呼ぶ必要があればここ
