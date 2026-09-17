// libhl.dll shim.
//
// Every export forwards to libhl_o.dll (the renamed original) except the two
// name-resolution entry points, which answer 127.0.0.1 for Shiro's master
// hosts and call through for everything else. On load it also starts
// wartales-mp.exe from the same directory, hidden.
//
// Signatures, from HashLink src/std/socket.c:
//
//	HL_PRIM int hl_host_resolve( vbyte *host );   // vbyte = unsigned char
//	DEFINE_PRIM(_I32, host_resolve, _BYTES);
//
// and from src/hl.h, which is what makes hlp_* a resolver rather than the
// function itself:
//
//	void *hlp_host_resolve( const char **sign );  // *sign = signature, ret = fn
//
// The return value is what inet_addr() produces: the four address bytes in
// network order packed into an int, so 127.0.0.1 is 0x0100007F.

#include "../shim_common.h"

#include <stdlib.h>
#include <string.h>

// LOOPBACK is inet_addr("127.0.0.1"): bytes 127,0,0,1 in memory order.
#define LOOPBACK 0x0100007F

typedef int (*host_resolve_fn)(unsigned char *host);
typedef void *(*hl_prim_fn)(const char **sign);

static host_resolve_fn real_host_resolve;
static hl_prim_fn real_hlp_host_resolve;

static INIT_ONCE originals_once = INIT_ONCE_STATIC_INIT;

static BOOL CALLBACK load_originals(PINIT_ONCE once, PVOID param, PVOID *ctx) {
	HMODULE h;
	(void)once;
	(void)param;
	(void)ctx;
	h = shim_original(L"libhl_o.dll");
	if (h != NULL) {
		real_host_resolve = (host_resolve_fn)(void *)GetProcAddress(h, "hl_host_resolve");
		real_hlp_host_resolve = (hl_prim_fn)(void *)GetProcAddress(h, "hlp_host_resolve");
	}
	return TRUE;
}

static void originals(void) {
	InitOnceExecuteOnce(&originals_once, load_originals, NULL, NULL);
}

// The names Wartales hard-codes for the Shiro master (Main.initMpman).
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

// hl_host_resolve replaces the libhl export of the same name.
int hl_host_resolve(unsigned char *host) {
	originals();
	if (host != NULL && is_master_host((const char *)host))
		return (int)LOOPBACK;
	if (real_host_resolve == NULL)
		return -1; // same failure value the original uses for an unknown host
	return real_host_resolve(host);
}

// hlp_host_resolve is the primitive resolver HashLink calls when binding
// std@host_resolve. The signature string is taken from the original so it can
// never drift from the real one; only the function pointer is ours.
void *hlp_host_resolve(const char **sign) {
	originals();
	if (real_hlp_host_resolve == NULL)
		return NULL;
	{
		void *orig = real_hlp_host_resolve(sign);
		if (orig != NULL)
			real_host_resolve = (host_resolve_fn)orig;
	}
	return (void *)&hl_host_resolve;
}

// launch starts wartales-mp.exe next to us, hidden and without a console. It
// runs on its own thread so that nothing heavier than CreateThread happens
// under the loader lock.
static DWORD WINAPI launch(LPVOID unused) {
	wchar_t exe[MAX_PATH * 2];
	wchar_t dir[MAX_PATH * 2];
	wchar_t cmdline[MAX_PATH * 2 + 4];
	STARTUPINFOW si;
	PROCESS_INFORMATION pi;

	(void)unused;
	// One helper per session, whatever loads us. The handle is deliberately
	// never closed: it must outlive this thread to keep the name taken.
	if (CreateMutexW(NULL, FALSE, L"Local\\wartales-mp-running") == NULL)
		return 0;
	if (GetLastError() == ERROR_ALREADY_EXISTS)
		return 0;

	if (!shim_dir_path(L"wartales-mp.exe", exe, MAX_PATH * 2))
		return 0;
	if (GetFileAttributesW(exe) == INVALID_FILE_ATTRIBUTES)
		return 0;
	if (!shim_dir_path(L"", dir, MAX_PATH * 2))
		return 0;

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
	return 0;
}

BOOL WINAPI DllMain(HINSTANCE inst, DWORD reason, LPVOID reserved) {
	(void)reserved;
	if (reason == DLL_PROCESS_ATTACH) {
		HANDLE t;
		shim_self = (HMODULE)inst;
		DisableThreadLibraryCalls(inst);
		t = CreateThread(NULL, 0, launch, NULL, 0, NULL);
		if (t != NULL)
			CloseHandle(t);
	}
	return TRUE;
}
