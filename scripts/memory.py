import json
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
            print(f"メモリーの取得中にエラーが発生しました: {e}")
            return {}

    def add_or_update_memory(self, key, value, type=None, user=None):
        """メモリーを追加または更新する"""
        try:
            embedding_model = "models/embedding-001"
            
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

            embedding_response = self.gemini_client.models.embed_content(
                model=embedding_model,
                contents=[document],
                config=types.EmbedContentConfig(task_type="retrieval_document")
            )
            if not (embedding_response and embedding_response.embeddings):
                print(f"メモリーの保存中にEmbeddingの生成に失敗しました: {key}")
                return
            embedding = embedding_response.embeddings[0].values

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

    def save_event_to_chroma(self, event_data: dict) -> None:
        """セッションイベントをChromaDBに直接保存する"""
        print(f"[DEBUG] save_event_to_chroma called with event_data: {event_data}")
        try:
            import uuid
            import logging
            from google.genai import types

            event_id = str(uuid.uuid4())
            content = event_data.get('content', '')
            metadata = {
                'type': event_data.get('type'),
                'source': event_data.get('source'),
                'timestamp': event_data.get('timestamp')
            }

            # Use the same embedding model as the summary saver
            embedding_model = "models/embedding-001"
            embedding_response = self.gemini_client.models.embed_content(
                model=embedding_model,
                contents=[content],
                config=types.EmbedContentConfig(task_type="retrieval_document")
            )
            if not (embedding_response and embedding_response.embeddings):
                logging.error(f"Failed to generate embedding for event: {event_id}")
                return
            embedding = embedding_response.embeddings[0].values

            self.collection.upsert(
                ids=[event_id],
                embeddings=[embedding],
                documents=[content],
                metadatas=[metadata]
            )
            print("[DEBUG] Successfully upserted event to ChromaDB.")
        except Exception as e:
            logging.error("Failed to save event to ChromaDB: %s", e)