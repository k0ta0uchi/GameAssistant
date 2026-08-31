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
from scripts.record import AudioService, DiscordAudioService
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
class DiscordSpeech:
    author: str = "Discord"
    content: str = ""
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
    events: List[Union[TwitchMessage, UserSpeech, DiscordSpeech, GeminiResponse]] = field(default_factory=list)

class SessionManager:
    def __init__(self, app, twitch_service):
        self.app = app
        self.session_running = False
        self.session_memory = None
        self.twitch_service = twitch_service
        self.twitch_service.message_callback = self.handle_twitch_message
        
        self.audio_service = AudioService(app)
        self.discord_audio_service = DiscordAudioService(app)
        
        # 初期状態ではエンジンを作成せず、start_session時に作成する
        self.transcriber = None
        self.discord_transcriber = None
        self.asr_engine_type = None
        
        self.auto_commentary_service = AutoCommentaryService(app, self)

        self._stop_event = threading.Event()
        
        # プロンプト処理用の状態管理
        self.is_collecting_prompt = False
        self.prompt_cooldown_until = 0.0 # この時刻まではプロンプトとして受け付けない
        self.last_wake_trigger_time = 0.0 # ウェイクワード二重連動防止タイマー

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
            self.asr_engine_type = self.app.state.asr_engine.get()
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
            
            # Discord 音声キャプチャ＆文字起こし開始 (オプション)
            if hasattr(self.app, 'state') and hasattr(self.app.state, 'enable_discord_capture') and self.app.state.enable_discord_capture.get():
                logging.debug("Starting Discord ASR Engine (Shared Model)...")
                self.discord_transcriber = StreamTranscriber(
                    shared_model=self.transcriber.model
                )
                self.discord_transcriber.start(self._on_discord_transcription_result)
                self.discord_audio_service.add_listener(self.discord_transcriber.add_audio)
                self.discord_audio_service.start_stream()

            # 自立型ツッコミサービスの開始
            self.auto_commentary_service.start()
            
            # Whisper監視ウォッチドッグの開始
            self._start_watchdog()
            
            logging.info("セッション開始処理が完了しました。")
        except Exception as e:
            logging.error(f"セッション開始中にエラーが発生しました: {e}", exc_info=True)
            self.stop_session()

    def _start_watchdog(self):
        self.watchdog_thread = threading.Thread(target=self._watchdog_loop, daemon=True)
        self.watchdog_thread.start()

    def _watchdog_loop(self):
        while self.session_running:
            time.sleep(5)
            if not self.session_running:
                break
            if self.transcriber:
                try:
                    if not self.transcriber.is_healthy():
                        logging.warning("🚨 Mic Whisper Watchdog detected unhealthy Transcriber! Auto-restarting...")
                        self.restart_whisper()
                except Exception as e:
                    logging.error(f"Mic Watchdog check error: {e}")
            if self.discord_transcriber:
                try:
                    if not self.discord_transcriber.is_healthy():
                        logging.warning("🚨 Discord Whisper Watchdog detected unhealthy Transcriber! Auto-restarting...")
                        self.restart_whisper()
                except Exception as e:
                    logging.error(f"Discord Watchdog check error: {e}")

    def restart_whisper(self):
        """Whisper (StreamTranscriber) を手動/自動で安全に再起動する"""
        if not self.session_running:
            logging.info("セッション未開始のため、Whisperの再起動をスキップします。")
            return
        
        logging.warning("🔄 Whisper Transcriber を再起動しています...")
        try:
            # 1. マイク用Transcriberの停止
            if self.transcriber:
                self.audio_service.remove_listener(self.transcriber.add_audio)
                try:
                    self.transcriber.stop()
                except Exception as e:
                    logging.warning(f"Error stopping old mic transcriber: {e}")
                self.transcriber = None

            # 2. Discord用Transcriberの停止
            if self.discord_transcriber:
                self.discord_audio_service.remove_listener(self.discord_transcriber.add_audio)
                try:
                    self.discord_transcriber.stop()
                except Exception as e:
                    logging.warning(f"Error stopping old discord transcriber: {e}")
                self.discord_transcriber = None

            model_size = "kotoba-tech/kotoba-whisper-v2.0-faster"
            if self.asr_engine_type == "tiny":
                model_size = "tiny"
            
            # マイク用Transcriber再起動
            self.transcriber = StreamTranscriber(
                model_size=model_size,
                compute_type="int8"
            )
            self.transcriber.start(self._on_transcription_result)
            self.audio_service.add_listener(self.transcriber.add_audio)

            # Discord用Transcriber再起動（モデル共有）
            if hasattr(self.app, 'state') and hasattr(self.app.state, 'enable_discord_capture') and self.app.state.enable_discord_capture.get():
                self.discord_transcriber = StreamTranscriber(
                    shared_model=self.transcriber.model
                )
                self.discord_transcriber.start(self._on_discord_transcription_result)
                self.discord_audio_service.add_listener(self.discord_transcriber.add_audio)
                if not self.discord_audio_service.is_running:
                    self.discord_audio_service.start_stream()

            logging.info("✅ Whisper Transcriber の再起動に成功しました。")
        except Exception as e:
            logging.error(f"Whisper 再起動エラー: {e}", exc_info=True)

    def stop_session(self):
        logging.info("セッションを停止します。")
        
        # 自立型ツッコミサービスの停止
        if hasattr(self, 'auto_commentary_service'):
            self.auto_commentary_service.stop()

        self.session_running = False
        self.twitch_service.disconnect_twitch_bot()
        
        self.audio_service.stop_stream()
        self.discord_audio_service.stop_stream()
        
        if self.transcriber:
            self.audio_service.remove_listener(self.transcriber.add_audio)
            self.transcriber.stop()
            self.transcriber = None

        if self.discord_transcriber:
            self.discord_audio_service.remove_listener(self.discord_transcriber.add_audio)
            self.discord_transcriber.stop()
            self.discord_transcriber = None
        
        if self.session_memory:
            self.session_memory.end_time = datetime.now()
            session_history = self.get_session_history()
            summary = self.app.gemini_service.summarize_session(session_history)
            if summary:
                self.app.memory_manager.add_or_update_memory(self.session_memory.session_id, summary, type='session_summary')

    def _on_wake_word(self):
        """openwakewordが「ねえぐり」を検知した時の処理"""
        logging.info("【openwakeword】ウェイクワード検知！プロンプト待機モードへ移行します。")
        # 頷き音を別スレッドで再生
        threading.Thread(target=voice.play_random_nod, daemon=True).start()
        self.is_collecting_prompt = True
        
        # 検知から1.5秒間は、直前のノイズや「ねえぐり」自身の残響を拾わないように無視する
        self.prompt_cooldown_until = time.time() + 1.5
        
        if self.app.state.current_window:
            self.app.state.cached_screenshot = self.app.capture_service.capture_window()

    def _on_stop_word(self):
        """openwakewordが「ストップ」を検知した時の処理"""
        logging.info("【openwakeword】ストップワード検知！再生を中断します。")
        voice.request_stop_playback()

    def _on_transcription_result(self, text, is_final):
        """Whisperからの認識結果"""
        if not text: return

        # UIへのリアルタイム表示
        self.app.root.after(0, lambda: self.app.update_asr_display(text, is_final))

        # VAD + Whisper ASR 即時ウェイクワード判定 ("ねえぐり", "ねぐり" 等の検出)
        wake_words = ["ねえぐり", "ねぐり", "ネグリ", "ねーぐり", "ねぇぐり", "ね〜ぐり", "neguri"]
        cleaned_text = re.sub(r'[\s\u3000\.,\?！!\-ー]', '', text.lower())

        if not self.is_collecting_prompt:
            if any(kw in cleaned_text for kw in wake_words):
                now = time.time()
                if (now - self.last_wake_trigger_time) > 2.0:
                    self.last_wake_trigger_time = now
                    logging.info(f"【VAD+Whisper】'ねえぐり'を即時検知しました！ (ASR Text: '{text}')")
                    self._on_wake_word()
                    return

        if not is_final:
            return

        logging.info(f"[ASR Final] {text}")
        
        # アクティビティ通知（自動ツッコミタイマーのリセット）
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

    def _on_discord_transcription_result(self, text, is_final):
        """Discord音声からの認識結果"""
        if not text: return

        # UIへのリアルタイム表示
        self.app.root.after(0, lambda: self.app.update_asr_display(f"[Discord] {text}", is_final))

        if not is_final:
            return

        logging.info(f"[Discord ASR Final] {text}")
        
        # アクティビティ通知（自動ツッコミタイマーのリセット）
        if hasattr(self, 'auto_commentary_service'):
            self.auto_commentary_service.notify_activity()

        # Discord会話ログ・メモリとして保存
        self._save_discord_speech(text)

    def _save_discord_speech(self, text):
        if not self.session_memory: return
        
        event = DiscordSpeech(author="Discord", content=text)
        self.session_memory.events.append(event)
        
        event_data = {
            'type': 'discord_speech',
            'source': 'Discord',
            'content': text,
            'timestamp': event.timestamp.isoformat()
        }
        self.app.memory_manager.enqueue_save(event_data)

    def _process_as_prompt(self, text):
        """テキストをプロンプトとしてAIに送信する"""
        logging.info(f"AIへのプロンプトを検出: {text}")
        # ユーザーの発話を受け取った合図として頷き音を再生
        threading.Thread(target=voice.play_random_nod, daemon=True).start()
        
        self._save_user_speech(text, is_prompt=True)
        
        screenshot_path = self.app.state.cached_screenshot
        if not screenshot_path and self.app.state.current_window:
            screenshot_path = self.app.capture_service.capture_window()
        self.app.state.cached_screenshot = None
        
        session_history = self.get_session_history()
        self.app.process_prompt(text, session_history, screenshot_path)

    def _save_user_speech(self, text, is_prompt):
        if not self.session_memory: return
        
        event = UserSpeech(author=self.app.state.user_name.get(), content=text, is_prompt=is_prompt)
        self.session_memory.events.append(event)
        
        event_data = {
            'type': 'user_speech',
            'source': self.app.state.user_name.get(),
            'content': text,
            'timestamp': event.timestamp.isoformat()
        }
        self.app.memory_manager.enqueue_save(event_data)

    def handle_twitch_message(self, message: Union[TwitchChatMessage, object]):
        if self.session_memory:
            author_name = getattr(message, 'author', getattr(message, 'chatter', None))
            if author_name: author_name = author_name.name
            content = getattr(message, 'content', getattr(message, 'text', ""))
            if author_name and content:
                event = TwitchMessage(author=author_name, content=content)
                self.session_memory.events.append(event)
                self.app.memory_manager.enqueue_save({
                    'type': 'twitch_chat', 'source': author_name, 'content': content, 'timestamp': event.timestamp.isoformat()
                })

    def get_session_history(self):
        if not self.session_memory: return ""
        history = ""
        for event in self.session_memory.events:
            if isinstance(event, TwitchMessage):
                history += f"Twitch ({event.author}): {event.content}\n"
            elif isinstance(event, UserSpeech):
                history += f"{event.author}: {event.content}\n"
            elif isinstance(event, DiscordSpeech):
                history += f"Discord: {event.content}\n"
            elif isinstance(event, GeminiResponse):
                history += f"Assistant: {event.content}\n"
        return history

    def get_session_conversation(self) -> list[dict[str, str]]:
        if not self.session_memory: return []
        conversation = []
        for event in self.session_memory.events:
            if isinstance(event, UserSpeech):
                conversation.append({"role": "User", "content": event.content})
            elif isinstance(event, DiscordSpeech):
                conversation.append({"role": "Discord", "content": event.content})
            elif isinstance(event, GeminiResponse):
                conversation.append({"role": "Assistant", "content": event.content})
        return conversation
