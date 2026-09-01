import logging
import threading
import mss
import mss.tools
import pygetwindow as gw
import ctypes, win32con, win32gui
import win32clipboard as w32clip
from struct import pack, calcsize
from ctypes import windll, wintypes
from PIL import Image

LOGGER = logging.getLogger("visual_capture")

user32, gdi32 = windll.user32, windll.gdi32
PW_RENDERFULLCONTENT = 2

def getWindowBMAP(hwnd, returnImage=False):
    # get Window size and crop pos/size
    L, T, R, B = win32gui.GetWindowRect(hwnd)
    W, H = R - L, B - T
    x, y, w, h = (8, 8, W - 16, H - 16) if user32.IsZoomed(hwnd) else (7, 0, W - 14, H - 7)

    # create dc's and bmp's
    dc = user32.GetWindowDC(hwnd)
    dc1, dc2 = gdi32.CreateCompatibleDC(dc), gdi32.CreateCompatibleDC(dc)
    bmp1, bmp2 = gdi32.CreateCompatibleBitmap(dc, W, H), gdi32.CreateCompatibleBitmap(dc, w, h)

    # render dc1 and dc2 (bmp1 and bmp2) (uncropped and cropped)
    obj1, obj2 = gdi32.SelectObject(dc1, bmp1), gdi32.SelectObject(dc2, bmp2)
    user32.PrintWindow(hwnd, dc1, PW_RENDERFULLCONTENT)
    gdi32.BitBlt(dc2, 0, 0, w, h, dc1, x, y, win32con.SRCCOPY)
    gdi32.SelectObject(dc1, obj1)
    gdi32.SelectObject(dc2, obj2)

    if returnImage:
        data = ctypes.create_string_buffer((w * 4) * h)
        bmi = ctypes.c_buffer(pack("IiiHHIIiiII", calcsize("IiiHHIIiiII"), w, -h, 1, 32, 0, 0, 0, 0, 0, 0))
        gdi32.GetDIBits(dc2, bmp2, 0, h, ctypes.byref(data), ctypes.byref(bmi), win32con.DIB_RGB_COLORS)
        img = Image.frombuffer('RGB', (w, h), data, 'raw', 'BGRX')

    # clean up
    gdi32.DeleteObject(bmp1)
    gdi32.DeleteDC(dc1)
    gdi32.DeleteDC(dc2)
    user32.ReleaseDC(hwnd, dc)

    return (bmp2, w, h, img) if returnImage else (bmp2, w, h)

def copyBitmap(hbmp):
    w32clip.OpenClipboard()
    w32clip.EmptyClipboard()
    w32clip.SetClipboardData(w32clip.CF_BITMAP, hbmp)
    w32clip.CloseClipboard()

def copySnapshot(hwnd):
    hbmp, w, h = getWindowBMAP(hwnd)
    copyBitmap(hbmp)
    gdi32.DeleteObject(hbmp)

def getSnapshot(hwnd):
    hbmp, w, h, img = getWindowBMAP(hwnd, True)
    gdi32.DeleteObject(hbmp)
    return img

