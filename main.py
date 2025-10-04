import ttkbootstrap as ttk
from twitchio.utils import setup_logging
import logging
from gui.app import GameAssistantApp
import scripts.record as record
import shutil
import os
from datetime import datetime

def backup_chromadb():
    """chromadbのバックアップを作成し、最大5個まで保持する"""
    backup_dir = "chroma_backup"
    source_dir = "chromadb"

    if not os.path.exists(backup_dir):
        os.makedirs(backup_dir)

    # バックアップを作成
    timestamp = datetime.now().strftime("%Y%m%d%H%M%S")
    backup_path = os.path.join(backup_dir, f"chromadb_{timestamp}")
    if os.path.exists(source_dir):
        shutil.copytree(source_dir, backup_path)
        print(f"'{source_dir}' のバックアップを '{backup_path}' に作成しました。")

    # バックアップの数を調整
    backups = sorted(
        [d for d in os.listdir(backup_dir) if os.path.isdir(os.path.join(backup_dir, d))],
        key=lambda x: os.path.getmtime(os.path.join(backup_dir, x))
    )
    while len(backups) > 5:
        oldest_backup = backups.pop(0)
        shutil.rmtree(os.path.join(backup_dir, oldest_backup))
        print(f"古いバックアップ '{oldest_backup}' を削除しました。")


def on_closing(app_instance):
    print("アプリケーションを終了します...")
    if app_instance.twitch_service.twitch_bot:
        app_instance.twitch_service.disconnect_twitch_bot()
    if record.p:
        record.p.terminate()
    app_instance.root.destroy()

if __name__ == "__main__":
    backup_chromadb()
    setup_logging(level=logging.DEBUG)
    root = ttk.Window(themename="superhero")
    root.geometry("1280x960")
    app = GameAssistantApp(root)
    root.protocol("WM_DELETE_WINDOW", lambda: on_closing(app))
    root.mainloop()
