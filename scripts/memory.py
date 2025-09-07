from .clients import get_chroma_client, get_gemini_client
from google.genai import types

class MemoryManager:
    def __init__(self, collection_name='memories'):
        self.chroma_client = get_chroma_client()
        self.gemini_client = get_gemini_client()
        self.collection = self.chroma_client.get_or_create_collection(name=collection_name)

    def get_all_memories(self):
        """すべてのメモリーを取得する"""
        try:
            results = self.collection.get()
            if not results or not results.get('ids') or not results.get('documents'):
                return {}
            
            # documentsがNoneを含む可能性があるためフィルタリング
            safe_docs = results['documents'] or []
            memories = {id: doc for id, doc in zip(results['ids'], safe_docs) if doc is not None}
            return memories
        except Exception as e:
            print(f"メモリーの取得中にエラーが発生しました: {e}")
            return {}

    def add_or_update_memory(self, key, value):
        """メモリーを追加または更新する"""
        try:
            embedding_model = "models/embedding-001"
            embedding_response = self.gemini_client.models.embed_content(
                model=embedding_model,
                contents=[value],
                config=types.EmbedContentConfig(task_type="retrieval_document")
            )
            embedding = embedding_response.embeddings[0].values

            self.collection.upsert(
                ids=[key],
                embeddings=[embedding],
                documents=[value]
            )
            print(f"メモリーを保存しました: {key}")
        except Exception as e:
            print(f"メモリーの保存中にエラーが発生しました: {e}")

    def delete_memory(self, key):
        """指定されたキーのメモリーを削除する"""
        try:
            self.collection.delete(ids=[key])
            print(f"メモリーを削除しました: {key}")
            return True
        except Exception as e:
            print(f"メモリーの削除中にエラーが発生しました: {e}")
            return False
