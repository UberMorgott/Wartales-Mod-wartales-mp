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
// How the session callbacks reach us matters. ISteamNetworkingUtils'
// SetGlobalCallback_MessagesSessionRequest/Failed accept a function pointer
// and return true, but with the Steam client (steam_api64 + steamclient) that
// pointer is never invoked: measured with a stand-alone probe against the
// game's own steam_api64.dll, only the classic Steam callback pipe delivered
// SteamNetworkingMessagesSessionRequest_t (1251) and ..SessionFailed_t (1252),
// during SteamAPI_RunCallbacks, to a CCallbackBase registered with
// SteamAPI_RegisterCallback -- the same mechanism hlsteam uses for every
// event the game handles. So the callbacks are registered that way (a
// CCallbackBase built by hand: vtable of Run/Run/GetCallbackSizeBytes,
// steam_api_common.h), and the global pointers stay only as a second,
// harmless path. Without this the host never accepted a session: a guest's
// connection sat pending until Steam's idle timeout while both sides logged
// nothing.
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

#include "lobby.h"
#include "sdr.h" // sdr_identity, sdr_msg: SteamNetworkingIdentity / SteamNetworkingMessage_t

C_ASSERT(sizeof(sdr_identity) == 136);
C_ASSERT(offsetof(sdr_msg, peer) == 16);
C_ASSERT(offsetof(sdr_msg, release) == 184);
C_ASSERT(offsetof(sdr_msg, channel) == 192);
C_ASSERT(sizeof(sdr_msg) == 216);
C_ASSERT(offsetof(sdr_conn_info, state) == 176);
C_ASSERT(offsetof(sdr_conn_info, end_reason) == 180);
C_ASSERT(offsetof(sdr_conn_info, end_debug) == 184);
C_ASSERT(offsetof(sdr_conn_info, description) == 312);
C_ASSERT(offsetof(sdr_conn_info, flags) == 440);
C_ASSERT(sizeof(sdr_conn_info) == 696);

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
typedef int (*sdr_conn_info_fn)(void *self, const sdr_identity *peer, sdr_conn_info *info, void *quick);
typedef void (*sdr_run_callbacks_fn)(void);

// CCallbackBase (steam_api_common.h): a vtable pointer, then
// uint8 m_nCallbackFlags and int m_iCallback. The vtable holds Run(void*),
// Run(void*, bool, SteamAPICall_t) and GetCallbackSizeBytes(); the two Run
// overloads are given the same handler, so their order in the compiler's
// vtable does not matter. steam_api's dispatch sets flag 0x01 on registration.
typedef struct sdr_cb {
	const void **vtable;
	unsigned char flags;
	int32_t icallback;
} sdr_cb;
typedef void (*sdr_register_cb_fn)(sdr_cb *cb, int icallback);
#define SDR_CB_SESSION_REQUEST 1251 // k_iSteamNetworkingMessagesCallbacks + 1
#define SDR_CB_SESSION_FAILED 1252  // k_iSteamNetworkingMessagesCallbacks + 2

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
	sdr_conn_info_fn conn_info; // optional: diagnostics only
	sdr_register_cb_fn register_cb;
} api;

// Diagnostics: the game's SteamAPI_RunCallbacks is what delivers the session
// callbacks below, so it is counted; the count is 0 until the hook is in.
static sdr_run_callbacks_fn real_run_callbacks;
static volatile LONG run_callbacks_count;
static BOOL run_callbacks_hooked;

static void detour_run_callbacks(void) {
	if (InterlockedIncrement(&run_callbacks_count) == 1)
		shim_log("sdr: the game called SteamAPI_RunCallbacks for the first time (Steam callbacks are being dispatched)");
	real_run_callbacks();
	lobby_pump();
}

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
	{"SteamAPI_RegisterCallback", (void **)&api.register_cb},
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

