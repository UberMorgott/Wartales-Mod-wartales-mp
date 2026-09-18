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
// It then proves the SDR transport without a Steam client. Three stand-ins
// are loaded from <fake dir> before the proxy: libhl.dll (hl_copy_bytes),
// steam.hdll (the legacy natives, which count every call and must stay at
// zero) and, in "sdr" mode, steam_api64.dll (a loopback ISteamNetworkingMessages).
// The natives are then called through their hlp_ resolvers exactly as the
// bytecode would, and the packet semantics are asserted: reliability flags
// per send type, packet boundaries, truncation, per-channel queues, the
// sender's SteamID, automatic session acceptance, close dropping the peer's
// queue. In "nosdr" mode there is no steam_api64.dll at all and the shim must
// fail closed: send false, nothing available, read null, reason in the log
// and in sdr.status, and still no legacy call.
//
//   shimcheck.exe <winmm.dll> <hlboot.dat> <scratch LOCALAPPDATA> <fake dir> sdr|nosdr

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <winsock2.h>
#include <ws2tcpip.h>

#include "../proxy/hlpatch.h"
#include "fake_steam.h"

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

// wait_log polls the log for needle for up to ms milliseconds.
static int wait_log(const wchar_t *log, const char *needle, DWORD ms) {
	DWORD start = GetTickCount();
	for (;;) {
		if (log_contains(log, needle))
			return 1;
		if (GetTickCount() - start > ms)
			return 0;
		Sleep(50);
	}
}

static int file_starts_with(const wchar_t *path, const char *prefix) {
	size_t n;
	unsigned char *b = read_raw(path, &n);
	int ok;
	if (b == NULL)
		return 0;
	b[n] = 0;
	ok = strncmp((char *)b, prefix, strlen(prefix)) == 0;
	free(b);
	return ok;
}

// ---------------------------------------------------------------- SDR check

// hlsteam's native signatures (native/networking.cpp).
typedef unsigned char *vuid;
typedef unsigned char (*send_fn)(vuid, unsigned char *, int, int, int);
typedef vuid (*read_fn)(unsigned char *, int, uint32_t *, int);
typedef unsigned char (*avail_fn)(uint32_t *, int);
typedef unsigned char (*session_fn)(vuid);
typedef void *(*session_data_fn)(vuid);
typedef void *(*hlp_fn)(const char **sign);
typedef void (*stats_fn)(fake_stats_t *);
typedef void (*inject_fn)(uint64_t, int, const void *, int);
typedef int (*fire_fn)(uint64_t);
typedef long (*legacy_fn)(void);

static HMODULE load_fake(const wchar_t *dir, const wchar_t *name) {
	wchar_t path[MAX_PATH * 2];
	HMODULE m;
	wcscpy(path, dir);
	wcscat(path, L"\\");
	wcscat(path, name);
	m = LoadLibraryW(path);
	printf("      %ls -> %p\n", path, (void *)m);
	return m;
}

// resolve goes through hlp_<name>, the way the HashLink module loader does.
static void *resolve(HMODULE steam, const char *name) {
	char hlp[64];
	const char *sign = NULL;
	hlp_fn f;
	strcpy(hlp, "hlp_");
	strcat(hlp, name);
	f = (hlp_fn)(void *)GetProcAddress(steam, hlp);
	return f != NULL ? f(&sign) : NULL;
}

static void put_uid(unsigned char *out, uint64_t id) { memcpy(out, &id, 8); }
static uint64_t get_uid(const unsigned char *in) {
	uint64_t id = 0;
	if (in != NULL)
		memcpy(&id, in, 8);
	return id;
}

