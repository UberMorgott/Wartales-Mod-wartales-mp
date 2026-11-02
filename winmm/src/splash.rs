//! The start-up status window (port of splash.c). Patching the bytecode takes
//! seconds on the game's main thread before the game has a window; this small
//! window says what is going on, shows how many co-op passes are applied (a
//! native progress bar fed by wartales_tips::progress) and the elapsed time,
//! and closes by itself once the game's own window is up. It runs on its own
//! thread with its own message loop. Only the real game process shows it
//! (`wanted`), so test hosts that load the DLL stay silent. A failed patch is
//! reported in a message box from that thread; the game is never held up.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering};
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, RECT, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontIndirectW,
    CreateSolidBrush, DeleteDC, DeleteObject, EndPaint, FillRect, GetStockObject, InvalidateRect,
    SelectObject, SetBkMode, SetTextColor, TextOutW, DEFAULT_GUI_FONT, FW_BOLD, HDC, HFONT,
    PAINTSTRUCT, SRCCOPY, TRANSPARENT,
};
use windows_sys::Win32::System::Console::{FreeConsole, GetConsoleProcessList, GetConsoleWindow};
use windows_sys::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows_sys::Win32::System::SystemInformation::GetTickCount;
use windows_sys::Win32::System::Threading::GetCurrentProcessId;
use windows_sys::Win32::UI::Controls::{
    InitCommonControlsEx, ICC_PROGRESS_CLASS, INITCOMMONCONTROLSEX, PBM_SETBARCOLOR,
    PBM_SETBKCOLOR, PBM_SETPOS, PBM_SETRANGE32, PBS_SMOOTH, PROGRESS_CLASSW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, EnumWindows, GetClientRect,
    GetMessageW, GetSystemMetrics, GetWindow, GetWindowRect, GetWindowThreadProcessId,
    IsWindowVisible, KillTimer, LoadCursorW, MessageBoxW, PostQuitMessage, RegisterClassW,
    SendMessageW, SetTimer, ShowWindow, SystemParametersInfoW, TranslateMessage, GW_OWNER,
    IDC_APPSTARTING, MB_ICONWARNING, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MSG, NONCLIENTMETRICSW,
    SM_CXSCREEN, SM_CYSCREEN, SPI_GETNONCLIENTMETRICS, SW_SHOWNOACTIVATE, WM_CLOSE, WM_DESTROY,
    WM_ERASEBKGND, WM_PAINT, WM_PRINTCLIENT, WM_TIMER, WNDCLASSW, WS_BORDER, WS_CHILD,
    WS_CLIPCHILDREN, WS_EX_APPWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};

use crate::dll::shim_log;

/// The release build passes WMP_VERSION (as build.ps1 did for the C shim).
const VERSION: &str = match option_env!("WMP_VERSION") {
    Some(v) => v,
    None => "dev",
};
const W: i32 = 520;
const H: i32 = 128;
const TIMER_ID: usize = 1;
const TIMER_MS: u32 = 40;
/// A patch that is done at once shows nothing.
const SHOW_DELAY_MS: u32 = 300;
/// Give up waiting for the game window after this.
const GAME_WINDOW_WAIT_MS: u32 = 120_000;

const PATCHING: u32 = 0;
const STARTING: u32 = 1;

static STARTED: AtomicBool = AtomicBool::new(false);
static PHASE: AtomicU32 = AtomicU32::new(PATCHING);
static PATCH_OK: AtomicBool = AtomicBool::new(true);
static START_TICK: AtomicU32 = AtomicU32::new(0);
static PHASE_TICK: AtomicU32 = AtomicU32::new(0);
static HWND_SELF: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static BAR: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
/// (title, text) fonts, made on the splash thread.
static FONTS: OnceLock<(usize, usize)> = OnceLock::new();

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn tick() -> u32 {
    // SAFETY: reads the system tick count.
    unsafe { GetTickCount() }
}

const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}

/// Only inside Wartales.exe itself.
fn wanted() -> bool {
    static WANTED: OnceLock<bool> = OnceLock::new();
    *WANTED.get_or_init(|| {
        let mut path = [0u16; 520];
        // SAFETY: path is writable for its stated length; NULL = the exe.
        let n =
            unsafe { GetModuleFileNameW(std::ptr::null_mut(), path.as_mut_ptr(), 520) } as usize;
        if n == 0 || n >= path.len() {
            return false;
        }
        let path = String::from_utf16_lossy(&path[..n]);
        path.rsplit('\\')
            .next()
            .is_some_and(|b| b.eq_ignore_ascii_case("Wartales.exe"))
    })
}

