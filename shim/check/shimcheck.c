// shimcheck: proves the winmm.dll proxy patches the bytecode without running
// the game.
//
// It loads the built winmm.dll into this process exactly as the Windows loader
// would (DllMain runs, hooks go in), then opens the retail hlboot.dat the way
// hashlink's load_code does (_wfopen + fread through ucrtbase) and the way a
// direct kernel32 import would (CreateFileW + chunked ReadFile), and asserts
// that what comes back differs from the on-disk original in exactly the bytes
// hlpatch.h names and nowhere else. It also checks the fallback (a file whose
// signature is broken comes back untouched), that write opens are left alone,
// and that the game folder is not modified.
//
// The game folder is only ever read. LOCALAPPDATA is pointed at a scratch
// directory before the DLL loads, so the real one is untouched, and the
// helper-exe mutex is taken first so the proxy does not start wartales-mp.exe.
//
//   shimcheck.exe <winmm.dll> <hlboot.dat> <scratch LOCALAPPDATA>

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "../proxy/hlpatch.h"

static int failures;

static void check(int ok, const char *what) {
	printf("%s  %s\n", ok ? "PASS" : "FAIL", what);
	if (!ok)
		failures++;
}

static unsigned char *read_raw(const wchar_t *path, size_t *size) {
	HANDLE f = CreateFileW(path, GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE, NULL,
		OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, NULL);
	LARGE_INTEGER sz;
	unsigned char *buf;
	size_t pos = 0;
	*size = 0;
	if (f == INVALID_HANDLE_VALUE)
		return NULL;
	if (!GetFileSizeEx(f, &sz)) {
		CloseHandle(f);
		return NULL;
	}
	buf = (unsigned char *)malloc((size_t)sz.QuadPart + 1);
	while (pos < (size_t)sz.QuadPart) {
		DWORD got = 0;
		// Deliberately small chunks: the shim must not depend on read size.
		DWORD want = (DWORD)((size_t)sz.QuadPart - pos);
		if (want > 65536)
			want = 65536;
		if (!ReadFile(f, buf + pos, want, &got, NULL) || got == 0)
			break;
		pos += got;
	}
	CloseHandle(f);
	*size = pos;
	return buf;
}

// read_crt mirrors hashlink src/main.c load_code: _wfopen "rb", size by ftell,
// fread until full.
static unsigned char *read_crt(const wchar_t *path, size_t *size) {
	FILE *f = _wfopen(path, L"rb");
	long n;
	size_t pos = 0;
	unsigned char *buf;
	*size = 0;
	if (f == NULL)
		return NULL;
	fseek(f, 0, SEEK_END);
	n = ftell(f);
	fseek(f, 0, SEEK_SET);
	buf = (unsigned char *)malloc((size_t)n + 1);
	while (pos < (size_t)n) {
		size_t r = fread(buf + pos, 1, (size_t)n - pos, f);
		if (r == 0)
			break;
		pos += r;
	}
	fclose(f);
	*size = pos;
	return buf;
}

static long find_once(const unsigned char *buf, size_t n, const unsigned char *needle, size_t len) {
	long hit = -1;
	size_t i;
	for (i = 0; i + len <= n; i++) {
		if (memcmp(buf + i, needle, len) != 0)
			continue;
		if (hit >= 0)
			return -1;
		hit = (long)i;
	}
	return hit;
}

// diff_is_exactly reports whether got differs from orig exactly at the
// HL_PATCH_COUNT offsets in at[], each holding its "to" byte.
static int diff_is_exactly(const unsigned char *orig, const unsigned char *got, size_t n,
	const long *at, const char *label) {
	size_t i, changed = 0, unexpected = 0;
	unsigned j;
	for (i = 0; i < n; i++) {
		if (orig[i] == got[i])
			continue;
		changed++;
		for (j = 0; j < HL_PATCH_COUNT; j++) {
			if ((long)i == at[j] && got[i] == hl_patches[j].to)
				break;
		}
		if (j == HL_PATCH_COUNT)
			unexpected++;
	}
	printf("      %s: %lu bytes differ from the original (%lu unexpected)\n", label,
		(unsigned long)changed, (unsigned long)unexpected);
	return changed == HL_PATCH_COUNT && unexpected == 0;
}

