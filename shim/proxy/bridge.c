// The SDR bridge: lets the helper (wartales-mp.exe) send and receive bytes
// over ISteamNetworkingMessages, so the proxy-link (master-to-master lobby
// traffic) can travel through Valve's relay network when the host has no
// public endpoint at all.
//
// SDR lives in the game process (this shim, through the game's own Steam
// client); the master lives in the helper. The bridge is a loopback TCP
// listener inside the game process, bound to 127.0.0.1 on a port the OS picks,
// advertised to the helper through sdr.status together with a random token
// the helper must present first. Loopback TCP over a named pipe because the
// helper side is then an ordinary net.Conn in Go (deadlines, Close unblocking
// Read, no overlapped I/O), the game already uses winsock, a 127.0.0.1-only
// bind raises no firewall prompt, and an OS-chosen port cannot collide.
//
// Framing on the socket, both directions:
//   [type:u8][peer SteamID64:u64 LE][len:u32 LE][payload]
//   0 AUTH  helper -> shim   payload = the token from sdr.status; must be first
//   1 SEND  helper -> shim   payload goes to peer, reliable, on BRIDGE_CHANNEL
//   2 RECV  shim -> helper   payload arrived from peer on BRIDGE_CHANNEL
//   3 ERR   shim -> helper   payload = text; a SEND that Steam refused, with
//                            the peer it was for
//
// BRIDGE_CHANNEL is 100: the game's SteamService only ever uses channel 0 and
// the SDR shim serves it channels 0..7, so nothing the game sends or reads can
// touch the bridge, and bridge traffic never enters the game's queues (the
// game's pump only pulls the channel it asked for). A peer that is not part of
// a lobby cannot inject into the master through it either: every proxy-link
// session over SDR must open with the key carried in the join code (the
// helper checks it), and the helper drops streams from peers that never
// presented it.

#define _CRT_RAND_S // rand_s for the token
#include "shim.h"

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <winsock2.h>
#include <ws2tcpip.h>

#include "sdr.h"

#define BRIDGE_CHANNEL 100
#define BRIDGE_MAX_PAYLOAD (512 * 1024) // k_cbMaxSteamNetworkingSocketsMessageSizeSend
#define FRAME_HEAD 13

enum { FR_AUTH = 0, FR_SEND = 1, FR_RECV = 2, FR_ERR = 3 };

static SOCKET listener = INVALID_SOCKET;
static SOCKET client = INVALID_SOCKET;
static BOOL client_authed;
static CRITICAL_SECTION out_lock; // serialises writes to the client socket
static CRITICAL_SECTION client_lock;
static char token[33];
static unsigned short port;
static unsigned long frames_in, frames_out, bytes_in, bytes_out, dropped;

static void put_u32(unsigned char *p, uint32_t v) {
	p[0] = (unsigned char)v;
	p[1] = (unsigned char)(v >> 8);
	p[2] = (unsigned char)(v >> 16);
	p[3] = (unsigned char)(v >> 24);
}
static void put_u64(unsigned char *p, uint64_t v) {
	put_u32(p, (uint32_t)v);
	put_u32(p + 4, (uint32_t)(v >> 32));
}
static uint32_t get_u32(const unsigned char *p) {
	return (uint32_t)p[0] | (uint32_t)p[1] << 8 | (uint32_t)p[2] << 16 | (uint32_t)p[3] << 24;
}
static uint64_t get_u64(const unsigned char *p) { return (uint64_t)get_u32(p) | (uint64_t)get_u32(p + 4) << 32; }

static BOOL send_all(SOCKET s, const unsigned char *p, size_t n) {
	while (n > 0) {
		int k = send(s, (const char *)p, (int)(n > 65536 ? 65536 : n), 0);
		if (k <= 0)
			return FALSE;
		p += k;
		n -= (size_t)k;
	}
	return TRUE;
}

