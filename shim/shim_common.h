// Shared plumbing for the two drop-in shims.
//
// Both shims are ordinary DLLs whose export table is a complete list of
// forwarders to a renamed copy of the original binary (see tools/gendef), with
// a handful of names implemented here instead. Nothing is injected, patched or
// hooked at runtime: the loader does all the work.
#ifndef WARTALES_MP_SHIM_COMMON_H
#define WARTALES_MP_SHIM_COMMON_H

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>

#include <wchar.h>

// shim_self is the handle of this DLL, captured in DllMain.
static HMODULE shim_self;

// shim_dir_path writes "<directory of this DLL>\<name>" into out.
static BOOL shim_dir_path(const wchar_t *name, wchar_t *out, DWORD cch) {
	DWORD n = GetModuleFileNameW(shim_self, out, cch);
	DWORD i;
	if (n == 0 || n >= cch)
		return FALSE;
	for (i = n; i > 0; i--) {
		if (out[i - 1] == L'\\' || out[i - 1] == L'/')
			break;
	}
	out[i] = 0;
	if (wcslen(out) + wcslen(name) + 1 > cch)
		return FALSE;
	wcscat(out, name);
	return TRUE;
}

// shim_original loads the renamed original next to us. The loader has almost
// certainly mapped it already while resolving our forwarders; this only makes
// the handle available to the overridden functions.
static HMODULE shim_original(const wchar_t *name) {
	wchar_t path[MAX_PATH * 2];
	HMODULE h = GetModuleHandleW(name);
	if (h != NULL)
		return h;
	if (shim_dir_path(name, path, MAX_PATH * 2))
		h = LoadLibraryW(path);
	if (h == NULL)
		h = LoadLibraryW(name);
	return h;
}

#endif // WARTALES_MP_SHIM_COMMON_H
