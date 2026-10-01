// A stand-in libhl.dll for shimcheck: hl_copy_bytes, which the SDR shim uses
// to hand the sender's SteamID back to the bytecode as fresh bytes, and
// hl_sys_print, which the shim copies into shim.log. The real libhl needs its
// GC initialised, which no test process has.
//
// Built by shim\check.ps1 into <OutDir>\check-steam\libhl.dll.

#include <stdlib.h>
#include <string.h>

static volatile int sys_print_calls;

// hl_sys_print is the native behind Sys.print: the UTF-16 bytes of a String.
__declspec(dllexport) void hl_sys_print(unsigned char *msg) {
	(void)msg;
	sys_print_calls++;
}

__declspec(dllexport) int fake_sys_print_calls(void) {
	return sys_print_calls;
}

__declspec(dllexport) unsigned char *hl_copy_bytes(const unsigned char *ptr, int size) {
	unsigned char *b = (unsigned char *)malloc(size > 0 ? (size_t)size : 1);
	if (b != NULL && size > 0)
		memcpy(b, ptr, (size_t)size);
	return b;
}
