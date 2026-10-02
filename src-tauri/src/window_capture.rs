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
            GetWindowThreadProcessId, IsIconic, IsWindowVisible, GWL_STYLE, GW_OWNER, WS_CHILD,
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
/// ウィンドウ種別（トップレベル vs 子コントロール）を区別する安定ビット。
/// WS_MAXIMIZE, WS_MINIMIZE などの状態ビットや、フルスクリーン/ボーダレス切替に伴う
/// 枠スタイル変化（WS_POPUP, WS_CAPTION, WS_THICKFRAME 等）は同一ウィンドウでも動的に変化するため除外する。
const WINDOW_TYPE_STYLE_MASK: u32 = WS_CHILD.0;

fn same_process_and_window_type(a: &WindowIdentity, b: &WindowIdentity) -> bool {
    a.pid == b.pid
        && a.thread_id == b.thread_id
        && a.class_name == b.class_name
        && a.is_root_owner == b.is_root_owner
        && (a.style & WINDOW_TYPE_STYLE_MASK) == (b.style & WINDOW_TYPE_STYLE_MASK)
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

    // クラッシュレポーターや設定ダイアログ、アップデータ等の非ゲーム・補助ウィンドウキーワードは拒否
    const DANGEROUS_KEYWORDS: &[&str] = &[
        "crash",
        "reporter",
        "report",
        "error",
        "bug",
        "diagnostic",
        "updater",
        "update",
        "installer",
        "setup",
        "wizard",
        "config",
        "settings",
        "feedback",
    ];
    for kw in DANGEROUS_KEYWORDS {
        if c_lower.contains(kw) && !t_lower.contains(kw) {
            return false;
        }
    }

    // ブラウザ等でよく見られる「<Page Title> <Separator> [Vendor] <App Name>」パターン（末尾一致）
    // 例: target = "Firefox", candidate = "GitHub — Mozilla Firefox"
    // 例: target = "Chrome", candidate = "YouTube - Google Chrome"
    if let Some((_, right)) = c.rsplit_once(['—', '-', '|', '–', ':']) {
        let right_lower = right.trim().to_lowercase();
        if right_lower == t_lower
            || right_lower.ends_with(&t_lower)
            || right_lower.starts_with(&t_lower)
        {
            return true;
        }
    }

    // ゲーム等で安全な「<Game Title><Separator><Safe Suffix>」パターン
    // target が candidate の先頭にあり、直後に安全な区切り（括弧や ' - ', ': ', ' | '）で続く
    if c_lower.starts_with(&t_lower) {
        let remainder = &c[t.len()..];
        let trimmed_remainder = remainder.trim_start();
        if trimmed_remainder.starts_with(['(', '[']) {
            if is_safe_suffix(trimmed_remainder) {
                return true;
            }
        } else if remainder.starts_with(" - ")
            || remainder.starts_with(": ")
            || remainder.starts_with(" | ")
        {
            let suffix = remainder[3..].trim();
            if is_safe_suffix(suffix) {
                return true;
            }
        }
    }

    // 逆に元の target が詳細タイトル（"AION2 - Chapter 1"）で、candidate がベースタイトル（"AION2"）の場合
    if t_lower.starts_with(&c_lower) {
        let remainder = &t[c.len()..];
        let trimmed_remainder = remainder.trim_start();
        if trimmed_remainder.starts_with(['(', '[']) {
            if is_safe_suffix(trimmed_remainder) {
                return true;
            }
        } else if remainder.starts_with(" - ")
            || remainder.starts_with(": ")
            || remainder.starts_with(" | ")
        {
            let suffix = remainder[3..].trim();
            if is_safe_suffix(suffix) {
                return true;
            }
        }
    }

    false
}

