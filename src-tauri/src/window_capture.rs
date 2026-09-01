use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::io::Cursor;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use image::{ImageBuffer, Rgba};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
    GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    HDC, SRCCOPY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetDesktopWindow, GetSystemMetrics, GetWindowLongW,
    GetWindowRect, GetWindowTextLengthW, GetWindowTextW, IsIconic, IsWindowVisible, IsZoomed,
    GWL_EXSTYLE, SM_CXSCREEN, SM_CYSCREEN, WS_EX_TOOLWINDOW,
};

#[link(name = "user32")]
extern "system" {
    fn PrintWindow(hwnd: HWND, hdc_blt: HDC, n_flags: u32) -> BOOL;
    fn GetWindowDC(hwnd: HWND) -> HDC;
}

const PW_RENDERFULLCONTENT: u32 = 2;

/// 開いているアクティブウィンドウのタイトル一覧を取得
pub fn list_windows() -> Vec<String> {
    let mut windows: Vec<String> = Vec::new();
    let lparam = &mut windows as *mut Vec<String> as isize;

    unsafe {
        let _ = EnumWindows(Some(enum_window_callback), LPARAM(lparam));
    }

    windows
}

unsafe extern "system" fn enum_window_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let windows = &mut *(lparam.0 as *mut Vec<String>);

    if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
        return BOOL(1);
    }

    let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
    if (ex_style & WS_EX_TOOLWINDOW.0) != 0 {
        return BOOL(1);
    }

    let length = GetWindowTextLengthW(hwnd);
    if length > 0 {
        let mut buffer: Vec<u16> = vec![0; (length + 1) as usize];
        let copied = GetWindowTextW(hwnd, &mut buffer);
        if copied > 0 {
            let title = OsString::from_wide(&buffer[..copied as usize])
                .to_string_lossy()
                .into_owned();

            let trimmed = title.trim();
            // 無視するシステムウィンドウ
            let ignore_titles = [
                "Program Manager",
                "Settings",
                "Microsoft Text Input Application",
                "Windows Input Experience",
            ];

            if !trimmed.is_empty() && !ignore_titles.contains(&trimmed) {
                windows.push(trimmed.to_string());
            }
        }
    }

    BOOL(1)
}

struct SearchContext<'a> {
    target: &'a str,
    found: Option<HWND>,
}

/// 指定したウィンドウタイトルに合致する実体 HWND を探す
fn find_hwnd_by_title(target_title: &str) -> Option<HWND> {
    let clean = target_title.trim();
    if clean.is_empty() {
        return None;
    }

    let mut ctx = SearchContext {
        target: clean,
        found: None,
    };

    let lparam = &mut ctx as *mut SearchContext as isize;
    unsafe {
        let _ = EnumWindows(Some(find_window_callback), LPARAM(lparam));
    }

    ctx.found
}

unsafe extern "system" fn find_window_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = &mut *(lparam.0 as *mut SearchContext);

    if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
        return BOOL(1);
    }

    let mut rect = RECT::default();
    if GetWindowRect(hwnd, &mut rect).is_err() {
        return BOOL(1);
    }

    let w = rect.right - rect.left;
    let h = rect.bottom - rect.top;
    if w < 50 || h < 50 {
        return BOOL(1);
    }

    let length = GetWindowTextLengthW(hwnd);
    if length > 0 {
        let mut buffer: Vec<u16> = vec![0; (length + 1) as usize];
        let copied = GetWindowTextW(hwnd, &mut buffer);
        if copied > 0 {
            let title = OsString::from_wide(&buffer[..copied as usize])
                .to_string_lossy()
                .into_owned();

            let trimmed = title.trim();
            // タイトルの完全一致または部分一致
            if trimmed.eq_ignore_ascii_case(ctx.target)
                || trimmed.to_lowercase().contains(&ctx.target.to_lowercase())
                || ctx.target.to_lowercase().contains(&trimmed.to_lowercase())
            {
                ctx.found = Some(hwnd);
                return BOOL(0); // 有効な実体ウィンドウが見つかったので終了
            }
        }
    }

    BOOL(1)
}

