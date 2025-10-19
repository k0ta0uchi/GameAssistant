# -*- coding: utf-8 -*-
from typing import Optional
import os
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

# --- Environment Setup ---
load_dotenv()

GEMINI_MODEL = os.environ.get("GEMINI_MODEL")
GEMINI_PRO_MODEL = os.environ.get("GEMINI_PRO_MODEL")
USER_ID_PRIVATE = os.environ.get("USER_ID_PRIVATE")
USER_ID_PUBLIC = os.environ.get("USER_ID_PUBLIC")

class GeminiSession:
    def __init__(self, custom_instruction: str | None = None, settings_manager=None):
        if not GEMINI_MODEL:
            raise ValueError("GEMINI_MODEL is not set.")

        self.client = get_gemini_client()
        self.settings_manager = settings_manager
        self.disable_thinking_mode = self.settings_manager.get("disable_thinking_mode", False) if self.settings_manager else False
        
        self.history = []
        if custom_instruction:
            self.history.append(types.Content(role='user', parts=[types.Part(text=custom_instruction)]))
            self.history.append(types.Content(role='model', parts=[types.Part(text='はい、承知いたしましただわん。')]))
        
        self.embedding_model = "models/embedding-001"

        self.memory_manager = MemoryManager(collection_name="memories")
        local_summarizer.initialize_llm()

    def generate_content(self, prompt: str, image_path: str | None = None, is_private: bool = True, memory_type: str = 'app', memory_user_id: str | None = None):
        target_user_id = memory_user_id if memory_user_id else (USER_ID_PRIVATE if is_private else USER_ID_PUBLIC)
        if not target_user_id:
            raise ValueError("User ID is not set for the selected privacy level or memory type.")

        thread = threading.Thread(target=self._run_add_memory_in_background, args=(prompt, target_user_id, memory_type))
        thread.start()

        query_embedding_response = self.client.models.embed_content(
            model=self.embedding_model,
            contents=[prompt],
            config=types.EmbedContentConfig(task_type="retrieval_query")
        )
        query_embedding = query_embedding_response.embeddings[0].values # type: ignore
        
        results = self.memory_manager.collection.query(
            query_embeddings=[query_embedding],
            n_results=5,
            where={"$and": [{"type": memory_type}, {"user": target_user_id}]}
        )
        
        documents = results.get('documents') if results else None
        if documents and documents[0]:
            memory_text = "\n".join([doc for doc in documents[0] if doc is not None])
        else:
            memory_text = ""
        memory = local_summarizer.summarize(memory_text) if memory_text else ""

        contents = []
        if image_path:
            try:
                img = Image.open(image_path)
                contents.append(img)
            except FileNotFoundError:
                print(f"Warning: Image file not found at {image_path}")
            except Exception as e:
                print(f"Warning: Failed to load image: {e}")

        prompt_with_memory = f"Previous conversations:\n{memory}\n\nUser: {prompt}\nAI:" if memory else f"User: {prompt}\nAI:"
        
        current_user_parts = []
        if image_path:
            try:
                img = Image.open(image_path)
                buf = BytesIO()
                fmt = getattr(img, "format", None) or "JPEG"
                img.save(buf, format=fmt)
                image_bytes = buf.getvalue()
                mime_type = mimetypes.guess_type(image_path)[0] or f"image/{fmt.lower()}"
                current_user_parts.append(types.Part.from_bytes(data=image_bytes, mime_type=mime_type))
            except FileNotFoundError:
                print(f"Warning: Image file not found at {image_path}")
            except Exception as e:
                print(f"Warning: Failed to load image: {e}")
        
        current_user_parts.append(types.Part(text=prompt_with_memory))

        self.history.append(types.Content(role='user', parts=current_user_parts))

        try:
            thinking_budget = 0 if self.disable_thinking_mode else -1
            config = types.GenerateContentConfig(
                thinking_config=types.ThinkingConfig(thinking_budget=thinking_budget)
            )
            response = self.client.models.generate_content(
                model=GEMINI_MODEL, # type: ignore
                contents=self.history,
                config=config
            )
            response_text = response.text
            
            if response and response.candidates:
                self.history.append(response.candidates[0].content)
            else:
                self.history.append(types.Content(role='model', parts=[types.Part(text="（応答がありませんでした）")]))

            return response_text
        except Exception as e:
            print(f"An error occurred during content generation: {e}")
            return "申し訳ありません、エラーが発生しましただわん。"

    def _run_add_memory_in_background(self, prompt: str, user_id: str, memory_type: str):
        loop = asyncio.new_event_loop()
        asyncio.set_event_loop(loop)
        loop.run_until_complete(self._add_memory_in_background(prompt, user_id, memory_type))

    async def _add_memory_in_background(self, prompt: str, user_id: str, memory_type: str):
        try:
            if not prompt.endswith(('?', '？')) and not any(q in prompt for q in ['何', 'どこ', 'いつ', '誰', 'なぜ']):
                summary = await asyncio.to_thread(local_summarizer.summarize, prompt)
                if summary and "要約できませんでした" not in summary and "エラーが発生しました" not in summary:
                    embedding_response = await asyncio.to_thread(
                        self.client.models.embed_content,
                        model=self.embedding_model,
                        contents=[summary],
                        config=types.EmbedContentConfig(task_type="retrieval_document")
                    )
                    if embedding_response and embedding_response.embeddings:
                        embedding = embedding_response.embeddings[0].values
                    else:
                        print(f"メモリーの保存中にEmbeddingの生成に失敗しました: {prompt}")
                        return

                    self.memory_manager.add_or_update_memory(
                        key=str(uuid.uuid4()),
                        value=summary,
                        type=memory_type,
                        user=user_id
                    )
                    print(f"バックグラウンドでメモリを追加しました: {summary} (type: {memory_type}, user: {user_id})")
        except Exception as e:
            print(f"バックグラウンドでのメモリ追加中にエラーが発生しました: {e}")

    def generate_speech(self, text: str, voice_name: str = 'Laomedeia'):
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
                )
            )
            if (
                response and
                response.candidates and
                response.candidates[0].content and
                response.candidates[0].content.parts and
                response.candidates[0].content.parts[0].inline_data
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
                    history_texts.append(f'role - {content.role}: {part.text}')
        return history_texts


class GeminiService:
    def __init__(self, custom_instruction, settings_manager):
        self.session = GeminiSession(custom_instruction, settings_manager)

    def ask(self, prompt: str, image_path: Optional[str] = None, is_private: bool = False, memory_type: str = 'local', memory_user_id: Optional[str] = None, session_history: Optional[str] = None) -> Optional[str]:
        if not prompt:
            return "プロンプトがありません。"
        if session_history:
            full_prompt = session_history + "\n\n" + prompt
        else:
            full_prompt = prompt
        return self.session.generate_content(full_prompt, image_path, is_private, memory_type, memory_user_id)

    def summarize_session(self, session_history: str) -> Optional[str]:
        prompt = f"以下の会話履歴を要約し、重要な情報のみを抽出してください。\n\n{session_history}"
        try:
            response = self.session.client.models.generate_content(
                model=GEMINI_MODEL, # type: ignore
                contents=prompt,
            )
            return response.text
        except Exception as e:
            print(f"セッションの要約中にエラーが発生しました: {e}")
            return None

    def generate_blog_post(self, conversation: list[dict[str, str]]) -> Optional[str]:
        if not conversation:
            return None

        system_prompt = """
あなたはプロのゲームライターです。
これから提供するユーザーとAIアシスタントの会話履歴を元に、読者の心を掴む魅力的なブログ記事を作成してください。

# 指示
- 会話履歴は、記事を作成するための素材です。会話をそのまま引用するのではなく、あなたの言葉でゲームプレイの状況や感情を生き生きと描写してください。
- 読者がまるでその場でプレイを見ているかのような臨場感あふれる文章を心がけてください。
- 専門用語やゲーム内スラングは、初心者にも分かるように簡単な解説を加えてください。
- ゲームの魅力、面白さ、そしてプレイヤーとアシスタントのやり取りの楽しさが伝わるように、情熱的に書き上げてください。

# 記事の構成
1.  **魅力的なタイトル**: 読者が思わずクリックしたくなるような、キャッチーなタイトルをつけてください。
2.  **導入**: なんのゲームの、どのような状況でのプレイ記録なのかを簡潔に紹介し、読者の興味を引きつけます。
3.  **プレイのハイライト**: 会話履歴を参考に、ゲームプレイ中の最も盛り上がった場面や印象的な出来事を複数取り上げ、詳細に描写します。プレイヤーとアシスタントの面白いやり取りもハイライトしてください。
4.  **ライターによる考察**: ゲームのデザイン、ストーリー、難易度などについて、あなた自身の専門的な視点から考察や感想を述べます。
5.  **まとめ**: 記事全体を締めくくり、読者にゲームへの興味を持たせたり、プレイヤーへの共感を促したりするような、心に残る言葉で結んでください。

# フォーマット
- Markdown形式で記述してください。
- 全体で5000字程度のボリュームにしてください。

それでは、以下の会話履歴を元に、最高のゲームレビュー記事を作成してください。
"""

        conversation_text = "\n".join(f"- {item['role']}: {item['content']}" for item in conversation)
        full_prompt = f"# 会話履歴\n{conversation_text}"

        if not GEMINI_PRO_MODEL:
            return

        try:
            # Gemini Proモデルを明示的に指定
            response = self.session.client.models.generate_content(
                model=GEMINI_PRO_MODEL,
                config=types.GenerateContentConfig(
                    system_instruction=system_prompt),
                contents=full_prompt
            )
            return response.text
        except Exception as e:
            print(f"ブログ記事の生成中にエラーが発生しました: {e}")
            return None

if __name__ == "__main__":
    image_file_path = "screenshot.png"
    if not os.path.exists(image_file_path):
        Image.new("RGB", (100, 100), color=0).save(image_file_path)