fn is_safe_suffix(suffix: &str) -> bool {
    let mut s = suffix.trim();
    if s.is_empty() {
        return false;
    }
    // 括弧で囲まれたサフィックス: (DirectX 11), [DX11], (64-bit), [Loading] 等
    // 括弧を外して中身を検証する（[Other Window] 等の未知サフィックスを拒否）
    if (s.starts_with('(') && s.ends_with(')')) || (s.starts_with('[') && s.ends_with(']')) {
        s = s[1..s.len() - 1].trim();
        if s.is_empty() {
            return false;
        }
    }
    let s_lower = s.to_lowercase();
    // ゲーム進行・章・ゾーン・サーバー・状態・レンダラー・ビルド等の安全な接頭辞
    const SAFE_PREFIXES: &[&str] = &[
        "chapter",
        "act",
        "episode",
        "part",
        "stage",
        "level",
        "zone",
        "server",
        "realm",
        "world",
        "loading",
        "connecting",
        "running",
        "paused",
        "in-game",
        "game",
        "play",
        "directx",
        "dx9",
        "dx10",
        "dx11",
        "dx12",
        "vulkan",
        "opengl",
        "64-bit",
        "32-bit",
        "x64",
        "x86",
        "build",
        "ver",
        "v0",
        "v1",
        "v2",
        "v3",
    ];
    for pfx in SAFE_PREFIXES {
        if s_lower.starts_with(pfx) {
            return true;
        }
    }
    // 数字・バージョン表記（例: "1.0.3", "1234"）
    if s.chars().all(|ch| ch.is_ascii_digit() || ch == '.') {
        return true;
    }
    false
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TargetBinding {
    pub base_title: String,
    pub identity: WindowIdentity,
}

pub(crate) fn is_title_compatible_with_binding(
    binding: &TargetBinding,
    candidate_title: &str,
) -> bool {
    candidate_title.eq_ignore_ascii_case(&binding.base_title)
        || candidate_title.eq_ignore_ascii_case(&binding.identity.title)
        || title_compatible(&binding.base_title, candidate_title)
        || title_compatible(&binding.identity.title, candidate_title)
}

fn resolve_binding(
    binding: &TargetBinding,
    candidates: &[WindowIdentity],
) -> Option<WindowIdentity> {
    // 1. HWND 一致時: 同一プロセス・属性かつタイトルが互換であることを必須とする。
    // HWNDが同一プロセス内で再利用されて別用途のウィンドウになった場合は拒否する。
    if let Some(found) = candidates.iter().find(|w| {
        w.hwnd == binding.identity.hwnd
            && same_process_and_window_type(&binding.identity, w)
            && is_title_compatible_with_binding(binding, &w.title)
    }) {
        return Some(found.clone());
    }
    // 2. HWND 再生成時（ロード画面・画面遷移等）:
    // 同一プロセス・同一属性の候補を抽出
    let compatible: Vec<_> = candidates
        .iter()
        .filter(|w| same_process_and_window_type(&binding.identity, w))
        .collect();
    // 2a. base_title または last title に完全一致が一意にあれば採用
    let exact: Vec<_> = compatible
        .iter()
        .filter(|w| {
            w.title.eq_ignore_ascii_case(&binding.base_title)
                || w.title.eq_ignore_ascii_case(&binding.identity.title)
        })
        .collect();
    if exact.len() == 1 {
        return Some((**exact[0]).clone());
    }
    // 2b. タイトル互換（ロード中などのタイトル変化）の候補を探す
    let title_matches: Vec<_> = compatible
        .iter()
        .filter(|w| is_title_compatible_with_binding(binding, &w.title))
        .collect();
    if title_matches.len() == 1 {
        return Some((**title_matches[0]).clone());
    }
    // 候補が複数（曖昧）または互換タイトルがない場合は別ウィンドウ誤認を防ぐため None
    None
}

#[cfg(test)]
fn resolve_identity(
    target: &WindowIdentity,
    candidates: &[WindowIdentity],
) -> Option<WindowIdentity> {
    let binding = TargetBinding {
        base_title: target.title.clone(),
        identity: target.clone(),
    };
    resolve_binding(&binding, candidates)
}

static TARGETS: OnceLock<Mutex<HashMap<String, TargetBinding>>> = OnceLock::new();
static CAPTURE_GATE: Mutex<()> = Mutex::new(());
/// An explicit user selection may bind a restarted process. Recovery never changes PID.
pub fn select_target(title: &str) {
    if let Ok(mut targets) = TARGETS.get_or_init(|| Mutex::new(HashMap::new())).lock() {
        targets.remove(title);
    }
    let _ = target_for(title);
}
fn target_for(title: &str) -> Option<TargetBinding> {
    if title.trim().is_empty() {
        return None;
    }
    let all = identities();
    let mut targets = TARGETS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .ok()?;
    let updated_binding = if let Some(existing) = targets.get(title) {
        let resolved = resolve_binding(existing, &all)?;
        TargetBinding {
            base_title: existing.base_title.clone(),
            identity: resolved,
        }
    } else {
        let matches: Vec<_> = all.iter().filter(|w| w.title == title).collect();
        if matches.len() != 1 {
            return None;
        }
        let matched = matches[0].clone();
        TargetBinding {
            base_title: title.to_string(),
            identity: matched,
        }
    };
    targets.insert(title.to_string(), updated_binding.clone());
    Some(updated_binding)
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
    binding: TargetBinding,
    sender: mpsc::SyncSender<Result<String, String>>,
}
impl GraphicsCaptureApiHandler for Snapshot {
    type Flags = (TargetBinding, mpsc::SyncSender<Result<String, String>>);
    type Error = CaptureError;
    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self {
            binding: ctx.flags.0,
            sender: ctx.flags.1,
        })
    }
    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        let result = (|| -> Result<String, CaptureError> {
            let before =
                read_identity(HWND(self.binding.identity.hwnd as *mut _)).ok_or("target closed")?;
            if !same_process_and_window_type(&self.binding.identity, &before)
                || !is_title_compatible_with_binding(&self.binding, &before.title)
            {
                return Err("target identity changed or reused by different window".into());
            }
            let crop = client_box(&self.binding.identity, frame.width(), frame.height())
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
            let after = read_identity(HWND(self.binding.identity.hwnd as *mut _))
                .ok_or("target closed during capture")?;
            if !same_process_and_window_type(&self.binding.identity, &after)
                || !is_title_compatible_with_binding(&self.binding, &after.title)
                || client_box(&self.binding.identity, frame.width(), frame.height()) != Some(crop)
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
    let binding = target_for(title).ok_or("target missing, inaccessible, or ambiguous")?;
    let target = binding.identity.clone();
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
        (binding, sender),
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

        // 対象タイトル "AION2" を接頭辞に含む "AION2 Crash Reporter" の場合も拒否
        let mut aion2_crash = original.clone();
        aion2_crash.title = "AION2 Crash Reporter".into();
        assert!(resolve_identity(&original, &[aion2_crash]).is_none());

        // HWNDが同一でも、未知の補助ウィンドウ名 ("AION2 - Other Window") の場合は拒否
        // HWNDが同一でも、未知の補助ウィンドウ名 ("AION2 - Other Window", "AION2 [Other Window]") の場合は拒否
        let mut other_window = original.clone();
        other_window.title = "AION2 - Other Window".into();
        assert!(resolve_identity(&original, &[other_window]).is_none());

        let mut bracketed_other = original.clone();
        bracketed_other.title = "AION2 [Other Window]".into();
        assert!(resolve_identity(&original, &[bracketed_other]).is_none());

        // HWNDが同一でも別スレッドやオーナー持ち（ダイアログ）の場合は拒否
        let mut child_dialog = original.clone();
        child_dialog.is_root_owner = false;
        assert!(resolve_identity(&original, &[child_dialog]).is_none());

        let mut diff_thread = original.clone();
        diff_thread.thread_id = 99;
        assert!(resolve_identity(&original, &[diff_thread]).is_none());

        // HWNDが同一でも子ウィンドウ（WS_CHILD）種別に変化した場合は拒否
        let mut child_style = original.clone();
        child_style.style |= WS_CHILD.0;
        assert!(resolve_identity(&original, &[child_style]).is_none());
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

        // HWND再生成だが "AION2 Crash Reporter" のような危険サフィックスの場合は拒否
        let mut crash_regen = target(11, 42);
        crash_regen.title = "AION2 Crash Reporter".into();
        assert!(resolve_identity(&original, &[crash_regen]).is_none());

        // HWND再生成だが未知のサフィックス ("AION2 - Other Window", "AION2 [Launcher]") の場合は拒否
        let mut other_regen = target(11, 42);
        other_regen.title = "AION2 - Other Window".into();
        assert!(resolve_identity(&original, &[other_regen]).is_none());

        let mut launcher_regen = target(11, 42);
        launcher_regen.title = "AION2 - Launcher".into();
        assert!(resolve_identity(&original, &[launcher_regen]).is_none());

        let mut bracket_launcher = target(11, 42);
        bracket_launcher.title = "AION2 [Launcher]".into();
        assert!(resolve_identity(&original, &[bracket_launcher]).is_none());
    }
    #[test]
    fn title_compatible_rules() {
        assert!(title_compatible("AION2", "AION2"));
        assert!(title_compatible("AION2", "aion2"));
        assert!(title_compatible("AION2", "AION2 - Loading"));
        assert!(title_compatible("AION2", "AION2 - Chapter 1"));
        assert!(title_compatible("AION2 - Chapter 1", "AION2"));
        assert!(title_compatible("AION2", "AION2 [DirectX 11]"));
        assert!(title_compatible("AION2", "AION2 (DirectX 11)"));
        assert!(title_compatible("AION2", "AION2 [DX11]"));
        assert!(title_compatible("AION2", "AION2 (64-bit)"));
        assert!(title_compatible("AION2", "AION2 [Loading]"));
        assert!(title_compatible("Firefox", "GitHub — Mozilla Firefox"));
        assert!(!title_compatible("AION2", "AION2 - Other Window"));
        assert!(!title_compatible("AION2", "AION2 - Launcher"));
        assert!(!title_compatible("AION2", "AION2 - Tool Window"));
        assert!(!title_compatible("AION2", "AION2 - SubWindow"));
        assert!(!title_compatible("AION2", "AION2 - Debug Console"));
        assert!(!title_compatible("AION2", "AION2 [Other Window]"));
        assert!(!title_compatible("AION2", "AION2 [Launcher]"));
        assert!(!title_compatible("AION2", "AION2 (Tool Window)"));
        assert!(!title_compatible("AION2", "AION2 (Debug Console)"));
        assert!(!title_compatible("AION2", "AION2 []"));
        assert!(!title_compatible("AION2", "AION2 ()"));
        assert!(!title_compatible("AION2", "Crash Reporter"));
        assert!(!title_compatible("AION2", "AION2 Crash Reporter"));
        assert!(!title_compatible("AION2", "AION2 Error"));
        assert!(!title_compatible("AION2", "AION2 Settings"));
        assert!(!title_compatible("AION2", "AION2 Updater"));
        assert!(!title_compatible("AION2", "AION2 Setup Wizard"));
        assert!(!title_compatible("AION2", "Settings"));
        assert!(!title_compatible("AION2", ""));
        assert!(!title_compatible("", "AION2"));
    }
    #[test]
    fn window_state_transitions_preserve_identity() {
        let original = target(10, 42);

        // 1. 最大化 (WS_MAXIMIZE = 0x01000000 が付与される)
        let mut maximized = original.clone();
        maximized.style |= 0x0100_0000;
        assert_eq!(resolve_identity(&original, &[maximized]).unwrap().hwnd, 10);

        // 2. 最小化 (WS_MINIMIZE = 0x20000000 が付与される)
        let mut minimized = original.clone();
        minimized.style |= 0x2000_0000;
        assert_eq!(resolve_identity(&original, &[minimized]).unwrap().hwnd, 10);

        // 3. フルスクリーン/ボーダレス切替 (WS_POPUP = 0x80000000 付与、枠スタイル変更)
        let mut fullscreen = original.clone();
        fullscreen.style = 0x8000_0000 | 0x1000_0000;
        assert_eq!(resolve_identity(&original, &[fullscreen]).unwrap().hwnd, 10);
    }
    #[test]
    fn consecutive_title_changes_preserve_binding_and_reconnection() {
        let mut binding = TargetBinding {
            base_title: "AION2".to_string(),
            identity: target(10, 42),
        };

        // 1回目のタイトル変化: "AION2" -> "AION2 - Loading"
        let mut step1 = target(10, 42);
        step1.title = "AION2 - Loading".into();
        let resolved1 = resolve_binding(&binding, &[step1]).expect("step1 should resolve");
        assert_eq!(resolved1.hwnd, 10);
        assert_eq!(resolved1.title, "AION2 - Loading");
        binding.identity = resolved1;

        // 2回目のタイトル変化: "AION2 - Loading" -> "AION2 - Chapter 1"
        // base_title ("AION2") が保持されているため、Loading -> Chapter 1 の遷移でも解決可能
        let mut step2 = target(10, 42);
        step2.title = "AION2 - Chapter 1".into();
        let resolved2 =
            resolve_binding(&binding, &[step2]).expect("step2 should resolve with base_title");
        assert_eq!(resolved2.hwnd, 10);
        assert_eq!(resolved2.title, "AION2 - Chapter 1");
        binding.identity = resolved2;

        // 3回目のタイトル変化: HWND再生成を伴う変化 ("AION2 - Chapter 1" -> "AION2 [DirectX 11]" on HWND 11)
        let mut step3 = target(11, 42);
        step3.title = "AION2 [DirectX 11]".into();
        let resolved3 =
            resolve_binding(&binding, &[step3]).expect("step3 should resolve on regenerated HWND");
        assert_eq!(resolved3.hwnd, 11);
        assert_eq!(resolved3.title, "AION2 [DirectX 11]");
        binding.identity = resolved3;

        // 4回目のタイトル変化: ベースタイトルへの復帰 ("AION2 [DirectX 11]" -> "AION2")
        let mut step4 = target(11, 42);
        step4.title = "AION2".into();
        let resolved4 =
            resolve_binding(&binding, &[step4]).expect("step4 should resolve back to base");
        assert_eq!(resolved4.hwnd, 11);
        assert_eq!(resolved4.title, "AION2");
    }
    #[test]
    fn snapshot_verification_allows_binding_title_transitions_during_frame() {
        let binding = TargetBinding {
            base_title: "AION2".to_string(),
            identity: {
                let mut id = target(10, 42);
                id.title = "AION2 - Loading".into();
                id
            },
        };

        // スナップショット実行中にタイトルが "AION2 - Chapter 1" に変化した場合
        let mut mid_capture = target(10, 42);
        mid_capture.title = "AION2 - Chapter 1".into();

        assert!(same_process_and_window_type(
            &binding.identity,
            &mid_capture
        ));
        // binding の base_title ("AION2") と互換判定されるため成功
        assert!(is_title_compatible_with_binding(
            &binding,
            &mid_capture.title
        ));

        // 一方でクラッシュレポーターなどの危険ウィンドウに変化した場合は確実に拒否
        let mut crash = target(10, 42);
        crash.title = "AION2 Crash Reporter".into();
        assert!(!is_title_compatible_with_binding(&binding, &crash.title));
    }
}