/// Releases the game's own (empty) console window (DllMain).
pub fn console_detach() {
    // SAFETY: console queries/release of this process only.
    unsafe {
        if !wanted() || GetConsoleWindow().is_null() {
            return;
        }
        let mut pids = [0u32; 2];
        if GetConsoleProcessList(pids.as_mut_ptr(), 2) != 1 {
            shim_log("console: shared with another process, kept");
            return;
        }
        if FreeConsole() != 0 {
            shim_log("console: released the game's own (empty) console window");
        } else {
            shim_log(&format!("console: FreeConsole failed ({})", GetLastError()));
        }
    }
}

/// (passes done, total, skipped) clamped for display.
fn progress() -> (u32, u32, u32) {
    let (done, total, skipped) = wartales_tips::progress();
    let done = done.min(total);
    (done, total, skipped.min(done))
}

fn status_text(phase: u32, ok: bool, secs: u32) -> String {
    let (done, total, skipped) = progress();
    let skip = if skipped > 0 {
        format!(", {skipped} skipped")
    } else {
        String::new()
    };
    let applied = done - skipped;
    match (phase, ok) {
        (PATCHING, _) if total == 0 => {
            format!("Preparing the game: reading the game code... {secs} s")
        }
        (PATCHING, _) if done < total => {
            format!("Applying co-op patches: {applied} of {total}{skip}... {secs} s")
        }
        (PATCHING, _) => {
            format!("Applied {applied} of {total} co-op patches{skip}, saving... {secs} s")
        }
        (_, true) => {
            format!("Applied {total} of {total} co-op patches, starting the game... {secs} s")
        }
        _ if skipped > 0 => {
            format!(
                "{skipped} of {total} patches do not fit this game version: starting without them"
            )
        }
        _ => "Co-op patches not applied: starting the game without them".into(),
    }
}

/// Renders the window into dc (WM_PAINT, WM_PRINTCLIENT), double-buffered.
///
/// # Safety
/// `h` is the splash window and `dc` a device context for it.
unsafe fn draw(h: HWND, dc: HDC) {
    let secs = tick().wrapping_sub(START_TICK.load(Ordering::Relaxed)) / 1000;
    let (title_font, text_font) = FONTS.get().copied().unwrap_or_default();
    let title = wide(&format!("Wartales Co-op Fix v{VERSION}"));
    let text = wide(&status_text(
        PHASE.load(Ordering::Relaxed),
        PATCH_OK.load(Ordering::Relaxed),
        secs,
    ));
    // SAFETY: GDI calls on objects created and released right here.
    unsafe {
        let mut rc: RECT = std::mem::zeroed();
        GetClientRect(h, &raw mut rc);
        let mem = CreateCompatibleDC(dc);
        let bmp = CreateCompatibleBitmap(dc, rc.right, rc.bottom);
        let old = SelectObject(mem, bmp);
        let bg = CreateSolidBrush(rgb(32, 30, 28));
        FillRect(mem, &raw const rc, bg);
        DeleteObject(bg);
        SetBkMode(mem, TRANSPARENT.cast_signed());
        SelectObject(mem, title_font as HFONT);
        SetTextColor(mem, rgb(232, 200, 140));
        TextOutW(
            mem,
            20,
            16,
            title.as_ptr(),
            i32::try_from(title.len() - 1).unwrap_or(0),
        );
        SelectObject(mem, text_font as HFONT);
        SetTextColor(mem, rgb(220, 220, 215));
        TextOutW(
            mem,
            20,
            52,
            text.as_ptr(),
            i32::try_from(text.len() - 1).unwrap_or(0),
        );
        BitBlt(dc, 0, 0, rc.right, rc.bottom, mem, 0, 0, SRCCOPY);
        SelectObject(mem, old);
        DeleteObject(bmp);
        DeleteDC(mem);
    }
}

/// Moves the native bar: amber while patching, green once applied, red when a
/// pass did not fit or patching failed. Only touched from the splash thread.
fn update_bar(phase: u32) {
    static SHOWN: std::sync::Mutex<(u32, u32, u32)> =
        std::sync::Mutex::new((u32::MAX, u32::MAX, u32::MAX));
    let bar = BAR.load(Ordering::Relaxed);
    if bar.is_null() {
        return;
    }
    let (mut done, mut total, skipped) = progress();
    let mut color = rgb(200, 150, 70);
    if phase != PATCHING {
        total = total.max(1);
        done = total; // finished either way; the colour tells which way
        color = if PATCH_OK.load(Ordering::Relaxed) {
            rgb(110, 170, 90)
        } else {
            rgb(200, 80, 60)
        };
    } else if skipped > 0 {
        color = rgb(200, 80, 60);
    }
    let mut shown = SHOWN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // SAFETY: messages to our own progress control.
    unsafe {
        if total != shown.0 {
            SendMessageW(bar, PBM_SETRANGE32, 0, total.max(1).cast_signed() as LPARAM);
            *shown = (total, u32::MAX, shown.2);
        }
        if done != shown.1 {
            SendMessageW(bar, PBM_SETPOS, done as WPARAM, 0);
            shown.1 = done;
        }
        if color != shown.2 {
            SendMessageW(bar, PBM_SETBARCOLOR, 0, color.cast_signed() as LPARAM);
            shown.2 = color;
        }
    }
}

