# -*- coding: utf-8 -*-
from dataclasses import dataclass, field
from datetime import datetime
from typing import List, Union
import uuid
import threading
import logging
import re
import time
from scripts.twitch_bot import TwitchService
from twitchio import ChatMessage as TwitchChatMessage
from scripts.record import AudioService
from scripts.streaming_whisper import StreamTranscriber
from scripts.voice import play_random_nod
import scripts.voice as voice
from scripts.auto_commentary import AutoCommentaryService

@dataclass
class TwitchMessage:
    author: str
    content: str
    event_id: str = field(default_factory=lambda: str(uuid.uuid4()))
    timestamp: datetime = field(default_factory=datetime.now)

@dataclass
class UserSpeech:
    author: str
    content: str
    is_prompt: bool = False
    priority: int = 10
    event_id: str = field(default_factory=lambda: str(uuid.uuid4()))
    timestamp: datetime = field(default_factory=datetime.now)

@dataclass
class GeminiResponse:
    content: str
    event_id: str = field(default_factory=lambda: str(uuid.uuid4()))
    timestamp: datetime = field(default_factory=datetime.now)

@dataclass
class SessionMemory:
    session_id: str = field(default_factory=lambda: str(uuid.uuid4()))
    start_time: datetime = field(default_factory=datetime.now)
    end_time: datetime = None
    events: List[Union[TwitchMessage, UserSpeech, GeminiResponse]] = field(default_factory=list)

