// A stand-in libhl.dll for shimcheck: only hl_copy_bytes, which the SDR shim
// uses to hand the sender's SteamID back to the bytecode as fresh bytes. The
// real libhl needs its GC initialised, which no test process has.
//
// Built by shim\check.ps1 into <OutDir>\check-steam\libhl.dll.

#include <stdlib.h>
#include <string.h>

__declspec(dllexport) unsigned char *hl_copy_bytes(const unsigned char *ptr, int size) {
	unsigned char *b = (unsigned char *)malloc(size > 0 ? (size_t)size : 1);
	if (b != NULL && size > 0)
		memcpy(b, ptr, (size_t)size);
	return b;
}
