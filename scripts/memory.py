import json
import logging
import asyncio
import uuid
from . import local_summarizer
from .clients import get_chroma_client, get_gemini_client
from google.genai import types
from sentence_transformers import SentenceTransformer
import google.generativeai as genai_async
import os

# Configure the async client
if os.environ.get("GOOGLE_API_KEY"):
    genai_async.configure(api_key=os.environ.get("GOOGLE_API_KEY"))

# グローバルにEmbeddingモデルを保持
_embedding_model = None

def get_embedding_model():
    """Embeddingモデルを取得（初回呼び出し時にロード）"""
    global _embedding_model
    if _embedding_model is None:
        model_name = "cl-nagoya/sup-simcse-ja-base"
        logging.info(f"Loading local embedding model: {model_name}")
        _embedding_model = SentenceTransformer(model_name)
    return _embedding_model

class MemoryAccessError(Exception):
    """メモリーへのアクセス中にエラーが発生した場合に発生する例外"""
    pass

class MemoryManager:
    def __init__(self, collection_name='memories'):
        """
        MemoryManagerを初期化し、ChromaDBとGeminiのクライアントを即座にセットアップする。
        """
        try:
            logging.debug("MemoryManagerの初期化を開始します...")
            self.chroma_client = get_chroma_client()
            self.gemini_client = get_gemini_client()
            self.collection_name = collection_name

            logging.info(f"Getting or creating collection '{self.collection_name}' with HNSW parameters.")
            self.collection = self.chroma_client.get_or_create_collection(
                name=self.collection_name,
                metadata={
                    "hnsw:space": "l2",
                    "hnsw:M": 16,
                    "hnsw:construction_ef": 256,
                    "hnsw:ef": 256
                }
            )
            logging.info(f"コレクションの準備ができました: '{self.collection_name}'")
            
            # 初期化時にモデルをロードしておく
            get_embedding_model()
            
        except Exception as e:
            logging.critical(f"MemoryManagerの初期化中に致命的なエラーが発生しました: {e}", exc_info=True)
            raise

    def get_all_memories(self):
        """すべてのメモリーを取得する"""
        try:
            results = self.collection.get(include=['metadatas', 'documents'])
            if not results or not results.get('ids'):
                return {}
             
            memories = {}
            for i in range(len(results['ids'])):
                id = results['ids'][i]
                doc = results['documents'][i] if results['documents'] and i < len(results['documents']) else ""
                meta = results['metadatas'][i] if results['metadatas'] and i < len(results['metadatas']) else {}
                
                value_obj = {
                    'document': doc,
                    'metadata': meta
                }
                memories[id] = json.dumps(value_obj, ensure_ascii=False, indent=2)
            return memories
        except Exception as e:
            logging.error(f"メモリーの取得中にエラーが発生しました: {e}", exc_info=True)
            return {}

    def add_or_update_memory(self, key, value, type=None, user=None):
        """メモリーを追加または更新する（ローカルEmbeddingを使用）"""
        try:
            document = ""
            metadata = {}

            try:
                data = json.loads(value)
                document = data.get('document', '')
                metadata = data.get('metadata', {})
            except (json.JSONDecodeError, TypeError):
                document = value

            if type:
                metadata['type'] = type
            if user:
                metadata['user'] = user

            # ローカルでEmbedding生成
            model = get_embedding_model()
            embedding = model.encode(document).tolist()

            existing_data = self.collection.get(ids=[key], include=['metadatas'])
            if existing_data and existing_data['metadatas'] and existing_data['metadatas'][0]:
                existing_metadata = existing_data['metadatas'][0]
                if 'created_at' in existing_metadata:
                    metadata['created_at'] = existing_metadata['created_at']

            if 'created_at' not in metadata:
                import datetime
                metadata['created_at'] = datetime.datetime.now().isoformat()
            
            self.collection.upsert(
                ids=[key],
                embeddings=[embedding],
                documents=[document],
                metadatas=[metadata]
            )
            logging.info(f"メモリーを保存しました: {key}")
        except Exception as e:
            logging.error(f"メモリーの保存中にエラーが発生しました: {e}", exc_info=True)

    def delete_memory(self, key):
        """指定されたキーのメモリーを削除する"""
        try:
            self.collection.delete(ids=[key])
            logging.info(f"メモリーを削除しました: {key}")
            return True
        except Exception as e:
            logging.error(f"メモリーの削除中にエラーが発生しました: {e}", exc_info=True)
            return False

    def save_event_to_chroma_sync(self, event_data: dict) -> None:
        """セッションイベントをChromaDBに同期的に保存する（ローカルEmbeddingを使用）"""
        logging.debug(f"ChromaDBへの同期イベント保存を開始: {event_data}")
        try:
            event_id = str(uuid.uuid4())
            content = event_data.get('content', '')
            metadata = {
                'type': event_data.get('type'),
                'source': event_data.get('source'),
                'timestamp': event_data.get('timestamp')
            }

            # ローカルでEmbedding生成
            model = get_embedding_model()
            embedding = model.encode(content).tolist()

            self.collection.upsert(
                ids=[event_id],
                embeddings=[embedding],
                documents=[content],
                metadatas=[metadata]
            )
            logging.debug(f"ChromaDBへの同期イベント保存に成功しました: {event_id}")
        except Exception as e:
            logging.error(f"ChromaDBへの同期イベント保存に失敗しました: {e}", exc_info=True)

    def query_collection(self, query_texts=None, query_embeddings=None, n_results=5, where=None):
        """コレクションに対してクエリを実行する"""
        try:
            # テキストが提供された場合はローカルでEmbedding化する
            if query_texts:
                model = get_embedding_model()
                query_embeddings = model.encode(query_texts).tolist()
                query_texts = None # embeddingsを優先

            return self.collection.query(query_embeddings=query_embeddings, n_results=n_results, where=where)
        except Exception as e:
            logging.error(f"コレクションのクエリ中にエラーが発生しました: {e}", exc_info=True)
            return None

    def summarize_and_add_memory(self, prompt: str, user_id: str, memory_type: str):
        """プロンプトを要約し、メモリに追加する"""
        try:
            # 質問形式のプロンプトは要約・保存しない
            if prompt.endswith(('?', '？')) or any(q in prompt for q in ['何', 'どこ', 'いつ', '誰', 'なぜ']):
                return

            logging.debug(f"プロンプトの要約を開始: {prompt}")
            summary = local_summarizer.summarize(prompt)

            if summary and "要約できませんでした" not in summary and "エラーが発生しました" not in summary:
                self.add_or_update_memory(
                    key=str(uuid.uuid4()),
                    value=summary,
                    type=memory_type,
                    user=user_id
                )
                logging.info(f"要約されたメモリを追加しました: {summary} (type: {memory_type}, user: {user_id})")
        except Exception as e:
            logging.error(f"要約メモリの追加中にエラーが発生しました: {e}", exc_info=True)