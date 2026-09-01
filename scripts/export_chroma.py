import os
import json
import sqlite3
import chromadb

def export_chroma():
    export_path = os.path.join("data", "chroma_export.json")
    os.makedirs("data", exist_ok=True)
    
    persist_dir = "chromadb"
    if not os.path.isdir(persist_dir):
        print("No chromadb directory found.")
        return

    print("Connecting to ChromaDB...")
    client = chromadb.PersistentClient(path=persist_dir)
    coll = client.get_collection("memories")
    count = coll.count()
    print(f"Found {count} records in 'memories' collection.")
    
    # ページングして全レコードを取得
    batch_size = 5000
    all_items = []
    
    for offset in range(0, count, batch_size):
        limit = min(batch_size, count - offset)
        print(f"Fetching records {offset} to {offset + limit}...")
        # get では include に documents, metadatas, embeddings (あれば) を指定
        data = coll.get(
            limit=limit,
            offset=offset,
            include=["documents", "metadatas", "embeddings"]
        )
        
        ids = data.get("ids", [])
        docs = data.get("documents", [])
        metas = data.get("metadatas", [])
        embeds = data.get("embeddings", [])
        
        for idx in range(len(ids)):
            meta = metas[idx] if metas and idx < len(metas) and metas[idx] else {}
            item = {
                "id": str(ids[idx]),
                "document": str(docs[idx] if docs and idx < len(docs) and docs[idx] else ""),
                "memory_type": str(meta.get("type", "general")),
                "source": str(meta.get("source", "User")),
                "timestamp": str(meta.get("timestamp", "")),
                "user_id": meta.get("user", None) or meta.get("user_id", None),
                "vector": embeds[idx] if embeds and idx < len(embeds) and embeds[idx] is not None else None
            }
            all_items.append(item)
            
    print(f"Total exported items: {len(all_items)}")
    with open(export_path, "w", encoding="utf-8") as f:
        json.dump(all_items, f, ensure_ascii=False, indent=2)
    print(f"Exported successfully to {export_path}")

if __name__ == "__main__":
    export_chroma()
