import ttkbootstrap as ttk
from twitchio.utils import setup_logging
import logging
from gui.app import GameAssistantApp
import scripts.record as record

def on_closing(app_instance):
    print("アプリケーションを終了します...")
    if app_instance.twitch_service.twitch_bot:
        app_instance.twitch_service.disconnect_twitch_bot()
    if record.p:
        record.p.terminate()
    app_instance.root.destroy()

if __name__ == "__main__":
    setup_logging(level=logging.DEBUG)
    root = ttk.Window(themename="superhero")
    root.geometry("1280x960")
    app = GameAssistantApp(root)
    root.protocol("WM_DELETE_WINDOW", lambda: on_closing(app))
    root.mainloop()
