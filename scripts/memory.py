import json
import logging
import asyncio
import uuid
import os
import torch

# ONNX Runtime の競合とGPU競合を避けるための環境変数設定
# ChromaDB (Embedding) が ONNX Runtime を使用する際、強制的に CPU を使わせる
# os.environ["ORT_TENSORRT_UNAVAILABLE"] = "1"
# os.environ["ORT_CUDA_UNAVAILABLE"] = "1"

from . import local_summarizer
from .clients import get_chroma_client, get_gemini_client
from google.genai import types
from sentence_transformers import SentenceTransformer
import google.generativeai as genai_async

# Configure the async client
if os.environ.get("GOOGLE_API_KEY"):
    genai_async.configure(api_key=os.environ.get("GOOGLE_API_KEY"))

# グローバルにEmbeddingモデルを保持
_embedding_model = None

def get_embedding_model():
    """Embeddingモデルを取得（ローカルパスを優先）"""
    global _embedding_model
    if _embedding_model is None:
        # pkshatech/GLuCoSE-base-ja を使用
        local_path = "./models/GLuCoSE-base-ja"
        
        # GPUが利用可能ならCUDAを使用
        device = 'cuda' if torch.cuda.is_available() else 'cpu'
        
        if os.path.exists(local_path):
            model_name = local_path
            logging.info(f"Loading local embedding model from: {model_name} (device={device})")
        else:
            model_name = "pkshatech/GLuCoSE-base-ja"
            logging.info(f"Local model not found. Downloading from HF: {model_name} (device={device})")
        
        _embedding_model = SentenceTransformer(model_name, device=device)
    return _embedding_model

class MemoryAccessError(Exception):
    """メモリーへのアクセス中にエラーが発生した場合に発生する例外"""
    pass

import queue
import threading

class MemoryManager:
    def __init__(self, collection_name='memories'):
        """
        MemoryManagerを初期化し、バックグラウンド保存スレッドを開始。
        """
        try:
            logging.debug("MemoryManagerの初期化を開始します...")
            self.chroma_client = get_chroma_client()
            self.gemini_client = get_gemini_client()
            self.collection_name = collection_name

            # バックグラウンド処理用
            self.task_queue = queue.Queue()
            self.is_running = True
            self.worker_thread = threading.Thread(target=self._worker_loop, daemon=True, name="Memory-Worker")

            self.collection = self.chroma_client.get_or_create_collection(
                name=self.collection_name,
                embedding_function=None,
                metadata={
                    "hnsw:space": "l2",
                    "hnsw:M": 16,
                    "hnsw:construction_ef": 256,
                    "hnsw:ef": 256
                }
            )
            
            get_embedding_model()
            self.worker_thread.start()
            logging.info(f"MemoryManager worker started for collection: '{self.collection_name}'")
            
        except Exception as e:
            logging.critical(f"MemoryManagerの初期化中に致命的なエラーが発生しました: {e}", exc_info=True)
            raise

    def stop(self):
        """ワーカー停止"""
        self.is_running = False
        self.task_queue.put(None)

    def enqueue_save(self, event_data):
        """保存タスクをキューに追加"""
        self.task_queue.put({'type': 'save', 'data': event_data})

    def enqueue_summarize(self, prompt, user_id, memory_type):
        """要約保存タスクをキューに追加"""
        self.task_queue.put({
            'type': 'summarize', 
            'data': {'prompt': prompt, 'user_id': user_id, 'memory_type': memory_type}
        })

    def run_query(self, query_texts, n_results=5, where=None):
        """
        クエリを実行。これは同期的（Future経由）に結果を待つ。
        gui/app.py から直接呼ぶ場合を想定。
        """
        from concurrent.futures import Future
        future = Future()
        self.task_queue.put({
            'type': 'query', 
            'future': future, 
            'data': {'query_texts': query_texts, 'n_results': n_results, 'where': where}
        })
        return future.result()

    def _worker_loop(self):
        """バックグラウンド処理の本体"""
        while self.is_running:
            try:
                task = self.task_queue.get()
                if task is None: break
                
                t_type = task.get('type')
                data = task.get('data')
                
                if t_type == 'save':
                    self.save_event_to_chroma_sync(data)
                elif t_type == 'summarize':
                    self.summarize_and_add_memory(**data)
                elif t_type == 'query':
                    res = self.query_collection(**data)
                    if task.get('future'): task['future'].set_result(res)
                
                self.task_queue.task_done()
            except Exception as e:
                logging.error(f"Memory worker error: {e}", exc_info=True)

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
            embedding = model.encode(document, show_progress_bar=False).tolist()

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
            embedding = model.encode(content, show_progress_bar=False).tolist()

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
                query_embeddings = model.encode(query_texts, show_progress_bar=False).tolist()
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