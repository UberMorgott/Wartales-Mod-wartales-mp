// SDR transport: the game's legacy Steam P2P natives, re-implemented on top of
// ISteamNetworkingMessages (Steam Datagram Relay).
//
// Wartales talks to Steam through hlsteam's steam.hdll, and its game-phase
// Steam transport (mpman.net.SteamService) is built on the DEPRECATED
// ISteamNetworking (SteamNetworking006) P2P calls: send_p2p_packet,
// read_p2p_packet, is_p2p_packet_available, accept_p2p_session,
// close_p2p_session, get_p2p_session_data. That legacy relay is precisely what
// fails for the players this mod exists for, so it is never allowed to carry
// traffic: every one of those natives is diverted here, permanently, and
// re-implemented on the modern ISteamNetworkingMessages interface through the
// flat C API of steam_api64.dll (SendMessageToUser / ReceiveMessagesOnChannel /
// AcceptSessionWithUser / CloseSessionWithUser). The game's unchanged calls
// then travel over Valve's modern relay network (SDR).
//
// Semantics the game depends on, preserved exactly (hlsteam native/common.cpp,
// native/networking.cpp; Valve's ISteamNetworking docs):
//   - a vuid is an 8-byte little-endian SteamID64 held in HashLink bytes;
//   - packets keep their boundaries: one SendMessageToUser per send, one
//     message per read;
//   - the send type (EP2PSend) maps onto the send flags of the new API:
//       0 Unreliable            -> Unreliable
//       1 UnreliableNoDelay     -> Unreliable | NoDelay | NoNagle
//       2 Reliable              -> Reliable | NoNagle
//       3 ReliableWithBuffering -> Reliable            (Nagle stays on)
//     plus AutoRestartBrokenSession, which is what the legacy API did on its
//     own when a session had dropped;
//   - per-channel FIFO queues: is_p2p_packet_available reports the size of
//     the NEXT message on that channel, read_p2p_packet returns it, truncated
//     to the caller's buffer exactly like ReadP2PPacket, and hands back the
//     sender's SteamID as a fresh 8-byte HashLink bytes value;
//   - incoming sessions are accepted automatically: the game's own accept is
//     driven by the legacy P2PSessionRequest_t callback, which never fires for
//     the new interface, so this file registers the modern session-request
//     callback and accepts there. accept_p2p_session still works when called.
//
// Fail closed, never fall back. When steam_api64.dll, the Steam client or
// ISteamNetworkingMessages is unavailable the natives behave as a transport
// that cannot connect (send fails, nothing is ever available to read), the
// exact reason goes to shim.log, and %LOCALAPPDATA%\wartales-mp\sdr.status
// carries the verdict for the helper, whose master reads it before choosing a
// transport. The original hlsteam implementations are never called.
//
// Everything below is verified against the shipped DLLs: the flat exports are
// present in steam_api64.dll (forwarded to steam_api64_o.dll by SmokeAPI) and
// the hlp_/steam_ pairs in steam.hdll; struct layouts follow
// steamnetworkingtypes.h, with the offsets asserted at compile time.

#include "shim.h"

#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

// ---------------------------------------------------------------------------
// Steamworks types (steamnetworkingtypes.h). Only what is touched here.
// ---------------------------------------------------------------------------

#include "sdr.h" // sdr_identity, sdr_msg: SteamNetworkingIdentity / SteamNetworkingMessage_t

C_ASSERT(sizeof(sdr_identity) == 136);
C_ASSERT(offsetof(sdr_msg, peer) == 16);
C_ASSERT(offsetof(sdr_msg, release) == 184);
C_ASSERT(offsetof(sdr_msg, channel) == 192);
C_ASSERT(sizeof(sdr_msg) == 216);

// k_nSteamNetworkingSend_*
#define SDR_SEND_UNRELIABLE 0
#define SDR_SEND_NO_NAGLE 1
#define SDR_SEND_NO_DELAY 4
#define SDR_SEND_RELIABLE 8
#define SDR_SEND_AUTO_RESTART 32

