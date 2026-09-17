// winmm.dll proxy: the one file wartales-mp drops into the game folder.
//
// Nothing in the game folder is renamed, replaced or modified. The game's
// libhl.dll has a *static* import of WINMM.dll and the game folder contains no
// winmm.dll of its own, so Windows -- which searches the application directory
// before System32 for a non-KnownDLL -- loads THIS file at process init, before
// any game or network code runs. WINMM.dll is chosen over VERSION.dll because
// it loads earliest (it is libhl.dll's own dependency, so libhl.dll is
// guaranteed mapped when we run) and its larger export table costs nothing: the
// forwarders are generated mechanically by tools/gendef. Neither name is a
// KnownDLL, so the app-directory search order applies.
//
// This DLL does three things:
//
//  1. Forwards every WINMM export to the real C:\Windows\System32\winmm.dll.
//     A PE forwarder string cannot name its own module, so each export is a
//     generated tail-call thunk (winmm_stubs.c) that jumps through a pointer we
//     fill here with GetProcAddress against the System32 copy loaded by its
//     full path -- never a bare "winmm.dll", which would find us and recurse.
//
//  2. Hooks two HashLink natives with inline trampolines (MinHook, BSD-2). The
//     bytecode resolves natives through hlp_<name> pointers at module load, not
//     through the IAT, so IAT patching would miss them; only an inline hook on
//     the function body catches every call:
//       - hl_host_resolve in libhl.dll: master*.shirogames.com -> 127.0.0.1.
//       - ssl_conf_set_ca in ssl.hdll: our local CA is added to the chain.
//     libhl.dll is already mapped, so its hook goes in immediately. ssl.hdll is
//     loaded lazily; rather than hooking the high-traffic LoadLibraryExW under
//     the loader lock, a background thread polls for the module and hooks it the
//     moment it appears -- conf_set_ca is called once, late (first TLS setup),
//     long after ssl.hdll loads, so the poll wins the race comfortably.
//
//  3. Hands the game a patched copy of its HashLink bytecode. CreateFileW in
//     kernelbase.dll is hooked; an open of hlboot.dat is answered with a handle
//     to %LOCALAPPDATA%\wartales-mp\hlboot.dat, a copy of the original with two
//     bytes changed (hlpatch.h, generated from internal/hlpatch). The copy is
//     rebuilt from the original whenever it does not match the original byte
//     for byte after patching, so a game update either re-patches or -- when
//     the signature is gone -- falls back to the untouched original. The game
//     folder is never written. See internal/hlpatch for why the game crashes
//     ("Null access", mpman/net/Client.hx:13) a minute into an idle lobby
//     without the patch. The hook must exist before the game's entry point
//     opens the file, so it is the one thing installed from DllMain.
//
//  4. Extracts the embedded wartales-mp.exe to %LOCALAPPDATA%\wartales-mp\ (only
//     when missing or when the embedded build differs by hash) and starts it
//     hidden. It watches the game PID and exits with the game on its own.
//
// Everything above reports to %LOCALAPPDATA%\wartales-mp\shim.log (append,
// one open/write/close per line, never throws).

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>

#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "MinHook.h"
#include "hlpatch.h"

// ---------------------------------------------------------------------------
// Diagnostics: %LOCALAPPDATA%\wartales-mp\shim.log.
//
// Every line is its own open/append/close, so a crash anywhere loses nothing
// already written and no handle or lock is held between calls. Any failure is
// swallowed: logging can never take the game down.
// ---------------------------------------------------------------------------

typedef HANDLE(WINAPI *create_file_w_fn)(LPCWSTR, DWORD, DWORD, LPSECURITY_ATTRIBUTES, DWORD, DWORD, HANDLE);
typedef HANDLE(WINAPI *create_file_a_fn)(LPCSTR, DWORD, DWORD, LPSECURITY_ATTRIBUTES, DWORD, DWORD, HANDLE);
static create_file_w_fn real_create_file_w; // MinHook trampoline to the original
static create_file_a_fn real_create_file_a;

// open_raw is CreateFileW that bypasses our own hook once it is in place.
static HANDLE open_raw(const wchar_t *path, DWORD access, DWORD share, DWORD disp, DWORD flags) {
	create_file_w_fn f = real_create_file_w != NULL ? real_create_file_w : CreateFileW;
	return f(path, access, share, NULL, disp, flags, NULL);
}

