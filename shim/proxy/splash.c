// The start-up status window. Patching the bytecode takes several seconds on
// the game's main thread before the game has any window of its own; without
// feedback the player sees nothing happen after pressing Play. This small
// window says what is going on, shows an indeterminate progress bar and the
// elapsed time, and closes by itself as soon as the game's own window is up.
//
// It lives on its own thread with its own message loop: the main thread is
// busy patching (and must not be blocked by us). Only the real game process
// shows it (see splash_wanted), so test harnesses that load the DLL stay
// silent. A failed patch is reported in a message box from the same thread,
// so the player knows the game runs without the co-op fixes and where the log
// is; the game itself is never held up by it.
#include "shim.h"
#include "splash.h"

#include <wchar.h>

#ifndef WMP_VERSION
#define WMP_VERSION L"dev"
#endif

#define SPLASH_CLASS L"WartalesMpSplash"
#define SPLASH_W 460
#define SPLASH_H 128
#define TIMER_ID 1
#define TIMER_MS 40
#define SHOW_DELAY_MS 300        // a patch that is instantly done shows nothing
#define GAME_WINDOW_WAIT_MS 120000 // give up waiting for the game window after this

enum { PHASE_PATCHING = 0, PHASE_STARTING = 1, PHASE_DONE = 2 };

static HANDLE splash_thread;
static volatile LONG splash_phase = PHASE_PATCHING;
static volatile LONG splash_patch_ok = TRUE;
static DWORD splash_start_tick;
static DWORD splash_phase_tick;
static HWND splash_hwnd;
static HFONT font_title, font_text;

// splash_wanted: only inside Wartales.exe itself.
static BOOL splash_wanted(void) {
	wchar_t path[MAX_PATH * 2];
	const wchar_t *base;
	DWORD n = GetModuleFileNameW(NULL, path, MAX_PATH * 2);
	if (n == 0 || n >= MAX_PATH * 2)
		return FALSE;
	base = wcsrchr(path, L'\\');
	base = base != NULL ? base + 1 : path;
	return _wcsicmp(base, L"Wartales.exe") == 0;
}

// A visible, captioned top-level window of this process that is not ours is
// the game's window.
static BOOL CALLBACK find_game_window(HWND h, LPARAM found) {
	DWORD pid = 0;
	GetWindowThreadProcessId(h, &pid);
	if (pid == GetCurrentProcessId() && h != splash_hwnd && IsWindowVisible(h) &&
			GetWindow(h, GW_OWNER) == NULL) {
		RECT r;
		if (GetWindowRect(h, &r) && r.right - r.left >= 200 && r.bottom - r.top >= 150) {
			*(BOOL *)found = TRUE;
			return FALSE;
		}
	}
	return TRUE;
}