static BOOL recv_all(SOCKET s, unsigned char *p, size_t n) {
	while (n > 0) {
		int k = recv(s, (char *)p, (int)(n > 65536 ? 65536 : n), 0);
		if (k <= 0)
			return FALSE;
		p += k;
		n -= (size_t)k;
	}
	return TRUE;
}

// write_frame sends one frame to the current client, if any.
static BOOL write_frame(unsigned char type, uint64_t peer, const void *payload, uint32_t len) {
	unsigned char head[FRAME_HEAD];
	SOCKET s;
	BOOL ok;
	head[0] = type;
	put_u64(head + 1, peer);
	put_u32(head + 9, len);
	EnterCriticalSection(&out_lock);
	s = client;
	ok = s != INVALID_SOCKET && client_authed && send_all(s, head, FRAME_HEAD) && (len == 0 || send_all(s, payload, len));
	if (ok) {
		frames_out++;
		bytes_out += len;
	}
	LeaveCriticalSection(&out_lock);
	return ok;
}

static void send_err(uint64_t peer, const char *text) { write_frame(FR_ERR, peer, text, (uint32_t)strlen(text)); }

// pump_thread moves whatever arrives on BRIDGE_CHANNEL to the helper. Polled
// at 20 ms: the proxy-link is command traffic, not the game loop.
static DWORD WINAPI pump_thread(LPVOID unused) {
	sdr_msg *batch[32];
	int n, i;
	(void)unused;
	for (;;) {
		n = sdr_bridge_receive(BRIDGE_CHANNEL, batch, 32);
		for (i = 0; i < n; i++) {
			sdr_msg *m = batch[i];
			if (m->peer.type == SDR_IDENTITY_STEAMID && m->size >= 0 && write_frame(FR_RECV, m->peer.u.steam_id, m->data, (uint32_t)m->size)) {
				frames_in++;
				bytes_in += (unsigned long)m->size;
			} else {
				dropped++;
				if (dropped == 1 || dropped % 100 == 0)
					shim_log("bridge: %lu message(s) on channel %d dropped (no helper connected)", dropped, BRIDGE_CHANNEL);
			}
			sdr_bridge_release(m);
		}
		Sleep(n > 0 ? 1 : 20);
	}
	return 0; // not reached
}

// serve_client runs one helper connection until it ends.
static void serve_client(SOCKET s) {
	unsigned char head[FRAME_HEAD];
	unsigned char *payload = NULL;
	size_t cap = 0;
	BOOL authed = FALSE;

	for (;;) {
		unsigned char type;
		uint64_t peer;
		uint32_t len;
		if (!recv_all(s, head, FRAME_HEAD))
			break;
		type = head[0];
		peer = get_u64(head + 1);
		len = get_u32(head + 9);
		if (len > BRIDGE_MAX_PAYLOAD) {
			shim_log("bridge: frame of %lu bytes refused, closing the helper connection", (unsigned long)len);
			break;
		}
		if (len > cap) {
			unsigned char *np = (unsigned char *)realloc(payload, len);
			if (np == NULL)
				break;
			payload = np;
			cap = len;
		}
		if (len > 0 && !recv_all(s, payload, len))
			break;
		if (!authed) {
			if (type != FR_AUTH || len != strlen(token) || memcmp(payload, token, len) != 0) {
				shim_log("bridge: connection without a valid token refused");
				break;
			}
			authed = TRUE;
			EnterCriticalSection(&out_lock);
			client_authed = TRUE;
			LeaveCriticalSection(&out_lock);
			shim_log("bridge: helper connected and authenticated");
			continue;
		}
		if (type == FR_SEND) {
			int res = sdr_bridge_send(peer, payload, len, BRIDGE_CHANNEL);
			if (res != SDR_RESULT_OK) {
				char text[96];
				_snprintf(text, sizeof(text) - 1, "SendMessageToUser failed: EResult %d", res);
				text[sizeof(text) - 1] = 0;
				shim_log("bridge: send of %lu bytes to %llu: %s", (unsigned long)len, (unsigned long long)peer, text);
				send_err(peer, text);
			}
			continue;
		}
		shim_log("bridge: unexpected frame type %u from the helper, ignored", type);
	}
	free(payload);
}

