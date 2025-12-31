# -*- coding: utf-8 -*-
from dataclasses import dataclass, field
from datetime import datetime
from typing import List, Union
import uuid
import os
import threading
import time
import queue
import logging
from scripts.twitch_bot import TwitchService
from twitchio import ChatMessage as TwitchChatMessage
from scripts.record import AudioService, wait_for_keyword
from scripts.whisper import recognize_speech
from scripts.voice import play_random_nod

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
class TranscriptionTask:
    priority: int
    audio_file_path: str
    is_prompt: bool = False
    screenshot_path: str = None

    def __lt__(self, other):
        return self.priority < other.priority

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
        self.transcription_queue = queue.PriorityQueue()
        self._stop_event = threading.Event()

    def is_session_active(self):
        return self.session_running

    def start_session(self):
        logging.info("セッションを開始します。")
        self.session_running = True
        self.session_memory = SessionMemory()
        self.twitch_service.connect_twitch_bot()
        self._stop_event.clear()
        self.recording_thread = threading.Thread(target=self.continuous_recording_thread)
        self.recording_thread.start()
        self.hotword_thread = threading.Thread(target=self.wait_for_hotword_thread)
        self.hotword_thread.start()
        self.transcription_thread = threading.Thread(target=self.transcription_worker)
        self.transcription_thread.start()

    def stop_session(self):
        logging.info("セッションを停止します。")
        self.session_running = False
        self.twitch_service.disconnect_twitch_bot()
        self._stop_event.set()
        if self.session_memory:
            self.session_memory.end_time = datetime.now()
            session_history = self.get_session_history()
            summary = self.app.gemini_service.summarize_session(session_history)
            if summary:
                self.app.memory_manager.add_or_update_memory(self.session_memory.session_id, summary, type='session_summary')

    def handle_twitch_message(self, message: Union[TwitchChatMessage, object]):
        logging.debug(f"handle_twitch_message received: {message}")
        if self.session_memory:
            author_name = ""
            content = ""
            if hasattr(message, 'author') and message.author:
                author_name = message.author.name
            elif hasattr(message, 'chatter') and message.chatter:
                author_name = message.chatter.name

            if hasattr(message, 'content'):
                content = message.content
            elif hasattr(message, 'text'):
                content = message.text
            elif hasattr(message, 'message') and hasattr(message.message, 'text'):
                content = message.message.text

            logging.debug(f"Extracted author: {author_name}, content: {content}")

            if author_name and content:
                event = TwitchMessage(author=author_name, content=content)
                self.session_memory.events.append(event)
                logging.info(f"Twitchメッセージを保存しました: {event}")
                event_data = {
                    'type': 'twitch_chat',
                    'source': author_name,
                    'content': content,
                    'timestamp': event.timestamp.isoformat()
                }
                self.app.db_save_queue.put({'type': 'save', 'data': event_data, 'future': None})

    def continuous_recording_thread(self):
        while not self._stop_event.is_set():
            audio_file = f"temp_recording_{int(time.time())}.wav"
            self.audio_service.record_chunk(30, audio_file, self.app.update_level_meter)
            if self._stop_event.is_set():
                break
            task = TranscriptionTask(priority=10, audio_file_path=audio_file)
            self.transcription_queue.put(task)

    def get_session_history(self):
        if not self.session_memory:
            return ""
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
        if not self.session_memory:
            return []
        
        conversation = []
        for event in self.session_memory.events:
            if isinstance(event, UserSpeech):
                conversation.append({"role": "User", "content": event.content})
            elif isinstance(event, GeminiResponse):
                conversation.append({"role": "Assistant", "content": event.content})
        return conversation

    def wait_for_hotword_thread(self):
        """キーワード待機と、検出後の録音・認識を直列に実行するスレッド"""
        while not self._stop_event.is_set():
            audio_file = f"prompt_recording_{int(time.time())}.wav"
            # ここで録音が完了するまでブロック
            result = wait_for_keyword(
                device_index=self.app.device_index,
                update_callback=self.app.update_level_meter,
                audio_file_path=audio_file,
                stop_event=self._stop_event
            )
            if result and not self._stop_event.is_set():
                play_random_nod()
                logging.info("ホットワード検出後の録音が完了しました。直列で文字起こしを開始します。")
                
                # ウィンドウキャプチャ
                screenshot_path = None
                if self.app.selected_window:
                    screenshot_path = self.app.capture_service.capture_window()
                
                # 文字起こし（同じスレッドで実行）
                text = recognize_speech(audio_file)
                
                # 結果の処理
                self._handle_transcription_result(text, is_prompt=True, audio_file_path=audio_file, screenshot_path=screenshot_path)

    def transcription_worker(self):
        """背景の雑談録音などを処理するワーカースレッド"""
        while not self._stop_event.is_set() or not self.transcription_queue.empty():
            try:
                task = self.transcription_queue.get(timeout=1)
                text = recognize_speech(task.audio_file_path)
                self._handle_transcription_result(text, is_prompt=task.is_prompt, audio_file_path=task.audio_file_path, screenshot_path=task.screenshot_path)
                self.transcription_queue.task_done()
            except queue.Empty:
                continue

    def _handle_transcription_result(self, text, is_prompt, audio_file_path, screenshot_path=None):
        """文字起こし結果をメモリに保存し、必要に応じてAI応答を開始する"""
        if text and text.strip() != "ごめん" and self.session_memory:
            event = UserSpeech(author=self.app.user_name.get(), content=text, is_prompt=is_prompt)
            self.session_memory.events.append(event)
            logging.info(f"音声を保存しました: {event}")
            
            event_data = {
                'type': 'user_speech',
                'source': self.app.user_name.get(),
                'content': text,
                'timestamp': event.timestamp.isoformat()
            }
            self.app.db_save_queue.put({'type': 'save', 'data': event_data, 'future': None})
            
            if is_prompt:
                session_history = self.get_session_history()
                # AI応答処理を開始（これは別スレッドで動く）
                self.app.process_prompt(text, session_history, screenshot_path)
        
        # 音声ファイルの削除
        try:
            if os.path.exists(audio_file_path):
                os.remove(audio_file_path)
        except Exception as e:
            logging.error(f"Error removing audio file: {e}")