// draw renders the window into dc (WM_PAINT and WM_PRINTCLIENT), double-buffered.
static void draw(HWND h, HDC dc) {
	RECT rc, bar, fill;
	HDC mem;
	HBITMAP bmp, old_bmp;
	HBRUSH bg, track, block;
	wchar_t line[160];
	DWORD now = GetTickCount();
	LONG phase = splash_phase;
	int w, x, block_w;

	GetClientRect(h, &rc);
	mem = CreateCompatibleDC(dc);
	bmp = CreateCompatibleBitmap(dc, rc.right, rc.bottom);
	old_bmp = (HBITMAP)SelectObject(mem, bmp);

	bg = CreateSolidBrush(RGB(32, 30, 28));
	FillRect(mem, &rc, bg);
	DeleteObject(bg);
	SetBkMode(mem, TRANSPARENT);

	SelectObject(mem, font_title);
	SetTextColor(mem, RGB(232, 200, 140));
	swprintf(line, 160, L"Wartales Co-op Fix v%ls", WMP_VERSION);
	TextOutW(mem, 20, 16, line, (int)wcslen(line));

	SelectObject(mem, font_text);
	SetTextColor(mem, RGB(220, 220, 215));
	if (phase == PHASE_PATCHING)
		swprintf(line, 160, L"Preparing the game: applying the co-op patches... %lu s",
			(unsigned long)((now - splash_start_tick) / 1000));
	else
		swprintf(line, 160, L"Patches ready, starting the game... %lu s",
			(unsigned long)((now - splash_start_tick) / 1000));
	TextOutW(mem, 20, 52, line, (int)wcslen(line));

	// Indeterminate progress: a block sweeping across a track.
	bar.left = 20;
	bar.right = rc.right - 20;
	bar.top = 86;
	bar.bottom = 98;
	track = CreateSolidBrush(RGB(64, 60, 56));
	FillRect(mem, &bar, track);
	DeleteObject(track);
	w = bar.right - bar.left;
	block_w = w / 4;
	x = (int)((now / 6) % (DWORD)(w + block_w)) - block_w;
	fill = bar;
	fill.left = bar.left + (x < 0 ? 0 : x);
	fill.right = bar.left + (x + block_w > w ? w : x + block_w);
	if (fill.right > fill.left) {
		block = CreateSolidBrush(phase == PHASE_PATCHING ? RGB(200, 150, 70) : RGB(110, 170, 90));
		FillRect(mem, &fill, block);
		DeleteObject(block);
	}

	BitBlt(dc, 0, 0, rc.right, rc.bottom, mem, 0, 0, SRCCOPY);
	SelectObject(mem, old_bmp);
	DeleteObject(bmp);
	DeleteDC(mem);
}

static LRESULT CALLBACK splash_proc(HWND h, UINT msg, WPARAM wp, LPARAM lp) {
	switch (msg) {
	case WM_TIMER: {
		DWORD now = GetTickCount();
		LONG phase = splash_phase;
		BOOL found = FALSE;
		if (phase == PHASE_DONE) {
			DestroyWindow(h);
			return 0;
		}
		if (!IsWindowVisible(h) && now - splash_start_tick >= SHOW_DELAY_MS)
			ShowWindow(h, SW_SHOWNOACTIVATE);
		if (phase == PHASE_STARTING) {
			EnumWindows(find_game_window, (LPARAM)&found);
			if (found || now - splash_phase_tick >= GAME_WINDOW_WAIT_MS) {
				DestroyWindow(h);
				return 0;
			}
		}
		if (IsWindowVisible(h))
			InvalidateRect(h, NULL, FALSE);
		return 0;
	}
	case WM_ERASEBKGND:
		return 1;
	case WM_PAINT: {
		PAINTSTRUCT ps;
		HDC dc = BeginPaint(h, &ps);
		draw(h, dc);
		EndPaint(h, &ps);
		return 0;
	}
	case WM_PRINTCLIENT:
		draw(h, (HDC)wp);
		return 0;
	case WM_CLOSE:
		return 0; // closes by itself; Alt+F4 must not leave the player wondering
	case WM_DESTROY:
		KillTimer(h, TIMER_ID);
		PostQuitMessage(0);
		return 0;
	}
	return DefWindowProcW(h, msg, wp, lp);
}

static void report_failure(void) {
	wchar_t logp[MAX_PATH * 2];
	wchar_t text[MAX_PATH * 2 + 400];
	if (!helper_path(L"\\shim.log", logp, MAX_PATH * 2))
		wcscpy(logp, L"%LOCALAPPDATA%\\wartales-mp\\shim.log");
	swprintf(text, sizeof(text) / sizeof(text[0]),
		L"The co-op patches could not be applied, so the game starts without them "
		L"(co-op with players who have them will not work).\n\n"
		L"This usually means a game update the mod does not support yet. "
		L"Details are in:\n%ls",
		logp);
	MessageBoxW(NULL, text, L"Wartales Co-op Fix v" WMP_VERSION, MB_OK | MB_ICONWARNING | MB_TOPMOST | MB_SETFOREGROUND);
}