// ---------------------------------------------------------------------------
// Session diagnostics. Every peer we send to or hear from is watched: a
// thread polls GetSessionConnectionInfo once a second and logs each change of
// state / end reason, plus the relay status and whether the game keeps
// calling SteamAPI_RunCallbacks. None of it depends on Steam callbacks
// firing, which is the point.
// ---------------------------------------------------------------------------

#define SDR_WATCH_MAX 16

// Lost-peer close. The legacy API told the game a peer was gone through
// P2PSessionConnectFail_t (hlsteam onConnectionError -> SteamService
// onUserClose@54887 -> stop(); onStop()); ISteamNetworkingMessages never
// raises it, and the peer's own goodbye (a code-8 packet, SteamService.stop)
// is lost when its process dies or its close outruns the send. So when a
// session the game is using drops for good, the shim hands the game the
// goodbye itself: one 7-byte code-8 frame from that peer on the channel the
// game reads, [0x08, pid 0x00 0x00, sid as last received]. The game's
// onUserData@54888 lets code 8 past the pid-order check, matches sid (bytes
// 3..6) against the session and runs stop(); onStop() -- the vanilla
// disconnect path (Host.connect closure @55008 -> message, back to title).
//   armed:   the game read a game packet (>= 7 bytes) from the peer since its
//            last close_p2p_session; that packet's sid and channel are kept.
//   lost:    while armed, Steam reports the session closed by the peer (4),
//            failed locally (5) or dead (-3), or SessionFailed_t arrives; or
//            it reports none (0) within SDR_LOST_IDLE_MS of the last packet
//            (a session Steam times out for inactivity also ends in 0, with
//            no callback, and a later send simply re-dials). A re-dial
//            (connecting / finding route) is not a loss.
//   inject:  once per armed session, SDR_LOST_CONFIRM_MS after the loss
//            unless Steam reports it connected again by then (an
//            AutoRestartBrokenSession re-dial that worked), and only after
//            every real message from the peer still queued on that channel
//            has been read. A new sid from the peer is a new session.
// close_p2p_session disarms. It also no longer closes at once: the
// CloseSessionWithUser call waits SDR_CLOSE_DELAY_MS on the diag thread so
// the reliable code-8 the game sent just before it (SteamService.stop) leaves
// first; ISteamNetworkingMessages has no linger option, CloseSessionWithUser
// frees the session "immediately" (isteamnetworkingmessages.h).
#define SDR_LOST_IDLE_MS 60000
#define SDR_CLOSE_DELAY_MS 1000
#define SDR_LOST_CONFIRM_MS 2000 // a re-dial that reaches connected by then cancels
#define SDR_FRAME_CLOSE 8 // SteamService message code: stop

static struct {
	uint64_t id;
	int state;
	int end_reason;
	DWORD state_since;
	BOOL stuck_logged;
	BOOL armed;          // the game holds a session with this peer
	BOOL lost;           // ... and it dropped; inject when the queue is clear
	DWORD lost_at;       // when that was seen
	BOOL injected;       // the close was handed to the game for this session
	unsigned char sid[4]; // bytes 3..6 of the last game packet, as received
	int channel;         // where the game read it
	DWORD last_recv;     // GetTickCount of that packet
	BOOL close_pending;  // CloseSessionWithUser due at close_at
	DWORD close_at;
} sdr_watch[SDR_WATCH_MAX];
static unsigned sdr_watch_n;
static int sdr_relay_last = -1000;
static BOOL sdr_diag_started;

static const char *state_name(int s) {
	switch (s) {
	case 0: return "none";
	case 1: return "connecting";
	case 2: return "finding route";
	case 3: return "connected";
	case 4: return "closed by peer";
	case 5: return "problem detected locally";
	case -1: return "fin wait";
	case -2: return "linger";
	case -3: return "dead";
	default: return "?";
	}
}

