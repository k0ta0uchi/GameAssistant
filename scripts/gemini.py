# -*- coding: utf-8 -*-
import os
from dotenv import load_dotenv
from PIL import Image
from mem0 import Memory
from google import genai
from google.genai import types
import wave

# --- Environment Setup ---
load_dotenv()

GOOGLE_API_KEY = os.environ.get("GOOGLE_API_KEY")
GEMINI_MODEL = os.environ.get("GEMINI_MODEL")
USER_ID_PRIVATE = os.environ.get("USER_ID_PRIVATE")
USER_ID_PUBLIC = os.environ.get("USER_ID_PUBLIC")

class GeminiSession:
    def __init__(self, custom_instruction: str | None = None):
        if not GOOGLE_API_KEY:
            raise ValueError("GOOGLE_API_KEY is not set.")
        if not GEMINI_MODEL:
            raise ValueError("GEMINI_MODEL is not set.")

        self.client = genai.Client(api_key=GOOGLE_API_KEY)
        
        # --- Chat Session Initialization ---
        history = []
        if custom_instruction:
            history.append({'role': 'user', 'parts': [{'text': custom_instruction}]})
            history.append({'role': 'model', 'parts': [{'text': 'はい、承知いたしましただわん。'}]})
        
        self.chat = self.client.chats.create(model=GEMINI_MODEL, history=history)

        # --- Mem0 Configuration ---
        vector_store_config = {
            "provider": "chroma",
            "config": {"path": "chromadb", "collection_name": "memories"},
        }
        mem0_config = {
            "llm": {"provider": "gemini", "config": {"model": "gemini-1.5-flash-latest", "temperature": 0.2, "max_tokens": 2000}},
            "embedder": {"provider": "gemini", "config": {"model": "models/embedding-001"}},
            "vector_store": vector_store_config,
        }
        self.m = Memory.from_config(mem0_config)

    def generate_content(self, prompt: str, image_path: str | None = None, is_private: bool = True):
        user_id = USER_ID_PRIVATE if is_private else USER_ID_PUBLIC
        if not user_id:
            raise ValueError("User ID is not set for the selected privacy level.")

        self.m.add(prompt, user_id=user_id)
        memory = self.m.search(query=prompt, user_id=user_id)

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

    def generate_speech(self, text: str, voice_name: str = 'Kore'):
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
                contents=f"Say cheerfully: {text}",
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