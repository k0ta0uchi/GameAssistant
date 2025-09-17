from .clients import get_chroma_client, get_gemini_client
from google.genai import types

class MemoryAccessError(Exception):
    """メモリーへのアクセス中にエラーが発生した場合に発生する例外"""
    pass

class MemoryManager:
    def __init__(self, collection_name='memories'):
        self.chroma_client = get_chroma_client()
        self.gemini_client = get_gemini_client()
        self.collection = self.chroma_client.get_or_create_collection(name=collection_name)

    def get_all_memories(self):
        """すべてのメモリーを取得する"""
        try:
            # metadatasも取得するように変更
            results = self.collection.get(include=['metadatas', 'documents', 'embeddings', 'ids'])
            if not results or not results.get('ids') or not results.get('documents'):
                return {}
             
            memories = {}
            for i in range(len(results['ids'])):
                id = results['ids'][i]
                doc = results['documents'][i]
                meta = results['metadatas'][i] if results.get('metadatas') and i < len(results['metadatas']) else None
                
                if doc is not None:
                    memories[id] = {'document': doc, 'metadata': meta}
            return memories
        except Exception as e:
            print(f"メモリーの取得中にエラーが発生しました: {e}")
            return {}

    def add_or_update_memory(self, key, value, type=None, user=None):
        """メモリーを追加または更新する"""
        try:
            embedding_model = "models/embedding-001"
            embedding_response = self.gemini_client.models.embed_content(
                model=embedding_model,
                contents=[value],
                config=types.EmbedContentConfig(task_type="retrieval_document")
            )
            if embedding_response and embedding_response.embeddings:
                embedding = embedding_response.embeddings[0].values
            else:
                print(f"メモリーの保存中にEmbeddingの生成に失敗しました: {key}")
                return

            metadata = {}
            if type:
                metadata['type'] = type
            if user:
                metadata['user'] = user
            
            self.collection.upsert(
                ids=[key],
                embeddings=[embedding],
                documents=[value],
                metadatas=[metadata] if metadata else None
            )
            print(f"メモリーを保存しました: {key} (type: {type}, user: {user})")
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