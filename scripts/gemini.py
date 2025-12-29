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
GEMINI_EMBEDDING = os.environ.get("GEMINI_EMBEDDING")
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

        self.embedding_model = GEMINI_EMBEDDING

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
            "future": None,  # 結果は待たない
            "data": {
                "prompt": prompt,
                "user_id": target_user_id,
                "memory_type": memory_type,
            },
        }
        self.app.db_save_queue.put(summarize_task)

        # --- DBからの過去の会話履歴の取得（タスク依頼） ---
        # --- DBからの過去の会話履歴の取得（タスク依頼） ---
        query_embedding_response = self.client.models.embed_content(
            model=self.embedding_model,
            contents=[prompt],
            config=types.EmbedContentConfig(
                task_type="retrieval_query",
                output_dimensionality=768
            ),
        )
        query_embedding = query_embedding_response.embeddings[0].values

        query_future = Future()
        query_task = {
            "type": "query",
            "future": query_future,
            "data": {
                "query_embeddings": [query_embedding],
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

        # --- DBからの過去の会話履歴の取得（タスク依頼） ---
        query_embedding_response = self.client.models.embed_content(
            model=self.embedding_model,
            contents=[prompt],
            config=types.EmbedContentConfig(
                task_type="retrieval_query",
                output_dimensionality=768
            ),
        )
        query_embedding = query_embedding_response.embeddings[0].values

        query_future = Future()
        query_task = {
            "type": "query",
            "future": query_future,
            "data": {
                "query_embeddings": [query_embedding],
                "n_results": 5,
                "where": {"$and": [{"type": memory_type}, {"user": target_user_id}]},
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
                contents=f"優しく控えめでオドオドしていて、萌え声でかわいく高く透明感のある声で: {text}",
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
        prompt = f"以下の会話履歴を要約し、重要な情報のみを抽出してください。\n\n{session_history}"
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
        self, conversation: list[dict[str, str]] | str
    ) -> Optional[str]:
        if not conversation:
            return None

        system_prompt = """
あなたはゲーム配信を行っているストリーマーです。
これから提供するユーザーとAIアシスタントの会話履歴を元に、
**自分自身のプレイ体験を振り返る「プレイ日誌（配信ログ）」としてのブログ記事**を作成してください。

このブログは「レビュー」ではなく、
・その日のプレイで何が起きたのか  
・どんな判断をして、何に迷い、どこで盛り上がったのか  
・配信中に考えていたこと、感じたこと  
を、あとから読み返せる記録であり、同時に読者も楽しめる内容であることを目的とします。

---

## 指示

- 会話履歴は**配信中の出来事・思考・やり取りのログ素材**です。  
  そのまま引用せず、ストリーマー本人の語りとして自然な文章に再構成してください。
- 「今この瞬間にプレイしている感覚」「配信画面越しの空気感」が伝わるよう、  
  臨場感・テンポ・感情の揺れを重視してください。
- 専門用語やゲーム内スラングは、  
  *初見リスナーやアーカイブ視聴者*にも伝わるよう、軽く補足説明を入れてください。
- ゲームそのものの面白さだけでなく、  
  **配信という場での選択・失敗・雑談・AIアシスタントとの掛け合い**も重要な見どころとして描写してください。
- 評価や断定よりも、「その時どう感じたか」「なぜそう動いたか」を中心に書いてください。

---

## 記事の構成

1. **タイトル**  
   - 配信タイトル、または配信後に見返したくなるような  
     “その日の象徴的な出来事”を含んだキャッチーなタイトルにしてください。

2. **導入（今日の配信について）**  
   - どのゲームを、どんな目的・進行状況でプレイしていた配信なのか。  
   - 配信前や序盤の空気感、軽い動機づけも含めて書いてください。

3. **プレイ日誌・ハイライト**  
   - 会話履歴を元に、配信中に特に印象に残った場面を時系列で描写します。
   - 操作ミス、判断の迷い、予想外の展開、盛り上がった瞬間などを具体的に。
   - AIアシスタントとのやり取りは、  
     *「一緒に配信している相棒」*のような距離感で自然に組み込んでください。

4. **配信者としての振り返り**  
   - プレイ後に感じたこと、次回への課題や期待。  
   - ゲームデザインや難易度についても、  
     *レビューではなく「配信していてどうだったか」*という視点で述べてください。
   - リスナー目線で「ここは見ていて楽しい／難しい」と感じた点も含めてください。

5. **まとめ（次回につなぐ一言）**  
   - 今日の配信を一言で振り返りつつ、  
     次の配信や続きを匂わせる形で締めくくってください。
   - 読者が「次のアーカイブも見たい」と思える余韻を残してください。

---

## フォーマット

- Markdown形式で記述してください。
- 全体で**約5000字程度**を目安にしてください。
- 文章は一人称（自分視点）で統一してください。

---

それでは、以下の会話履歴を元に、
**ストリーマーのプレイ日誌として最高のブログ記事**を書いてください。
"""

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

        try:
            # Gemini Proモデルを明示的に指定
            response = self.session.client.models.generate_content(
                model=GEMINI_MODEL,
                config=types.GenerateContentConfig(system_instruction=system_prompt),
                contents=full_prompt,
            )
            return response.text
        except Exception as e:
            print(f"ブログ記事の生成中にエラーが発生しました: {e}")
            return None


if __name__ == "__main__":
    image_file_path = "screenshot.png"
    if not os.path.exists(image_file_path):
        Image.new("RGB", (100, 100), color=0).save(image_file_path)
