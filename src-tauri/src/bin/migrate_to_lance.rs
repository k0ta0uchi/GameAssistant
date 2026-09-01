use std::fs::File;
use std::io::BufReader;
use std::time::Instant;
use gameassistant_lib::lance_memory::{self, MemoryItem};

#[derive(serde::Deserialize)]
struct ChromaExportItem {
    id: String,
    document: String,
    memory_type: String,
    source: String,
    timestamp: String,
    user_id: Option<String>,
    vector: Option<Vec<f32>>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== LanceDB Rust Migration Tool ===");
    let root_dir = std::env::current_dir()?;
    let export_file = root_dir.join("data").join("chroma_export.json");
    
    if !export_file.exists() {
        eprintln!("Export file not found: {:?}", export_file);
        return Ok(());
    }

    println!("Reading {:?} ...", export_file);
    let start_time = Instant::now();
    let file = File::open(&export_file)?;
    let reader = BufReader::new(file);
    let raw_items: Vec<ChromaExportItem> = serde_json::from_reader(reader)?;
    println!("Loaded {} records in {:?}", raw_items.len(), start_time.elapsed());

    let mut memory_items = Vec::with_capacity(raw_items.len());
    let mut vectors = Vec::with_capacity(raw_items.len());

    for item in raw_items {
        memory_items.push(MemoryItem {
            id: item.id,
            document: item.document,
            memory_type: item.memory_type,
            source: item.source,
            timestamp: item.timestamp,
            user_id: item.user_id,
        });
        if let Some(v) = item.vector {
            vectors.push(v);
        } else {
            vectors.push(vec![0.0f32; 768]);
        }
    }

    println!("Importing into LanceDB at {:?} ...", root_dir.join(lance_memory::LANCE_DB_DIR));
    let insert_start = Instant::now();
    
    // 5000件ずつバッチ挿入
    let batch_size = 5000;
    let mut total_inserted = 0;

    for chunk_start in (0..memory_items.len()).step_by(batch_size) {
        let chunk_end = (chunk_start + batch_size).min(memory_items.len());
        let chunk_items = memory_items[chunk_start..chunk_end].to_vec();
        let chunk_vectors = vectors[chunk_start..chunk_end].to_vec();

        println!("Inserting batch {}..{} ...", chunk_start, chunk_end);
        let count = lance_memory::insert_memory_batch(&root_dir, chunk_items, Some(chunk_vectors)).await
            .map_err(|e| format!("Insert batch error: {}", e))?;
        total_inserted += count;
    }

    println!("✅ Successfully imported {} records to LanceDB in {:?}", total_inserted, insert_start.elapsed());
    
    // 検証: 取得テスト
    println!("Verifying LanceDB records...");
    let sample = lance_memory::list_memories(&root_dir, Some(5), None).await
        .map_err(|e| format!("List memories error: {}", e))?;
    println!("Total records queryable: {}", sample.total);
    for (i, m) in sample.memories.iter().enumerate() {
        println!("  [Sample {}] ID: {} | Type: {} | Doc: {}", i + 1, m.id, m.memory_type, m.document);
    }

    println!("=== Migration Completed Successfully ===");
    Ok(())
}
