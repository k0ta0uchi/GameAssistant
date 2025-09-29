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

    async def handle_twitch_message(self, message: Union[TwitchChatMessage, object]):
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
                logging.debug("Calling save_event_to_chroma for twitch_chat...")
                event_data = {
                    'type': 'twitch_chat',
                    'source': author_name,
                    'content': content,
                    'timestamp': event.timestamp.isoformat()
                }
                await self.app.memory_manager.save_event_to_chroma(event_data)

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
            logging.debug("get_session_history - セッションメモリがありません。")
            return ""
        history = ""
        for event in self.session_memory.events:
            if isinstance(event, TwitchMessage):
                history += f"Twitch ({event.author}): {event.content}\n"
            elif isinstance(event, UserSpeech):
                history += f"{event.author}: {event.content}\n"
            elif isinstance(event, GeminiResponse):
                history += f"Assistant: {event.content}\n"
        
        logging.debug("--- get_session_history ---")
        logging.debug(history)
        logging.debug("------------------------------------")
        return history

    def wait_for_hotword_thread(self):
        while not self._stop_event.is_set():
            result = wait_for_keyword(
                device_index=self.app.device_index,
                update_callback=self.app.update_level_meter,
                audio_file_path=self.app.audio_file_path,
                stop_event=self._stop_event
            )
            if result:
                play_random_nod()
                logging.info("ホットワードを検出しました。プロンプトの録音を開始します。")
                screenshot_path = self.app.capture_service.capture_window()
                task = TranscriptionTask(priority=1, audio_file_path=self.app.audio_file_path, is_prompt=True, screenshot_path=screenshot_path)
                self.transcription_queue.put(task)

    def transcription_worker(self):
        while not self._stop_event.is_set() or not self.transcription_queue.empty():
            try:
                task = self.transcription_queue.get(timeout=1)
                text = recognize_speech(task.audio_file_path)
                if text and text.strip() != "ごめん" and self.session_memory:
                    event = UserSpeech(author=self.app.user_name.get(), content=text, is_prompt=task.is_prompt)
                    self.session_memory.events.append(event)
                    logging.info(f"音声を保存しました: {event}")
                    event_data = {
                        'type': 'user_speech',
                        'source': self.app.user_name.get(),
                        'content': text,
                        'timestamp': event.timestamp.isoformat()
                    }
                    if self.app.twitch_service.twitch_bot_loop:
                        import asyncio
                        asyncio.run_coroutine_threadsafe(
                            self.app.memory_manager.save_event_to_chroma(event_data),
                            self.app.twitch_service.twitch_bot_loop
                        )
                    if task.is_prompt:
                        session_history = self.get_session_history()
                        self.app.process_prompt(text, session_history, task.screenshot_path)
                # Clean up the audio file
                try:
                    if os.path.exists(task.audio_file_path):
                        os.remove(task.audio_file_path)
                except Exception as e:
                    logging.error(f"Error removing audio file: {e}", exc_info=True)
                self.transcription_queue.task_done()
            except queue.Empty:
                continue

