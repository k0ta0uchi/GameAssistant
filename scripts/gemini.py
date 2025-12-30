# -*- coding: utf-8 -*-
from typing import Optional, Generator
import os
import re
from dotenv import load_dotenv
from PIL import Image
from google.genai import types
from io import BytesIO
import mimetypes
import wave
from .clients import get_gemini_client
from . import local_summarizer
from .memory import MemoryManager
from .prompts import BLOG_WRITER_SYSTEM_PROMPT, SESSION_SUMMARIZE_PROMPT, TTS_STYLE_INSTRUCTION
import uuid
import google.generativeai as genai
import threading
import asyncio
import time
import logging
from concurrent.futures import Future


def split_into_sentences(
    tokens_generator: Generator[str, None, None],
) -> Generator[str, None, None]:
    """
    ストリーミングトークンを受け取り、文が完成するたびにyieldする。
    """
    buffer = ""
    sentence_endings = r"[。！？\n]"

    for token in tokens_generator:
        buffer += token
        parts = re.split(f"({sentence_endings})", buffer)

        for i in range(0, len(parts) - 1, 2):
            sentence = parts[i] + parts[i + 1]
            if sentence.strip():
                yield sentence.strip()

        buffer = parts[-1]

    if buffer.strip():
        yield buffer.strip()


# --- Environment Setup ---
load_dotenv()

GEMINI_MODEL = os.environ.get("GEMINI_MODEL")
GEMINI_PRO_MODEL = os.environ.get("GEMINI_PRO_MODEL")
USER_ID_PRIVATE = os.environ.get("USER_ID_PRIVATE")
USER_ID_PUBLIC = os.environ.get("USER_ID_PUBLIC")


