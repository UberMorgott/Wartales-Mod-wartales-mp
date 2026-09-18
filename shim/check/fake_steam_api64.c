// A stand-in steam_api64.dll for shimcheck: the flat ISteamNetworkingMessages /
// ISteamNetworkingUtils entry points the SDR shim binds, implemented as an
// in-process loopback so the shim's packet semantics can be exercised without
// a Steam client. A message sent to identity X is queued as if received from
// X on the same channel. Test-only exports (fake_*) let shimcheck inject
// messages from other peers, raise a session request and read counters.
//
// Built by shim\check.ps1 into <OutDir>\check-steam\steam_api64.dll.

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>

#include <stdlib.h>
#include <string.h>

#include "fake_steam.h"

#define EXPORT __declspec(dllexport)
#define CHANNELS 9 // 0..7 for the game, slot 8 = channel 100 (the bridge)
#define BRIDGE_CHANNEL 100
#define IDENTITY_STEAMID 16

// slot maps a Steam channel number onto a queue, or -1.
static int slot(int channel) {
	if (channel >= 0 && channel < 8)
		return channel;
	return channel == BRIDGE_CHANNEL ? 8 : -1;
}

typedef struct node {
	SteamNetworkingMessage_t *m;
	struct node *next;
} node;

static struct {
	node *head, *tail;
} queue[CHANNELS];

static int g_msgs, g_utils; // the "interfaces": only their addresses matter
static int g_hsteamuser = 1;
static int g_send_result = 1; // k_EResultOK
static fake_stats_t st;
static void (*request_cb)(void *);
static void (*failed_cb)(void *);
static CRITICAL_SECTION lock;

BOOL WINAPI DllMain(HINSTANCE inst, DWORD reason, LPVOID reserved) {
	(void)inst;
	(void)reserved;
	if (reason == DLL_PROCESS_ATTACH)
		InitializeCriticalSection(&lock);
	return TRUE;
}

static void release(SteamNetworkingMessage_t *m) {
	free(m->m_pData);
	free(m);
	st.released++;
}

static void enqueue(uint64_t from, int channel, const void *data, int len) {
	int q = slot(channel);
	SteamNetworkingMessage_t *m = (SteamNetworkingMessage_t *)calloc(1, sizeof(*m));
	node *n = (node *)calloc(1, sizeof(*n));
	m->m_pData = malloc((size_t)len + 1);
	memcpy(m->m_pData, data, (size_t)len);
	m->m_cbSize = len;
	m->m_identityPeer.m_eType = IDENTITY_STEAMID;
	m->m_identityPeer.m_cbSize = 8;
	m->m_identityPeer.m_steamID64 = from;
	m->m_nChannel = channel;
	m->m_pfnRelease = release;
	n->m = m;
	if (queue[q].tail != NULL)
		queue[q].tail->next = n;
	else
		queue[q].head = n;
	queue[q].tail = n;
	st.queued++;
}

// ------------------------------------------------------------ steam_api_flat

EXPORT int32_t SteamAPI_GetHSteamUser(void) { return g_hsteamuser; }
EXPORT void *SteamAPI_SteamNetworkingMessages_SteamAPI_v002(void) { return &g_msgs; }
EXPORT void *SteamAPI_SteamNetworkingUtils_SteamAPI_v004(void) { return &g_utils; }

EXPORT int SteamAPI_ISteamNetworkingMessages_SendMessageToUser(void *self, const SteamNetworkingIdentity *to,
	const void *data, uint32_t len, int flags, int channel) {
	int res;
	EnterCriticalSection(&lock);
	if (self != &g_msgs || to == NULL || to->m_eType != IDENTITY_STEAMID || slot(channel) < 0)
		res = 8; // k_EResultInvalidParam
	else
		res = g_send_result;
	st.last_send_result = res;
	if (res == 1) {
		st.sends++;
		st.last_flags = flags;
		st.last_channel = channel;
		st.last_send_to = to->m_steamID64;
		st.last_send_id_type = to->m_eType;
		enqueue(to->m_steamID64, channel, data, (int)len); // loopback
	}
	LeaveCriticalSection(&lock);
	return res;
}