/// A visible, captioned top-level window of this process that is not ours is
/// the game's window.
unsafe extern "system" fn find_game_window(h: HWND, found: LPARAM) -> i32 {
    // SAFETY: window queries; found points at the caller's bool.
    unsafe {
        let mut pid = 0;
        GetWindowThreadProcessId(h, &raw mut pid);
        if pid == GetCurrentProcessId()
            && h != HWND_SELF.load(Ordering::Relaxed)
            && IsWindowVisible(h) != 0
            && GetWindow(h, GW_OWNER).is_null()
        {
            let mut r: RECT = std::mem::zeroed();
            if GetWindowRect(h, &raw mut r) != 0
                && r.right - r.left >= 200
                && r.bottom - r.top >= 150
            {
                *(found as *mut bool) = true;
                return 0;
            }
        }
    }
    1
}

unsafe extern "system" fn wnd_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: standard window procedure work on our own window.
    unsafe {
        match msg {
            WM_TIMER => {
                let now = tick();
                let phase = PHASE.load(Ordering::Relaxed);
                if IsWindowVisible(h) == 0
                    && now.wrapping_sub(START_TICK.load(Ordering::Relaxed)) >= SHOW_DELAY_MS
                {
                    ShowWindow(h, SW_SHOWNOACTIVATE);
                }
                if phase == STARTING {
                    let mut found = false;
                    EnumWindows(Some(find_game_window), &raw mut found as LPARAM);
                    if found
                        || now.wrapping_sub(PHASE_TICK.load(Ordering::Relaxed))
                            >= GAME_WINDOW_WAIT_MS
                    {
                        DestroyWindow(h);
                        return 0;
                    }
                }
                if IsWindowVisible(h) != 0 {
                    update_bar(phase);
                    InvalidateRect(h, std::ptr::null(), 0);
                }
                0
            }
            WM_ERASEBKGND => 1,
            WM_PAINT => {
                let mut ps: PAINTSTRUCT = std::mem::zeroed();
                let dc = BeginPaint(h, &raw mut ps);
                draw(h, dc);
                EndPaint(h, &raw const ps);
                0
            }
            WM_PRINTCLIENT => {
                draw(h, wp as HDC);
                0
            }
            WM_CLOSE => 0, // closes by itself; Alt+F4 must not leave the player wondering
            WM_DESTROY => {
                KillTimer(h, TIMER_ID);
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(h, msg, wp, lp),
        }
    }
}

fn report_failure() {
    let log = crate::applog::dir().join("shim.log");
    let text = wide(&format!(
        "The co-op patches could not be applied, so the game starts without them \
         (co-op with players who have them will not work).\n\n\
         This usually means a game update the mod does not support yet. Details are in:\n{}",
        log.display()
    ));
    let caption = wide(&format!("Wartales Co-op Fix v{VERSION}"));
    // SAFETY: both strings are NUL-terminated and live across the call.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            caption.as_ptr(),
            MB_OK | MB_ICONWARNING | MB_TOPMOST | MB_SETFOREGROUND,
        );
    }
}

/// The title and text fonts from the system message font (stock font fallback).
fn make_fonts() {
    // SAFETY: GDI font creation; the handles live for the process.
    unsafe {
        let mut ncm: NONCLIENTMETRICSW = std::mem::zeroed();
        ncm.cbSize = u32::try_from(size_of::<NONCLIENTMETRICSW>()).unwrap_or(0);
        let (mut title, mut text) = (std::ptr::null_mut(), std::ptr::null_mut());
        if SystemParametersInfoW(
            SPI_GETNONCLIENTMETRICS,
            ncm.cbSize,
            (&raw mut ncm).cast(),
            0,
        ) != 0
        {
            ncm.lfMessageFont.lfHeight = -16;
            text = CreateFontIndirectW(&raw const ncm.lfMessageFont);
            ncm.lfMessageFont.lfHeight = -20;
            ncm.lfMessageFont.lfWeight = FW_BOLD.cast_signed();
            title = CreateFontIndirectW(&raw const ncm.lfMessageFont);
        }
        let stock = GetStockObject(DEFAULT_GUI_FONT);
        let pick = |f: HFONT| {
            if f.is_null() {
                stock as usize
            } else {
                f as usize
            }
        };
        let _ = FONTS.set((pick(title), pick(text)));
    }
}

