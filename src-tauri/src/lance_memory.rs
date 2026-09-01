use std::path::Path;
use std::sync::Arc;
use arrow_array::{Array, FixedSizeListArray, Float32Array, RecordBatch, RecordBatchIterator, RecordBatchReader, StringArray};
use arrow_schema::{DataType, Field, Schema};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};
use lancedb::{connect, Connection, Table};
use serde::{Deserialize, Serialize};

pub const LANCE_DB_DIR: &str = "data/lancedb";
pub const MEMORIES_TABLE: &str = "memories";
pub const VECTOR_DIM: i32 = 768;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryItem {
    pub id: String,
    pub document: String,
    pub memory_type: String,
    pub source: String,
    pub timestamp: String,
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryListResponse {
    pub success: bool,
    pub total: usize,
    pub memories: Vec<MemoryItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationStats {
    pub success: bool,
    pub imported_count: usize,
    pub message: String,
}

pub fn get_memory_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("document", DataType::Utf8, false),
        Field::new("memory_type", DataType::Utf8, false),
        Field::new("source", DataType::Utf8, false),
        Field::new("timestamp", DataType::Utf8, false),
        Field::new("user_id", DataType::Utf8, true),
        Field::new(
            "vector",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                VECTOR_DIM,
            ),
            false,
        ),
    ]))
}

pub async fn get_or_create_db(root_dir: &Path) -> Result<Connection, String> {
    let db_path = root_dir.join(LANCE_DB_DIR);
    if !db_path.exists() {
        std::fs::create_dir_all(&db_path).map_err(|e| format!("Failed to create LanceDB dir: {}", e))?;
    }
    let db_path_str = db_path.to_str().ok_or_else(|| "Invalid DB path".to_string())?;
    connect(db_path_str)
        .execute()
        .await
        .map_err(|e| format!("Failed to connect to LanceDB: {}", e))
}

pub async fn get_or_create_memories_table(db: &Connection) -> Result<Table, String> {
    let table_names = db.table_names().execute().await.map_err(|e| format!("Failed to list tables: {}", e))?;
    if table_names.contains(&MEMORIES_TABLE.to_string()) {
        db.open_table(MEMORIES_TABLE)
            .execute()
            .await
            .map_err(|e| format!("Failed to open memories table: {}", e))
    } else {
        let schema = get_memory_schema();
        // 空のレコードバッチを作成してテーブルを初期化
        let id_array = Arc::new(StringArray::from(Vec::<String>::new()));
        let doc_array = Arc::new(StringArray::from(Vec::<String>::new()));
        let type_array = Arc::new(StringArray::from(Vec::<String>::new()));
        let src_array = Arc::new(StringArray::from(Vec::<String>::new()));
        let ts_array = Arc::new(StringArray::from(Vec::<String>::new()));
        let uid_array = Arc::new(StringArray::from(Vec::<Option<String>>::new()));
        
        let values = Float32Array::from(Vec::<f32>::new());
        let vector_array = Arc::new(FixedSizeListArray::new(
            Arc::new(Field::new("item", DataType::Float32, true)),
            VECTOR_DIM,
            Arc::new(values),
            None,
        ));

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                id_array,
                doc_array,
                type_array,
                src_array,
                ts_array,
                uid_array,
                vector_array,
            ],
        ).map_err(|e| format!("Failed to create empty batch: {}", e))?;

        let batches = RecordBatchIterator::new(vec![Ok(batch)], schema);
        let reader: Box<dyn RecordBatchReader + Send> = Box::new(batches);
        db.create_table(MEMORIES_TABLE, reader)
            .execute()
            .await
            .map_err(|e| format!("Failed to create memories table: {}", e))
    }
}

