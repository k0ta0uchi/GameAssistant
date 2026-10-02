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
            EnumWindows, GetClassNameW, GetClientRect, GetWindowTextW, GetWindowThreadProcessId,
            IsIconic, IsWindowVisible,
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
    pub title: String,
    pub class_name: String,
    pub executable: String,
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
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
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
        Some(WindowIdentity {
            hwnd: hwnd.0 as usize,
            pid,
            title: String::from_utf16_lossy(&title[..n as usize]),
            class_name: String::from_utf16_lossy(&class[..c as usize]),
            executable: String::from_utf16_lossy(&exe[..size as usize]),
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
fn same_process(a: &WindowIdentity, b: &WindowIdentity) -> bool {
    a.pid == b.pid
        && a.class_name == b.class_name
        && a.executable.eq_ignore_ascii_case(&b.executable)
}
fn resolve_identity(
    target: &WindowIdentity,
    candidates: &[WindowIdentity],
) -> Option<WindowIdentity> {
    if let Some(found) = candidates
        .iter()
        .find(|w| w.hwnd == target.hwnd && same_process(target, w))
    {
        return Some(found.clone());
    }
    let compatible: Vec<_> = candidates
        .iter()
        .filter(|w| same_process(target, w))
        .collect();
    let exact: Vec<_> = compatible
        .iter()
        .filter(|w| w.title == target.title)
        .collect();
    if exact.len() == 1 {
        return Some((**exact[0]).clone());
    }
    if compatible.len() == 1 {
        return Some(compatible[0].clone());
    }
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
            if !same_process(&self.target, &before) {
                return Err("target identity changed".into());
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
            if !same_process(&self.target, &after)
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
            title: "AION2".into(),
            class_name: "game".into(),
            executable: "game.exe".into(),
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
    fn unchanged_hwnd_must_still_belong_to_the_target() {
        let original = target(10, 42);
        let mut changed_title = original.clone();
        changed_title.title = "Loading".into();
        assert_eq!(
            resolve_identity(&original, &[changed_title]).unwrap().hwnd,
            10
        );
    }
}