class SessionManager:
    def __init__(self, app, twitch_service):
        self.app = app
        self.session_running = False
        self.session_memory = None
        self.twitch_service = twitch_service
        self.twitch_service.message_callback = self.handle_twitch_message
        
        self.audio_service = AudioService(app)
        
        # 初期状態ではエンジンを作成せず、start_session時に作成する
        self.transcriber = None
        self.asr_engine_type = None
        
        self.auto_commentary_service = AutoCommentaryService(app, self)

        self._stop_event = threading.Event()
        
        # プロンプト処理用の状態管理
        self.is_collecting_prompt = False
        self.prompt_cooldown_until = 0.0 # この時刻まではプロンプトとして受け付けない

    def is_session_active(self):
        return self.session_running

    def start_session(self):
        logging.info("セッション（ハイブリッド認識）を開始します。")
        try:
            # 既存のtranscriberがあれば確実に停止する
            if self.transcriber:
                logging.info("Stopping existing transcriber instance...")
                try:
                    self.transcriber.stop()
                except Exception as e:
                    logging.warning(f"Error stopping old transcriber: {e}")
                self.transcriber = None

            self.session_running = True
            self.session_memory = SessionMemory()
            logging.debug("SessionMemory initialized.")
            
            # 設定からエンジンを選択して作成
            self.asr_engine_type = self.app.asr_engine.get()
            logging.info(f"Using ASR Engine Mode: {self.asr_engine_type}")
            
            model_size = "kotoba-tech/kotoba-whisper-v2.0-faster" # Default (Large)
            if self.asr_engine_type == "tiny":
                # 軽量モデル (Faster-Whisper Tiny)
                model_size = "tiny"
                logging.info("Selected Tiny model for lightweight performance.")
            
            self.transcriber = StreamTranscriber(
                model_size=model_size,
                compute_type="int8"
            )

            logging.debug("Starting Twitch connection...")
            self.twitch_service.connect_twitch_bot()
            
            logging.debug(f"Starting ASR Engine ({model_size})...")
            self.transcriber.start(self._on_transcription_result)
            
            self.audio_service.add_listener(self.transcriber.add_audio)
            
            logging.debug("Starting AudioService stream...")
            self.audio_service.start_stream(
                wake_word_callback=self._on_wake_word,
                stop_word_callback=self._on_stop_word
            )
            
            # 自立型ツッコミサービスの開始
            self.auto_commentary_service.start()
            
            logging.info("セッション開始処理が完了しました。")
        except Exception as e:
            logging.error(f"セッション開始中にエラーが発生しました: {e}", exc_info=True)
            self.stop_session()

    def stop_session(self):
        logging.info("セッションを停止します。")
        
        # 自立型ツッコミサービスの停止
        if hasattr(self, 'auto_commentary_service'):
            self.auto_commentary_service.stop()

        self.session_running = False
        self.twitch_service.disconnect_twitch_bot()
        
        self.audio_service.stop_stream()
        
        if self.transcriber:
            self.audio_service.remove_listener(self.transcriber.add_audio)
            self.transcriber.stop()
            self.transcriber = None
        
        if self.session_memory:
            self.session_memory.end_time = datetime.now()
            session_history = self.get_session_history()
            summary = self.app.gemini_service.summarize_session(session_history)
            if summary:
                self.app.memory_manager.add_or_update_memory(self.session_memory.session_id, summary, type='session_summary')

    def _on_wake_word(self):
        """Porcupineが「ねえぐり」を検知した時の処理"""
        logging.info("【Porcupine】ウェイクワード検知！プロンプト待機モードへ移行します。")
        # 頷き音を別スレッドで再生
        threading.Thread(target=voice.play_random_nod, daemon=True).start()
        self.is_collecting_prompt = True
        
        # 検知から1.5秒間は、直前のノイズや「ねえぐり」自身の残響を拾わないように無視する
        self.prompt_cooldown_until = time.time() + 1.5
        
        if self.app.selected_window:
            self.app.cached_screenshot = self.app.capture_service.capture_window()

    def _on_stop_word(self):
        """Porcupineが「ストップ」を検知した時の処理"""
        logging.info("【Porcupine】ストップワード検知！再生を中断します。")
        voice.request_stop_playback()

    def _on_transcription_result(self, text, is_final):
        """Whisperからの認識結果"""
        if not text: return

        # UIへのリアルタイム表示
        self.app.root.after(0, lambda: self.app.update_asr_display(text, is_final))

        if not is_final:
            return

        logging.info(f"[ASR Final] {text}")
        
        # オートコメンタリーのタイマーをリセット
        if hasattr(self, 'auto_commentary_service'):
            self.auto_commentary_service.notify_activity()

        # プロンプト待機モード中の場合
        if self.is_collecting_prompt:
            # クールダウン中かチェック（Nod音声の誤認識防止）
            if time.time() < self.prompt_cooldown_until:
                logging.info(f"クールダウン中のため無視（待機継続）: {text}")
                return

            # 空文字や極端に短いノイズを無視
            if len(text.strip()) <= 1:
                logging.info(f"テキストが短すぎるため無視（待機継続）: {text}")
                return

            logging.info(f"プロンプトとして処理: {text}")
            self._process_as_prompt(text)
            self.is_collecting_prompt = False
            return

        # 通常の会話ログとして保存
        self._save_user_speech(text, is_prompt=False)

    def _process_as_prompt(self, text):
        """テキストをプロンプトとしてAIに送信する"""
        logging.info(f"AIへのプロンプトを検出: {text}")
        # ユーザーの発話を受け取った合図として頷き音を再生
        threading.Thread(target=voice.play_random_nod, daemon=True).start()
        
        self._save_user_speech(text, is_prompt=True)
        
        screenshot_path = getattr(self.app, 'cached_screenshot', None)
        if not screenshot_path and self.app.selected_window:
            screenshot_path = self.app.capture_service.capture_window()
        self.app.cached_screenshot = None
        
        session_history = self.get_session_history()
        self.app.process_prompt(text, session_history, screenshot_path)

    def _save_user_speech(self, text, is_prompt):
        if not self.session_memory: return
        
        event = UserSpeech(author=self.app.user_name.get(), content=text, is_prompt=is_prompt)
        self.session_memory.events.append(event)
        
        event_data = {
            'type': 'user_speech',
            'source': self.app.user_name.get(),
            'content': text,
            'timestamp': event.timestamp.isoformat()
        }
        self.app.db_save_queue.put({'type': 'save', 'data': event_data, 'future': None})

    def handle_twitch_message(self, message: Union[TwitchChatMessage, object]):
        if self.session_memory:
            author_name = getattr(message, 'author', getattr(message, 'chatter', None))
            if author_name: author_name = author_name.name
            content = getattr(message, 'content', getattr(message, 'text', ""))
            if author_name and content:
                # オートコメンタリーのタイマーをリセット
                if hasattr(self, 'auto_commentary_service'):
                    self.auto_commentary_service.notify_activity()
                
                event = TwitchMessage(author=author_name, content=content)
                self.session_memory.events.append(event)
                self.app.db_save_queue.put({'type': 'save', 'data': {
                    'type': 'twitch_chat', 'source': author_name, 'content': content, 'timestamp': event.timestamp.isoformat()
                }, 'future': None})

    def get_session_history(self):
        if not self.session_memory: return ""
        history = ""
        for event in self.session_memory.events:
            if isinstance(event, TwitchMessage):
                history += f"Twitch ({event.author}): {event.content}\n"
            elif isinstance(event, UserSpeech):
                history += f"{event.author}: {event.content}\n"
            elif isinstance(event, GeminiResponse):
                history += f"Assistant: {event.content}\n"
        return history

    def get_session_conversation(self) -> list[dict[str, str]]:
        if not self.session_memory: return []
        conversation = []
        for event in self.session_memory.events:
            if isinstance(event, UserSpeech):
                conversation.append({"role": "User", "content": event.content})
            elif isinstance(event, GeminiResponse):
                conversation.append({"role": "Assistant", "content": event.content})
        return conversation
