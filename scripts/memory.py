import sqlite3
import os
import uuid
from datetime import datetime

class MemoryManager:
    def __init__(self, db_path=os.path.join('chromadb', 'chroma.sqlite3'), collection_name='memories'):
        self.db_path = db_path
        self.collection_name = collection_name

    def _get_collection_id(self):
        """コレクション名からコレクションIDを取得する"""
        if not os.path.exists(self.db_path): return None
        try:
            with sqlite3.connect(self.db_path) as conn:
                cursor = conn.cursor()
                cursor.execute("SELECT id FROM collections WHERE name = ?", (self.collection_name,))
                result = cursor.fetchone()
                return result[0] if result else None
        except sqlite3.Error:
            return None

    def get_all_memories(self):
        """すべてのメモリーを取得する"""
        if not os.path.exists(self.db_path): return {}
        memories = {}
        try:
            with sqlite3.connect(self.db_path) as conn:
                cursor = conn.cursor()
                sql = """
                SELECT e.embedding_id, m.string_value
                FROM embeddings e
                JOIN embedding_metadata m ON e.id = m.id
                WHERE m.key = 'data'
                """
                cursor.execute(sql)
                for row in cursor.fetchall():
                    memories[row[0]] = row[1]
            return memories
        except sqlite3.Error as e:
            print(f"メモリーの取得中にSQLiteエラーが発生しました: {e}")
            return {}

    def add_or_update_memory(self, key, value):
        """メモリーを追加または更新する"""
        # この操作はChromaDBの内部構造に依存するため、注意が必要です。
        # mem0が使用している'metadata'セグメントIDを特定する必要がありますが、
        # ここでは単純化のため、既存のレコードを更新することに焦点を当てます。
        if not os.path.exists(self.db_path): return
        try:
            with sqlite3.connect(self.db_path) as conn:
                cursor = conn.cursor()
                # 1. embedding_id (key) を使って、embeddingsテーブルの主キー(id)を取得
                cursor.execute("SELECT id FROM embeddings WHERE embedding_id = ?", (key,))
                result = cursor.fetchone()
                if result:
                    embedding_pk = result[0]
                    # 2. 主キーを使って、embedding_metadataテーブルのstring_valueを更新
                    cursor.execute(
                        "UPDATE embedding_metadata SET string_value = ? WHERE id = ? AND key = 'data'",
                        (value, embedding_pk)
                    )
                else:
                    # 新規追加はさらに複雑なため、ここでは実装しません。
                    print(f"警告: キー '{key}' のメモリーが見つからないため、更新できませんでした。")
        except sqlite3.Error as e:
            print(f"メモリーの更新中にSQLiteエラーが発生しました: {e}")

    def delete_memory(self, key):
        """指定されたキーのメモリーを削除する"""
        if not os.path.exists(self.db_path): return False
        try:
            with sqlite3.connect(self.db_path) as conn:
                cursor = conn.cursor()
                # 1. embedding_id (key) を使って、embeddingsテーブルの主キー(id)を取得
                cursor.execute("SELECT id FROM embeddings WHERE embedding_id = ?", (key,))
                result = cursor.fetchone()
                if result:
                    embedding_pk = result[0]
                    # 2. 主キーを使って、embedding_metadataとembeddingsテーブルからレコードを削除
                    cursor.execute("DELETE FROM embedding_metadata WHERE id = ?", (embedding_pk,))
                    cursor.execute("DELETE FROM embeddings WHERE id = ?", (embedding_pk,))
                    print(f"メモリーを削除しました: {key}")
                    return True
                else:
                    print(f"警告: キー '{key}' のメモリーが見つからないため、削除できませんでした。")
                    return False
        except sqlite3.Error as e:
            print(f"メモリーの削除中にSQLiteエラーが発生しました: {e}")
            return False
