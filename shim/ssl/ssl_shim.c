// ssl.hdll shim.
//
// Every export forwards to ssl_o.hdll (the renamed original) except the CA
// entry point, which appends our local CA to whatever chain the game passes in
// and then hands the result to the original. Certificate verification stays on
// and untouched: the game keeps requiring a valid chain and a matching host
// name, it just also trusts one more root.
//
// Signatures, from hashlink libs/ssl/ssl.c (HL_NAME(x) expands to ssl_##x for
// this library, which is why the export is ssl_conf_set_ca):
//
//	HL_PRIM void HL_NAME(conf_set_ca)(mbedtls_ssl_config *conf, hl_ssl_cert *cert);
//	DEFINE_PRIM(_VOID, conf_set_ca, TCONF TCERT);
//
//	HL_PRIM hl_ssl_cert *HL_NAME(cert_add_pem)(hl_ssl_cert *cert, vbyte *data);
//	DEFINE_PRIM(TCERT, cert_add_pem, TCERT _BYTES);
//
// and from src/hl.h:
//
//	void *hlp_conf_set_ca( const char **sign );   // *sign = signature, ret = fn
//
// cert_add_pem appends to cert->c in place when cert is non-NULL and allocates
// a fresh chain when it is NULL, so calling it is exactly "add one more CA".
// Doing it through the original hdll means no mbedtls types or symbols are
// needed here.

#include "../shim_common.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

// hl_ssl_cert is opaque to us; only the original hdll ever dereferences it.
typedef struct _hl_ssl_cert hl_ssl_cert;

typedef void (*conf_set_ca_fn)(void *conf, hl_ssl_cert *cert);
typedef hl_ssl_cert *(*cert_add_pem_fn)(hl_ssl_cert *cert, unsigned char *data);
typedef void *(*hl_prim_fn)(const char **sign);

static conf_set_ca_fn real_conf_set_ca;
static cert_add_pem_fn real_cert_add_pem;
static hl_prim_fn real_hlp_conf_set_ca;

static INIT_ONCE originals_once = INIT_ONCE_STATIC_INIT;

static BOOL CALLBACK load_originals(PINIT_ONCE once, PVOID param, PVOID *ctx) {
	HMODULE h;
	(void)once;
	(void)param;
	(void)ctx;
	h = shim_original(L"ssl_o.hdll");
	if (h != NULL) {
		real_conf_set_ca = (conf_set_ca_fn)(void *)GetProcAddress(h, "ssl_conf_set_ca");
		real_cert_add_pem = (cert_add_pem_fn)(void *)GetProcAddress(h, "ssl_cert_add_pem");
		real_hlp_conf_set_ca = (hl_prim_fn)(void *)GetProcAddress(h, "hlp_conf_set_ca");
	}
	return TRUE;
}

static void originals(void) {
	InitOnceExecuteOnce(&originals_once, load_originals, NULL, NULL);
}

// PEM_HEADER is checked before handing the file to mbedtls, so a truncated or
// half-written ca.crt degrades to "no extra CA" instead of throwing inside the
// game's TLS setup.
#define PEM_HEADER "-----BEGIN CERTIFICATE-----"
#define CA_MAX_BYTES (256 * 1024)

static char *ca_pem;          // NUL-terminated PEM, or NULL
static INIT_ONCE ca_once = INIT_ONCE_STATIC_INIT;

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

static BOOL CALLBACK load_ca(PINIT_ONCE once, PVOID param, PVOID *ctx) {
	wchar_t path[MAX_PATH * 2];
	HANDLE f;
	LARGE_INTEGER size;
	DWORD got = 0;
	char *buf;

	(void)once;
	(void)param;
	(void)ctx;
	if (!ca_path(path, MAX_PATH * 2))
		return TRUE;
	f = CreateFileW(path, GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE, NULL,
		OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, NULL);
	if (f == INVALID_HANDLE_VALUE)
		return TRUE;
	if (!GetFileSizeEx(f, &size) || size.QuadPart <= 0 || size.QuadPart > CA_MAX_BYTES) {
		CloseHandle(f);
		return TRUE;
	}
	buf = (char *)malloc((size_t)size.QuadPart + 1);
	if (buf == NULL) {
		CloseHandle(f);
		return TRUE;
	}
	if (!ReadFile(f, buf, (DWORD)size.QuadPart, &got, NULL) || got == 0) {
		free(buf);
		CloseHandle(f);
		return TRUE;
	}
	CloseHandle(f);
	buf[got] = 0;
	if (strstr(buf, PEM_HEADER) == NULL) {
		free(buf);
		return TRUE;
	}
	ca_pem = buf;
	return TRUE;
}

// local_ca returns the PEM of our CA, or NULL if it is not installed yet. The
// read is retried until it succeeds because wartales-mp.exe generates the file
// concurrently with the game starting up.
static const char *local_ca(void) {
	if (ca_pem == NULL) {
		InitOnceExecuteOnce(&ca_once, load_ca, NULL, NULL);
		if (ca_pem == NULL) {
			// Fall back to a direct retry: the one-shot above may have run
			// before wartales-mp.exe had written the file.
			load_ca(NULL, NULL, NULL);
		}
	}
	return ca_pem;
}

// ssl_conf_set_ca replaces the ssl.hdll export of the same name.
void ssl_conf_set_ca(void *conf, hl_ssl_cert *cert) {
	const char *pem;
	originals();
	if (real_conf_set_ca == NULL)
		return;
	pem = local_ca();
	if (pem != NULL && real_cert_add_pem != NULL) {
		hl_ssl_cert *merged = real_cert_add_pem(cert, (unsigned char *)pem);
		if (merged != NULL)
			cert = merged;
	}
	real_conf_set_ca(conf, cert);
}

// hlp_conf_set_ca is the primitive resolver HashLink calls when binding
// ssl@conf_set_ca. The signature string comes from the original, so it cannot
// drift; only the function pointer is ours.
void *hlp_conf_set_ca(const char **sign) {
	originals();
	if (real_hlp_conf_set_ca == NULL)
		return NULL;
	{
		void *orig = real_hlp_conf_set_ca(sign);
		if (orig != NULL)
			real_conf_set_ca = (conf_set_ca_fn)orig;
	}
	return (void *)&ssl_conf_set_ca;
}

BOOL WINAPI DllMain(HINSTANCE inst, DWORD reason, LPVOID reserved) {
	(void)reserved;
	if (reason == DLL_PROCESS_ATTACH) {
		shim_self = (HMODULE)inst;
		DisableThreadLibraryCalls(inst);
	}
	return TRUE;
}