static void check_sdr(int with_api, HMODULE steam, HMODULE api, const wchar_t *log, const wchar_t *scratch) {
	send_fn send = (send_fn)resolve(steam, "send_p2p_packet");
	read_fn read = (read_fn)resolve(steam, "read_p2p_packet");
	avail_fn avail = (avail_fn)resolve(steam, "is_p2p_packet_available");
	session_fn accept = (session_fn)resolve(steam, "accept_p2p_session");
	session_fn close = (session_fn)resolve(steam, "close_p2p_session");
	session_data_fn sdata = (session_data_fn)resolve(steam, "get_p2p_session_data");
	legacy_fn legacy = (legacy_fn)(void *)GetProcAddress(steam, "fake_legacy_calls");
	stats_fn stats = api != NULL ? (stats_fn)(void *)GetProcAddress(api, "fake_stats") : NULL;
	inject_fn inject = api != NULL ? (inject_fn)(void *)GetProcAddress(api, "fake_inject") : NULL;
	fire_fn fire = api != NULL ? (fire_fn)(void *)GetProcAddress(api, "fake_fire_session_request") : NULL;
	unsigned char peer[8], other[8], third[8], buf[64];
	uint32_t size, len;
	vuid from;
	fake_stats_t st;
	wchar_t status[MAX_PATH * 2];
	const uint64_t PEER = 0x0102030405060708ULL, OTHER = 0x00000000AABBCCDDULL, THIRD = 0x1234567890ABCDEFULL;

	wcscpy(status, scratch);
	wcscat(status, L"\\wartales-mp\\sdr.status");
	put_uid(peer, PEER);
	put_uid(other, OTHER);
	put_uid(third, THIRD);

	check(send != NULL && read != NULL && avail != NULL && accept != NULL && close != NULL && sdata != NULL && legacy != NULL,
		"legacy natives resolve through hlp_<name>");
	if (send == NULL || read == NULL || avail == NULL || accept == NULL || close == NULL || sdata == NULL || legacy == NULL)
		return;
	check(wait_log(log, "sdr: 6 of 6 legacy P2P natives diverted", 10000), "shim.log records all 6 natives diverted");
	check(((unsigned char *)send)[0] == 0xE9, "steam_send_p2p_packet carries an inline jmp (hook installed)");

	if (!with_api) {
		check(wait_log(log, "sdr: UNAVAILABLE, transport disabled: steam_api64.dll is not loaded", 10000),
			"shim.log names the reason SDR is unavailable");
		check(file_starts_with(status, "unavailable steam_api64.dll is not loaded"), "sdr.status carries the verdict for the helper");
		check(send(peer, (unsigned char *)"abcde", 5, 2, 0) == 0, "fail closed: send_p2p_packet returns false");
		size = 77;
		check(avail(&size, 0) == 0, "fail closed: is_p2p_packet_available returns false");
		len = 77;
		check(read(buf, sizeof(buf), &len, 0) == NULL, "fail closed: read_p2p_packet returns null");
		check(accept(peer) == 0, "fail closed: accept_p2p_session returns false");
		check(close(peer) == 1, "fail closed: close_p2p_session reports nothing open");
		check(sdata(peer) == NULL, "get_p2p_session_data answers null");
		check(legacy() == 0, "the legacy hlsteam bodies were never called");
		return;
	}

	check(stats != NULL && inject != NULL && fire != NULL, "fake steam_api64.dll test exports resolve");
	if (stats == NULL || inject == NULL || fire == NULL)
		return;
	check(wait_log(log, "sdr: READY: ISteamNetworkingMessages at", 10000), "warm-up bound ISteamNetworkingMessages before first use");
	check(log_contains(log, "sdr: InitRelayNetworkAccess called, relay status 100"), "InitRelayNetworkAccess called early");
	check(file_starts_with(status, "ok"), "sdr.status says ok");
	stats(&st);
	check(st.relay_inits == 1 && st.request_cb_set && st.failed_cb_set, "relay access requested once, session callbacks registered");

	// Send type -> reliability flags (EP2PSend -> k_nSteamNetworkingSend_*).
	check(send(peer, (unsigned char *)"abcde", 5, 2, 0) == 1, "send type 2 (Reliable) succeeds");
	stats(&st);
	check(st.sends == 1 && st.last_flags == (8 | 1 | 32), "type 2 -> Reliable|NoNagle|AutoRestart (0x29)");
	check(st.last_send_to == PEER && st.last_send_id_type == 16 && st.last_channel == 0, "target identity is SteamID64 from the 8-byte vuid, channel 0");
	check(send(peer, (unsigned char *)"u", 1, 0, 0) == 1, "send type 0 (Unreliable) succeeds");
	stats(&st);
	check(st.last_flags == 32, "type 0 -> Unreliable|AutoRestart (0x20)");
	check(send(peer, (unsigned char *)"xy", 2, 1, 0) == 1, "send type 1 (UnreliableNoDelay) succeeds");
	stats(&st);
	check(st.last_flags == (4 | 1 | 32), "type 1 -> Unreliable|NoDelay|NoNagle|AutoRestart (0x25)");
	check(send(peer, (unsigned char *)"rwb", 3, 3, 0) == 1, "send type 3 (ReliableWithBuffering) succeeds");
	stats(&st);
	check(st.last_flags == (8 | 32), "type 3 -> Reliable|AutoRestart (0x28)");
	check(send(peer, (unsigned char *)"x", 1, 7, 0) == 0, "unknown send type is refused");
	check(send(peer, (unsigned char *)"x", 1, 2, 99) == 0, "out-of-range channel is refused");
	stats(&st);
	check(st.sends == 4, "refused sends never reach Steam");

	// Queue order, next-size reporting, packet boundaries and truncation.
	size = 0;
	check(avail(&size, 0) == 1 && size == 5, "is_p2p_packet_available reports the next message's size (5)");
	len = 0;
	memset(buf, 0, sizeof(buf));
	from = read(buf, 3, &len, 0);
	check(from != NULL && len == 3 && memcmp(buf, "abc", 3) == 0, "read into a 3-byte buffer truncates to 3 bytes (\"abc\")");
	check(get_uid(from) == PEER, "read_p2p_packet returns the sender's SteamID as 8 bytes");
	size = 0;
	check(avail(&size, 0) == 1 && size == 1, "the truncated remainder is gone: next message is the 1-byte one");
	from = read(buf, sizeof(buf), &len, 0);
	check(from != NULL && len == 1 && buf[0] == 'u', "second packet read whole (\"u\")");
	from = read(buf, sizeof(buf), &len, 0);
	check(from != NULL && len == 2 && memcmp(buf, "xy", 2) == 0, "third packet read whole (\"xy\")");
	from = read(buf, sizeof(buf), &len, 0);
	check(from != NULL && len == 3 && memcmp(buf, "rwb", 3) == 0, "fourth packet read whole (\"rwb\")");
	check(read(buf, sizeof(buf), &len, 0) == NULL, "read on an empty channel returns null");
	check(avail(&size, 0) == 0, "is_p2p_packet_available is false on an empty channel");

	// Channels are independent queues.
	check(send(peer, (unsigned char *)"one", 3, 2, 1) == 1 && send(peer, (unsigned char *)"zero", 4, 2, 0) == 1,
		"sends on channels 1 and 0");
	check(avail(&size, 0) == 1 && size == 4, "channel 0 sees only its own message (4 bytes)");
	check(avail(&size, 1) == 1 && size == 3, "channel 1 sees only its own message (3 bytes)");
	from = read(buf, sizeof(buf), &len, 1);
	check(from != NULL && len == 3 && memcmp(buf, "one", 3) == 0, "channel 1 reads \"one\"");
	from = read(buf, sizeof(buf), &len, 0);
	check(from != NULL && len == 4 && memcmp(buf, "zero", 4) == 0, "channel 0 reads \"zero\"");

	// Sender identity comes from the message, not from whoever we sent to.
	inject(OTHER, 0, "from-other", 10);
	from = read(buf, sizeof(buf), &len, 0);
	check(from != NULL && get_uid(from) == OTHER && len == 10, "a message from another peer reports that peer's SteamID");

	// Sessions: incoming requests are accepted automatically; explicit
	// accept/close still reach Steam; close drops that peer's queued messages.
	check(fire(THIRD) == 1, "fake raised a session request");
	stats(&st);
	check(st.accepts == 1 && st.last_accept == THIRD, "session request auto-accepted for the requesting peer");
	check(log_contains(log, "sdr: session request from 1311768467294899695 (type 16): accepted"), "shim.log records the auto-accept");
	check(accept(peer) == 1, "accept_p2p_session forwards to AcceptSessionWithUser");
	stats(&st);
	check(st.accepts == 2 && st.last_accept == PEER, "explicit accept named the right peer");
	inject(PEER, 0, "p1", 2);
	inject(OTHER, 0, "o1", 2);
	inject(PEER, 1, "p2", 2);
	check(avail(&size, 0) == 1 && size == 2, "queued messages before close");
	check(close(peer) == 1, "close_p2p_session forwards to CloseSessionWithUser");
	stats(&st);
	check(st.closes == 1 && st.last_close == PEER, "close named the right peer");
	from = read(buf, sizeof(buf), &len, 0);
	check(from != NULL && get_uid(from) == OTHER && memcmp(buf, "o1", 2) == 0, "close dropped the closed peer's messages, kept the other peer's");
	check(read(buf, sizeof(buf), &len, 0) == NULL && read(buf, sizeof(buf), &len, 1) == NULL, "nothing from the closed peer remains on any channel");
	check(sdata(peer) == NULL, "get_p2p_session_data answers null");

	stats(&st);
	check(st.allocated == st.released, "every message handed out by Steam was released");
	printf("      fake: %u sends, %u allocated, %u released, %u accepts, %u closes\n", st.sends, st.allocated,
		st.released, st.accepts, st.closes);
	check(legacy() == 0, "the legacy hlsteam bodies were never called");
}