// Flat API (steam_api_flat.h): C++ references arrive as pointers, bool as a
// byte in AL.
typedef void *(*sdr_accessor_fn)(void);
typedef int32_t (*sdr_hsteamuser_fn)(void);
typedef int (*sdr_send_fn)(void *self, const sdr_identity *to, const void *data, uint32_t len, int flags, int channel);
typedef int (*sdr_receive_fn)(void *self, int channel, sdr_msg **out, int max);
typedef unsigned char (*sdr_session_fn)(void *self, const sdr_identity *peer);
typedef void (*sdr_release_fn)(sdr_msg *m);
typedef void (*sdr_init_relay_fn)(void *self);
typedef int (*sdr_relay_status_fn)(void *self, void *details);
typedef unsigned char (*sdr_set_cb_fn)(void *self, void (*cb)(void *ev));

static struct {
	sdr_hsteamuser_fn get_hsteamuser;
	sdr_accessor_fn messages_v002;
	sdr_accessor_fn utils_v004;
	sdr_send_fn send;
	sdr_receive_fn receive;
	sdr_session_fn accept;
	sdr_session_fn close;
	sdr_release_fn release;
	sdr_init_relay_fn init_relay;
	sdr_relay_status_fn relay_status;
	sdr_set_cb_fn on_request;
	sdr_set_cb_fn on_failed;
} api;

// Resolved by name; a missing export is a hard failure with the name logged.
static const struct {
	const char *name;
	void **slot;
} api_exports[] = {
	{"SteamAPI_GetHSteamUser", (void **)&api.get_hsteamuser},
	{"SteamAPI_SteamNetworkingMessages_SteamAPI_v002", (void **)&api.messages_v002},
	{"SteamAPI_SteamNetworkingUtils_SteamAPI_v004", (void **)&api.utils_v004},
	{"SteamAPI_ISteamNetworkingMessages_SendMessageToUser", (void **)&api.send},
	{"SteamAPI_ISteamNetworkingMessages_ReceiveMessagesOnChannel", (void **)&api.receive},
	{"SteamAPI_ISteamNetworkingMessages_AcceptSessionWithUser", (void **)&api.accept},
	{"SteamAPI_ISteamNetworkingMessages_CloseSessionWithUser", (void **)&api.close},
	{"SteamAPI_SteamNetworkingMessage_t_Release", (void **)&api.release},
	{"SteamAPI_ISteamNetworkingUtils_InitRelayNetworkAccess", (void **)&api.init_relay},
	{"SteamAPI_ISteamNetworkingUtils_GetRelayNetworkStatus", (void **)&api.relay_status},
	{"SteamAPI_ISteamNetworkingUtils_SetGlobalCallback_MessagesSessionRequest", (void **)&api.on_request},
	{"SteamAPI_ISteamNetworkingUtils_SetGlobalCallback_MessagesSessionFailed", (void **)&api.on_failed},
};

// ---------------------------------------------------------------------------
// HashLink side (hlsteam): vuid = vbyte* to 8 bytes; results are fresh bytes.
// ---------------------------------------------------------------------------

typedef unsigned char *vuid;
typedef unsigned char *(*hl_copy_bytes_fn)(const unsigned char *ptr, int size);
static hl_copy_bytes_fn hl_copy_bytes_p;

static uint64_t uid_to_u64(vuid v) {
	uint64_t id;
	if (v == NULL)
		return 0;
	memcpy(&id, v, 8);
	return id;
}

static void identity_of(sdr_identity *out, uint64_t id) {
	memset(out, 0, sizeof(*out));
	out->type = SDR_IDENTITY_STEAMID;
	out->cb = sizeof(uint64_t);
	out->u.steam_id = id;
}

// ---------------------------------------------------------------------------
// State.
// ---------------------------------------------------------------------------

enum sdr_state { SDR_IDLE, SDR_READY, SDR_FAILED };

