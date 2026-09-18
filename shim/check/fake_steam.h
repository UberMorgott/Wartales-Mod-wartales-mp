// Shared by the fake steam_api64.dll and shimcheck: Valve's layouts
// (steamnetworkingtypes.h), written independently of shim/proxy/sdr.c so the
// check catches a layout mistake there, and the test-only stats the fake
// exposes.
#ifndef WARTALES_MP_FAKE_STEAM_H
#define WARTALES_MP_FAKE_STEAM_H

#include <stdint.h>

typedef struct SteamNetworkingIdentity {
	int32_t m_eType; // 16 = k_ESteamNetworkingIdentityType_SteamID
	int32_t m_cbSize;
	union {
		uint64_t m_steamID64;
		char m_szUnknownRawString[128];
	};
} SteamNetworkingIdentity;

typedef struct SteamNetworkingMessage_t {
	void *m_pData;
	int32_t m_cbSize;
	uint32_t m_conn;
	SteamNetworkingIdentity m_identityPeer;
	int64_t m_nConnUserData;
	int64_t m_usecTimeReceived;
	int64_t m_nMessageNumber;
	void (*m_pfnFreeData)(struct SteamNetworkingMessage_t *);
	void (*m_pfnRelease)(struct SteamNetworkingMessage_t *);
	int32_t m_nChannel;
	int32_t m_nFlags;
	int64_t m_nUserData;
	uint16_t m_idxLane;
	uint16_t _pad1__;
} SteamNetworkingMessage_t;

typedef struct SteamNetConnectionInfo_t {
	SteamNetworkingIdentity m_identityRemote;
	int64_t m_nUserData;
	uint32_t m_hListenSocket;
	uint8_t m_addrRemote[18]; // SteamNetworkingIPAddr: 16-byte IP + uint16 port
	uint16_t m__pad1;
	uint32_t m_idPOPRemote;
	uint32_t m_idPOPRelay;
	int32_t m_eState;
	int32_t m_eEndReason;
	char m_szEndDebug[128];
	char m_szConnectionDescription[128];
	int32_t m_nFlags;
	uint32_t reserved[63];
} SteamNetConnectionInfo_t;

// What the fake observed. Read through fake_stats().
typedef struct fake_stats_t {
	unsigned sends;         // successful SendMessageToUser calls
	int last_send_result;   // EResult of the last SendMessageToUser
	int last_flags;         // nSendFlags of the last accepted send
	int last_channel;       // nRemoteChannel of the last accepted send
	uint64_t last_send_to;  // identity of the last accepted send
	int last_send_id_type;  // m_eType of that identity
	unsigned accepts;       // AcceptSessionWithUser calls
	uint64_t last_accept;   // its identity
	unsigned closes;        // CloseSessionWithUser calls
	uint64_t last_close;
	unsigned allocated;     // messages handed out by ReceiveMessagesOnChannel
	unsigned released;      // SteamNetworkingMessage_t_Release calls
	unsigned relay_inits;   // InitRelayNetworkAccess calls
	int request_cb_set;     // SetGlobalCallback_MessagesSessionRequest called
	int failed_cb_set;      // SetGlobalCallback_MessagesSessionFailed called
	unsigned queued;        // messages waiting in the fake, all channels
	unsigned conn_infos;    // GetSessionConnectionInfo calls
	unsigned run_callbacks; // SteamAPI_RunCallbacks calls that reached the fake
	int registered_1251;    // SteamAPI_RegisterCallback(SteamNetworkingMessagesSessionRequest_t)
	int registered_1252;    // SteamAPI_RegisterCallback(SteamNetworkingMessagesSessionFailed_t)
	int size_1251;          // what that object's GetCallbackSizeBytes() answered
	int size_1252;
} fake_stats_t;

// CCallbackBase as steam_api sees it (steam_api_common.h): vtable, flags, id.
typedef struct CCallbackBase {
	const void **vtable; // Run(void*), Run(void*, bool, SteamAPICall_t), GetCallbackSizeBytes()
	uint8_t m_nCallbackFlags;
	int32_t m_iCallback;
} CCallbackBase;

#endif
