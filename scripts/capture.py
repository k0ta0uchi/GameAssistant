import pygetwindow as gw
import ctypes, win32con, win32gui
import win32clipboard as w32clip
from struct import pack, calcsize
from ctypes import windll, wintypes
from PIL import Image
user32,gdi32 = windll.user32,windll.gdi32
PW_RENDERFULLCONTENT = 2

def getWindowBMAP(hwnd,returnImage=False):
    # get Window size and crop pos/size
    L,T,R,B = win32gui.GetWindowRect(hwnd); W,H = R-L,B-T
    x,y,w,h = (8,8,W-16,H-16) if user32.IsZoomed(hwnd) else (7,0,W-14,H-7)

    # create dc's and bmp's
    dc = user32.GetWindowDC(hwnd)
    dc1,dc2 = gdi32.CreateCompatibleDC(dc),gdi32.CreateCompatibleDC(dc)
    bmp1,bmp2 = gdi32.CreateCompatibleBitmap(dc,W,H),gdi32.CreateCompatibleBitmap(dc,w,h)

    # render dc1 and dc2 (bmp1 and bmp2) (uncropped and cropped)
    obj1,obj2 = gdi32.SelectObject(dc1,bmp1),gdi32.SelectObject(dc2,bmp2) # select bmp's into dc's
    user32.PrintWindow(hwnd,dc1,PW_RENDERFULLCONTENT) # render window to dc1
    gdi32.BitBlt(dc2,0,0,w,h,dc1,x,y,win32con.SRCCOPY) # copy dc1 (x,y,w,h) to dc2 (0,0,w,h)
    gdi32.SelectObject(dc1,obj1); gdi32.SelectObject(dc2,obj2) # restore dc's default obj's

    if returnImage: # create Image from bmp2
        data = ctypes.create_string_buffer((w*4)*h)
        bmi = ctypes.c_buffer(pack("IiiHHIIiiII",calcsize("IiiHHIIiiII"),w,-h,1,32,0,0,0,0,0,0))
        gdi32.GetDIBits(dc2,bmp2,0,h,ctypes.byref(data),ctypes.byref(bmi),win32con.DIB_RGB_COLORS)
        img = Image.frombuffer('RGB',(w,h),data,'raw','BGRX')

    # clean up
    gdi32.DeleteObject(bmp1) # delete bmp1 (uncropped)
    gdi32.DeleteDC(dc1); gdi32.DeleteDC(dc2) # delete created dc's
    user32.ReleaseDC(hwnd,dc) # release retrieved dc

    return (bmp2,w,h,img) if returnImage else (bmp2,w,h)

def copyBitmap(hbmp): # copy HBITMAP to clipboard
    w32clip.OpenClipboard(); w32clip.EmptyClipboard()
    w32clip.SetClipboardData(w32clip.CF_BITMAP,hbmp); w32clip.CloseClipboard()

def copySnapshot(hwnd): # copy Window HBITMAP to clipboard
    hbmp,w,h = getWindowBMAP(hwnd); copyBitmap(hbmp); gdi32.DeleteObject(hbmp)

def getSnapshot(hwnd): # get Window HBITMAP as Image
    hbmp,w,h,img = getWindowBMAP(hwnd,True); gdi32.DeleteObject(hbmp); return img

def capture_screen(window, output_file="screenshot.png"):
    screenshot = None
    hwnd = win32gui.FindWindow(None, window.title)

    # ウィンドウのスクリーンショットを取得
    screenshot = getSnapshot(hwnd)

    # 取得した画像を保存
    # screenshot_img = Image.frombytes("RGB", screenshot.size, screenshot.bgra, "raw", "BGRX")
    screenshot.save(output_file)

    print(f"スクリーンショットが保存されました: screenshot.png")
    return screenshot

def list_available_windows():
    """現在開かれているウィンドウのタイトルリストを取得する"""
    windows = gw.getAllWindows()
    active_windows = [window for window in windows if not window.isMinimized and window.visible]
    window_titles = [window.title for window in active_windows if window.title]  # 空のタイトルを除外
    return window_titles

def get_window_by_title(title):
    """タイトルに一致するウィンドウオブジェクトを取得する"""
    try:
        window = gw.getWindowsWithTitle(title)[0]  # 最初に見つかったウィンドウを返す
        return window
    except IndexError:
        return None  # ウィンドウが見つからない場合

if __name__ == "__main__":
    # ウィンドウの一覧を表示し、ユーザーに選択させる
    window_list = list_available_windows()
    if window_list:
        print("利用可能なウィンドウ:")
        for title in window_list:
            print(title)
    else:
        print("利用可能なウィンドウが見つかりませんでした。")