static CRITICAL_SECTION sdr_lock;
static BOOL sdr_lock_ready;
static enum sdr_state state;
static void *sdr_msgs;  // ISteamNetworkingMessages*
static void *sdr_utils; // ISteamNetworkingUtils*
static DWORD sdr_retry_at;
static char sdr_last_reason[256];

// Per-channel FIFO of received messages, pulled in batches from Steam.
#define SDR_CHANNELS 8
#define SDR_PULL_BATCH 64

typedef struct sdr_node {
	sdr_msg *m;
	struct sdr_node *next;
} sdr_node;

static struct {
	sdr_node *head, *tail;
} sdr_queue[SDR_CHANNELS];

// Counters for the log.
static unsigned long sdr_sent, sdr_send_failed, sdr_received, sdr_dropped, sdr_accepted;

static void sdr_init_lock(void) {
	if (!sdr_lock_ready) {
		InitializeCriticalSection(&sdr_lock);
		sdr_lock_ready = TRUE;
	}
}

// write_status leaves the verdict where the helper can read it.
static void write_status(const char *text) {
	wchar_t path[MAX_PATH * 2];
	HANDLE f;
	DWORD put;
	if (!helper_path(L"\\sdr.status", path, MAX_PATH * 2))
		return;
	f = CreateFileW(path, GENERIC_WRITE, FILE_SHARE_READ, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
	if (f == INVALID_HANDLE_VALUE)
		return;
	WriteFile(f, text, (DWORD)strlen(text), &put, NULL);
	CloseHandle(f);
}

void sdr_reset_status(void) {
	wchar_t path[MAX_PATH * 2];
	sdr_init_lock();
	if (helper_path(L"\\sdr.status", path, MAX_PATH * 2))
		DeleteFileW(path);
}

// fail records a reason. Hard failures are final; soft ones (the Steam API is
// simply not up yet) are retried every two seconds.
static void fail(BOOL hard, const char *reason) {
	char line[300];
	if (strcmp(sdr_last_reason, reason) != 0) {
		strncpy(sdr_last_reason, reason, sizeof(sdr_last_reason) - 1);
		shim_log("sdr: %s: %s", hard ? "UNAVAILABLE, transport disabled" : "not ready", reason);
		_snprintf(line, sizeof(line) - 1, "%s %s\n", hard ? "unavailable" : "pending", reason);
		line[sizeof(line) - 1] = 0;
		write_status(line);
	}
	if (hard)
		state = SDR_FAILED;
	else
		sdr_retry_at = GetTickCount() + 2000;
}

// on_session_request is the modern counterpart of P2PSessionRequest_t; the
// game only ever answers the legacy one, which the new interface never raises.
static void on_session_request(void *ev) {
	const sdr_identity *peer = (const sdr_identity *)ev; // SteamNetworkingMessagesSessionRequest_t
	unsigned char ok = 0;
	if (peer == NULL || sdr_msgs == NULL)
		return;
	ok = api.accept(sdr_msgs, peer);
	sdr_accepted++;
	shim_log("sdr: session request from %llu (type %d): %s", (unsigned long long)peer->u.steam_id,
		peer->type, ok ? "accepted" : "accept FAILED");
}

static void on_session_failed(void *ev) {
	// SteamNetworkingMessagesSessionFailed_t { SteamNetConnectionInfo_t m_info }:
	// m_identityRemote is the first field of the info struct.
	const sdr_identity *peer = (const sdr_identity *)ev;
	if (peer == NULL)
		return;
	shim_log("sdr: session with %llu FAILED (Steam reports the connection dropped)",
		(unsigned long long)peer->u.steam_id);
}

static BOOL ready(void);

// try_init resolves the flat API and the interfaces. Lock held.
static BOOL try_init(void) {
	HMODULE mod;
	unsigned i;
	int relay;
	char line[128];

	if (state == SDR_READY)
		return TRUE;
	if (state == SDR_FAILED)
		return FALSE;
	if (sdr_retry_at != 0 && (LONG)(GetTickCount() - sdr_retry_at) < 0)
		return FALSE;

	mod = GetModuleHandleW(L"steam_api64.dll");
	if (mod == NULL) {
		fail(TRUE, "steam_api64.dll is not loaded in this process");
		return FALSE;
	}
	for (i = 0; i < sizeof(api_exports) / sizeof(api_exports[0]); i++) {
		*api_exports[i].slot = (void *)GetProcAddress(mod, api_exports[i].name);
		if (*api_exports[i].slot == NULL) {
			_snprintf(line, sizeof(line) - 1, "steam_api64.dll lacks %s", api_exports[i].name);
			line[sizeof(line) - 1] = 0;
			fail(TRUE, line);
			return FALSE;
		}
	}
	if (hl_copy_bytes_p == NULL) {
		fail(TRUE, "libhl.dll lacks hl_copy_bytes");
		return FALSE;
	}
	if (api.get_hsteamuser() == 0) {
		fail(FALSE, "Steam API not initialised yet (SteamAPI_GetHSteamUser() == 0)");
		return FALSE;
	}
	sdr_msgs = api.messages_v002();
	if (sdr_msgs == NULL) {
		fail(TRUE, "SteamNetworkingMessages002 unavailable (Steam client too old, or not running)");
		return FALSE;
	}
	sdr_utils = api.utils_v004();
	if (sdr_utils == NULL) {
		shim_log("sdr: SteamNetworkingUtils004 unavailable: no relay warm-up, sessions must be accepted by the game");
	} else {
		api.on_request(sdr_utils, on_session_request);
		api.on_failed(sdr_utils, on_session_failed);
		api.init_relay(sdr_utils);
		relay = api.relay_status(sdr_utils, NULL);
		shim_log("sdr: InitRelayNetworkAccess called, relay status %d (100 = current)", relay);
	}
	state = SDR_READY;
	sdr_last_reason[0] = 0;
	shim_log("sdr: READY: ISteamNetworkingMessages at %p, utils at %p", sdr_msgs, sdr_utils);
	{
		// The helper's way in. Without it the game's own SDR path still works;
		// the helper then reports the lobby phase as unavailable over SDR.
		char bridge[160], status[200];
		if (bridge_start(bridge, sizeof(bridge))) {
			_snprintf(status, sizeof(status) - 1, "ok %s\n", bridge);
			status[sizeof(status) - 1] = 0;
			write_status(status);
		} else {
			write_status("ok bridge=none\n");
		}
	}
	return TRUE;
}

// ---------------------------------------------------------------------------
// The bridge's access to the transport (bridge.c). These take the lock like
// the game-facing natives; the interface itself is thread-safe.
// ---------------------------------------------------------------------------

int sdr_bridge_send(uint64_t peer, const void *data, uint32_t len, int channel) {
	sdr_identity to;
	int res;
	if (!ready())
		return -1;
	identity_of(&to, peer);
	EnterCriticalSection(&sdr_lock);
	res = api.send(sdr_msgs, &to, data, len, SDR_SEND_RELIABLE | SDR_SEND_NO_NAGLE | SDR_SEND_AUTO_RESTART, channel);
	LeaveCriticalSection(&sdr_lock);
	return res;
}

int sdr_bridge_receive(int channel, sdr_msg **out, int max) {
	int n;
	if (!sdr_lock_ready)
		return 0;
	EnterCriticalSection(&sdr_lock);
	n = state == SDR_READY ? api.receive(sdr_msgs, channel, out, max) : 0;
	LeaveCriticalSection(&sdr_lock);
	return n > 0 ? n : 0;
}

void sdr_bridge_release(sdr_msg *m) { api.release(m); }

static BOOL ready(void) {
	BOOL ok;
	if (!sdr_lock_ready)
		return FALSE;
	EnterCriticalSection(&sdr_lock);
	ok = try_init();
	LeaveCriticalSection(&sdr_lock);
	return ok;
}

// ---------------------------------------------------------------------------
// Queues. Lock held by the callers.
// ---------------------------------------------------------------------------

static void queue_push(int channel, sdr_msg *m) {
	sdr_node *n = (sdr_node *)malloc(sizeof(*n));
	if (n == NULL) {
		api.release(m);
		sdr_dropped++;
		return;
	}
	n->m = m;
	n->next = NULL;
	if (sdr_queue[channel].tail != NULL)
		sdr_queue[channel].tail->next = n;
	else
		sdr_queue[channel].head = n;
	sdr_queue[channel].tail = n;
}

static sdr_msg *queue_pop(int channel) {
	sdr_node *n = sdr_queue[channel].head;
	sdr_msg *m;
	if (n == NULL)
		return NULL;
	sdr_queue[channel].head = n->next;
	if (sdr_queue[channel].head == NULL)
		sdr_queue[channel].tail = NULL;
	m = n->m;
	free(n);
	return m;
}

// pump moves whatever Steam holds for channel into our queue, so the head is
// always the next message the game will read.
static void pump(int channel) {
	sdr_msg *batch[SDR_PULL_BATCH];
	int n, i;
	for (;;) {
		n = api.receive(sdr_msgs, channel, batch, SDR_PULL_BATCH);
		if (n <= 0)
			return;
		for (i = 0; i < n; i++) {
			if (batch[i]->size < 0 || batch[i]->peer.type != SDR_IDENTITY_STEAMID) {
				api.release(batch[i]); // not a Steam peer: nothing the game could name
				sdr_dropped++;
				continue;
			}
			queue_push(channel, batch[i]);
			sdr_received++;
		}
		if (n < SDR_PULL_BATCH)
			return;
	}
}

// drop_peer discards queued messages from one SteamID, like CloseP2PSessionWithUser.
static void drop_peer(uint64_t id) {
	int c;
	for (c = 0; c < SDR_CHANNELS; c++) {
		sdr_node **link = &sdr_queue[c].head;
		sdr_queue[c].tail = NULL;
		while (*link != NULL) {
			sdr_node *n = *link;
			if (n->m->peer.u.steam_id == id) {
				*link = n->next;
				api.release(n->m);
				free(n);
				sdr_dropped++;
				continue;
			}
			sdr_queue[c].tail = n;
			link = &n->next;
		}
	}
}

static BOOL channel_ok(int channel) {
	if (channel >= 0 && channel < SDR_CHANNELS)
		return TRUE;
	shim_log("sdr: channel %d is outside 0..%d, refused", channel, SDR_CHANNELS - 1);
	return FALSE;
}

// ---------------------------------------------------------------------------
// The diverted natives. Signatures are hlsteam's (native/networking.cpp).
// ---------------------------------------------------------------------------

static unsigned char detour_send_p2p_packet(vuid uid, unsigned char *data, int length, int type, int channel) {
	sdr_identity to;
	int flags, res;
	static int last_res;
	static DWORD last_log;

	if (uid == NULL || data == NULL || length < 0 || !channel_ok(channel) || !ready())
		return 0;
	switch (type) {
	case 0: flags = SDR_SEND_UNRELIABLE; break;                                   // k_EP2PSendUnreliable
	case 1: flags = SDR_SEND_UNRELIABLE | SDR_SEND_NO_DELAY | SDR_SEND_NO_NAGLE; break; // k_EP2PSendUnreliableNoDelay
	case 2: flags = SDR_SEND_RELIABLE | SDR_SEND_NO_NAGLE; break;                 // k_EP2PSendReliable
	case 3: flags = SDR_SEND_RELIABLE; break;                                     // k_EP2PSendReliableWithBuffering
	default:
		shim_log("sdr: send type %d unknown, packet refused", type);
		return 0;
	}
	flags |= SDR_SEND_AUTO_RESTART;
	identity_of(&to, uid_to_u64(uid));

	EnterCriticalSection(&sdr_lock);
	res = api.send(sdr_msgs, &to, data, (uint32_t)length, flags, channel);
	if (res == SDR_RESULT_OK) {
		sdr_sent++;
		if (sdr_sent == 1)
			shim_log("sdr: first packet sent to %llu, %d bytes, type %d -> flags 0x%x, channel %d",
				(unsigned long long)to.u.steam_id, length, type, flags, channel);
	} else {
		sdr_send_failed++;
		if (res != last_res || (DWORD)(GetTickCount() - last_log) > 5000) {
			last_res = res;
			last_log = GetTickCount();
			shim_log("sdr: SendMessageToUser(%llu, %d bytes, flags 0x%x, channel %d) = EResult %d (%lu failed so far)",
				(unsigned long long)to.u.steam_id, length, flags, channel, res, sdr_send_failed);
		}
	}
	LeaveCriticalSection(&sdr_lock);
	return res == SDR_RESULT_OK;
}

static unsigned char detour_is_p2p_packet_available(uint32_t *msg_size, int channel) {
	sdr_node *head;
	if (!channel_ok(channel) || !ready())
		return 0;
	EnterCriticalSection(&sdr_lock);
	pump(channel);
	head = sdr_queue[channel].head;
	if (head != NULL && msg_size != NULL)
		*msg_size = (uint32_t)head->m->size;
	LeaveCriticalSection(&sdr_lock);
	return head != NULL;
}

static vuid detour_read_p2p_packet(unsigned char *data, int max_length, uint32_t *length, int channel) {
	sdr_msg *m;
	uint64_t from;
	int n;
	vuid out;

	if (data == NULL || max_length < 0 || !channel_ok(channel) || !ready())
		return NULL;
	EnterCriticalSection(&sdr_lock);
	pump(channel);
	m = queue_pop(channel);
	if (m == NULL) {
		LeaveCriticalSection(&sdr_lock);
		return NULL;
	}
	n = m->size < max_length ? m->size : max_length; // truncate, like ReadP2PPacket
	memcpy(data, m->data, (size_t)n);
	from = m->peer.u.steam_id;
	api.release(m);
	if (sdr_received == 1)
		shim_log("sdr: first packet received from %llu, %d bytes, channel %d", (unsigned long long)from, n, channel);
	LeaveCriticalSection(&sdr_lock);
	if (length != NULL)
		*length = (uint32_t)n;
	out = hl_copy_bytes_p((const unsigned char *)&from, 8);
	return out;
}

static unsigned char detour_accept_p2p_session(vuid uid) {
	sdr_identity peer;
	unsigned char ok;
	if (uid == NULL || !ready())
		return 0;
	identity_of(&peer, uid_to_u64(uid));
	EnterCriticalSection(&sdr_lock);
	ok = api.accept(sdr_msgs, &peer);
	LeaveCriticalSection(&sdr_lock);
	shim_log("sdr: game accepted session with %llu: %s", (unsigned long long)peer.u.steam_id, ok ? "ok" : "no pending session");
	return ok;
}

static unsigned char detour_close_p2p_session(vuid uid) {
	sdr_identity peer;
	unsigned char ok;
	if (uid == NULL || !ready())
		return 1; // nothing was open
	identity_of(&peer, uid_to_u64(uid));
	EnterCriticalSection(&sdr_lock);
	drop_peer(peer.u.steam_id);
	ok = api.close(sdr_msgs, &peer);
	LeaveCriticalSection(&sdr_lock);
	shim_log("sdr: session with %llu closed (%s); sent %lu, failed %lu, received %lu, dropped %lu, auto-accepted %lu",
		(unsigned long long)peer.u.steam_id, ok ? "ok" : "was not open", sdr_sent, sdr_send_failed, sdr_received,
		sdr_dropped, sdr_accepted);
	return ok;
}

static void *detour_get_p2p_session_data(vuid uid) {
	static BOOL warned;
	(void)uid;
	if (!warned) {
		warned = TRUE;
		shim_log("sdr: get_p2p_session_data is not provided by the SDR transport, answering null");
	}
	return NULL; // the legacy "no session" answer; the game never calls this
}

// ---------------------------------------------------------------------------
// Installation.
// ---------------------------------------------------------------------------

typedef void *(*hlp_fn)(const char **sign);

static const struct {
	const char *name;
	void *detour;
	void **orig; // the trampoline, kept only so MinHook has somewhere to put it
} natives[] = {
	{"send_p2p_packet", (void *)detour_send_p2p_packet, NULL},
	{"read_p2p_packet", (void *)detour_read_p2p_packet, NULL},
	{"is_p2p_packet_available", (void *)detour_is_p2p_packet_available, NULL},
	{"accept_p2p_session", (void *)detour_accept_p2p_session, NULL},
	{"close_p2p_session", (void *)detour_close_p2p_session, NULL},
	{"get_p2p_session_data", (void *)detour_get_p2p_session_data, NULL},
};
static void *native_orig[sizeof(natives) / sizeof(natives[0])];

unsigned sdr_hook_steam(HMODULE steam, HMODULE libhl) {
	unsigned i, hooked = 0;
	char hlp_name[64], direct_name[64], label[80];

	sdr_init_lock();
	hl_copy_bytes_p = (hl_copy_bytes_fn)(void *)GetProcAddress(libhl, "hl_copy_bytes");
	if (hl_copy_bytes_p == NULL)
		shim_log("sdr: libhl!hl_copy_bytes not found; read_p2p_packet cannot return a sender");

	for (i = 0; i < sizeof(natives) / sizeof(natives[0]); i++) {
		const char *sign = NULL;
		void *target, *direct;
		hlp_fn hlp;

		_snprintf(hlp_name, sizeof(hlp_name) - 1, "hlp_%s", natives[i].name);
		_snprintf(direct_name, sizeof(direct_name) - 1, "steam_%s", natives[i].name);
		_snprintf(label, sizeof(label) - 1, "steam!%s", direct_name);
		hlp_name[sizeof(hlp_name) - 1] = direct_name[sizeof(direct_name) - 1] = label[sizeof(label) - 1] = 0;

		// The bytecode resolves natives through hlp_<name>, which returns the
		// address of the C function; hook exactly what it returns.
		hlp = (hlp_fn)(void *)GetProcAddress(steam, hlp_name);
		direct = (void *)GetProcAddress(steam, direct_name);
		target = hlp != NULL ? hlp(&sign) : direct;
		if (hlp != NULL && target != direct)
			shim_log("sdr: %s resolves to %p, export %s is %p (hooking the resolved one)", hlp_name, target,
				direct_name, direct);
		if (hook_one(label, target, natives[i].detour, &native_orig[i]))
			hooked++;
	}
	shim_log("sdr: %u of %u legacy P2P natives diverted; the legacy ISteamNetworking path is never used",
		hooked, (unsigned)(sizeof(natives) / sizeof(natives[0])));
	return hooked;
}

void sdr_warm_up(void) {
	int tries;
	// Up to ~5 minutes for SteamAPI_Init: the game does it early, but a slow
	// Steam client may take a while.
	for (tries = 0; tries < 600; tries++) {
		if (!sdr_lock_ready)
			return;
		EnterCriticalSection(&sdr_lock);
		if (state != SDR_IDLE) {
			LeaveCriticalSection(&sdr_lock);
			return;
		}
		sdr_retry_at = 0; // the warm-up sets its own pace
		try_init();
		if (state != SDR_IDLE) {
			LeaveCriticalSection(&sdr_lock);
			return;
		}
		LeaveCriticalSection(&sdr_lock);
		Sleep(500);
	}
	shim_log("sdr: warm-up gave up waiting for the Steam API; will retry on first use");
}