// ------------------------------------------------------------- bridge check

// Frames on the bridge socket: [type:u8][peer:u64 LE][len:u32 LE][payload].
enum { FR_AUTH = 0, FR_SEND = 1, FR_RECV = 2, FR_ERR = 3 };
#define BRIDGE_CHANNEL 100

static int send_frame(SOCKET s, unsigned char type, uint64_t peer, const void *payload, uint32_t len) {
	unsigned char head[13];
	head[0] = type;
	memcpy(head + 1, &peer, 8);
	memcpy(head + 9, &len, 4);
	if (send(s, (const char *)head, 13, 0) != 13)
		return 0;
	return len == 0 || send(s, (const char *)payload, (int)len, 0) == (int)len;
}

static int recv_exact(SOCKET s, unsigned char *p, int n) {
	while (n > 0) {
		int k = recv(s, (char *)p, n, 0);
		if (k <= 0)
			return 0;
		p += k;
		n -= k;
	}
	return 1;
}

// recv_frame reads one frame into type/peer/buf (NUL-terminated); 0 on close
// or timeout.
static int recv_frame(SOCKET s, unsigned char *type, uint64_t *peer, unsigned char *buf, uint32_t cap, uint32_t *len) {
	unsigned char head[13];
	if (!recv_exact(s, head, 13))
		return 0;
	*type = head[0];
	memcpy(peer, head + 1, 8);
	memcpy(len, head + 9, 4);
	if (*len >= cap || !recv_exact(s, buf, (int)*len))
		return 0;
	buf[*len] = 0;
	return 1;
}