/// ウィンドウをキャプチャし、Base64 画像データ (data:image/png;base64,...) を生成
/// 1. PrintWindow による直接ウィンドウキャプチャ
/// 2. デスクトップ BitBlt (DirectX / Vulkan / Firefox / Chrome GPU描画アプリ)
/// 3. プライマリスクリーン全体
pub fn capture_window_base64(title: &str) -> Option<String> {
    let clean_title = title.trim();
    if clean_title.is_empty() || clean_title == "(No active windows)" || clean_title == "全画面" {
        return capture_primary_screen_base64();
    }

    let hwnd = match find_hwnd_by_title(clean_title) {
        Some(h) => h,
        None => return capture_primary_screen_base64(),
    };

    unsafe {
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() {
            return capture_primary_screen_base64();
        }

        let full_w = rect.right - rect.left;
        let full_h = rect.bottom - rect.top;

        if full_w <= 0 || full_h <= 0 {
            return capture_primary_screen_base64();
        }

        let is_zoomed = IsZoomed(hwnd).as_bool();
        let (crop_x, crop_y, crop_w, crop_h) = if is_zoomed {
            (8, 8, (full_w - 16).max(1), (full_h - 16).max(1))
        } else {
            (7, 0, (full_w - 14).max(1), (full_h - 7).max(1))
        };

        // 1. 裏画面対応: Python の getWindowBMAP と完全同一の GDI パイプライン
        let mut captured_buffer: Option<(i32, i32, Vec<u8>)> = None;
        let hdc_window = GetWindowDC(hwnd);
        if !hdc_window.is_invalid() {
            let mut bi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: crop_w,
                    biHeight: -crop_h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };

            // 試行 1: PrintWindow (PW_RENDERFULLCONTENT = 2)
            {
                let hdc1 = CreateCompatibleDC(hdc_window);
                let hdc2 = CreateCompatibleDC(hdc_window);
                let bmp1 = CreateCompatibleBitmap(hdc_window, full_w, full_h);
                let bmp2 = CreateCompatibleBitmap(hdc_window, crop_w, crop_h);

                let old1 = SelectObject(hdc1, bmp1);
                let old2 = SelectObject(hdc2, bmp2);

                let _ = PrintWindow(hwnd, hdc1, PW_RENDERFULLCONTENT);
                let _ = BitBlt(hdc2, 0, 0, crop_w, crop_h, hdc1, crop_x, crop_y, SRCCOPY);

                // ★重要: GetDIBits 呼び出し前に SelectObject を解除
                SelectObject(hdc1, old1);
                SelectObject(hdc2, old2);

                let mut buffer: Vec<u8> = vec![0; (crop_w * crop_h * 4) as usize];
                GetDIBits(
                    hdc2,
                    bmp2,
                    0,
                    crop_h as u32,
                    Some(buffer.as_mut_ptr() as *mut _),
                    &mut bi,
                    DIB_RGB_COLORS,
                );

                let _ = DeleteObject(bmp1);
                let _ = DeleteObject(bmp2);
                let _ = DeleteDC(hdc1);
                let _ = DeleteDC(hdc2);

                if buffer.iter().any(|&b| b != 0) {
                    captured_buffer = Some((crop_w, crop_h, buffer));
                }
            }

            // 試行 2: レガシー PrintWindow (フラグ 0)
            if captured_buffer.is_none() {
                let hdc1 = CreateCompatibleDC(hdc_window);
                let hdc2 = CreateCompatibleDC(hdc_window);
                let bmp1 = CreateCompatibleBitmap(hdc_window, full_w, full_h);
                let bmp2 = CreateCompatibleBitmap(hdc_window, crop_w, crop_h);

                let old1 = SelectObject(hdc1, bmp1);
                let old2 = SelectObject(hdc2, bmp2);

                let _ = PrintWindow(hwnd, hdc1, 0);
                let _ = BitBlt(hdc2, 0, 0, crop_w, crop_h, hdc1, crop_x, crop_y, SRCCOPY);

                SelectObject(hdc1, old1);
                SelectObject(hdc2, old2);

                let mut buffer: Vec<u8> = vec![0; (crop_w * crop_h * 4) as usize];
                GetDIBits(
                    hdc2,
                    bmp2,
                    0,
                    crop_h as u32,
                    Some(buffer.as_mut_ptr() as *mut _),
                    &mut bi,
                    DIB_RGB_COLORS,
                );

                let _ = DeleteObject(bmp1);
                let _ = DeleteObject(bmp2);
                let _ = DeleteDC(hdc1);
                let _ = DeleteDC(hdc2);

                if buffer.iter().any(|&b| b != 0) {
                    captured_buffer = Some((crop_w, crop_h, buffer));
                }
            }

            // 試行 3: WindowDC から直接 BitBlt
            if captured_buffer.is_none() {
                let hdc2 = CreateCompatibleDC(hdc_window);
                let bmp2 = CreateCompatibleBitmap(hdc_window, crop_w, crop_h);
                let old2 = SelectObject(hdc2, bmp2);

                let _ = BitBlt(hdc2, 0, 0, crop_w, crop_h, hdc_window, crop_x, crop_y, SRCCOPY);

                SelectObject(hdc2, old2);

                let mut buffer: Vec<u8> = vec![0; (crop_w * crop_h * 4) as usize];
                GetDIBits(
                    hdc2,
                    bmp2,
                    0,
                    crop_h as u32,
                    Some(buffer.as_mut_ptr() as *mut _),
                    &mut bi,
                    DIB_RGB_COLORS,
                );

                let _ = DeleteObject(bmp2);
                let _ = DeleteDC(hdc2);

                if buffer.iter().any(|&b| b != 0) {
                    captured_buffer = Some((crop_w, crop_h, buffer));
                }
            }

            ReleaseDC(hwnd, hdc_window);
        }

        // 2. 最終フォールバック
        let (final_w, final_h, final_buffer) = match captured_buffer {
            Some(res) => res,
            None => {
                let hwnd_desktop = GetDesktopWindow();
                let hdc_desktop = GetDC(hwnd_desktop);
                if hdc_desktop.is_invalid() {
                    return capture_primary_screen_base64();
                }

                let hdc_mem = CreateCompatibleDC(hdc_desktop);
                let hbitmap = CreateCompatibleBitmap(hdc_desktop, full_w, full_h);
                let old = SelectObject(hdc_mem, hbitmap);

                let _ = BitBlt(hdc_mem, 0, 0, full_w, full_h, hdc_desktop, rect.left, rect.top, SRCCOPY);

                SelectObject(hdc_mem, old);

                let mut bi = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: full_w,
                        biHeight: -full_h,
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB.0,
                        ..Default::default()
                    },
                    ..Default::default()
                };

                let mut buffer: Vec<u8> = vec![0; (full_w * full_h * 4) as usize];
                GetDIBits(
                    hdc_mem,
                    hbitmap,
                    0,
                    full_h as u32,
                    Some(buffer.as_mut_ptr() as *mut _),
                    &mut bi,
                    DIB_RGB_COLORS,
                );

                let _ = DeleteObject(hbitmap);
                let _ = DeleteDC(hdc_mem);
                ReleaseDC(hwnd_desktop, hdc_desktop);

                (full_w, full_h, buffer)
            }
        };

        // BGRA -> RGBA 変換
        let mut rgba_buffer = final_buffer;
        for chunk in rgba_buffer.chunks_exact_mut(4) {
            chunk.swap(0, 2);
            chunk[3] = 255;
        }

        let img: ImageBuffer<Rgba<u8>, Vec<u8>> = match ImageBuffer::from_raw(final_w as u32, final_h as u32, rgba_buffer) {
            Some(im) => im,
            None => return capture_primary_screen_base64(),
        };

        // メモリ上で PNG エンコード（ファイルウォッチャーの再起動ループを防止）
        let mut png_bytes: Vec<u8> = Vec::new();
        let mut cursor = Cursor::new(&mut png_bytes);
        if img.write_to(&mut cursor, image::ImageFormat::Png).is_ok() {
            let b64 = BASE64.encode(&png_bytes);
            Some(format!("data:image/png;base64,{}", b64))
        } else {
            capture_primary_screen_base64()
        }
    }
}