// watch_peer starts following id; returns TRUE when it is new. Lock held.
static BOOL watch_peer(uint64_t id) {
	unsigned i;
	for (i = 0; i < sdr_watch_n; i++)
		if (sdr_watch[i].id == id)
			return FALSE;
	if (sdr_watch_n == SDR_WATCH_MAX)
		return FALSE;
	memset(&sdr_watch[sdr_watch_n], 0, sizeof(sdr_watch[0]));
	sdr_watch[sdr_watch_n].id = id;
	sdr_watch[sdr_watch_n].state = -1000; // unknown yet
	sdr_watch[sdr_watch_n].state_since = GetTickCount();
	sdr_watch_n++;
	return TRUE;
}

// find_watch returns the watch slot of id, or -1. Lock held.
static int find_watch(uint64_t id) {
	unsigned i;
	for (i = 0; i < sdr_watch_n; i++)
		if (sdr_watch[i].id == id)
			return (int)i;
	return -1;
}

// cancel_close drops a pending deferred CloseSessionWithUser of slot w. The
// messages API keeps one session per peer: a close armed for the old session
// that fires after the peer came back (a co-op reload: the guest closes, then
// re-dials at once) kills the new session and its traffic. So the peer's new
// session request, any message from it and a new session seen by the diag
// thread all cancel it, like the game sending to it again does. Lock held.
static void cancel_close(int w, const char *why) {
	if (w < 0 || !sdr_watch[w].close_pending)
		return;
	sdr_watch[w].close_pending = FALSE;
	shim_log("sdr: deferred close of %llu cancelled: %s", (unsigned long long)sdr_watch[w].id, why);
}

// mark_lost flags an armed session as gone; the diag thread injects. Lock held.
static void mark_lost(int w, int st, int end_reason, const char *why) {
	if (w < 0 || !sdr_watch[w].armed || sdr_watch[w].lost || sdr_watch[w].injected)
		return;
	sdr_watch[w].lost = TRUE;
	sdr_watch[w].lost_at = GetTickCount();
	shim_log("sdr: session with %llu lost (%s: state %d (%s), end reason %d); the game still holds it, a close will be injected",
		(unsigned long long)sdr_watch[w].id, why, st, state_name(st), end_reason);
}

static void lost_tick(DWORD now);

// diag_tick polls what is watched. Lock held.
static void diag_tick(void) {
	unsigned i;
	int relay;
	DWORD now = GetTickCount();
	static DWORD last_run_count_change;
	static LONG last_run_count = -1;
	static BOOL run_warned;
	LONG runs = run_callbacks_count;

	if (state != SDR_READY)
		return;
	if (sdr_utils != NULL) {
		relay = api.relay_status(sdr_utils, NULL);
		if (relay != sdr_relay_last) {
			shim_log("sdr: relay network status %d -> %d (100 = current, 2 = waiting, 3 = attempting, <0 = failed)", sdr_relay_last, relay);
			sdr_relay_last = relay;
		}
	}
	if (runs != last_run_count) {
		last_run_count = runs;
		last_run_count_change = now;
		run_warned = FALSE;
	} else if (run_callbacks_hooked && sdr_watch_n > 0 && !run_warned && now - last_run_count_change > 5000) {
		run_warned = TRUE;
		shim_log("sdr: WARNING: the game has not called SteamAPI_RunCallbacks for 5 s (%ld calls so far); "
			"session request/failed callbacks cannot be delivered while that lasts", runs);
	}
	if (api.conn_info == NULL) {
		lost_tick(now);
		return;
	}
	for (i = 0; i < sdr_watch_n; i++) {
		sdr_identity peer;
		sdr_conn_info info;
		int st;
		identity_of(&peer, sdr_watch[i].id);
		memset(&info, 0, sizeof(info));
		st = api.conn_info(sdr_msgs, &peer, &info, NULL);
		info.end_debug[sizeof(info.end_debug) - 1] = 0;
		info.description[sizeof(info.description) - 1] = 0;
		if (st != sdr_watch[i].state || info.end_reason != sdr_watch[i].end_reason) {
			int prev = sdr_watch[i].state;
			shim_log("sdr: session with %llu: state %d (%s)%s%s, end reason %d '%s', relay POP %u, RunCallbacks %ld; %s",
				(unsigned long long)sdr_watch[i].id, st, state_name(st),
				sdr_watch[i].state == -1000 ? "" : " was ", sdr_watch[i].state == -1000 ? "" : state_name(sdr_watch[i].state),
				info.end_reason, info.end_debug, (unsigned)info.pop_relay, runs, info.description);
			sdr_watch[i].state = st;
			sdr_watch[i].end_reason = info.end_reason;
			sdr_watch[i].state_since = now;
			sdr_watch[i].stuck_logged = FALSE;
			if (st >= 1 && st <= 3 && (prev == 0 || prev == 4 || prev == 5 || prev == -3))
				cancel_close((int)i, "a new session with the peer");
			if (st == 3 && sdr_watch[i].lost && !sdr_watch[i].injected) {
				sdr_watch[i].lost = FALSE;
				shim_log("sdr: session with %llu connected again, no close injected", (unsigned long long)sdr_watch[i].id);
			} else if (st == 4 || st == 5 || st == -3)
				mark_lost((int)i, st, info.end_reason, "session ended");
			else if (st == 0 && prev >= 1 && prev <= 3 && sdr_watch[i].armed && now - sdr_watch[i].last_recv < SDR_LOST_IDLE_MS)
				mark_lost((int)i, st, info.end_reason, "session vanished while in use");
		} else if ((st == 1 || st == 2) && !sdr_watch[i].stuck_logged && now - sdr_watch[i].state_since > 10000) {
			sdr_watch[i].stuck_logged = TRUE;
			shim_log("sdr: session with %llu still %s after 10 s (no answer from the peer's Steam client yet); %s",
				(unsigned long long)sdr_watch[i].id, state_name(st), info.description);
		}
	}
	lost_tick(now);
}