static int final_path_is(HANDLE h, const wchar_t *want) {
	wchar_t got[MAX_PATH * 2];
	DWORD n = GetFinalPathNameByHandleW(h, got, MAX_PATH * 2, FILE_NAME_NORMALIZED);
	const wchar_t *g = got;
	if (n == 0 || n >= MAX_PATH * 2)
		return 0;
	if (wcsncmp(g, L"\\\\?\\", 4) == 0)
		g += 4;
	printf("      handle -> %ls\n", g);
	return _wcsicmp(g, want) == 0;
}

// dir_snapshot hashes (name, size, mtime) of every entry in dir.
static unsigned long long dir_snapshot(const wchar_t *dir, unsigned *count) {
	wchar_t pat[MAX_PATH * 2];
	WIN32_FIND_DATAW fd;
	HANDLE h;
	unsigned long long acc = 1469598103934665603ULL;
	*count = 0;
	wcscpy(pat, dir);
	wcscat(pat, L"\\*");
	h = FindFirstFileW(pat, &fd);
	if (h == INVALID_HANDLE_VALUE)
		return 0;
	do {
		// Access times change on read; attributes, creation/write times,
		// size and name must not. cFileName is hashed only up to its NUL.
		unsigned long long e = 0;
		const unsigned char *p;
		size_t i;
#define MIX(field) \
	for (p = (const unsigned char *)&(field), i = 0; i < sizeof(field); i++) \
		e = (e ^ p[i]) * 1099511628211ULL
		MIX(fd.dwFileAttributes);
		MIX(fd.ftCreationTime);
		MIX(fd.ftLastWriteTime);
		MIX(fd.nFileSizeHigh);
		MIX(fd.nFileSizeLow);
#undef MIX
		for (i = 0; fd.cFileName[i] != 0; i++)
			e = (e ^ (unsigned long long)fd.cFileName[i]) * 1099511628211ULL;
		acc += e * 0x9E3779B97F4A7C15ULL;
		(*count)++;
	} while (FindNextFileW(h, &fd));
	FindClose(h);
	return acc;
}

static int log_contains(const wchar_t *log, const char *needle) {
	size_t n;
	unsigned char *b = read_raw(log, &n);
	int ok;
	if (b == NULL)
		return 0;
	b[n] = 0;
	ok = strstr((char *)b, needle) != NULL;
	free(b);
	return ok;
}

static void print_log(const wchar_t *log) {
	size_t n;
	unsigned char *b = read_raw(log, &n);
	if (b == NULL)
		return;
	b[n] = 0;
	printf("---- shim.log ----\n%s---- end ----\n", (char *)b);
	free(b);
}