/// プライマリスクリーン（画面全体）のキャプチャを Base64 文字列（PNG）で取得
pub fn capture_primary_screen_base64() -> Option<String> {
    unsafe {
        let hwnd_desktop = GetDesktopWindow();
        let hdc_screen = GetDC(hwnd_desktop);
        if hdc_screen.is_invalid() {
            return None;
        }

        let width = GetSystemMetrics(SM_CXSCREEN);
        let height = GetSystemMetrics(SM_CYSCREEN);

        if width <= 0 || height <= 0 {
            ReleaseDC(hwnd_desktop, hdc_screen);
            return None;
        }

        let hdc_mem = CreateCompatibleDC(hdc_screen);
        let hbitmap = CreateCompatibleBitmap(hdc_screen, width, height);
        let h_old = SelectObject(hdc_mem, hbitmap);

        let _ = BitBlt(hdc_mem, 0, 0, width, height, hdc_screen, 0, 0, SRCCOPY);

        let mut bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut buffer: Vec<u8> = vec![0; (width * height * 4) as usize];
        GetDIBits(
            hdc_mem,
            hbitmap,
            0,
            height as u32,
            Some(buffer.as_mut_ptr() as *mut _),
            &mut bi,
            DIB_RGB_COLORS,
        );

        SelectObject(hdc_mem, h_old);
        let _ = DeleteObject(hbitmap);
        let _ = DeleteDC(hdc_mem);
        ReleaseDC(hwnd_desktop, hdc_screen);

        let mut rgba_buffer = buffer;
        for chunk in rgba_buffer.chunks_exact_mut(4) {
            chunk.swap(0, 2);
            chunk[3] = 255;
        }

        let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
            ImageBuffer::from_raw(width as u32, height as u32, rgba_buffer)?;

        let mut png_bytes: Vec<u8> = Vec::new();
        let mut cursor = Cursor::new(&mut png_bytes);
        if img.write_to(&mut cursor, image::ImageFormat::Png).is_ok() {
            let b64 = BASE64.encode(&png_bytes);
            Some(format!("data:image/png;base64,{}", b64))
        } else {
            None
        }
    }
}
