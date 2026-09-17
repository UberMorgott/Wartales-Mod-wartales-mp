// Shared between proxy.c and sdr.c: the log and the SDR transport entry points.
#ifndef WARTALES_MP_SHIM_H
#define WARTALES_MP_SHIM_H

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>

// shim_log appends one line to %LOCALAPPDATA%\wartales-mp\shim.log (proxy.c).
void shim_log(const char *fmt, ...);

// helper_path builds %LOCALAPPDATA%\wartales-mp<tail> (proxy.c).
BOOL helper_path(const wchar_t *tail, wchar_t *out, DWORD cch);

// hook_one installs an inline hook (proxy.c).
BOOL hook_one(const char *name, void *proc, void *detour, void **orig);

// sdr_reset_status removes the status file left by a previous run. DllMain.
void sdr_reset_status(void);

// sdr_hook_steam diverts the legacy P2P natives of steam.hdll (already mapped)
// onto the SDR transport. Returns the number of natives hooked.
unsigned sdr_hook_steam(HMODULE steam, HMODULE libhl);

// sdr_warm_up initialises the transport as soon as the Steam API is up, so
// relay access is requested before the first packet. Worker thread; returns
// once it succeeded, failed for good, or gave up waiting.
void sdr_warm_up(void);

#endif
