# -*- coding: utf-8 -*-
import os
from dotenv import load_dotenv
from PIL import Image
from google.genai import types
import wave
from .clients import get_gemini_client # get_chroma_clientはMemoryManager内で使用するため不要に
from . import local_summarizer
from .memory import MemoryManager
import uuid
import google.generativeai as genai
import threading

# --- Environment Setup ---
load_dotenv()

GEMINI_MODEL = os.environ.get("GEMINI_MODEL")
USER_ID_PRIVATE = os.environ.get("USER_ID_PRIVATE")
USER_ID_PUBLIC = os.environ.get("USER_ID_PUBLIC")

import asyncio

class GeminiSession:
    def __init__(self, custom_instruction: str | None = None):
        if not GEMINI_MODEL:
            raise ValueError("GEMINI_MODEL is not set.")

        self.client = get_gemini_client()
        
        # --- Chat Session Initialization ---
        history = []
        if custom_instruction:
            history.append({'role': 'user', 'parts': [{'text': custom_instruction}]})
            history.append({'role': 'model', 'parts': [{'text': 'はい、承知いたしましただわん。'}]})
        
        self.chat = self.client.chats.create(model=GEMINI_MODEL, history=history)
        self.embedding_model = "models/embedding-001"

        # --- Memory Manager and Summarizer Initialization ---
        self.memory_manager = MemoryManager(collection_name="memories")
        local_summarizer.initialize_llm()

    async def generate_content(self, prompt: str, image_path: str | None = None, is_private: bool = True, memory_type: str = 'app', memory_user_id: str | None = None):
        # メモリ保存用のユーザーIDを決定
        # memory_user_idが明示的に渡された場合はそれを優先し、そうでなければ環境変数から取得
        target_user_id = memory_user_id if memory_user_id else (USER_ID_PRIVATE if is_private else USER_ID_PUBLIC)
        if not target_user_id:
            raise ValueError("User ID is not set for the selected privacy level or memory type.")

        # メモリ保存処理を非同期タスクとして実行
        asyncio.create_task(self._add_memory_in_background(prompt, target_user_id, memory_type))

        # クエリのEmbeddingを生成
        query_embedding_response = self.client.models.embed_content(
            model=self.embedding_model,
            contents=[prompt],
            config=types.EmbedContentConfig(task_type="retrieval_query")
        )
        query_embedding = query_embedding_response.embeddings[0].values # type: ignore
        
        # 関連性の高い会話を検索
        # 注意: ここでは target_user_id を metadatas の 'user' として扱う
        # また、memory_type で指定されたタイプのみを検索対象とする
        results = self.memory_manager.collection.query(
            query_embeddings=[query_embedding],
            n_results=5,
            where={"$and": [{"type": memory_type}, {"user": target_user_id}]}
        )
        
        # 検索結果を要約
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
        contents.append(prompt_with_memory)

        try:
            response = self.chat.send_message(contents)
            return response.text
        except Exception as e:
            print(f"An error occurred during content generation: {e}")
            return "申し訳ありません、エラーが発生しましただわん。"



    async def _add_memory_in_background(self, prompt: str, user_id: str, memory_type: str):
        """（バックグラウンド処理）メモリへの追加を行う"""
        try:
            # 疑問形でない場合のみメモリに保存
            if not prompt.endswith(('?', '？')) and not any(q in prompt for q in ['何', 'どこ', 'いつ', '誰', 'なぜ']):
                # プロンプトを要約
                summary = await asyncio.to_thread(local_summarizer.summarize, prompt)
                if summary and "要約できませんでした" not in summary and "エラーが発生しました" not in summary:
                    # Embeddingを生成
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

                    # 要約した会話をベクトルDBに追加
                    # MemoryManagerを使用し、typeとuserを指定
                    self.memory_manager.add_or_update_memory(
                        key=str(uuid.uuid4()), # 新しいキーを生成
                        value=summary,
                        type=memory_type,
                        user=user_id
                    )
                    print(f"バックグラウンドでメモリを追加しました: {summary} (type: {memory_type}, user: {user_id})")
        except Exception as e:
            print(f"バックグラウンドでのメモリ追加中にエラーが発生しました: {e}")

    def generate_speech(self, text: str, voice_name: str = 'Laomedeia'):
        """
        Gemini APIを使用してテキストから音声を生成します。

        Args:
            text (str): 音声に変換するテキスト。
            voice_name (str): 使用する音声の名前。

        Returns:
            bytes: 生成された音声データ(PCM)。
        """
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
        chat_history = self.chat.get_history()
        if chat_history:
            for message in chat_history:
                if message and hasattr(message, 'parts') and message.parts:
                    for part in message.parts:
                        if hasattr(part, 'text'):
                            history_texts.append(f'role - {message.role}: {part.text}')
        return history_texts


class GeminiService:
    def __init__(self, custom_instruction):
        self.session = GeminiSession(custom_instruction)

    def ask(self, prompt, image_path=None, is_private=True, memory_type='app', memory_user_id=None):
        if not prompt:
            return "プロンプトがありません。"
        return self.session.generate_content(prompt, image_path, is_private, memory_type, memory_user_id)

if __name__ == "__main__":
    image_file_path = "screenshot.png"
    if not os.path.exists(image_file_path):
        Image.new("RGB", (100, 100), color=0).save(image_file_path)

    session = GeminiSession(
        custom_instruction="You are a helpful AI assistant."
    )

    prompt1 = "Describe this image."
    response1 = session.generate_content(prompt1, image_path=image_file_path)
    print(f"AI (1): {response1}")

    prompt2 = "Tell me more about the colors."
    response2 = session.generate_content(prompt2)
    print(f"AI (2): {response2}")

    print("\nConversation History:")
    for entry in session.get_history():
        print(entry)