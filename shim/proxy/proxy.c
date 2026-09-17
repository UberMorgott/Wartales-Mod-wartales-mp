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
//  3. Patches two bytes of the HashLink bytecode on its way into memory, in the
//     ReadFile that loads hlboot.dat -- before libhl JITs it and long before any
//     of it runs. The file on disk is never touched. See hlpatch.h and
//     internal/hlpatch for what the two bytes are and why the game crashes
//     ("Null access", mpman/net/Client.hx:13) about a minute into an idle lobby
//     without them. The hook must exist before the game's entry point runs, so
//     it is the one thing installed from DllMain.
//
//  4. Extracts the embedded wartales-mp.exe to %LOCALAPPDATA%\wartales-mp\ (only
//     when missing or when the embedded build differs by hash) and starts it
//     hidden. It watches the game PID and exits with the game on its own.

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>

#include <stdlib.h>
#include <string.h>

#include "MinHook.h"
#include "hlpatch.h"

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
	unsigned i;
	if (n == 0 || n + 16 >= MAX_PATH)
		return;
	wcscpy(path + n, L"\\winmm.dll");
	real_winmm = LoadLibraryW(path);
	if (real_winmm == NULL)
		return;
	for (i = 0; i < mp_exports_count; i++)
		*mp_exports[i].slot = (void *)GetProcAddress(real_winmm, mp_exports[i].name);
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
// Hook 3: ReadFile (kernel32) -- patch the HashLink bytecode in flight.
//
// The game reads all of hlboot.dat with one fread into a heap buffer and hands
// that buffer straight to the bytecode reader and the JIT, so the read is the
// last moment at which the image is both complete and still only data. We do
// not hook the open: a read that returns megabytes starting with "HLB" is the
// bytecode and nothing else in the game does that. Anything unexpected (a
// signature that is missing, ambiguous, or no longer holds the byte we mean to
// replace -- a game update, say) leaves the image completely untouched, which
// costs us the fix and nothing else.
// ---------------------------------------------------------------------------

static BOOL hook_one(void *proc, void *detour, void **orig); // defined below

typedef BOOL(WINAPI *read_file_fn)(HANDLE, LPVOID, DWORD, LPDWORD, LPOVERLAPPED);
static read_file_fn real_read_file; // MinHook trampoline to the original
static volatile LONG hl_patched;

// HL_MIN_IMAGE is far below hlboot.dat (19 MB) and far above any read that
// could start with the magic by accident.
#define HL_MIN_IMAGE (4u * 1024u * 1024u)

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
// resolved to exactly one site holding exactly the byte it expects.
static BOOL patch_bytecode(unsigned char *buf, size_t n) {
	unsigned char *at[HL_PATCH_COUNT];
	unsigned i;

	for (i = 0; i < HL_PATCH_COUNT; i++) {
		const unsigned char *hit = find_once(buf, n, hl_patches[i].needle, hl_patches[i].len);
		if (hit == NULL)
			return FALSE;
		at[i] = (unsigned char *)hit + hl_patches[i].index;
		if (*at[i] != hl_patches[i].from)
			return FALSE;
	}
	for (i = 0; i < HL_PATCH_COUNT; i++)
		*at[i] = hl_patches[i].to;
	return TRUE;
}

static BOOL WINAPI detour_read_file(HANDLE file, LPVOID buf, DWORD count,
	LPDWORD got, LPOVERLAPPED ov) {
	BOOL ok = real_read_file(file, buf, count, got, ov);
	if (ok && !hl_patched && ov == NULL && got != NULL && *got >= HL_MIN_IMAGE &&
			memcmp(buf, HL_MAGIC, sizeof(HL_MAGIC) - 1) == 0) {
		if (patch_bytecode((unsigned char *)buf, (size_t)*got))
			hl_patched = 1;
	}
	return ok;
}