class GeminiSession:
    def __init__(
        self, app, custom_instruction: str | None = None, settings_manager=None
    ):
        if not GEMINI_MODEL:
            raise ValueError("GEMINI_MODEL is not set.")

        self.app = app
        self.client = get_gemini_client()
        self.settings_manager = settings_manager
        self.disable_thinking_mode = (
            self.settings_manager.get("disable_thinking_mode", False)
            if self.settings_manager
            else False
        )

        self.history = []
        if custom_instruction:
            self.history.append(
                types.Content(role="user", parts=[types.Part(text=custom_instruction)])
            )
            self.history.append(
                types.Content(
                    role="model",
                    parts=[types.Part(text="はい、承知いたしましただわん。")],
                )
            )

        self.memory_manager = MemoryManager(collection_name="memories")
        local_summarizer.initialize_llm()

    def generate_content(
        self,
        prompt: str,
        image_path: str | None = None,
        is_private: bool = True,
        memory_type: str = "app",
        memory_user_id: str | None = None,
    ):
        target_user_id = (
            memory_user_id
            if memory_user_id
            else (USER_ID_PRIVATE if is_private else USER_ID_PUBLIC)
        )
        if not target_user_id:
            raise ValueError(
                "User ID is not set for the selected privacy level or memory type."
            )

        # --- バックグラウンドでの要約・保存タスクをキューに追加 ---
        summarize_task = {
            "type": "summarize_and_save",
            "future": None,
            "data": {
                "prompt": prompt,
                "user_id": target_user_id,
                "memory_type": memory_type,
            },
        }
        self.app.db_save_queue.put(summarize_task)

        # --- DBからの過去の会話履歴の取得（タスク依頼 / ローカルEmbedding） ---
        query_future = Future()
        query_task = {
            "type": "query",
            "future": query_future,
            "data": {
                "query_texts": [prompt],
                "n_results": 5,
                "where": {"$and": [{"type": memory_type}, {"user": target_user_id}]},
            },
        }
        self.app.db_save_queue.put(query_task)

        # DBワーカースレッドからの結果を待つ
        results = query_future.result()

        documents = results.get("documents") if results else None
        if documents and documents[0]:
            memory_text = "\n".join([doc for doc in documents[0] if doc is not None])
        else:
            memory_text = ""
        memory = local_summarizer.summarize(memory_text) if memory_text else ""

        # --- AIへの応答要求 ---
        current_user_parts = []
        if image_path:
            try:
                img = Image.open(image_path)
                buf = BytesIO()
                fmt = getattr(img, "format", None) or "JPEG"
                img.save(buf, format=fmt)
                image_bytes = buf.getvalue()
                mime_type = (
                    mimetypes.guess_type(image_path)[0] or f"image/{fmt.lower()}"
                )
                current_user_parts.append(
                    types.Part.from_bytes(data=image_bytes, mime_type=mime_type)
                )
            except Exception as e:
                print(f"Warning: Failed to load image: {e}")

        prompt_with_memory = (
            f"Previous conversations:\n{memory}\n\nUser: {prompt}\nAI:"
            if memory
            else f"User: {prompt}\nAI:"
        )
        current_user_parts.append(types.Part(text=prompt_with_memory))

        self.history.append(types.Content(role="user", parts=current_user_parts))

        try:
            thinking_budget = 0 if self.disable_thinking_mode else -1
            config = types.GenerateContentConfig(
                thinking_config=types.ThinkingConfig(thinking_budget=thinking_budget)
            )
            response = self.client.models.generate_content(
                model=GEMINI_MODEL, contents=self.history, config=config
            )
            response_text = response.text

            if response and response.candidates:
                self.history.append(response.candidates[0].content)
            else:
                self.history.append(
                    types.Content(
                        role="model",
                        parts=[types.Part(text="（応答がありませんでした）")],
                    )
                )

            return response_text
        except Exception as e:
            print(f"An error occurred during content generation: {e}")
            return "申し訳ありません、エラーが発生しましただわん。"

    def generate_content_stream(
        self,
        prompt: str,
        image_path: str | None = None,
        is_private: bool = True,
        memory_type: str = "app",
        memory_user_id: str | None = None,
    ):
        target_user_id = (
            memory_user_id
            if memory_user_id
            else (USER_ID_PRIVATE if is_private else USER_ID_PUBLIC)
        )
        if not target_user_id:
            raise ValueError(
                "User ID is not set for the selected privacy level or memory type."
            )

        summarize_task = {
            "type": "summarize_and_save",
            "future": None,
            "data": {
                "prompt": prompt,
                "user_id": target_user_id,
                "memory_type": memory_type,
            },
        }
        self.app.db_save_queue.put(summarize_task)

        # --- DBからの過去の会話履歴の取得（タスク依頼 / ローカルEmbedding） ---
        query_future = Future()
        query_task = {
            "type": "query",
            "future": query_future,
            "data": {
                "query_texts": [prompt],
                "n_results": 5,
                "where": {"$and": [{"type": memory_type}, {"user": target_user_id}]}
            },
        }
        self.app.db_save_queue.put(query_task)
        results = query_future.result()

        documents = results.get("documents") if results else None
        if documents and documents[0]:
            memory_text = "\n".join([doc for doc in documents[0] if doc is not None])
        else:
            memory_text = ""
        memory = local_summarizer.summarize(memory_text) if memory_text else ""

        current_user_parts = []
        if image_path:
            try:
                img = Image.open(image_path)
                buf = BytesIO()
                fmt = getattr(img, "format", None) or "JPEG"
                img.save(buf, format=fmt)
                image_bytes = buf.getvalue()
                mime_type = (
                    mimetypes.guess_type(image_path)[0] or f"image/{fmt.lower()}"
                )
                current_user_parts.append(
                    types.Part.from_bytes(data=image_bytes, mime_type=mime_type)
                )
            except Exception as e:
                print(f"Warning: Failed to load image: {e}")

        prompt_with_memory = (
            f"Previous conversations:\n{memory}\n\nUser: {prompt}\nAI:"
            if memory
            else f"User: {prompt}\nAI:"
        )
        current_user_parts.append(types.Part(text=prompt_with_memory))
        self.history.append(types.Content(role="user", parts=current_user_parts))

        try:
            thinking_budget = 0 if self.disable_thinking_mode else -1
            config = types.GenerateContentConfig(
                thinking_config=types.ThinkingConfig(thinking_budget=thinking_budget)
            )

            full_response_text = ""
            for response in self.client.models.generate_content_stream(
                model=GEMINI_MODEL, contents=self.history, config=config
            ):
                chunk_text = response.text
                full_response_text += chunk_text
                yield chunk_text

            self.history.append(
                types.Content(role="model", parts=[types.Part(text=full_response_text)])
            )

        except Exception as e:
            print(f"An error occurred during content generation: {e}")
            yield "申し訳ありません、エラーが発生しましただわん。"

    def generate_speech(self, text: str, voice_name: str = "Laomedeia"):
        try:
            response = self.client.models.generate_content(
                model="gemini-2.5-flash-preview-tts",
                contents=f"{TTS_STYLE_INSTRUCTION}{text}",
                config=types.GenerateContentConfig(
                    response_modalities=["AUDIO"],
                    speech_config=types.SpeechConfig(
                        voice_config=types.VoiceConfig(
                            prebuilt_voice_config=types.PrebuiltVoiceConfig(
                                voice_name=voice_name,
                            )
                        )
                    ),
                ),
            )
            if (
                response
                and response.candidates
                and response.candidates[0].content
                and response.candidates[0].content.parts
                and response.candidates[0].content.parts[0].inline_data
            ):
                return response.candidates[0].content.parts[0].inline_data.data
            else:
                print("Speech generation response is empty or invalid.")
                return None
        except Exception as e:
            print(f"An error occurred during speech generation: {e}")
            return None

    def get_history(self):
        history_texts = []
        if self.history:
            for content in self.history:
                for part in content.parts:
                    history_texts.append(f"role - {content.role}: {part.text}")
        return history_texts