pub async fn list_memories(root_dir: &Path, limit: Option<usize>, offset: Option<usize>) -> Result<MemoryListResponse, String> {
    let db = get_or_create_db(root_dir).await?;
    let table = get_or_create_memories_table(&db).await?;

    let query = table.query();
    let mut stream = query.execute().await.map_err(|e| format!("Query execute error: {}", e))?;
    let mut memories = Vec::new();

    while let Some(batch) = stream.try_next().await.map_err(|e| format!("Stream batch error: {}", e))? {
        let id_col = batch.column(0).as_any().downcast_ref::<StringArray>().ok_or("Invalid id column")?;
        let doc_col = batch.column(1).as_any().downcast_ref::<StringArray>().ok_or("Invalid doc column")?;
        let type_col = batch.column(2).as_any().downcast_ref::<StringArray>().ok_or("Invalid type column")?;
        let src_col = batch.column(3).as_any().downcast_ref::<StringArray>().ok_or("Invalid src column")?;
        let ts_col = batch.column(4).as_any().downcast_ref::<StringArray>().ok_or("Invalid ts column")?;
        let uid_col = batch.column(5).as_any().downcast_ref::<StringArray>();

        for row in 0..batch.num_rows() {
            let id = id_col.value(row).to_string();
            let document = doc_col.value(row).to_string();
            let memory_type = type_col.value(row).to_string();
            let source = src_col.value(row).to_string();
            let timestamp = ts_col.value(row).to_string();
            let user_id = uid_col.and_then(|c| if c.is_null(row) { None } else { Some(c.value(row).to_string()) });

            memories.push(MemoryItem {
                id,
                document,
                memory_type,
                source,
                timestamp,
                user_id,
            });
        }
    }

    let total = memories.len();

    // タイムスタンプ降順（最新の記憶が先頭に来るようにソート）
    memories.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

    // offset & limit を最新順の配列に適用
    let start = offset.unwrap_or(0);
    let memories_slice = if start < memories.len() {
        let end = match limit {
            Some(l) => (start + l).min(memories.len()),
            None => memories.len(),
        };
        memories[start..end].to_vec()
    } else {
        Vec::new()
    };

    Ok(MemoryListResponse {
        success: true,
        total,
        memories: memories_slice,
    })
}

/// 768 次元ベクトルによるセマンティック類似度検索 (LanceDB Vector Search)
pub async fn search_similar_memories(
    root_dir: &Path,
    query_vector: &[f32],
    limit: usize,
) -> Result<Vec<MemoryItem>, String> {
    if query_vector.len() != (VECTOR_DIM as usize) {
        return Err(format!("Query vector length mismatch: expected {}, got {}", VECTOR_DIM, query_vector.len()));
    }

    let db = get_or_create_db(root_dir).await?;
    let table = get_or_create_memories_table(&db).await?;

    let q_vec = query_vector.to_vec();
    let stream = table
        .vector_search(q_vec)
        .map_err(|e| format!("Vector search build error: {}", e))?
        .limit(limit)
        .execute()
        .await
        .map_err(|e| format!("Vector search execute error: {}", e))?;

    let mut memories = Vec::new();
    let mut s = stream;
    while let Some(batch) = s.try_next().await.map_err(|e| format!("Stream error: {}", e))? {
        let id_col = batch.column(0).as_any().downcast_ref::<StringArray>().ok_or("Invalid id column")?;
        let doc_col = batch.column(1).as_any().downcast_ref::<StringArray>().ok_or("Invalid doc column")?;
        let type_col = batch.column(2).as_any().downcast_ref::<StringArray>().ok_or("Invalid type column")?;
        let src_col = batch.column(3).as_any().downcast_ref::<StringArray>().ok_or("Invalid src column")?;
        let ts_col = batch.column(4).as_any().downcast_ref::<StringArray>().ok_or("Invalid ts column")?;
        let uid_col = batch.column(5).as_any().downcast_ref::<StringArray>();

        for row in 0..batch.num_rows() {
            let id = id_col.value(row).to_string();
            let document = doc_col.value(row).to_string();
            let memory_type = type_col.value(row).to_string();
            let source = src_col.value(row).to_string();
            let timestamp = ts_col.value(row).to_string();
            let user_id = uid_col.and_then(|c| if c.is_null(row) { None } else { Some(c.value(row).to_string()) });

            if !document.is_empty() {
                memories.push(MemoryItem {
                    id,
                    document,
                    memory_type,
                    source,
                    timestamp,
                    user_id,
                });
            }
        }
    }

    Ok(memories)
}