// helper_path builds %LOCALAPPDATA%\wartales-mp and appends tail (may be empty).
static BOOL helper_path(const wchar_t *tail, wchar_t *out, DWORD cch) {
	DWORD n = GetEnvironmentVariableW(L"LOCALAPPDATA", out, cch);
	if (n == 0 || n >= cch)
		return FALSE;
	if (wcslen(out) + wcslen(L"\\wartales-mp") + wcslen(tail) + 1 > cch)
		return FALSE;
	wcscat(out, L"\\wartales-mp");
	wcscat(out, tail);
	return TRUE;
}

#define LOG_MAX_BYTES (1024 * 1024)

static wchar_t log_path[MAX_PATH * 2];
static BOOL log_ready;

static void shim_log(const char *fmt, ...) {
	char line[1024];
	int n;
	va_list ap;
	HANDLE f;
	DWORD put;

	if (!log_ready)
		return;
	n = _snprintf(line, sizeof(line) - 2, "[%lu %lu %lu] ", (unsigned long)GetCurrentProcessId(),
		(unsigned long)GetCurrentThreadId(), (unsigned long)GetTickCount());
	if (n < 0)
		return;
	va_start(ap, fmt);
	n += _vsnprintf(line + n, sizeof(line) - 2 - (size_t)n, fmt, ap);
	va_end(ap);
	if (n < 0 || n > (int)sizeof(line) - 2)
		n = (int)sizeof(line) - 2;
	line[n++] = '\r';
	line[n++] = '\n';
	f = open_raw(log_path, FILE_APPEND_DATA, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
		OPEN_ALWAYS, FILE_ATTRIBUTE_NORMAL);
	if (f == INVALID_HANDLE_VALUE)
		return;
	WriteFile(f, line, (DWORD)n, &put, NULL);
	CloseHandle(f);
}

// log_wpath logs a wide path as UTF-8 under a short label.
static void log_wpath(const char *label, const wchar_t *path) {
	char utf8[MAX_PATH * 4];
	int n = WideCharToMultiByte(CP_UTF8, 0, path, -1, utf8, sizeof(utf8), NULL, NULL);
	if (n <= 0)
		strcpy(utf8, "?");
	shim_log("%s %s", label, utf8);
}

// log_open creates the directory and truncates an oversized log. Runs first in
// DllMain, so every later step has somewhere to report.
static void log_open(void) {
	wchar_t dir[MAX_PATH * 2];
	WIN32_FILE_ATTRIBUTE_DATA st;
	if (!helper_path(L"", dir, MAX_PATH * 2))
		return;
	CreateDirectoryW(dir, NULL);
	if (!helper_path(L"\\shim.log", log_path, MAX_PATH * 2))
		return;
	if (GetFileAttributesExW(log_path, GetFileExInfoStandard, &st) &&
			(st.nFileSizeHigh != 0 || st.nFileSizeLow > LOG_MAX_BYTES))
		DeleteFileW(log_path);
	log_ready = TRUE;
}

// ---------------------------------------------------------------------------
// Forwarding: fill the generated thunk pointer table from the System32 copy.
// ---------------------------------------------------------------------------

struct mp_export {
	const char *name;
	void **slot;
};
extern const struct mp_export mp_exports[];
extern const unsigned mp_exports_count;

static HMODULE real_winmm;

// bind_forwards loads System32\winmm.dll by absolute path and points every
// generated thunk at the matching real export. Done in DllMain so the pointers
// are live before libhl.dll (which imports us) can call a forwarded function.
static void bind_forwards(void) {
	wchar_t path[MAX_PATH];
	UINT n = GetSystemDirectoryW(path, MAX_PATH); // no trailing backslash
	unsigned i, missing = 0;
	if (n == 0 || n + 16 >= MAX_PATH) {
		shim_log("forwards: GetSystemDirectoryW failed (%lu)", (unsigned long)GetLastError());
		return;
	}
	wcscpy(path + n, L"\\winmm.dll");
	real_winmm = LoadLibraryW(path);
	if (real_winmm == NULL) {
		shim_log("forwards: LoadLibraryW(System32\\winmm.dll) failed (%lu)", (unsigned long)GetLastError());
		return;
	}
	for (i = 0; i < mp_exports_count; i++) {
		*mp_exports[i].slot = (void *)GetProcAddress(real_winmm, mp_exports[i].name);
		if (*mp_exports[i].slot == NULL)
			missing++;
	}
	shim_log("forwards: %u exports bound, %u missing", mp_exports_count - missing, missing);
}

// ---------------------------------------------------------------------------
// Hook 1: hl_host_resolve (libhl.dll).
// ---------------------------------------------------------------------------