static DWORD WINAPI diag_thread(LPVOID unused) {
	(void)unused;
	for (;;) {
		Sleep(1000);
		EnterCriticalSection(&sdr_lock);
		diag_tick();
		LeaveCriticalSection(&sdr_lock);
	}
	return 0; // not reached
}

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

// session_request is the modern counterpart of P2PSessionRequest_t; the
// game only ever answers the legacy one, which the new interface never raises.
// via names the delivery path, for the log.
static void session_request(const sdr_identity *peer, const char *via) {
	unsigned char ok = 0;
	if (peer == NULL || sdr_msgs == NULL)
		return;
	ok = api.accept(sdr_msgs, peer);
	sdr_accepted++;
	shim_log("sdr: session request from %llu (type %d) via %s: %s (RunCallbacks %ld)", (unsigned long long)peer->u.steam_id,
		peer->type, via, ok ? "accepted" : "accept FAILED", run_callbacks_count);
	if (peer->type == SDR_IDENTITY_STEAMID) {
		EnterCriticalSection(&sdr_lock);
		watch_peer(peer->u.steam_id);
		cancel_close(find_watch(peer->u.steam_id), "the peer asks for a new session");
		LeaveCriticalSection(&sdr_lock);
	}
}

static void session_failed(const sdr_conn_info *info, const char *via) {
	char end_debug[sizeof(info->end_debug)], description[sizeof(info->description)];
	if (info == NULL)
		return;
	memcpy(end_debug, info->end_debug, sizeof(end_debug));
	memcpy(description, info->description, sizeof(description));
	end_debug[sizeof(end_debug) - 1] = description[sizeof(description) - 1] = 0;
	shim_log("sdr: session with %llu FAILED (via %s): state %d (%s), end reason %d '%s'; %s",
		(unsigned long long)info->peer.u.steam_id, via, info->state, state_name(info->state), info->end_reason,
		end_debug, description);
	if (info->peer.type == SDR_IDENTITY_STEAMID && sdr_lock_ready) {
		EnterCriticalSection(&sdr_lock);
		mark_lost(find_watch(info->peer.u.steam_id), info->state, info->end_reason, "SessionFailed");
		LeaveCriticalSection(&sdr_lock);
	}
}