pub async fn insert_memory_batch(
    root_dir: &Path,
    items: Vec<MemoryItem>,
    vectors: Option<Vec<Vec<f32>>>,
) -> Result<usize, String> {
    if items.is_empty() {
        return Ok(0);
    }
    let db = get_or_create_db(root_dir).await?;
    let table = get_or_create_memories_table(&db).await?;
    let schema = get_memory_schema();

    let count = items.len();
    let ids: Vec<String> = items.iter().map(|i| i.id.clone()).collect();
    let docs: Vec<String> = items.iter().map(|i| i.document.clone()).collect();
    let types: Vec<String> = items.iter().map(|i| i.memory_type.clone()).collect();
    let srcs: Vec<String> = items.iter().map(|i| i.source.clone()).collect();
    let tss: Vec<String> = items.iter().map(|i| i.timestamp.clone()).collect();
    let uids: Vec<Option<String>> = items.iter().map(|i| i.user_id.clone()).collect();

    let mut flat_vectors: Vec<f32> = Vec::with_capacity(count * (VECTOR_DIM as usize));
    if let Some(v_list) = vectors {
        for v in v_list {
            if v.len() == (VECTOR_DIM as usize) {
                flat_vectors.extend(v);
            } else {
                flat_vectors.extend(vec![0.0f32; VECTOR_DIM as usize]);
            }
        }
    } else {
        flat_vectors.extend(vec![0.0f32; count * (VECTOR_DIM as usize)]);
    }

    let id_array = Arc::new(StringArray::from(ids));
    let doc_array = Arc::new(StringArray::from(docs));
    let type_array = Arc::new(StringArray::from(types));
    let src_array = Arc::new(StringArray::from(srcs));
    let ts_array = Arc::new(StringArray::from(tss));
    let uid_array = Arc::new(StringArray::from(uids));

    let values = Float32Array::from(flat_vectors);
    let vector_array = Arc::new(FixedSizeListArray::new(
        Arc::new(Field::new("item", DataType::Float32, true)),
        VECTOR_DIM,
        Arc::new(values),
        None,
    ));

    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            id_array,
            doc_array,
            type_array,
            src_array,
            ts_array,
            uid_array,
            vector_array,
        ],
    ).map_err(|e| format!("Failed to create insert batch: {}", e))?;

    let batches = RecordBatchIterator::new(vec![Ok(batch)], schema);
    let reader: Box<dyn RecordBatchReader + Send> = Box::new(batches);
    table.add(reader).execute().await.map_err(|e| format!("Failed to add to table: {}", e))?;

    Ok(count)
}

pub async fn delete_memory(root_dir: &Path, id: &str) -> Result<bool, String> {
    let db = get_or_create_db(root_dir).await?;
    let table = get_or_create_memories_table(&db).await?;
    let predicate = format!("id = '{}'", id.replace('\'', "''"));
    table.delete(&predicate).await.map_err(|e| format!("Delete error: {}", e))?;
    Ok(true)
}

pub async fn delete_memories_bulk(root_dir: &Path, ids: &[String]) -> Result<usize, String> {
    if ids.is_empty() {
        return Ok(0);
    }
    let db = get_or_create_db(root_dir).await?;
    let table = get_or_create_memories_table(&db).await?;
    let escaped_ids: Vec<String> = ids.iter().map(|id| format!("'{}'", id.replace('\'', "''"))).collect();
    let predicate = format!("id IN ({})", escaped_ids.join(", "));
    table.delete(&predicate).await.map_err(|e| format!("Delete bulk error: {}", e))?;
    Ok(ids.len())
}