// LOOPBACK is inet_addr("127.0.0.1"): bytes 127,0,0,1 packed as the original's
// return value, so 127.0.0.1 is 0x0100007F.
#define LOOPBACK 0x0100007F

typedef int (*host_resolve_fn)(unsigned char *host);
static host_resolve_fn real_host_resolve; // MinHook trampoline to the original

static const char *const master_hosts[] = {
	"master.shirogames.com",
	"master2.shirogames.com",
};

static int is_master_host(const char *host) {
	size_t i;
	for (i = 0; i < sizeof(master_hosts) / sizeof(master_hosts[0]); i++) {
		if (_stricmp(host, master_hosts[i]) == 0)
			return 1;
	}
	return 0;
}

static int detour_host_resolve(unsigned char *host) {
	if (host != NULL && is_master_host((const char *)host))
		return (int)LOOPBACK;
	if (real_host_resolve == NULL)
		return -1;
	return real_host_resolve(host);
}

// ---------------------------------------------------------------------------
// Hook 2: ssl_conf_set_ca (ssl.hdll). Certificate verification stays on; we
// only add one more trusted root.
// ---------------------------------------------------------------------------

typedef struct _hl_ssl_cert hl_ssl_cert; // opaque; only ssl.hdll dereferences it
typedef void (*conf_set_ca_fn)(void *conf, hl_ssl_cert *cert);
typedef hl_ssl_cert *(*cert_add_pem_fn)(hl_ssl_cert *cert, unsigned char *data);

static conf_set_ca_fn real_conf_set_ca; // MinHook trampoline to the original
static cert_add_pem_fn real_cert_add_pem;

#define PEM_HEADER "-----BEGIN CERTIFICATE-----"
#define CA_MAX_BYTES (256 * 1024)

static char *ca_pem; // NUL-terminated PEM, or NULL until %ProgramData% has it

// ca_path builds %ProgramData%\wartales-mp\ca.crt, the file internal/install
// writes.
static BOOL ca_path(wchar_t *out, DWORD cch) {
	DWORD n = GetEnvironmentVariableW(L"ProgramData", out, cch);
	if (n == 0 || n >= cch)
		return FALSE;
	if (wcslen(out) + wcslen(L"\\wartales-mp\\ca.crt") + 1 > cch)
		return FALSE;
	wcscat(out, L"\\wartales-mp\\ca.crt");
	return TRUE;
}

// load_ca reads the CA once. wartales-mp.exe writes it concurrently with the
// game starting, so a truncated or missing file simply degrades to "no extra
// CA" and is retried on the next call.
static const char *load_ca(void) {
	wchar_t path[MAX_PATH * 2];
	HANDLE f;
	LARGE_INTEGER size;
	DWORD got = 0;
	char *buf;

	if (ca_pem != NULL)
		return ca_pem;
	if (!ca_path(path, MAX_PATH * 2))
		return NULL;
	f = CreateFileW(path, GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE, NULL,
		OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, NULL);
	if (f == INVALID_HANDLE_VALUE)
		return NULL;
	if (!GetFileSizeEx(f, &size) || size.QuadPart <= 0 || size.QuadPart > CA_MAX_BYTES) {
		CloseHandle(f);
		return NULL;
	}
	buf = (char *)malloc((size_t)size.QuadPart + 1);
	if (buf == NULL) {
		CloseHandle(f);
		return NULL;
	}
	if (!ReadFile(f, buf, (DWORD)size.QuadPart, &got, NULL) || got == 0) {
		free(buf);
		CloseHandle(f);
		return NULL;
	}
	CloseHandle(f);
	buf[got] = 0;
	if (strstr(buf, PEM_HEADER) == NULL) {
		free(buf);
		return NULL;
	}
	ca_pem = buf;
	return ca_pem;
}

static void detour_conf_set_ca(void *conf, hl_ssl_cert *cert) {
	const char *pem;
	if (real_conf_set_ca == NULL)
		return;
	pem = load_ca();
	if (pem != NULL && real_cert_add_pem != NULL) {
		hl_ssl_cert *merged = real_cert_add_pem(cert, (unsigned char *)pem);
		if (merged != NULL)
			cert = merged;
	}
	real_conf_set_ca(conf, cert);
}