// The global function-pointer path (SetGlobalCallback_*). Kept, although the
// Steam client was never seen to invoke it; see the header comment.
static void on_session_request(void *ev) { session_request((const sdr_identity *)ev, "global callback"); }
static void on_session_failed(void *ev) { session_failed((const sdr_conn_info *)ev, "global callback"); }

// The Steam callback pipe (SteamAPI_RegisterCallback), dispatched by the
// game's SteamAPI_RunCallbacks. Both Run overloads take the payload first and
// ignore the rest, so the vtable order of the two does not matter.
static void cb_request_run(sdr_cb *self, void *ev) {
	(void)self;
	session_request((const sdr_identity *)ev, "SteamAPI_RegisterCallback"); // SteamNetworkingMessagesSessionRequest_t
}
static void cb_request_run_result(sdr_cb *self, void *ev, unsigned char io_failure, uint64_t call) {
	(void)io_failure;
	(void)call;
	cb_request_run(self, ev);
}
static int cb_request_size(sdr_cb *self) {
	(void)self;
	return (int)sizeof(sdr_identity);
}
static void cb_failed_run(sdr_cb *self, void *ev) {
	(void)self;
	session_failed((const sdr_conn_info *)ev, "SteamAPI_RegisterCallback"); // SteamNetworkingMessagesSessionFailed_t
}
static void cb_failed_run_result(sdr_cb *self, void *ev, unsigned char io_failure, uint64_t call) {
	(void)io_failure;
	(void)call;
	cb_failed_run(self, ev);
}
static int cb_failed_size(sdr_cb *self) {
	(void)self;
	return (int)sizeof(sdr_conn_info);
}
static const void *cb_request_vtable[] = {(const void *)cb_request_run, (const void *)cb_request_run_result, (const void *)cb_request_size};
static const void *cb_failed_vtable[] = {(const void *)cb_failed_run, (const void *)cb_failed_run_result, (const void *)cb_failed_size};
static sdr_cb cb_request = {cb_request_vtable, 0, SDR_CB_SESSION_REQUEST};
static sdr_cb cb_failed = {cb_failed_vtable, 0, SDR_CB_SESSION_FAILED};

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
	// Session callbacks, the way the game itself receives Steam events.
	api.register_cb(&cb_request, SDR_CB_SESSION_REQUEST);
	api.register_cb(&cb_failed, SDR_CB_SESSION_FAILED);
	shim_log("sdr: session request/failed callbacks registered with SteamAPI_RegisterCallback (%d/%d, flags 0x%02x/0x%02x; 0x01 = registered)",
		SDR_CB_SESSION_REQUEST, SDR_CB_SESSION_FAILED, cb_request.flags, cb_failed.flags);
	sdr_utils = api.utils_v004();
	if (sdr_utils == NULL) {
		shim_log("sdr: SteamNetworkingUtils004 unavailable: no relay warm-up");
	} else {
		unsigned char r1 = api.on_request(sdr_utils, on_session_request);
		unsigned char r2 = api.on_failed(sdr_utils, on_session_failed);
		shim_log("sdr: SetGlobalCallback_MessagesSessionRequest/Failed = %u/%u (a second path; the Steam client was not seen to use it)", r1, r2);
		api.init_relay(sdr_utils);
		relay = api.relay_status(sdr_utils, NULL);
		sdr_relay_last = relay;
		shim_log("sdr: InitRelayNetworkAccess called, relay status %d (100 = current)", relay);
	}
	// Diagnostics, all optional: a missing export only costs the detail.
	api.conn_info = (sdr_conn_info_fn)(void *)GetProcAddress(mod, "SteamAPI_ISteamNetworkingMessages_GetSessionConnectionInfo");
	if (api.conn_info == NULL)
		shim_log("sdr: steam_api64.dll lacks GetSessionConnectionInfo; session states will not be logged");
	if (!run_callbacks_hooked) {
		void *rc = (void *)GetProcAddress(mod, "SteamAPI_RunCallbacks");
		run_callbacks_hooked = rc != NULL && hook_one("steam_api64!SteamAPI_RunCallbacks", rc, (void *)detour_run_callbacks, (void **)&real_run_callbacks);
		if (!run_callbacks_hooked)
			shim_log("sdr: SteamAPI_RunCallbacks not hooked; callback dispatch will not be counted");
	}
	if (!sdr_diag_started) {
		HANDLE t = CreateThread(NULL, 0, diag_thread, NULL, 0, NULL);
		if (t != NULL) {
			CloseHandle(t);
			sdr_diag_started = TRUE;
		}
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
	if (watch_peer(peer) || res != SDR_RESULT_OK)
		shim_log("sdr: bridge send to %llu: %lu bytes on channel %d = EResult %d (1 = OK), relay status %d, RunCallbacks %ld",
			(unsigned long long)peer, (unsigned long)len, channel, res,
			sdr_utils != NULL ? api.relay_status(sdr_utils, NULL) : -1000, run_callbacks_count);
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

// A message the shim made itself (the injected close) never belongs to Steam.
static void synth_release(sdr_msg *m) {
	free(m->data);
	free(m);
}

static void msg_release(sdr_msg *m) {
	if (m->release == synth_release)
		synth_release(m);
	else
		api.release(m);
}

static void queue_push(int channel, sdr_msg *m) {
	sdr_node *n = (sdr_node *)malloc(sizeof(*n));
	if (n == NULL) {
		msg_release(m);
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
			cancel_close(find_watch(batch[i]->peer.u.steam_id), "the peer sends again");
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
				msg_release(n->m);
				free(n);
				sdr_dropped++;
				continue;
			}
			sdr_queue[c].tail = n;
			link = &n->next;
		}
	}
}