class GeminiService:
    def __init__(self, app, custom_instruction, settings_manager):
        self.session = GeminiSession(app, custom_instruction, settings_manager)

    def ask(
        self,
        prompt: str,
        image_path: Optional[str] = None,
        is_private: bool = False,
        memory_type: str = "local",
        memory_user_id: Optional[str] = None,
        session_history: Optional[str] = None,
    ) -> Optional[str]:
        if not prompt:
            return "プロンプトがありません。"
        full_prompt = (session_history + "\n\n" + prompt) if session_history else prompt
        return self.session.generate_content(
            full_prompt, image_path, is_private, memory_type, memory_user_id
        )

    def ask_stream(
        self,
        prompt: str,
        image_path: Optional[str] = None,
        is_private: bool = False,
        memory_type: str = "local",
        memory_user_id: Optional[str] = None,
        session_history: Optional[str] = None,
    ):
        if not prompt:
            yield "プロンプトがありません。"
            return
        full_prompt = (session_history + "\n\n" + prompt) if session_history else prompt
        yield from self.session.generate_content_stream(
            full_prompt, image_path, is_private, memory_type, memory_user_id
        )

    def summarize_session(self, session_history: str) -> Optional[str]:
        prompt = f"{SESSION_SUMMARIZE_PROMPT}{session_history}"
        try:
            response = self.session.client.models.generate_content(
                model=GEMINI_MODEL,
                contents=prompt,
            )
            return response.text
        except Exception as e:
            print(f"セッションの要約中にエラーが発生しました: {e}")
            return None

    def generate_blog_post(
        self,
        conversation: list[dict[str, str]] | str
    ) -> Optional[str]:
        if not conversation:
            return None

        system_prompt = BLOG_WRITER_SYSTEM_PROMPT

        if isinstance(conversation, str):
            conversation_text = conversation
        elif isinstance(conversation, list):
            conversation_text = "\n".join(
                f"- {item['role']}: {item['content']}" for item in conversation
            )
        else:
            logging.error(f"予期しない会話の型: {type(conversation)}")
            return None

        full_prompt = f"# 会話履歴\n{conversation_text}"

        if not GEMINI_MODEL:
            return

        max_retries = 5
        base_delay = 10 # 最初の待機時間（秒）

        for attempt in range(max_retries):
            try:
                logging.info(f"ブログ記事の生成を試行中... (試行 {attempt + 1}/{max_retries})")
                response = self.session.client.models.generate_content(
                    model=GEMINI_MODEL,
                    config=types.GenerateContentConfig(system_instruction=system_prompt),
                    contents=full_prompt,
                )
                if response and response.text:
                    return response.text
                else:
                    logging.warning("ブログ記事の生成応答が空でした。")
            except Exception as e:
                # 429 Too Many Requests やその他のエラーをキャッチ
                error_msg = str(e)
                if "429" in error_msg or "Too Many Requests" in error_msg.lower() or "ResourceExhausted" in error_msg:
                    delay = base_delay * (2 ** attempt) # 指数バックオフ
                    logging.warning(f"レート制限(429)を検出しました。{delay}秒後に再試行します... エラー: {e}")
                    time.sleep(delay)
                else:
                    logging.error(f"ブログ記事の生成中に致命的なエラーが発生しました: {e}", exc_info=True)
                    break # 429以外はリトライせずに終了

        logging.error("最大リトライ回数に達したため、ブログ記事の生成を断念しました。")
        return None


if __name__ == "__main__":
    image_file_path = "screenshot.png"
    if not os.path.exists(image_file_path):
        Image.new("RGB", (100, 100), color=0).save(image_file_path)