// ---------------------------------------------------------------------------
// Hook 3: CreateFileW / CreateFileA (kernelbase) -- open a patched copy.
//
// The game loads the bytecode with hashlink's load_code (src/main.c): _wfopen
// + fread of the whole file. That fread lives in ucrtbase.dll, which imports
// ReadFile/CreateFileW from api-ms-win-core-file-l1-1-0 -> kernelbase.dll and
// never passes through kernel32's export stubs, so an inline hook on
// kernel32!ReadFile (the previous design) is simply never called for this
// file. kernel32's own stubs jump into kernelbase, so hooking the kernelbase
// bodies covers every caller: the CRT, direct kernel32 imports, and the api
// sets.
//
// Rather than patching read buffers (chunk sizes, overlap, partial reads all
// become our problem), we intercept the open: when hlboot.dat is opened for
// reading we materialise %LOCALAPPDATA%\wartales-mp\hlboot.dat -- the original
// with hlpatch.h applied -- and return a handle to that copy. The copy is
// trusted only when it is byte-for-byte what patching the original yields
// right now; otherwise it is rewritten, and if the original no longer matches
// every signature exactly once the game gets its own file. Nothing in the game
// folder is written, and a broken copy is impossible to hand out.
// ---------------------------------------------------------------------------

static BOOL hook_one(const char *name, void *proc, void *detour, void **orig); // defined below

// HL_MAX_IMAGE bounds what we are willing to hold in memory (hlboot.dat is 19 MB).
#define HL_MAX_IMAGE (256u * 1024u * 1024u)

static CRITICAL_SECTION hl_lock; // serialises copy (re)generation
static BOOL hl_lock_ready;

// bytecode_names are the files the hashlink loader reads as its main image.
static const wchar_t *const bytecode_names[] = {L"hlboot.dat", L"sdlboot.dat"};

static const wchar_t *base_name(const wchar_t *path) {
	const wchar_t *p = path, *s;
	for (s = path; *s != 0; s++) {
		if (*s == L'\\' || *s == L'/')
			p = s + 1;
	}
	return p;
}

static const char *base_name_a(const char *path) {
	const char *p = path, *s;
	for (s = path; *s != 0; s++) {
		if (*s == '\\' || *s == '/')
			p = s + 1;
	}
	return p;
}

static BOOL is_bytecode_name(const wchar_t *name) {
	size_t i;
	for (i = 0; i < sizeof(bytecode_names) / sizeof(bytecode_names[0]); i++) {
		if (_wcsicmp(name, bytecode_names[i]) == 0)
			return TRUE;
	}
	return FALSE;
}

static BOOL is_bytecode_name_a(const char *name) {
	wchar_t wide[64];
	if (MultiByteToWideChar(CP_ACP, 0, name, -1, wide, 64) <= 0)
		return FALSE;
	return is_bytecode_name(wide);
}

// find_once returns the one occurrence of needle in buf, or NULL if there is
// none or more than one.
static const unsigned char *find_once(const unsigned char *buf, size_t n,
	const unsigned char *needle, size_t len) {
	const unsigned char *hit = NULL;
	size_t i;

	if (len == 0 || n < len)
		return NULL;
	for (i = 0; i + len <= n; i++) {
		if (buf[i] != needle[0] || memcmp(buf + i, needle, len) != 0)
			continue;
		if (hit != NULL)
			return NULL; // ambiguous: not the code we reverse-engineered
		hit = buf + i;
	}
	return hit;
}

// patch_bytecode rewrites the image in place, and only once every patch has
// resolved to exactly one site holding exactly the byte it expects. Returns
// the number of bytes changed (HL_PATCH_COUNT) or 0 with nothing written.
static unsigned patch_bytecode(unsigned char *buf, size_t n) {
	unsigned char *at[HL_PATCH_COUNT];
	unsigned i;

	if (n < sizeof(HL_MAGIC) - 1 || memcmp(buf, HL_MAGIC, sizeof(HL_MAGIC) - 1) != 0) {
		shim_log("patch: image does not start with %s", HL_MAGIC);
		return 0;
	}
	for (i = 0; i < HL_PATCH_COUNT; i++) {
		const unsigned char *hit = find_once(buf, n, hl_patches[i].needle, hl_patches[i].len);
		if (hit == NULL) {
			shim_log("patch %u: signature missing or ambiguous, image left untouched", i);
			return 0;
		}
		at[i] = (unsigned char *)hit + hl_patches[i].index;
		if (*at[i] != hl_patches[i].from) {
			shim_log("patch %u: byte at %lu is 0x%02x, want 0x%02x, image left untouched",
				i, (unsigned long)(at[i] - buf), *at[i], hl_patches[i].from);
			return 0;
		}
	}
	for (i = 0; i < HL_PATCH_COUNT; i++) {
		*at[i] = hl_patches[i].to;
		shim_log("patch %u: offset %lu 0x%02x -> 0x%02x", i, (unsigned long)(at[i] - buf),
			hl_patches[i].from, hl_patches[i].to);
	}
	return HL_PATCH_COUNT;
}