EXPORT int SteamAPI_ISteamNetworkingMessages_ReceiveMessagesOnChannel(void *self, int channel,
	SteamNetworkingMessage_t **out, int max) {
	int n = 0, q = slot(channel);
	if (self != &g_msgs || q < 0)
		return -1;
	EnterCriticalSection(&lock);
	while (n < max && queue[q].head != NULL) {
		node *h = queue[q].head;
		queue[q].head = h->next;
		if (queue[q].head == NULL)
			queue[q].tail = NULL;
		out[n++] = h->m;
		free(h);
		st.queued--;
		st.allocated++;
	}
	LeaveCriticalSection(&lock);
	return n;
}

EXPORT unsigned char SteamAPI_ISteamNetworkingMessages_AcceptSessionWithUser(void *self,
	const SteamNetworkingIdentity *peer) {
	if (self != &g_msgs || peer == NULL || peer->m_eType != IDENTITY_STEAMID)
		return 0;
	EnterCriticalSection(&lock);
	st.accepts++;
	st.last_accept = peer->m_steamID64;
	LeaveCriticalSection(&lock);
	return 1;
}

EXPORT unsigned char SteamAPI_ISteamNetworkingMessages_CloseSessionWithUser(void *self,
	const SteamNetworkingIdentity *peer) {
	if (self != &g_msgs || peer == NULL || peer->m_eType != IDENTITY_STEAMID)
		return 0;
	int c;
	EnterCriticalSection(&lock);
	st.closes++;
	st.last_close = peer->m_steamID64;
	// Like the real thing: whatever that peer had queued is gone.
	for (c = 0; c < CHANNELS; c++) {
		node **link = &queue[c].head;
		queue[c].tail = NULL;
		while (*link != NULL) {
			node *n = *link;
			if (n->m->m_identityPeer.m_steamID64 == peer->m_steamID64) {
				*link = n->next;
				release(n->m);
				st.released--; // never handed out, so not a Release the shim owes
				free(n);
				st.queued--;
				continue;
			}
			queue[c].tail = n;
			link = &n->next;
		}
	}
	LeaveCriticalSection(&lock);
	return 1;
}

EXPORT void SteamAPI_SteamNetworkingMessage_t_Release(SteamNetworkingMessage_t *m) {
	EnterCriticalSection(&lock);
	m->m_pfnRelease(m);
	LeaveCriticalSection(&lock);
}

// The session with whoever we last sent to is "connected"; everyone else: none.
EXPORT int SteamAPI_ISteamNetworkingMessages_GetSessionConnectionInfo(void *self, const SteamNetworkingIdentity *peer,
	SteamNetConnectionInfo_t *info, void *quick) {
	int state;
	(void)quick;
	if (self != &g_msgs || peer == NULL || peer->m_eType != IDENTITY_STEAMID)
		return 0;
	EnterCriticalSection(&lock);
	st.conn_infos++;
	state = st.sends > 0 && peer->m_steamID64 == st.last_send_to ? 3 : 0; // k_ESteamNetworkingConnectionState_Connected
	LeaveCriticalSection(&lock);
	if (info != NULL) {
		memset(info, 0, sizeof(*info));
		info->m_identityRemote = *peer;
		info->m_eState = state;
		info->m_idPOPRelay = state == 3 ? 0x666b6521 : 0; // "fke!"
		strcpy(info->m_szConnectionDescription, state == 3 ? "fake loopback connection" : "no connection");
	}
	return state;
}

EXPORT void SteamAPI_RunCallbacks(void) {
	EnterCriticalSection(&lock);
	st.run_callbacks++;
	LeaveCriticalSection(&lock);
}

EXPORT void SteamAPI_ISteamNetworkingUtils_InitRelayNetworkAccess(void *self) {
	if (self == &g_utils)
		st.relay_inits++;
}

EXPORT int SteamAPI_ISteamNetworkingUtils_GetRelayNetworkStatus(void *self, void *details) {
	(void)details;
	return self == &g_utils ? 100 : 0; // k_ESteamNetworkingAvailability_Current
}

EXPORT unsigned char SteamAPI_ISteamNetworkingUtils_SetGlobalCallback_MessagesSessionRequest(void *self,
	void (*cb)(void *)) {
	if (self != &g_utils)
		return 0;
	request_cb = cb;
	st.request_cb_set = cb != NULL;
	return 1;
}