// hook_bytecode_loader runs from DllMain: the bytecode is read before the
// worker thread could possibly start, so this one hook cannot wait for it.
// Only kernel32 is touched, which is mapped and initialised long before us.
static void hook_bytecode_loader(void) {
	HMODULE k32;
	if (MH_Initialize() != MH_OK)
		return;
	k32 = GetModuleHandleW(L"kernel32.dll");
	if (k32 == NULL)
		return;
	hook_one((void *)GetProcAddress(k32, "ReadFile"), (void *)detour_read_file,
		(void **)&real_read_file);
}

// ---------------------------------------------------------------------------
// Embedded wartales-mp.exe: extract to %LOCALAPPDATA% and launch hidden.
// ---------------------------------------------------------------------------

extern const unsigned char _binary_wartales_mp_exe_start[];
extern const unsigned char _binary_wartales_mp_exe_end[];

// fnv1a is a cheap content check: good enough to notice a rebuilt exe and
// re-extract, which is all the "embedded build differs" test needs.
static unsigned long long fnv1a(const unsigned char *p, size_t n) {
	unsigned long long h = 1469598103934665603ULL;
	size_t i;
	for (i = 0; i < n; i++) {
		h ^= p[i];
		h *= 1099511628211ULL;
	}
	return h;
}

// helper_dir builds %LOCALAPPDATA%\wartales-mp and appends tail (may be empty).
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
	if (CreateMutexW(NULL, FALSE, L"Local\\wartales-mp-running") == NULL)
		return;
	if (GetLastError() == ERROR_ALREADY_EXISTS)
		return;

	if (size == 0)
		return;
	if (!helper_path(L"", dir, MAX_PATH * 2))
		return;
	CreateDirectoryW(dir, NULL);
	if (!helper_path(L"\\wartales-mp.exe", exe, MAX_PATH * 2))
		return;
	if (extract_needed(exe, data, size)) {
		if (!write_exe(exe, data, size))
			return;
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
		CloseHandle(pi.hThread);
		CloseHandle(pi.hProcess);
	}
}

// ---------------------------------------------------------------------------
// Worker thread: install the hooks and start the helper, off the loader lock.
// ---------------------------------------------------------------------------

// hook_one creates and enables an inline hook on proc, returning the trampoline
// (the callable original) in *orig.
static BOOL hook_one(void *proc, void *detour, void **orig) {
	if (proc == NULL)
		return FALSE;
	if (MH_CreateHook(proc, detour, orig) != MH_OK)
		return FALSE;
	return MH_EnableHook(proc) == MH_OK;
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
	if (st != MH_OK && st != MH_ERROR_ALREADY_INITIALIZED)
		return 0;

	// libhl.dll imports us, so it is already mapped: hook it now.
	libhl = GetModuleHandleW(L"libhl.dll");
	if (libhl != NULL) {
		void *p = (void *)GetProcAddress(libhl, "hl_host_resolve");
		hook_one(p, (void *)detour_host_resolve, (void **)&real_host_resolve);
	}

	// ssl.hdll is loaded lazily on first TLS use; poll until it appears. Up to
	// ~10 minutes at 200ms; conf_set_ca fires long after the module loads.
	for (tries = 0; tries < 3000; tries++) {
		ssl = GetModuleHandleW(L"ssl.hdll");
		if (ssl != NULL) {
			void *p = (void *)GetProcAddress(ssl, "ssl_conf_set_ca");
			real_cert_add_pem = (cert_add_pem_fn)(void *)GetProcAddress(ssl, "ssl_cert_add_pem");
			hook_one(p, (void *)detour_conf_set_ca, (void **)&real_conf_set_ca);
			break;
		}
		Sleep(200);
	}
	return 0;
}

BOOL WINAPI DllMain(HINSTANCE inst, DWORD reason, LPVOID reserved) {
	(void)reserved;
	if (reason == DLL_PROCESS_ATTACH) {
		HANDLE t;
		DisableThreadLibraryCalls(inst);
		bind_forwards();        // before anything can call a forwarded export
		hook_bytecode_loader(); // before the game's entry point reads hlboot.dat
		t = CreateThread(NULL, 0, worker, NULL, 0, NULL);
		if (t != NULL)
			CloseHandle(t);
	}
	return TRUE;
}