// read_all reads a whole file (at most HL_MAX_IMAGE bytes) into a malloc'd
// buffer. *size is 0 and the result NULL when the file is absent or unusable.
static unsigned char *read_all(const wchar_t *path, size_t *size) {
	HANDLE f;
	LARGE_INTEGER sz;
	unsigned char *buf;
	size_t pos = 0;

	*size = 0;
	f = open_raw(path, GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
		OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL);
	if (f == INVALID_HANDLE_VALUE)
		return NULL;
	if (!GetFileSizeEx(f, &sz) || sz.QuadPart <= 0 || sz.QuadPart > HL_MAX_IMAGE) {
		CloseHandle(f);
		return NULL;
	}
	buf = (unsigned char *)malloc((size_t)sz.QuadPart);
	if (buf == NULL) {
		CloseHandle(f);
		return NULL;
	}
	while (pos < (size_t)sz.QuadPart) {
		DWORD got = 0;
		if (!ReadFile(f, buf + pos, (DWORD)((size_t)sz.QuadPart - pos), &got, NULL) || got == 0) {
			free(buf);
			CloseHandle(f);
			return NULL;
		}
		pos += got;
	}
	CloseHandle(f);
	*size = pos;
	return buf;
}

// write_atomic writes data to path via a temp file and rename, so a partial
// copy can never be opened as the real thing.
static BOOL write_atomic(const wchar_t *path, const unsigned char *data, size_t size) {
	wchar_t tmp[MAX_PATH * 2];
	HANDLE f;
	DWORD put = 0;

	if (wcslen(path) + 5 > MAX_PATH * 2)
		return FALSE;
	wcscpy(tmp, path);
	wcscat(tmp, L".new");
	f = open_raw(tmp, GENERIC_WRITE, 0, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL);
	if (f == INVALID_HANDLE_VALUE)
		return FALSE;
	if (!WriteFile(f, data, (DWORD)size, &put, NULL) || put != size) {
		CloseHandle(f);
		DeleteFileW(tmp);
		return FALSE;
	}
	CloseHandle(f);
	if (!MoveFileExW(tmp, path, MOVEFILE_REPLACE_EXISTING)) {
		DeleteFileW(tmp);
		return FALSE;
	}
	return TRUE;
}

// fnv1a is a cheap content check; here it only identifies images in the log.
static unsigned long long fnv1a(const unsigned char *p, size_t n) {
	unsigned long long h = 1469598103934665603ULL;
	size_t i;
	for (i = 0; i < n; i++) {
		h ^= p[i];
		h *= 1099511628211ULL;
	}
	return h;
}

// materialise_copy makes sure copy holds exactly patch(source) and reports
// whether it may be handed out. FALSE means: use the original.
static BOOL materialise_copy(const wchar_t *source, const wchar_t *copy) {
	unsigned char *img, *cur;
	size_t n, m;
	BOOL ok = FALSE;

	img = read_all(source, &n);
	if (img == NULL) {
		shim_log("bytecode: cannot read the original (%lu)", (unsigned long)GetLastError());
		return FALSE;
	}
	shim_log("bytecode: original %lu bytes, fnv1a %08lx%08lx", (unsigned long)n,
		(unsigned long)(fnv1a(img, n) >> 32), (unsigned long)fnv1a(img, n));
	if (patch_bytecode(img, n) != HL_PATCH_COUNT) {
		free(img);
		return FALSE;
	}
	shim_log("bytecode: patched image fnv1a %08lx%08lx", (unsigned long)(fnv1a(img, n) >> 32),
		(unsigned long)fnv1a(img, n));

	cur = read_all(copy, &m);
	if (cur != NULL && m == n && memcmp(cur, img, n) == 0) {
		shim_log("bytecode: existing copy verified byte-for-byte, reused");
		ok = TRUE;
	} else {
		shim_log("bytecode: copy %s, regenerating", cur == NULL ? "missing" : "stale");
		if (write_atomic(copy, img, n)) {
			shim_log("bytecode: copy written, %lu bytes", (unsigned long)n);
			ok = TRUE;
		} else {
			shim_log("bytecode: writing the copy failed (%lu)", (unsigned long)GetLastError());
		}
	}
	free(cur);
	free(img);
	return ok;
}