static DWORD WINAPI accept_thread(LPVOID unused) {
	(void)unused;
	for (;;) {
		SOCKET s = accept(listener, NULL, NULL);
		SOCKET old;
		if (s == INVALID_SOCKET) {
			shim_log("bridge: accept failed (%d), listener closed", WSAGetLastError());
			return 0;
		}
		// One helper at a time; a newcomer replaces a stale connection.
		EnterCriticalSection(&client_lock);
		EnterCriticalSection(&out_lock);
		old = client;
		client = s;
		client_authed = FALSE;
		LeaveCriticalSection(&out_lock);
		LeaveCriticalSection(&client_lock);
		if (old != INVALID_SOCKET) {
			shim_log("bridge: a new helper connection replaces the previous one");
			closesocket(old);
		}
		serve_client(s);
		EnterCriticalSection(&out_lock);
		if (client == s) {
			client = INVALID_SOCKET;
			client_authed = FALSE;
		}
		LeaveCriticalSection(&out_lock);
		closesocket(s);
		shim_log("bridge: helper connection closed; %lu frames/%lu bytes in, %lu frames/%lu bytes out, %lu dropped",
			frames_in, bytes_in, frames_out, bytes_out, dropped);
	}
}

BOOL bridge_start(char *status, size_t cch) {
	WSADATA wsa;
	struct sockaddr_in addr;
	int len = sizeof(addr);
	HANDLE t;
	unsigned i;
	static const char hexd[] = "0123456789abcdef";

	if (listener != INVALID_SOCKET) {
		_snprintf(status, cch - 1, "bridge=127.0.0.1:%u token=%s", port, token);
		status[cch - 1] = 0;
		return TRUE;
	}
	InitializeCriticalSection(&out_lock);
	InitializeCriticalSection(&client_lock);
	if (WSAStartup(MAKEWORD(2, 2), &wsa) != 0) {
		shim_log("bridge: WSAStartup failed");
		return FALSE;
	}
	listener = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
	if (listener == INVALID_SOCKET) {
		shim_log("bridge: socket failed (%d)", WSAGetLastError());
		return FALSE;
	}
	memset(&addr, 0, sizeof(addr));
	addr.sin_family = AF_INET;
	addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
	addr.sin_port = 0; // any free port; the helper learns it from sdr.status
	if (bind(listener, (struct sockaddr *)&addr, sizeof(addr)) != 0 || listen(listener, 2) != 0 ||
			getsockname(listener, (struct sockaddr *)&addr, &len) != 0) {
		shim_log("bridge: bind/listen on 127.0.0.1 failed (%d)", WSAGetLastError());
		closesocket(listener);
		listener = INVALID_SOCKET;
		return FALSE;
	}
	port = ntohs(addr.sin_port);

	// A per-run token: only the helper that read sdr.status may use the bridge.
	for (i = 0; i < 32; i++) {
		unsigned r;
		rand_s(&r);
		token[i] = hexd[r & 15];
	}
	token[32] = 0;

	t = CreateThread(NULL, 0, accept_thread, NULL, 0, NULL);
	if (t != NULL)
		CloseHandle(t);
	t = CreateThread(NULL, 0, pump_thread, NULL, 0, NULL);
	if (t != NULL)
		CloseHandle(t);
	shim_log("bridge: listening on 127.0.0.1:%u for the helper, relaying on channel %d", port, BRIDGE_CHANNEL);
	_snprintf(status, cch - 1, "bridge=127.0.0.1:%u token=%s", port, token);
	status[cch - 1] = 0;
	return TRUE;
}