// queued_from tells whether anything from id still waits on channel. Lock held.
static BOOL queued_from(int channel, uint64_t id) {
	sdr_node *n;
	for (n = sdr_queue[channel].head; n != NULL; n = n->next)
		if (n->m->peer.u.steam_id == id)
			return TRUE;
	return FALSE;
}

// inject_close queues the vanilla code-8 frame from a lost peer. Lock held.
static BOOL inject_close(int w) {
	sdr_msg *m = (sdr_msg *)calloc(1, sizeof(*m));
	unsigned char *frame = (unsigned char *)malloc(7);
	if (m == NULL || frame == NULL) {
		free(m);
		free(frame);
		return FALSE;
	}
	frame[0] = SDR_FRAME_CLOSE;
	frame[1] = frame[2] = 0; // pid: code 8 skips the order check
	memcpy(frame + 3, sdr_watch[w].sid, 4);
	m->data = frame;
	m->size = 7;
	identity_of(&m->peer, sdr_watch[w].id);
	m->channel = sdr_watch[w].channel;
	m->release = synth_release;
	queue_push(sdr_watch[w].channel, m);
	return TRUE;
}

// lost_tick runs on the diag thread: deferred session closes fall due, and
// lost sessions get their close once the game has read what the peer sent.
// Lock held.
static void lost_tick(DWORD now) {
	unsigned i;
	for (i = 0; i < sdr_watch_n; i++) {
		if (sdr_watch[i].close_pending && (LONG)(now - sdr_watch[i].close_at) >= 0) {
			sdr_identity peer;
			unsigned char ok;
			sdr_watch[i].close_pending = FALSE;
			identity_of(&peer, sdr_watch[i].id);
			ok = api.close(sdr_msgs, &peer);
			shim_log("sdr: deferred CloseSessionWithUser(%llu) = %u", (unsigned long long)sdr_watch[i].id, ok);
		}
		if (sdr_watch[i].lost && !sdr_watch[i].injected && now - sdr_watch[i].lost_at >= SDR_LOST_CONFIRM_MS) {
			int c = sdr_watch[i].channel;
			pump(c);
			if (queued_from(c, sdr_watch[i].id))
				continue; // the peer's last real messages go first
			if (!inject_close((int)i))
				continue;
			sdr_watch[i].injected = TRUE;
			{
				int32_t sid; // the game's getInt32(3): little-endian
				memcpy(&sid, sdr_watch[i].sid, 4);
				shim_log("sdr: peer lost, injected close for %llu sid %d on channel %d (code 8: the game runs its own disconnect)",
					(unsigned long long)sdr_watch[i].id, (int)sid, c);
			}
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
	{
		cancel_close(find_watch(to.u.steam_id), "the game sends to it again");
	}
	res = api.send(sdr_msgs, &to, data, (uint32_t)length, flags, channel);
	if (watch_peer(to.u.steam_id))
		shim_log("sdr: first packet to %llu: %d bytes, type %d -> flags 0x%x, channel %d = EResult %d (1 = OK)",
			(unsigned long long)to.u.steam_id, length, type, flags, channel, res);
	if (res == SDR_RESULT_OK) {
		sdr_sent++;
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
	if (watch_peer(from) || sdr_received == 1)
		shim_log("sdr: first packet received from %llu, %d bytes, channel %d", (unsigned long long)from, n, channel);
	if (m->release != synth_release && m->size >= 7) {
		// A game packet: [code][pid:2][sid:4]...; the session is in use.
		int w = find_watch(from);
		if (w >= 0) {
			if (!sdr_watch[w].armed || memcmp(sdr_watch[w].sid, (const unsigned char *)m->data + 3, 4) != 0) {
				sdr_watch[w].lost = FALSE; // a new session: its own close
				sdr_watch[w].injected = FALSE;
			}
			sdr_watch[w].armed = TRUE;
			memcpy(sdr_watch[w].sid, (const unsigned char *)m->data + 3, 4);
			sdr_watch[w].channel = channel;
			sdr_watch[w].last_recv = GetTickCount();
		}
	}
	msg_release(m);
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

// The queues are dropped at once, like CloseP2PSessionWithUser; Steam's own
// close waits SDR_CLOSE_DELAY_MS on the diag thread (see the lost-peer
// comment), so the game's last reliable message (code 8) is not discarded.
// Without the diag thread (or a watch slot) it closes at once.
static unsigned char detour_close_p2p_session(vuid uid) {
	sdr_identity peer;
	unsigned char ok = 1;
	int w;
	if (uid == NULL || !ready())
		return 1; // nothing was open
	identity_of(&peer, uid_to_u64(uid));
	EnterCriticalSection(&sdr_lock);
	for (w = 0; w < SDR_CHANNELS; w++)
		pump(w); // what Steam still holds from the peer goes too, as a close would discard it
	drop_peer(peer.u.steam_id);
	w = find_watch(peer.u.steam_id);
	if (w >= 0) {
		sdr_watch[w].armed = sdr_watch[w].lost = sdr_watch[w].injected = FALSE; // the game let go
		sdr_watch[w].close_pending = TRUE;
		sdr_watch[w].close_at = GetTickCount() + SDR_CLOSE_DELAY_MS;
	}
	if (w < 0 || !sdr_diag_started)
		ok = api.close(sdr_msgs, &peer);
	if (w >= 0 && !sdr_diag_started)
		sdr_watch[w].close_pending = FALSE;
	LeaveCriticalSection(&sdr_lock);
	shim_log("sdr: session with %llu closed (%s); sent %lu, failed %lu, received %lu, dropped %lu, auto-accepted %lu",
		(unsigned long long)peer.u.steam_id,
		w >= 0 && sdr_diag_started ? "ok, CloseSessionWithUser deferred 1 s" : ok ? "ok" : "was not open", sdr_sent,
		sdr_send_failed, sdr_received, sdr_dropped, sdr_accepted);
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