// redirect_bytecode decides whether an open of name should go to our patched
// copy instead and, if so, fills copy with its path.
static BOOL redirect_bytecode(const wchar_t *name, DWORD access, DWORD disp,
	wchar_t *copy, DWORD cch) {
	wchar_t full[MAX_PATH * 2];
	wchar_t tail[64];
	DWORD n;
	BOOL ok;

	if (!is_bytecode_name(base_name(name)))
		return FALSE;
	if ((access & (GENERIC_WRITE | FILE_WRITE_DATA | FILE_APPEND_DATA | DELETE)) != 0 ||
			disp != OPEN_EXISTING)
		return FALSE; // only plain reads are the loader's; anything else is not ours
	n = GetFullPathNameW(name, MAX_PATH * 2, full, NULL);
	if (n == 0 || n >= MAX_PATH * 2)
		return FALSE;
	wcscpy(tail, L"\\");
	wcscat(tail, base_name(full));
	if (!helper_path(tail, copy, cch))
		return FALSE;
	if (_wcsicmp(full, copy) == 0)
		return FALSE; // this is our own copy being opened: pass through
	if (!hl_lock_ready)
		return FALSE;

	EnterCriticalSection(&hl_lock);
	log_wpath("bytecode: open", full);
	ok = materialise_copy(full, copy);
	if (ok)
		log_wpath("bytecode: redirected to", copy);
	else
		shim_log("bytecode: handing out the untouched original");
	LeaveCriticalSection(&hl_lock);
	return ok;
}

static HANDLE WINAPI detour_create_file_w(LPCWSTR name, DWORD access, DWORD share,
	LPSECURITY_ATTRIBUTES sa, DWORD disp, DWORD flags, HANDLE tmpl) {
	wchar_t copy[MAX_PATH * 2];
	if (name != NULL && redirect_bytecode(name, access, disp, copy, MAX_PATH * 2)) {
		HANDLE h = real_create_file_w(copy, access, share, sa, disp, flags, tmpl);
		if (h != INVALID_HANDLE_VALUE)
			return h;
		shim_log("bytecode: opening the copy failed (%lu), falling back to the original",
			(unsigned long)GetLastError());
	}
	return real_create_file_w(name, access, share, sa, disp, flags, tmpl);
}

static HANDLE WINAPI detour_create_file_a(LPCSTR name, DWORD access, DWORD share,
	LPSECURITY_ATTRIBUTES sa, DWORD disp, DWORD flags, HANDLE tmpl) {
	wchar_t wide[MAX_PATH * 2];
	wchar_t copy[MAX_PATH * 2];
	if (name != NULL && is_bytecode_name_a(base_name_a(name)) &&
			MultiByteToWideChar(CP_ACP, 0, name, -1, wide, MAX_PATH * 2) > 0 &&
			redirect_bytecode(wide, access, disp, copy, MAX_PATH * 2)) {
		HANDLE h = real_create_file_w(copy, access, share, sa, disp, flags, tmpl);
		if (h != INVALID_HANDLE_VALUE)
			return h;
		shim_log("bytecode: opening the copy failed (%lu), falling back to the original",
			(unsigned long)GetLastError());
	}
	return real_create_file_a(name, access, share, sa, disp, flags, tmpl);
}

// hook_bytecode_open runs from DllMain: the bytecode is opened from the game's
// main() before the worker thread could possibly start, so this hook cannot
// wait for it. Only kernelbase/kernel32 are touched; both are mapped and
// initialised long before us.
static void hook_bytecode_open(void) {
	HMODULE kb, k32;
	MH_STATUS st;
	void *w, *a;

	InitializeCriticalSection(&hl_lock);
	hl_lock_ready = TRUE;

	st = MH_Initialize();
	if (st != MH_OK) {
		shim_log("hook: MH_Initialize failed: %s", MH_StatusToString(st));
		return;
	}
	kb = GetModuleHandleW(L"kernelbase.dll");
	k32 = GetModuleHandleW(L"kernel32.dll");
	if (k32 != NULL) {
		shim_log("hook: kernel32!CreateFileW=%p kernel32!ReadFile=%p",
			(void *)GetProcAddress(k32, "CreateFileW"), (void *)GetProcAddress(k32, "ReadFile"));
	}
	if (kb != NULL) {
		shim_log("hook: kernelbase!CreateFileW=%p kernelbase!ReadFile=%p",
			(void *)GetProcAddress(kb, "CreateFileW"), (void *)GetProcAddress(kb, "ReadFile"));
	} else {
		shim_log("hook: kernelbase.dll not loaded, hooking kernel32 instead");
		kb = k32;
	}
	if (kb == NULL) {
		shim_log("hook: no kernel module, bytecode patch disabled");
		return;
	}
	w = (void *)GetProcAddress(kb, "CreateFileW");
	a = (void *)GetProcAddress(kb, "CreateFileA");
	hook_one("kernelbase!CreateFileW", w, (void *)detour_create_file_w, (void **)&real_create_file_w);
	hook_one("kernelbase!CreateFileA", a, (void *)detour_create_file_a, (void **)&real_create_file_a);
}