static DWORD WINAPI splash_main(LPVOID inst) {
	WNDCLASSW wc;
	MSG m;
	int sx, sy;
	NONCLIENTMETRICSW ncm;

	ZeroMemory(&wc, sizeof(wc));
	wc.lpfnWndProc = splash_proc;
	wc.hInstance = (HINSTANCE)inst;
	wc.hCursor = LoadCursorW(NULL, (LPCWSTR)IDC_APPSTARTING);
	wc.lpszClassName = SPLASH_CLASS;
	if (RegisterClassW(&wc) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS) {
		shim_log("splash: RegisterClassW failed (%lu)", (unsigned long)GetLastError());
		goto out;
	}

	ZeroMemory(&ncm, sizeof(ncm));
	ncm.cbSize = sizeof(ncm);
	if (SystemParametersInfoW(SPI_GETNONCLIENTMETRICS, sizeof(ncm), &ncm, 0)) {
		ncm.lfMessageFont.lfHeight = -16;
		font_text = CreateFontIndirectW(&ncm.lfMessageFont);
		ncm.lfMessageFont.lfHeight = -20;
		ncm.lfMessageFont.lfWeight = FW_BOLD;
		font_title = CreateFontIndirectW(&ncm.lfMessageFont);
	}
	if (font_text == NULL)
		font_text = (HFONT)GetStockObject(DEFAULT_GUI_FONT);
	if (font_title == NULL)
		font_title = (HFONT)GetStockObject(DEFAULT_GUI_FONT);

	sx = (GetSystemMetrics(SM_CXSCREEN) - SPLASH_W) / 2;
	sy = (GetSystemMetrics(SM_CYSCREEN) - SPLASH_H) / 2;
	splash_hwnd = CreateWindowExW(WS_EX_TOPMOST | WS_EX_APPWINDOW, SPLASH_CLASS,
		L"Wartales Co-op Fix: starting", WS_POPUP | WS_BORDER,
		sx, sy, SPLASH_W, SPLASH_H, NULL, NULL, (HINSTANCE)inst, NULL);
	if (splash_hwnd == NULL) {
		shim_log("splash: CreateWindowExW failed (%lu)", (unsigned long)GetLastError());
		goto out;
	}
	SetTimer(splash_hwnd, TIMER_ID, TIMER_MS, NULL);
	while (GetMessageW(&m, NULL, 0, 0) > 0) {
		TranslateMessage(&m);
		DispatchMessageW(&m);
	}
	splash_hwnd = NULL;
	shim_log("splash: closed after %lu ms", (unsigned long)(GetTickCount() - splash_start_tick));

out:
	if (!splash_patch_ok)
		report_failure();
	return 0;
}

void splash_begin(HINSTANCE inst) {
	if (splash_thread != NULL || !splash_wanted())
		return;
	splash_start_tick = GetTickCount();
	splash_phase = PHASE_PATCHING;
	splash_thread = CreateThread(NULL, 0, splash_main, (LPVOID)inst, 0, NULL);
	if (splash_thread == NULL)
		shim_log("splash: CreateThread failed (%lu)", (unsigned long)GetLastError());
}

void console_detach(void) {
	DWORD pids[2];
	if (!splash_wanted() || GetConsoleWindow() == NULL)
		return;
	if (GetConsoleProcessList(pids, 2) != 1) {
		shim_log("console: shared with another process, kept");
		return;
	}
	if (FreeConsole())
		shim_log("console: released the game's own (empty) console window");
	else
		shim_log("console: FreeConsole failed (%lu)", (unsigned long)GetLastError());
}

static DWORD WINAPI failure_main(LPVOID unused) {
	(void)unused;
	report_failure();
	return 0;
}

void splash_patched(BOOL ok) {
	splash_patch_ok = ok;
	if (splash_thread == NULL) {
		if (!ok && splash_wanted()) {
			// No status window (it could not start): still tell the player.
			HANDLE t = CreateThread(NULL, 0, failure_main, NULL, 0, NULL);
			if (t != NULL)
				CloseHandle(t);
		}
		return;
	}
	splash_phase_tick = GetTickCount();
	InterlockedExchange(&splash_phase, PHASE_STARTING);
}