int main(int argc, char **argv) {
	wchar_t dll[MAX_PATH * 2], game[MAX_PATH * 2], scratch[MAX_PATH * 2];
	wchar_t game_dir[MAX_PATH * 2], copy[MAX_PATH * 2], log[MAX_PATH * 2];
	wchar_t fake_dir[MAX_PATH * 2], fake[MAX_PATH * 2];
	unsigned char *orig, *exp, *got, *fake_img;
	size_t n, m;
	long at[HL_PATCH_COUNT];
	unsigned i;
	unsigned long long snap_before, snap_after;
	unsigned count_before, count_after;
	HMODULE kb, mod;
	unsigned char *kb_cfw;
	HANDLE h;
	wchar_t *slash;

	if (argc != 4) {
		fprintf(stderr, "usage: shimcheck <winmm.dll> <hlboot.dat> <scratch LOCALAPPDATA>\n");
		return 2;
	}
	// Absolute paths throughout: the check changes cwd to the game folder.
	MultiByteToWideChar(CP_ACP, 0, argv[1], -1, game_dir, MAX_PATH * 2);
	GetFullPathNameW(game_dir, MAX_PATH * 2, dll, NULL);
	MultiByteToWideChar(CP_ACP, 0, argv[3], -1, game_dir, MAX_PATH * 2);
	GetFullPathNameW(game_dir, MAX_PATH * 2, scratch, NULL);
	MultiByteToWideChar(CP_ACP, 0, argv[2], -1, game, MAX_PATH * 2);
	GetFullPathNameW(game, MAX_PATH * 2, game_dir, NULL);
	wcscpy(game, game_dir);
	slash = wcsrchr(game_dir, L'\\');
	if (slash != NULL)
		*slash = 0;

	// 1. The original, read before anything is hooked, and what we expect.
	orig = read_raw(game, &n);
	check(orig != NULL && n > 0, "read the original hlboot.dat");
	if (orig == NULL)
		return 1;
	printf("      original: %lu bytes\n", (unsigned long)n);
	exp = (unsigned char *)malloc(n);
	memcpy(exp, orig, n);
	for (i = 0; i < HL_PATCH_COUNT; i++) {
		long p = find_once(orig, n, hl_patches[i].needle, hl_patches[i].len);
		at[i] = p < 0 ? -1 : p + (long)hl_patches[i].index;
		if (at[i] >= 0)
			exp[at[i]] = hl_patches[i].to;
		printf("      patch %u expected at offset %ld\n", i, at[i]);
		check(at[i] >= 0 && orig[at[i]] == hl_patches[i].from, "signature found exactly once in the original");
	}
	snap_before = dir_snapshot(game_dir, &count_before);

	// 2. Isolate: scratch LOCALAPPDATA, no helper launch.
	CreateDirectoryW(scratch, NULL);
	SetEnvironmentVariableW(L"LOCALAPPDATA", scratch);
	check(CreateMutexW(NULL, FALSE, L"Local\\wartales-mp-running") != NULL, "helper mutex taken (no wartales-mp.exe launch)");
	wcscpy(copy, scratch);
	wcscat(copy, L"\\wartales-mp\\hlboot.dat");
	wcscpy(log, scratch);
	wcscat(log, L"\\wartales-mp\\shim.log");
	DeleteFileW(copy);
	DeleteFileW(log);

	// 3. Load the shim like the loader would.
	kb = GetModuleHandleW(L"kernelbase.dll");
	kb_cfw = (unsigned char *)GetProcAddress(kb, "CreateFileW");
	printf("      kernelbase!CreateFileW first byte before load: 0x%02x\n", kb_cfw[0]);
	mod = LoadLibraryW(dll);
	check(mod != NULL, "LoadLibraryW(winmm.dll proxy)");
	if (mod == NULL)
		return 1;
	printf("      kernelbase!CreateFileW first byte after load:  0x%02x\n", kb_cfw[0]);
	check(kb_cfw[0] == 0xE9, "kernelbase!CreateFileW carries an inline jmp (hook installed)");
	check(log_contains(log, "attach: winmm.dll proxy loaded"), "shim.log records the attach");
	check(log_contains(log, "hook kernelbase!CreateFileW: installed"), "shim.log records the CreateFileW hook");

	// 4. The hashlink way: _wfopen + fread via ucrtbase.
	got = read_crt(game, &m);
	check(got != NULL && m == n, "_wfopen/fread returns a full-size image");
	check(got != NULL && m == n && diff_is_exactly(orig, got, n, at, "fread"), "fread image differs in exactly the patched bytes");
	check(got != NULL && m == n && memcmp(got, exp, n) == 0, "fread image equals the expected patched image");
	free(got);
	check(log_contains(log, "bytecode: copy missing, regenerating") && log_contains(log, "bytecode: copy written"),
		"shim.log records the copy being generated");

	// 5. The kernel32 way: CreateFileW + 64 KB ReadFile chunks, handle identity.
	h = CreateFileW(game, GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE, NULL, OPEN_EXISTING,
		FILE_ATTRIBUTE_NORMAL, NULL);
	check(h != INVALID_HANDLE_VALUE, "CreateFileW(hlboot.dat) succeeds");
	check(h != INVALID_HANDLE_VALUE && final_path_is(h, copy), "handle points at the patched copy under LOCALAPPDATA");
	if (h != INVALID_HANDLE_VALUE)
		CloseHandle(h);
	got = read_raw(game, &m);
	check(got != NULL && m == n && memcmp(got, exp, n) == 0, "chunked ReadFile image equals the expected patched image");
	free(got);
	check(log_contains(log, "bytecode: existing copy verified byte-for-byte, reused"), "shim.log records the copy being reused");

	// 6. Relative name with the game folder as cwd (how Steam launches it).
	check(SetCurrentDirectoryW(game_dir), "chdir to the game folder");
	got = read_crt(L"hlboot.dat", &m);
	check(got != NULL && m == n && memcmp(got, exp, n) == 0, "relative \"hlboot.dat\" is redirected too");
	free(got);

	// 7. Our own copy, opened by its full path, is passed through (no recursion).
	got = read_raw(copy, &m);
	check(got != NULL && m == n && memcmp(got, exp, n) == 0, "the copy on disk is the expected patched image");
	free(got);

	// 8. Fallback: a bytecode whose signature is gone is served untouched.
	wcscpy(fake_dir, scratch);
	wcscat(fake_dir, L"\\fakegame");
	CreateDirectoryW(fake_dir, NULL);
	wcscpy(fake, fake_dir);
	wcscat(fake, L"\\hlboot.dat");
	fake_img = (unsigned char *)malloc(n);
	memcpy(fake_img, orig, n);
	fake_img[at[0]] = 0xff; // a "game update" that moved the code
	{
		HANDLE w = CreateFileW(fake, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
		DWORD put = 0;
		check(w != INVALID_HANDLE_VALUE && WriteFile(w, fake_img, (DWORD)n, &put, NULL) && put == n,
			"wrote a broken-signature hlboot.dat into the scratch folder");
		if (w != INVALID_HANDLE_VALUE)
			CloseHandle(w);
	}
	h = CreateFileW(fake, GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, NULL);
	check(h != INVALID_HANDLE_VALUE && final_path_is(h, fake), "broken signature: handle is the original file");
	if (h != INVALID_HANDLE_VALUE)
		CloseHandle(h);
	got = read_raw(fake, &m);
	check(got != NULL && m == n && memcmp(got, fake_img, n) == 0, "broken signature: contents are untouched");
	free(got);
	check(log_contains(log, "signature missing or ambiguous, image left untouched") &&
			log_contains(log, "handing out the untouched original"), "shim.log records the fallback");
	got = read_raw(copy, &m);
	check(got != NULL && m == n && memcmp(got, exp, n) == 0, "the good copy was not clobbered by the fallback");
	free(got);

	// 9. A write open is never redirected.
	h = CreateFileW(fake, GENERIC_READ | GENERIC_WRITE, 0, NULL, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, NULL);
	check(h != INVALID_HANDLE_VALUE && final_path_is(h, fake), "GENERIC_WRITE open goes to the real file");
	if (h != INVALID_HANDLE_VALUE)
		CloseHandle(h);

	// 10. The game folder is exactly as it was.
	snap_after = dir_snapshot(game_dir, &count_after);
	printf("      game folder: %u entries before, %u after\n", count_before, count_after);
	check(count_before == count_after && snap_before == snap_after, "game folder untouched (names, sizes, mtimes)");

	print_log(log);
	printf("%s: %d failure(s)\n", failures == 0 ? "OK" : "FAILED", failures);
	free(orig);
	free(exp);
	free(fake_img);
	return failures == 0 ? 0 : 1;
}
