//! Target-only WGC capture. No desktop, foreground, or title-substring fallback.
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use image::{ImageBuffer, Rgba};
use std::{
    collections::HashMap,
    io::Cursor,
    sync::{mpsc, Mutex, OnceLock},
    time::Duration,
};
use windows::{
    core::PWSTR,
    Win32::{
        Foundation::{CloseHandle, BOOL, HWND, LPARAM, POINT, RECT},
        Graphics::{
            Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS},
            Gdi::ClientToScreen,
        },
        System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
            PROCESS_QUERY_LIMITED_INFORMATION,
        },
        UI::WindowsAndMessaging::{
            EnumWindows, GetClassNameW, GetClientRect, GetWindow, GetWindowLongW, GetWindowTextW,
            GetWindowThreadProcessId, IsIconic, IsWindowVisible, GWL_STYLE, GW_OWNER,
        },
    },
};
use windows_capture::{
    capture::{Context, GraphicsCaptureApiHandler},
    frame::Frame,
    graphics_capture_api::InternalCaptureControl,
    settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
        MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
    },
    window::Window,
};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WindowIdentity {
    pub hwnd: usize,
    pub pid: u32,
    pub thread_id: u32,
    pub title: String,
    pub class_name: String,
    pub executable: String,
    pub is_root_owner: bool,
    pub style: u32,
}
fn read_identity(hwnd: HWND) -> Option<WindowIdentity> {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            return None;
        }
        let mut title = [0u16; 4096];
        let n = GetWindowTextW(hwnd, &mut title);
        if n == 0 {
            return None;
        }
        let mut class = [0u16; 256];
        let c = GetClassNameW(hwnd, &mut class);
        let mut pid = 0;
        let thread_id = GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut exe = vec![0u16; 32768];
        let mut size = exe.len() as u32;
        let result = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_FORMAT(0),
            PWSTR(exe.as_mut_ptr()),
            &mut size,
        );
        let _ = CloseHandle(process);
        result.ok()?;
        let owner = GetWindow(hwnd, GW_OWNER).unwrap_or_default();
        let is_root_owner = owner.0 as usize == 0;
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        Some(WindowIdentity {
            hwnd: hwnd.0 as usize,
            pid,
            thread_id,
            title: String::from_utf16_lossy(&title[..n as usize]),
            class_name: String::from_utf16_lossy(&class[..c as usize]),
            executable: String::from_utf16_lossy(&exe[..size as usize]),
            is_root_owner,
            style,
        })
    }
}
unsafe extern "system" fn enumerate(hwnd: HWND, arg: LPARAM) -> BOOL {
    if let Some(identity) = read_identity(hwnd) {
        (*(arg.0 as *mut Vec<WindowIdentity>)).push(identity);
    }
    BOOL(1)
}
fn identities() -> Vec<WindowIdentity> {
    let mut result = Vec::new();
    unsafe {
        let _ = EnumWindows(
            Some(enumerate),
            LPARAM(&mut result as *mut Vec<WindowIdentity> as isize),
        );
    }
    result
}
pub fn list_windows() -> Vec<String> {
    let mut titles: Vec<_> = identities().into_iter().map(|w| w.title).collect();
    titles.sort();
    titles.dedup();
    titles
}
fn same_process_and_window_type(a: &WindowIdentity, b: &WindowIdentity) -> bool {
    a.pid == b.pid
        && a.thread_id == b.thread_id
        && a.class_name == b.class_name
        && a.is_root_owner == b.is_root_owner
        && a.executable.eq_ignore_ascii_case(&b.executable)
}
pub(crate) fn title_compatible(target_title: &str, candidate_title: &str) -> bool {
    let t = target_title.trim();
    let c = candidate_title.trim();
    if t.is_empty() || c.is_empty() {
        return false;
    }
    if t.eq_ignore_ascii_case(c) {
        return true;
    }
    let t_lower = t.to_lowercase();
    let c_lower = c.to_lowercase();
    if t_lower.len() >= 3 && c_lower.contains(&t_lower) {
        return true;
    }
    if c_lower.len() >= 3 && t_lower.contains(&c_lower) {
        return true;
    }
    let t_first = t
        .split(|ch: char| {
            ch.is_whitespace() || ch == '-' || ch == ':' || ch == '|' || ch == '[' || ch == '('
        })
        .find(|s| !s.is_empty());
    let c_first = c
        .split(|ch: char| {
            ch.is_whitespace() || ch == '-' || ch == ':' || ch == '|' || ch == '[' || ch == '('
        })
        .find(|s| !s.is_empty());
    if let (Some(tf), Some(cf)) = (t_first, c_first) {
        if tf.len() >= 3 && tf.eq_ignore_ascii_case(cf) {
            return true;
        }
    }
    false
}
fn resolve_identity(
    target: &WindowIdentity,
    candidates: &[WindowIdentity],
) -> Option<WindowIdentity> {
    // 1. HWND 一致時: 同一プロセス・属性かつタイトルが互換であることを必須とする。
    // HWNDが同一プロセス内で再利用されて別用途のウィンドウになった場合は拒否する。
    if let Some(found) = candidates.iter().find(|w| {
        w.hwnd == target.hwnd
            && same_process_and_window_type(target, w)
            && title_compatible(&target.title, &w.title)
    }) {
        return Some(found.clone());
    }
    // 2. HWND 再生成時（ロード画面・画面遷移等）:
    // 同一プロセス・同一属性の候補を抽出
    let compatible: Vec<_> = candidates
        .iter()
        .filter(|w| same_process_and_window_type(target, w))
        .collect();
    // 2a. タイトル完全一致が一意にあれば採用
    let exact: Vec<_> = compatible
        .iter()
        .filter(|w| w.title == target.title)
        .collect();
    if exact.len() == 1 {
        return Some((**exact[0]).clone());
    }
    // 2b. タイトル互換（ロード中などのタイトル変化）の候補を探す
    let title_matches: Vec<_> = compatible
        .iter()
        .filter(|w| title_compatible(&target.title, &w.title))
        .collect();
    if title_matches.len() == 1 {
        return Some((**title_matches[0]).clone());
    }
    // 候補が複数（曖昧）または互換タイトルがない場合は別ウィンドウ誤認を防ぐため None
    None
}
static TARGETS: OnceLock<Mutex<HashMap<String, WindowIdentity>>> = OnceLock::new();
static CAPTURE_GATE: Mutex<()> = Mutex::new(());
/// An explicit user selection may bind a restarted process. Recovery never changes PID.
pub fn select_target(title: &str) {
    if let Ok(mut targets) = TARGETS.get_or_init(|| Mutex::new(HashMap::new())).lock() {
        targets.remove(title);
    }
    let _ = target_for(title);
}
fn target_for(title: &str) -> Option<WindowIdentity> {
    if title.trim().is_empty() {
        return None;
    }
    let all = identities();
    let mut targets = TARGETS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .ok()?;
    let target = if let Some(original) = targets.get(title) {
        resolve_identity(original, &all)?
    } else {
        let matches: Vec<_> = all.iter().filter(|w| w.title == title).collect();
        if matches.len() != 1 {
            return None;
        }
        matches[0].clone()
    };
    targets.insert(title.to_string(), target.clone());
    Some(target)
}
// Same coordinate mapping as OBS libobs-winrt get_client_box, rejecting uncertain bounds.
fn client_box(target: &WindowIdentity, width: u32, height: u32) -> Option<(u32, u32, u32, u32)> {
    unsafe {
        let hwnd = HWND(target.hwnd as *mut _);
        if IsIconic(hwnd).as_bool() {
            return None;
        }
        let mut client = RECT::default();
        let mut bounds = RECT::default();
        let mut origin = POINT::default();
        GetClientRect(hwnd, &mut client).ok()?;
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut bounds as *mut _ as *mut _,
            std::mem::size_of::<RECT>() as u32,
        )
        .ok()?;
        if bounds.right - bounds.left != width as i32 || bounds.bottom - bounds.top != height as i32
        {
            return None;
        }
        if !ClientToScreen(hwnd, &mut origin).as_bool() {
            return None;
        }
        let x = u32::try_from(origin.x - bounds.left).ok()?;
        let y = u32::try_from(origin.y - bounds.top).ok()?;
        let right = x.checked_add(u32::try_from(client.right - client.left).ok()?)?;
        let bottom = y.checked_add(u32::try_from(client.bottom - client.top).ok()?)?;
        if right > width || bottom > height || x >= right || y >= bottom {
            return None;
        }
        Some((x, y, right, bottom))
    }
}
type CaptureError = Box<dyn std::error::Error + Send + Sync>;
struct Snapshot {
    target: WindowIdentity,
    sender: mpsc::SyncSender<Result<String, String>>,
}
impl GraphicsCaptureApiHandler for Snapshot {
    type Flags = (WindowIdentity, mpsc::SyncSender<Result<String, String>>);
    type Error = CaptureError;
    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self {
            target: ctx.flags.0,
            sender: ctx.flags.1,
        })
    }
    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        let result = (|| -> Result<String, CaptureError> {
            let before = read_identity(HWND(self.target.hwnd as *mut _)).ok_or("target closed")?;
            if !same_process_and_window_type(&self.target, &before)
                || !title_compatible(&self.target.title, &before.title)
            {
                return Err("target identity changed or reused by different window".into());
            }
            let crop = client_box(&self.target, frame.width(), frame.height())
                .ok_or("client bounds unavailable or resized")?;
            let buffer = frame.buffer_crop(crop.0, crop.1, crop.2, crop.3)?;
            let mut packed = Vec::new();
            let pixels = buffer.as_nopadding_buffer(&mut packed).to_vec();
            drop(buffer);
            let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
                ImageBuffer::from_raw(crop.2 - crop.0, crop.3 - crop.1, pixels)
                    .ok_or("invalid frame")?;
            let mut bytes = Cursor::new(Vec::new());
            img.write_to(&mut bytes, image::ImageFormat::Png)?;
            let after = read_identity(HWND(self.target.hwnd as *mut _))
                .ok_or("target closed during capture")?;
            if !same_process_and_window_type(&self.target, &after)
                || !title_compatible(&self.target.title, &after.title)
                || client_box(&self.target, frame.width(), frame.height()) != Some(crop)
            {
                return Err("target changed during capture".into());
            }
            Ok(format!(
                "data:image/png;base64,{}",
                BASE64.encode(bytes.into_inner())
            ))
        })()
        .map_err(|e| e.to_string());
        let _ = self.sender.try_send(result);
        control.stop();
        Ok(())
    }
    fn on_closed(&mut self) -> Result<(), Self::Error> {
        let _ = self.sender.try_send(Err("target closed".into()));
        Ok(())
    }
}
pub fn capture_window_base64(title: &str) -> Option<String> {
    capture_window_with_log(title, None)
}
pub fn capture_window_with_log(
    title: &str,
    log: Option<&crate::logger::LogManager>,
) -> Option<String> {
    let result = capture_target(title, log);
    if let Some(log) = log {
        match &result {
            Ok((target, _)) => log.info("Capture", &format!("target_title={:?} target_hwnd={} target_pid={} target_executable={:?} target_class={:?} capture_method=WGC capture_hwnd={} capture_status=ready reconnect_reason=snapshot_session fallback_reason=none", title, target.hwnd, target.pid, target.executable, target.class_name, target.hwnd)),
            Err(error) => log.warn("Capture", &format!("target_title={:?} capture_method=WGC capture_status=CaptureUnavailable reconnect_reason={:?} fallback_reason=disabled", title, error)),
        }
    }
    result.ok().map(|(_, image)| image)
}
fn capture_target(
    title: &str,
    log: Option<&crate::logger::LogManager>,
) -> Result<(WindowIdentity, String), String> {
    let _guard = CAPTURE_GATE.try_lock().map_err(|_| "capture_busy")?;
    let target = target_for(title).ok_or("target missing, inaccessible, or ambiguous")?;
    if let Some(log) = log {
        log.info("Capture", &format!("target_title={:?} target_hwnd={} target_pid={} target_executable={:?} target_class={:?} capture_method=WGC capture_hwnd={} capture_status=starting reconnect_reason=new_snapshot_session fallback_reason=disabled", title, target.hwnd, target.pid, target.executable, target.class_name, target.hwnd));
    }
    let (sender, receiver) = mpsc::sync_channel(1);
    let settings = Settings::new(
        Window::from_raw_hwnd(target.hwnd as *mut _),
        CursorCaptureSettings::WithoutCursor,
        DrawBorderSettings::Default,
        SecondaryWindowSettings::Exclude,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Rgba8,
        (target.clone(), sender),
    );
    let control = Snapshot::start_free_threaded(settings).map_err(|e| e.to_string())?;
    let result = receiver
        .recv_timeout(Duration::from_secs(2))
        .map_err(|_| "frame timeout".to_string())
        .and_then(|r| r);
    let _ = control.stop();
    result.map(|image| (target, image))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn target(hwnd: usize, pid: u32) -> WindowIdentity {
        WindowIdentity {
            hwnd,
            pid,
            thread_id: 1,
            title: "AION2".into(),
            class_name: "game".into(),
            executable: "game.exe".into(),
            is_root_owner: true,
            style: 0x14cf0000,
        }
    }
    #[test]
    fn empty_target_does_not_capture_desktop() {
        assert!(capture_window_base64("").is_none());
    }
    #[test]
    fn platform_missing_target_does_not_capture_desktop() {
        assert!(capture_window_base64("GameAssistant-nonexistent-window-54-unique").is_none());
    }
    #[test]
    fn reconnect_never_selects_another_process_or_ambiguous_window() {
        let original = target(10, 42);
        assert_eq!(
            resolve_identity(&original, &[target(11, 42)]).unwrap().hwnd,
            11
        );
        assert!(resolve_identity(&original, &[target(10, 43)]).is_none());
        assert!(resolve_identity(&original, &[target(11, 42), target(12, 42)]).is_none());
        let mut other = target(11, 42);
        other.executable = "firefox.exe".into();
        assert!(resolve_identity(&original, &[other]).is_none());
    }
    #[test]
    fn unchanged_hwnd_must_have_compatible_title_and_attributes() {
        let original = target(10, 42);
        // ロード画面等の互換タイトルなら同一HWNDで受理
        let mut loading = original.clone();
        loading.title = "AION2 - Loading".into();
        assert_eq!(resolve_identity(&original, &[loading]).unwrap().hwnd, 10);

        // HWNDが再利用されて全く別用途（非互換タイトル）になった場合は拒否
        let mut crash_dialog = original.clone();
        crash_dialog.title = "Crash Reporter".into();
        assert!(resolve_identity(&original, &[crash_dialog]).is_none());

        // HWNDが同一でも別スレッドやオーナー持ち（ダイアログ）の場合は拒否
        let mut child_dialog = original.clone();
        child_dialog.is_root_owner = false;
        assert!(resolve_identity(&original, &[child_dialog]).is_none());

        let mut diff_thread = original.clone();
        diff_thread.thread_id = 99;
        assert!(resolve_identity(&original, &[diff_thread]).is_none());
    }
    #[test]
    fn reconnected_hwnd_supports_compatible_title_if_unique() {
        let original = target(10, 42);
        // HWND再生成かつタイトルが "AION2 [DX11]" に変化した場合も一意なら復帰
        let mut regenerated = target(11, 42);
        regenerated.title = "AION2 [DX11]".into();
        assert_eq!(
            resolve_identity(&original, &[regenerated]).unwrap().hwnd,
            11
        );

        // HWND再生成だがタイトルが無関係な場合は拒否
        let mut unrelated = target(11, 42);
        unrelated.title = "Setup Wizard".into();
        assert!(resolve_identity(&original, &[unrelated]).is_none());
    }
    #[test]
    fn title_compatible_rules() {
        assert!(title_compatible("AION2", "AION2"));
        assert!(title_compatible("AION2", "aion2"));
        assert!(title_compatible("AION2", "AION2 - Chapter 1"));
        assert!(title_compatible("AION2 - Chapter 1", "AION2"));
        assert!(title_compatible("AION2", "AION2 [DirectX 11]"));
        assert!(title_compatible("Firefox", "GitHub — Mozilla Firefox"));
        assert!(!title_compatible("AION2", "Crash Reporter"));
        assert!(!title_compatible("AION2", "Settings"));
        assert!(!title_compatible("AION2", ""));
        assert!(!title_compatible("", "AION2"));
    }
}