static SOCKET bridge_connect(unsigned short port) {
	SOCKET s = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
	struct sockaddr_in a;
	DWORD tmo = 5000;
	memset(&a, 0, sizeof(a));
	a.sin_family = AF_INET;
	a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
	a.sin_port = htons(port);
	setsockopt(s, SOL_SOCKET, SO_RCVTIMEO, (const char *)&tmo, sizeof(tmo));
	if (connect(s, (struct sockaddr *)&a, sizeof(a)) != 0) {
		closesocket(s);
		return INVALID_SOCKET;
	}
	return s;
}

typedef void (*set_result_fn)(int);

static void check_bridge(int with_api, HMODULE api, const wchar_t *log, const wchar_t *scratch, avail_fn avail) {
	wchar_t status_path[MAX_PATH * 2];
	size_t n;
	unsigned char *status;
	char token[64] = {0};
	unsigned port = 0;
	SOCKET s;
	WSADATA wsa;
	unsigned char type, buf[256];
	uint64_t peer;
	uint32_t len, size;
	fake_stats_t st;
	stats_fn stats = api != NULL ? (stats_fn)(void *)GetProcAddress(api, "fake_stats") : NULL;
	inject_fn inject = api != NULL ? (inject_fn)(void *)GetProcAddress(api, "fake_inject") : NULL;
	set_result_fn set_result = api != NULL ? (set_result_fn)(void *)GetProcAddress(api, "fake_set_send_result") : NULL;
	const uint64_t PEER = 0x0102030405060708ULL, OTHER = 0x00000000AABBCCDDULL;

	wcscpy(status_path, scratch);
	wcscat(status_path, L"\\wartales-mp\\sdr.status");
	status = read_raw(status_path, &n);
	if (status == NULL) {
		check(0, "sdr.status exists");
		return;
	}
	status[n] = 0;
	printf("      sdr.status: %s", (char *)status);
	if (!with_api) {
		check(strncmp((char *)status, "unavailable", 11) == 0 && strstr((char *)status, "bridge=") == NULL,
			"nosdr: no bridge is advertised, the helper is told SDR is unavailable");
		free(status);
		return;
	}
	check(sscanf((char *)status, "ok bridge=127.0.0.1:%u token=%63s", &port, token) == 2 && port > 0 && strlen(token) == 32,
		"sdr.status advertises the bridge port and a 32-hex token");
	free(status);
	check(log_contains(log, "bridge: listening on 127.0.0.1:"), "shim.log records the bridge listener");
	if (port == 0 || stats == NULL || inject == NULL || set_result == NULL)
		return;
	WSAStartup(MAKEWORD(2, 2), &wsa);

	// A wrong token is refused before anything else.
	s = bridge_connect((unsigned short)port);
	check(s != INVALID_SOCKET, "bridge accepts a loopback connection");
	check(send_frame(s, FR_AUTH, 0, "0000000000000000000000000000000000000000", 32) &&
			recv_frame(s, &type, &peer, buf, sizeof(buf), &len) == 0,
		"a wrong token gets the connection closed");
	closesocket(s);
	check(wait_log(log, "bridge: connection without a valid token refused", 5000), "shim.log records the refused token");

	// The real helper handshake.
	s = bridge_connect((unsigned short)port);
	check(s != INVALID_SOCKET && send_frame(s, FR_AUTH, 0, token, 32), "bridge connection with the token");
	check(wait_log(log, "bridge: helper connected and authenticated", 5000), "shim.log records the authenticated helper");

	// SEND goes to the peer reliably on channel 100 and, on the loopback fake,
	// comes straight back as RECV from that peer.
	check(send_frame(s, FR_SEND, PEER, "lobby-hello", 11), "SEND frame to the peer");
	check(recv_frame(s, &type, &peer, buf, sizeof(buf), &len) && type == FR_RECV && peer == PEER && len == 11 &&
			memcmp(buf, "lobby-hello", 11) == 0,
		"RECV frame comes back from that peer with the same 11 bytes");
	stats(&st);
	check(st.last_channel == BRIDGE_CHANNEL && st.last_flags == (8 | 1 | 32) && st.last_send_to == PEER,
		"bridge sends on channel 100, Reliable|NoNagle|AutoRestart");

	// A message from another peer on channel 100 reaches the helper tagged
	// with that peer; the game's channels never see it.
	inject(OTHER, BRIDGE_CHANNEL, "from-other", 10);
	check(recv_frame(s, &type, &peer, buf, sizeof(buf), &len) && type == FR_RECV && peer == OTHER && len == 10,
		"RECV from another peer is tagged with that peer's SteamID");
	size = 0;
	check(avail(&size, 0) == 0, "bridge traffic is invisible on the game's channel 0");

	// A refused send is reported to the helper, not swallowed.
	set_result(3); // k_EResultNoConnection
	check(send_frame(s, FR_SEND, PEER, "x", 1) && recv_frame(s, &type, &peer, buf, sizeof(buf), &len) &&
			type == FR_ERR && peer == PEER && strstr((char *)buf, "EResult 3") != NULL,
		"a failed send comes back as an ERR frame naming the peer and the EResult");
	set_result(1);

	closesocket(s);
	check(wait_log(log, "bridge: helper connection closed", 5000), "shim.log records the helper leaving");
	stats(&st);
	check(st.allocated == st.released, "bridge released every message it pulled");
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
	HMODULE kb, mod, fake_hl, fake_steam, fake_api = NULL;
	unsigned char *kb_cfw;
	HANDLE h;
	wchar_t *slash;
	wchar_t fake_dir_w[MAX_PATH * 2];
	int with_api;

	if (argc != 6 || (strcmp(argv[5], "sdr") != 0 && strcmp(argv[5], "nosdr") != 0)) {
		fprintf(stderr, "usage: shimcheck <winmm.dll> <hlboot.dat> <scratch LOCALAPPDATA> <fake dir> sdr|nosdr\n");
		return 2;
	}
	with_api = strcmp(argv[5], "sdr") == 0;
	MultiByteToWideChar(CP_ACP, 0, argv[4], -1, game_dir, MAX_PATH * 2);
	GetFullPathNameW(game_dir, MAX_PATH * 2, fake_dir_w, NULL);
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

	// 2b. The Steam stand-ins go in first, so the proxy's worker finds
	//     steam.hdll (and, in sdr mode, steam_api64.dll) the way it would in
	//     the game.
	printf("      mode: %s\n", with_api ? "sdr (loopback steam_api64.dll)" : "nosdr (no steam_api64.dll at all)");
	fake_hl = load_fake(fake_dir_w, L"libhl.dll");
	fake_steam = load_fake(fake_dir_w, L"steam.hdll");
	check(fake_hl != NULL && fake_steam != NULL, "fake libhl.dll and steam.hdll loaded");
	if (with_api) {
		fake_api = load_fake(fake_dir_w, L"steam_api64.dll");
		check(fake_api != NULL, "fake steam_api64.dll loaded");
	}
	check(GetModuleHandleW(L"steam_api64.dll") == fake_api, "steam_api64.dll presence matches the mode");

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

	// 11. The SDR transport, through the hooked natives.
	if (fake_steam != NULL)
		check_sdr(with_api, fake_steam, fake_api, log, scratch);

	// 12. The helper's bridge onto SDR (channel 100).
	if (fake_steam != NULL)
		check_bridge(with_api, fake_api, log, scratch, (avail_fn)resolve(fake_steam, "is_p2p_packet_available"));

	print_log(log);
	printf("%s: %d failure(s)\n", failures == 0 ? "OK" : "FAILED", failures);
	free(orig);
	free(exp);
	free(fake_img);
	return failures == 0 ? 0 : 1;
}
