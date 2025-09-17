import os
import sys
import shutil
import datetime
import logging

# 親ディレクトリをsys.pathに追加
sys.path.append(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from scripts.memory import MemoryManager, MemoryAccessError

# ロギング設定
log_dir = "logs"
os.makedirs(log_dir, exist_ok=True)
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(levelname)s - %(message)s',
    handlers=[
        logging.FileHandler(os.path.join(log_dir, "migration.log")),
        logging.StreamHandler()
    ]
)

def backup_chroma_data():
    """ChromaDBのデータディレクトリをバックアップする"""
    # ChromaDBのデータパスは `get_chroma_client` の実装に依存します。
    # ここでは一般的な 'chroma' ディレクトリを想定しています。
    # 正確なパスは clients.py を確認する必要があります。
    source_dir = "chromadb" 
    if not os.path.isdir(source_dir):
        logging.warning(f"ChromaDBのデータディレクトリ '{source_dir}' が見つかりません。バックアップをスキップします。")
        return False
        
    timestamp = datetime.datetime.now().strftime("%Y%m%d_%H%M%S")
    backup_dir = f"chroma_backup_{timestamp}"
    
    try:
        shutil.copytree(source_dir, backup_dir)
        logging.info(f"ChromaDBのデータを '{backup_dir}' にバックアップしました。")
        return True
    except Exception as e:
        logging.error(f"バックアップ中にエラーが発生しました: {e}", exc_info=True)
        return False

def migrate_memories():
    """既存のメモリーを新しい形式に移行する"""
    logging.info("メモリー移行スクリプトを開始します。")
    
    if not backup_chroma_data():
        if input("バックアップに失敗しました。処理を続行しますか？ (y/n): ").lower() != 'y':
            logging.info("ユーザーにより処理が中断されました。")
            return

    try:
        memory_manager = MemoryManager()
        all_memories = memory_manager.get_all_memories()
    except MemoryAccessError as e:
        logging.error(f"メモリーの取得に失敗しました: {e}")
        return

    if not all_memories:
        logging.info("移行対象のメモリーはありませんでした。")
        return

    logging.info(f"{len(all_memories)}件のメモリーを移行します...")
    
    success_count = 0
    failure_count = 0
    skip_count = 0

    for key, data in all_memories.items():
        try:
            document = None
            if isinstance(data, dict):
                document = data.get('document')
                metadata = data.get('metadata')
                if metadata and 'type' in metadata and 'user' in metadata:
                    logging.info(f"キー'{key}'は既に新しい形式のためスキップします。")
                    skip_count += 1
                    continue
            else:
                document = data
            
            if document is None:
                logging.warning(f"キー'{key}'のドキュメントがNoneのためスキップします。")
                skip_count += 1
                continue

            logging.info(f"キー'{key}'を移行中...")
            memory_manager.add_or_update_memory(
                key=key,
                value=document,
                type='app',
                user='default'
            )
            logging.info(f"キー'{key}'の移行が完了しました。")
            success_count += 1
        except MemoryAccessError as e:
            logging.error(f"キー'{key}'の移行中にエラーが発生しました: {e}")
            failure_count += 1
        except Exception as e:
            logging.error(f"予期せぬエラーが発生しました (キー: {key}): {e}", exc_info=True)
            failure_count += 1

    summary = f"移行完了: 成功={success_count}件, 失敗={failure_count}件, スキップ={skip_count}件"
    logging.info(summary)

if __name__ == "__main__":
    migrate_memories()