EXPORT unsigned char SteamAPI_ISteamNetworkingUtils_SetGlobalCallback_MessagesSessionFailed(void *self,
	void (*cb)(void *)) {
	if (self != &g_utils)
		return 0;
	failed_cb = cb;
	st.failed_cb_set = cb != NULL;
	return 1;
}

// ---------------------------------------------------------------- test only

EXPORT void fake_stats(fake_stats_t *out) {
	EnterCriticalSection(&lock);
	*out = st;
	LeaveCriticalSection(&lock);
}

EXPORT void fake_inject(uint64_t from, int channel, const void *data, int len) {
	if (slot(channel) < 0)
		return;
	EnterCriticalSection(&lock);
	enqueue(from, channel, data, len);
	LeaveCriticalSection(&lock);
}

EXPORT void fake_set_send_result(int res) { g_send_result = res; }

// SteamAPI_RegisterCallback, as steam_api does it: remember the object, set
// the registered flag. Like the real Steam client, the fake delivers session
// events ONLY through these objects, never through the SetGlobalCallback_*
// function pointers (measured; see shim/proxy/sdr.c).
static CCallbackBase *cb_1251, *cb_1252;

EXPORT void SteamAPI_RegisterCallback(CCallbackBase *cb, int icallback) {
	typedef int (*size_fn)(CCallbackBase *);
	int size;
	if (cb == NULL)
		return;
	cb->m_nCallbackFlags |= 0x01; // k_ECallbackFlagsRegistered
	size = ((size_fn)cb->vtable[2])(cb);
	EnterCriticalSection(&lock);
	if (icallback == 1251) {
		cb_1251 = cb;
		st.registered_1251 = 1;
		st.size_1251 = size;
	} else if (icallback == 1252) {
		cb_1252 = cb;
		st.registered_1252 = 1;
		st.size_1252 = size;
	}
	LeaveCriticalSection(&lock);
}

EXPORT void SteamAPI_UnregisterCallback(CCallbackBase *cb) {
	if (cb != NULL)
		cb->m_nCallbackFlags &= (uint8_t)~0x01;
}

// dispatch calls Run(void*) on a registered object the way SteamAPI_RunCallbacks
// does. MSVC lays the two Run overloads out in reverse declaration order, so
// Run(void*) is vtable slot 1; the shim answers either slot identically.
static void dispatch(CCallbackBase *cb, void *payload) {
	typedef void (*run_fn)(CCallbackBase *, void *);
	((run_fn)cb->vtable[1])(cb, payload);
}

// fake_fire_session_request posts SteamNetworkingMessagesSessionRequest_t the
// way the Steam client does; returns 0 when nobody registered for it.
EXPORT int fake_fire_session_request(uint64_t from) {
	SteamNetworkingIdentity req; // SteamNetworkingMessagesSessionRequest_t is exactly this
	if (cb_1251 == NULL)
		return 0;
	memset(&req, 0, sizeof(req));
	req.m_eType = IDENTITY_STEAMID;
	req.m_cbSize = 8;
	req.m_steamID64 = from;
	dispatch(cb_1251, &req);
	return 1;
}

// fake_fire_session_failed posts SteamNetworkingMessagesSessionFailed_t with
// the given end reason and debug text.
EXPORT int fake_fire_session_failed(uint64_t from, int end_reason, const char *debug) {
	SteamNetConnectionInfo_t info; // SteamNetworkingMessagesSessionFailed_t { m_info }
	if (cb_1252 == NULL)
		return 0;
	memset(&info, 0, sizeof(info));
	info.m_identityRemote.m_eType = IDENTITY_STEAMID;
	info.m_identityRemote.m_cbSize = 8;
	info.m_identityRemote.m_steamID64 = from;
	info.m_eState = 5; // k_ESteamNetworkingConnectionState_ProblemDetectedLocally
	info.m_eEndReason = end_reason;
	strncpy(info.m_szEndDebug, debug, sizeof(info.m_szEndDebug) - 1);
	strcpy(info.m_szConnectionDescription, "fake P2P connection");
	dispatch(cb_1252, &info);
	return 1;
}