// ---------------------------------------------------------------------------
// Embedded wartales-mp.exe: extract to %LOCALAPPDATA% and launch hidden.
// ---------------------------------------------------------------------------

extern const unsigned char _binary_wartales_mp_exe_start[];
extern const unsigned char _binary_wartales_mp_exe_end[];

// extract_needed reports whether the on-disk copy is missing or a different
// build from the embedded one.
static BOOL extract_needed(const wchar_t *exe, const unsigned char *data, size_t size) {
	HANDLE f;
	LARGE_INTEGER cur;
	unsigned char *buf;
	DWORD got = 0;
	BOOL differ = TRUE;

	f = CreateFileW(exe, GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING,
		FILE_ATTRIBUTE_NORMAL, NULL);
	if (f == INVALID_HANDLE_VALUE)
		return TRUE;
	if (!GetFileSizeEx(f, &cur) || (unsigned long long)cur.QuadPart != size) {
		CloseHandle(f);
		return TRUE;
	}
	buf = (unsigned char *)malloc(size);
	if (buf == NULL) {
		CloseHandle(f);
		return TRUE;
	}
	if (ReadFile(f, buf, (DWORD)size, &got, NULL) && got == size)
		differ = fnv1a(buf, size) != fnv1a(data, size);
	free(buf);
	CloseHandle(f);
	return differ;
}