/// The splash thread's window and message loop; false when no window came up.
fn run() -> bool {
    // SAFETY: Win32 window creation and message loop on this thread's own window.
    unsafe {
        let inst = crate::dll::instance();
        let class = wide("WartalesMpSplash");
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(wnd_proc);
        wc.hInstance = inst;
        wc.hCursor = LoadCursorW(std::ptr::null_mut(), IDC_APPSTARTING);
        wc.lpszClassName = class.as_ptr();
        if RegisterClassW(&raw const wc) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
            shim_log(&format!(
                "splash: RegisterClassW failed ({})",
                GetLastError()
            ));
            return false;
        }
        make_fonts();

        let x = (GetSystemMetrics(SM_CXSCREEN) - W) / 2;
        let y = (GetSystemMetrics(SM_CYSCREEN) - H) / 2;
        let caption = wide("Wartales Co-op Fix: starting");
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_APPWINDOW,
            class.as_ptr(),
            caption.as_ptr(),
            WS_POPUP | WS_BORDER | WS_CLIPCHILDREN,
            x,
            y,
            W,
            H,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            inst,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            shim_log(&format!(
                "splash: CreateWindowExW failed ({})",
                GetLastError()
            ));
            return false;
        }
        HWND_SELF.store(hwnd, Ordering::Relaxed);
        let icc = INITCOMMONCONTROLSEX {
            dwSize: u32::try_from(size_of::<INITCOMMONCONTROLSEX>()).unwrap_or(0),
            dwICC: ICC_PROGRESS_CLASS,
        };
        InitCommonControlsEx(&raw const icc);
        let mut rc: RECT = std::mem::zeroed();
        GetClientRect(hwnd, &raw mut rc);
        let bar = CreateWindowExW(
            0,
            PROGRESS_CLASSW,
            std::ptr::null(),
            WS_CHILD | WS_VISIBLE | PBS_SMOOTH,
            20,
            86,
            rc.right - 40,
            12,
            hwnd,
            std::ptr::null_mut(),
            inst,
            std::ptr::null(),
        );
        if bar.is_null() {
            shim_log(&format!(
                "splash: progress bar not created ({})",
                GetLastError()
            ));
        } else {
            SendMessageW(
                bar,
                PBM_SETBKCOLOR,
                0,
                rgb(64, 60, 56).cast_signed() as LPARAM,
            );
            BAR.store(bar, Ordering::Relaxed);
        }
        update_bar(PATCHING);
        SetTimer(hwnd, TIMER_ID, TIMER_MS, None);
        let mut m: MSG = std::mem::zeroed();
        while GetMessageW(&raw mut m, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&raw const m);
            DispatchMessageW(&raw const m);
        }
        HWND_SELF.store(std::ptr::null_mut(), Ordering::Relaxed);
        BAR.store(std::ptr::null_mut(), Ordering::Relaxed);
    }
    shim_log(&format!(
        "splash: closed after {} ms",
        tick().wrapping_sub(START_TICK.load(Ordering::Relaxed))
    ));
    true
}

fn spawn(name: &str, f: fn()) -> bool {
    let spawned = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            if std::panic::catch_unwind(f).is_err() {
                shim_log("splash: panicked");
            }
        });
    if let Err(e) = &spawned {
        shim_log(&format!("splash: thread failed to start: {e}"));
    }
    spawned.is_ok()
}

/// Shows the window while the bytecode is patched (once per process).
pub fn begin() {
    if !wanted() || STARTED.swap(true, Ordering::Relaxed) {
        return;
    }
    START_TICK.store(tick(), Ordering::Relaxed);
    PHASE.store(PATCHING, Ordering::Relaxed);
    if !spawn("wartales-mp-splash", || {
        // No window: patched() reports a failure itself (the C shim then never did).
        if !run() {
            STARTED.store(false, Ordering::Release);
        }
        if !PATCH_OK.load(Ordering::Acquire) {
            report_failure();
        }
    }) {
        STARTED.store(false, Ordering::Relaxed);
    }
}

/// Patching finished: the window waits for the game's own, or reports failure.
pub fn patched(ok: bool) {
    PATCH_OK.store(ok, Ordering::Relaxed);
    if !STARTED.load(Ordering::Relaxed) {
        // No status window (it could not start): still tell the player.
        if !ok && wanted() {
            spawn("wartales-mp-failure", report_failure);
        }
        return;
    }
    PHASE_TICK.store(tick(), Ordering::Relaxed);
    PHASE.store(STARTING, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_wanted_outside_the_game() {
        assert!(!wanted());
    }
}