def capture_screen(window, output_file="screenshot.png"):
    if not window:
        LOGGER.warning("ウィンドウが指定されていません。")
        return None

    if hasattr(window, 'get') and callable(window.get):
        raw_title = window.get()
    elif hasattr(window, 'title'):
        raw_title = window.title
    else:
        raw_title = window

    if callable(raw_title):
        raw_title = raw_title()

    title = str(raw_title).strip() if raw_title is not None else ""
    if not title:
        LOGGER.warning("有効なウィンドウタイトルが取得できませんでした。")
        return None

    hwnd = win32gui.FindWindow(None, title)

    if not hwnd:
        # 部分一致検索フォールバック
        found = []
        def enum_cb(h, extra):
            if win32gui.IsWindowVisible(h):
                wt = win32gui.GetWindowText(h)
                if wt and (title.lower() in wt.lower() or wt.lower() in title.lower()):
                    extra.append(h)
        win32gui.EnumWindows(enum_cb, found)
        if found:
            hwnd = found[0]
        else:
            LOGGER.warning(f"ウィンドウが見つかりません: {title}")
            return None

    # 1. バックグラウンド対応: PrintWindow (PW_RENDERFULLCONTENT) による直接ウィンドウキャプチャ
    try:
        img = getSnapshot(hwnd)
        if img and img.width > 0 and img.height > 0:
            img.save(output_file, format="PNG")
            LOGGER.info(f"📷 バックグラウンド・スクリーンショット保存完了 (PrintWindow): {title} ({img.width}x{img.height}) -> {output_file}")
            return img
    except Exception as pe:
        LOGGER.debug(f"PrintWindow キャプチャ失敗、フォールバックを試行します: {pe}")

    # 2. フォールバック: mss による画面領域キャプチャ
    try:
        with mss.mss() as sct:
            rect = win32gui.GetWindowRect(hwnd)
            monitor = {
                "top": rect[1],
                "left": rect[0],
                "width": rect[2] - rect[0],
                "height": rect[3] - rect[1]
            }
            
            if monitor["width"] <= 0 or monitor["height"] <= 0:
                LOGGER.warning(f"無効なウィンドウサイズです: {monitor}")
                return None

            sct_img = sct.grab(monitor)
            mss.tools.to_png(sct_img.rgb, sct_img.size, output=output_file)
            LOGGER.info(f"📷 スクリーンショット保存完了 (mssフォールバック): {title} -> {output_file}")
            return Image.open(output_file)
            
    except Exception as e:
        LOGGER.error(f"スクリーンショット取得に失敗しました: {e}")
        return None

def list_available_windows():
    """現在開かれているウィンドウのタイトルリストを取得する"""
    windows = gw.getAllWindows()
    active_windows = [window for window in windows if not window.isMinimized and window.visible]
    window_titles = [window.title for window in active_windows if window.title]
    return window_titles

def get_window_by_title(title):
    """タイトルに一致するウィンドウオブジェクトを取得する"""
    try:
        windows = gw.getWindowsWithTitle(title)
        if windows:
            return windows[0]
        return None
    except Exception:
        return None


class CaptureService:
    def __init__(self, app_logic):
        self.app = app_logic

    def capture_window(self, window_title=None):
        LOGGER.debug("ウィンドウをキャプチャします…")
        try:
            target = window_title or getattr(self.app.state, 'current_window', None) or getattr(self.app.state, 'window_title', None)
            result = capture_screen(target, self.app.state.screenshot_file_path)
            if result:
                self.load_and_display_image(self.app.state.screenshot_file_path)
                return self.app.state.screenshot_file_path
            return None
        except Exception as e:
            LOGGER.error(f"キャプチャできませんでした: {e}")
            return None

    def load_and_display_image(self, image_path):
        # Tkinter UI が存在する場合のみ非同期サムネイル処理を実行
        if hasattr(self.app, 'image_label') and self.app.image_label is not None:
            threading.Thread(target=self.process_image, args=(image_path,), daemon=True).start()

    def process_image(self, image_path):
        try:
            from PIL import ImageTk
            image = Image.open(image_path)
            max_size = (400, 300)
            image.thumbnail(max_size)
            self.app.state.image = ImageTk.PhotoImage(image)
            if hasattr(self.app, 'root') and hasattr(self.app.root, 'after'):
                self.app.root.after(0, self.update_image_label)
        except Exception as e:
            LOGGER.debug(f"Tkinter 画像プレビュー更新スキップ: {e}")

    def update_image_label(self):
        if hasattr(self.app, 'state') and getattr(self.app.state, 'image', None) and hasattr(self.app, 'image_label') and self.app.image_label:
            try:
                self.app.image_label.config(image=self.app.state.image)
            except Exception:
                pass

if __name__ == "__main__":
    window_list = list_available_windows()
    if window_list:
        print("利用可能なウィンドウ:")
        for title in window_list:
            print(title)
    else:
        print("利用可能なウィンドウが見つかりませんでした。")