// write_exe writes data to a temp file next to exe, then atomically renames it
// into place, so a half-written exe is never launched.
static BOOL write_exe(const wchar_t *exe, const unsigned char *data, size_t size) {
	wchar_t tmp[MAX_PATH * 2];
	HANDLE f;
	DWORD put = 0;

	if (wcslen(exe) + 5 > MAX_PATH * 2)
		return FALSE;
	wcscpy(tmp, exe);
	wcscat(tmp, L".new");
	f = CreateFileW(tmp, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
	if (f == INVALID_HANDLE_VALUE)
		return FALSE;
	if (!WriteFile(f, data, (DWORD)size, &put, NULL) || put != size) {
		CloseHandle(f);
		DeleteFileW(tmp);
		return FALSE;
	}
	CloseHandle(f);
	if (!MoveFileExW(tmp, exe, MOVEFILE_REPLACE_EXISTING)) {
		DeleteFileW(tmp);
		return FALSE;
	}
	return TRUE;
}

// start_helper extracts (if needed) and launches wartales-mp.exe hidden. Runs
// on the worker thread, never under the loader lock.
static void start_helper(void) {
	const unsigned char *data = _binary_wartales_mp_exe_start;
	size_t size = (size_t)(_binary_wartales_mp_exe_end - _binary_wartales_mp_exe_start);
	wchar_t dir[MAX_PATH * 2];
	wchar_t exe[MAX_PATH * 2];
	wchar_t cmdline[MAX_PATH * 2 + 4];
	STARTUPINFOW si;
	PROCESS_INFORMATION pi;

	// One helper per session, whoever loads us. The handle is deliberately
	// never closed: it must outlive this thread to keep the name taken.
	if (CreateMutexW(NULL, FALSE, L"Local\\wartales-mp-running") == NULL) {
		shim_log("helper: CreateMutexW failed (%lu)", (unsigned long)GetLastError());
		return;
	}
	if (GetLastError() == ERROR_ALREADY_EXISTS) {
		shim_log("helper: already running in this session, not started");
		return;
	}

	if (size == 0) {
		shim_log("helper: no embedded exe");
		return;
	}
	if (!helper_path(L"", dir, MAX_PATH * 2))
		return;
	CreateDirectoryW(dir, NULL);
	if (!helper_path(L"\\wartales-mp.exe", exe, MAX_PATH * 2))
		return;
	if (extract_needed(exe, data, size)) {
		if (!write_exe(exe, data, size)) {
			shim_log("helper: extract failed (%lu)", (unsigned long)GetLastError());
			return;
		}
		shim_log("helper: extracted %lu bytes", (unsigned long)size);
	}

	cmdline[0] = L'"';
	cmdline[1] = 0;
	wcscat(cmdline, exe);
	wcscat(cmdline, L"\"");

	ZeroMemory(&si, sizeof(si));
	si.cb = sizeof(si);
	si.dwFlags = STARTF_USESHOWWINDOW;
	si.wShowWindow = SW_HIDE;
	ZeroMemory(&pi, sizeof(pi));
	if (CreateProcessW(exe, cmdline, NULL, NULL, FALSE,
			CREATE_NO_WINDOW | DETACHED_PROCESS, NULL, dir, &si, &pi)) {
		shim_log("helper: started pid %lu", (unsigned long)pi.dwProcessId);
		CloseHandle(pi.hThread);
		CloseHandle(pi.hProcess);
	} else {
		shim_log("helper: CreateProcessW failed (%lu)", (unsigned long)GetLastError());
	}
}

// ---------------------------------------------------------------------------
// Worker thread: install the hooks and start the helper, off the loader lock.
// ---------------------------------------------------------------------------

// hook_one creates and enables an inline hook on proc, returning the trampoline
// (the callable original) in *orig.
static BOOL hook_one(const char *name, void *proc, void *detour, void **orig) {
	MH_STATUS st;
	if (proc == NULL) {
		shim_log("hook %s: symbol not found", name);
		return FALSE;
	}
	st = MH_CreateHook(proc, detour, orig);
	if (st != MH_OK) {
		shim_log("hook %s: MH_CreateHook failed: %s", name, MH_StatusToString(st));
		return FALSE;
	}
	st = MH_EnableHook(proc);
	if (st != MH_OK) {
		shim_log("hook %s: MH_EnableHook failed: %s", name, MH_StatusToString(st));
		return FALSE;
	}
	shim_log("hook %s: installed at %p", name, proc);
	return TRUE;
}

static DWORD WINAPI worker(LPVOID unused) {
	HMODULE libhl;
	HMODULE ssl;
	int tries;

	MH_STATUS st;

	(void)unused;
	start_helper();

	// DllMain already initialised MinHook for the bytecode hook.
	st = MH_Initialize();
	if (st != MH_OK && st != MH_ERROR_ALREADY_INITIALIZED) {
		shim_log("worker: MH_Initialize failed: %s", MH_StatusToString(st));
		return 0;
	}

	// libhl.dll imports us, so it is already mapped: hook it now.
	libhl = GetModuleHandleW(L"libhl.dll");
	if (libhl != NULL) {
		void *p = (void *)GetProcAddress(libhl, "hl_host_resolve");
		hook_one("libhl!hl_host_resolve", p, (void *)detour_host_resolve, (void **)&real_host_resolve);
	} else {
		shim_log("hook libhl!hl_host_resolve: libhl.dll not loaded");
	}

	// ssl.hdll is loaded lazily on first TLS use; poll until it appears. Up to
	// ~10 minutes at 200ms; conf_set_ca fires long after the module loads.
	for (tries = 0; tries < 3000; tries++) {
		ssl = GetModuleHandleW(L"ssl.hdll");
		if (ssl != NULL) {
			void *p = (void *)GetProcAddress(ssl, "ssl_conf_set_ca");
			real_cert_add_pem = (cert_add_pem_fn)(void *)GetProcAddress(ssl, "ssl_cert_add_pem");
			if (real_cert_add_pem == NULL)
				shim_log("hook ssl!ssl_conf_set_ca: ssl_cert_add_pem not found, CA cannot be added");
			hook_one("ssl!ssl_conf_set_ca", p, (void *)detour_conf_set_ca, (void **)&real_conf_set_ca);
			return 0;
		}
		Sleep(200);
	}
	shim_log("hook ssl!ssl_conf_set_ca: ssl.hdll never loaded, gave up");
	return 0;
}

BOOL WINAPI DllMain(HINSTANCE inst, DWORD reason, LPVOID reserved) {
	(void)reserved;
	if (reason == DLL_PROCESS_ATTACH) {
		HANDLE t;
		DisableThreadLibraryCalls(inst);
		log_open();
		shim_log("attach: winmm.dll proxy loaded");
		bind_forwards();      // before anything can call a forwarded export
		hook_bytecode_open(); // before the game's entry point opens hlboot.dat
		t = CreateThread(NULL, 0, worker, NULL, 0, NULL);
		if (t != NULL)
			CloseHandle(t);
		else
			shim_log("attach: CreateThread(worker) failed (%lu)", (unsigned long)GetLastError());
	}
	return TRUE;
}
