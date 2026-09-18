// Shared between sdr.c (the transport) and bridge.c (the helper's way in).
#ifndef WARTALES_MP_SDR_H
#define WARTALES_MP_SDR_H

#include <stddef.h>
#include <stdint.h>

#define SDR_IDENTITY_STEAMID 16 // k_ESteamNetworkingIdentityType_SteamID
#define SDR_RESULT_OK 1         // k_EResultOK

typedef struct sdr_identity {
	int32_t type;
	int32_t cb;
	union {
		uint64_t steam_id;
		unsigned char raw[128];
	} u;
} sdr_identity;

typedef struct sdr_msg {
	void *data;
	int32_t size;
	uint32_t conn;
	sdr_identity peer;
	int64_t conn_user_data;
	int64_t usec_received;
	int64_t message_number;
	void (*free_data)(struct sdr_msg *);
	void (*release)(struct sdr_msg *);
	int32_t channel;
	int32_t flags;
	int64_t user_data;
	uint16_t lane;
	uint16_t pad;
} sdr_msg;

// sdr_bridge_send sends one reliable message to peer on channel; returns the
// EResult (SDR_RESULT_OK on success), or -1 when the transport is not ready.
int sdr_bridge_send(uint64_t peer, const void *data, uint32_t len, int channel);

// sdr_bridge_receive pulls up to max messages from channel; 0 when the
// transport is not ready. Each must be given back with sdr_bridge_release.
int sdr_bridge_receive(int channel, sdr_msg **out, int max);
void sdr_bridge_release(sdr_msg *m);

// bridge_start (bridge.c) opens the loopback listener once the transport is
// ready and fills status with "bridge=127.0.0.1:<port> token=<hex>".
BOOL bridge_start(char *status, size_t cch);

#endif