/// 再帰的ディレクトリコピー
fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &dst.join(entry.file_name()))?;
        } else {
            std::fs::copy(entry.path(), dst.join(entry.file_name()))?;
        }
    }
    Ok(())
}

/// LanceDB のタイムスタンプ付きスナップショットバックアップ作成（世代管理: 5世代保持）
pub fn backup_lance_db(root_dir: &Path) -> Result<String, String> {
    let db_src = root_dir.join(LANCE_DB_DIR);
    if !db_src.exists() {
        return Err("LanceDB directory does not exist".to_string());
    }

    let backups_base = root_dir.join("data/lancedb_backups");
    let _ = std::fs::create_dir_all(&backups_base);

    let now_str = chrono::Local::now().format("%Y%m%d_%H%M%S").to_string();
    let target_dir_name = format!("backup_{}", now_str);
    let target_dir = backups_base.join(&target_dir_name);

    copy_dir_all(&db_src, &target_dir)
        .map_err(|e| format!("Failed to copy LanceDB snapshot: {}", e))?;

    // 世代管理 (最新 5 件を残して古いものを削除)
    if let Ok(entries) = std::fs::read_dir(&backups_base) {
        let mut dirs: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .collect();
        dirs.sort_by_key(|e| e.file_name());

        if dirs.len() > 5 {
            let to_remove = dirs.len() - 5;
            for d in dirs.iter().take(to_remove) {
                let _ = std::fs::remove_dir_all(d.path());
            }
        }
    }

    Ok(target_dir_name)
}

/// バックアップ一覧取得
pub fn list_lance_backups(root_dir: &Path) -> Result<Vec<String>, String> {
    let backups_base = root_dir.join("data/lancedb_backups");
    if !backups_base.exists() {
        return Ok(Vec::new());
    }

    let mut names = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&backups_base) {
        for entry in entries.filter_map(|e| e.ok()) {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                if let Some(s) = entry.file_name().to_str() {
                    names.push(s.to_string());
                }
            }
        }
    }
    names.sort();
    names.reverse(); // 最新順
    Ok(names)
}

/// 指定バックアップから LanceDB を復元
pub fn restore_lance_backup(root_dir: &Path, backup_name: &str) -> Result<(), String> {
    let clean_name = backup_name.trim();
    if clean_name.is_empty() {
        return Err("Backup name is empty".to_string());
    }

    let backup_dir = root_dir.join("data/lancedb_backups").join(clean_name);
    if !backup_dir.exists() {
        return Err(format!("Backup folder '{}' not found", clean_name));
    }

    let db_dst = root_dir.join(LANCE_DB_DIR);
    if db_dst.exists() {
        std::fs::remove_dir_all(&db_dst)
            .map_err(|e| format!("Failed to clear current LanceDB dir: {}", e))?;
    }

    copy_dir_all(&backup_dir, &db_dst)
        .map_err(|e| format!("Failed to restore backup: {}", e))?;

    Ok(())
}

/// 全件を JSON ファイルにエクスポート
pub async fn export_lance_memories_json(
    root_dir: &Path,
    output_filename: Option<String>,
) -> Result<String, String> {
    let res = list_memories(root_dir, None, None).await?;
    let data_dir = root_dir.join("data");
    let _ = std::fs::create_dir_all(&data_dir);

    let fname = output_filename.unwrap_or_else(|| "lance_export.json".to_string());
    let out_path = data_dir.join(&fname);

    let json_str = serde_json::to_string_pretty(&res.memories)
        .map_err(|e| format!("JSON serialization error: {}", e))?;

    std::fs::write(&out_path, json_str)
        .map_err(|e| format!("Failed to write export JSON: {}", e))?;

    Ok(format!("Exported {} memories to {:?}", res.total, out_path))
}

