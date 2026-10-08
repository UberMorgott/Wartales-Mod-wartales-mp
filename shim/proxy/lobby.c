// Optional Steam friends-list advertisement. It does not carry game packets or
// open the overlay. Flat signatures: steam_api_flat.h; result: LobbyCreated_t
// from isteammatchmaking.h (callback 513, Windows pack 8).
#include "lobby.h"
#include "shim.h"
#include <stddef.h>
#include <stdint.h>
#include <string.h>

typedef struct {
	int32_t result;
	uint64_t lobby;
} lobby_created;
_Static_assert(sizeof(lobby_created) == 16, "LobbyCreated_t Windows size");
_Static_assert(offsetof(lobby_created, lobby) == 8, "LobbyCreated_t SteamID offset");
static struct {
	void *(*matchmaking)(void);
	void *(*utils)(void);
	uint64_t (*create)(void *, int, int);
	unsigned char (*set_data)(void *, uint64_t, const char *, const char *);
	void (*leave)(void *, uint64_t);
	unsigned char (*completed)(void *, uint64_t, unsigned char *);
	unsigned char (*result)(void *, uint64_t, void *, int, int, unsigned char *);
} api;
static SRWLOCK lock = SRWLOCK_INIT;
static char desired[33];
static uint64_t generation, pending, pending_generation, published, published_generation;
static unsigned attempts;
static ULONGLONG retry_at;
static int bound; // 0 retry when module appears, 1 available, -1 missing exports

int lobby_set_invite(const unsigned char *value, size_t len) {
	size_t i;
	if (len > 32 || (len && !value))
		return 0;
	for (i = 0; i < len; i++)
		if (value[i] < 33 || value[i] > 126)
			return 0;
	AcquireSRWLockExclusive(&lock);
	if (strlen(desired) != len || (len && memcmp(desired, value, len))) {
		if (len)
			memcpy(desired, value, len);
		desired[len] = 0;
		generation++;
		attempts = 0;
		retry_at = 0;
	}
	ReleaseSRWLockExclusive(&lock);
	return 1;
}

static int bind_api(void) {
	HMODULE mod;
	if (bound)
		return bound > 0;
	mod = GetModuleHandleW(L"steam_api64.dll");
	if (!mod)
		return 0;
#define RESOLVE(field, name)                                                                                                               \
	do {                                                                                                                                   \
		*(void **)(&api.field) = (void *)GetProcAddress(mod, name);                                                                        \
		if (!api.field) {                                                                                                                  \
			shim_log("lobby: automatic Steam publication unavailable: missing %s", name);                                                  \
			bound = -1;                                                                                                                    \
			return 0;                                                                                                                      \
		}                                                                                                                                  \
	} while (0)
	RESOLVE(matchmaking, "SteamAPI_SteamMatchmaking_v009");
	RESOLVE(utils, "SteamAPI_SteamUtils_v010");
	RESOLVE(create, "SteamAPI_ISteamMatchmaking_CreateLobby");
	RESOLVE(set_data, "SteamAPI_ISteamMatchmaking_SetLobbyData");
	RESOLVE(leave, "SteamAPI_ISteamMatchmaking_LeaveLobby");
	RESOLVE(completed, "SteamAPI_ISteamUtils_IsAPICallCompleted");
	RESOLVE(result, "SteamAPI_ISteamUtils_GetAPICallResult");
#undef RESOLVE
	bound = 1;
	return 1;
}

// Steam backend failures are transient. Three attempts per desired generation
// bound load and log noise; a clear/new host resets the budget.
static void retry_later(void) {
	if (pending_generation != generation || !desired[0])
		return;
	if (attempts < 3) {
		DWORD delay = attempts == 1 ? 2000 : 5000;
		retry_at = GetTickCount64() + delay;
		shim_log("lobby: automatic Steam publication retry %u/3 in %lu ms", attempts + 1, (unsigned long)delay);
	} else {
		shim_log("lobby: automatic Steam publication stopped after 3 attempts; recreate the host lobby to retry");
	}
}

void lobby_pump(void) {
	void *mm, *utils;
	// Steam calls and desired updates are serialized, so a clear cannot race
	// publication and a pending result always belongs to one generation.
	AcquireSRWLockExclusive(&lock);
	if ((!desired[0] && !pending && !published) || !bind_api())
		goto done;
	mm = api.matchmaking();
	utils = api.utils();
	if (!mm || !utils)
		goto done; // Steam may not be initialized yet.
	if (published && published_generation != generation) {
		api.leave(mm, published);
		shim_log("lobby: left automatic Steam lobby %llu", (unsigned long long)published);
		published = 0;
	}
	if (pending) {
		unsigned char failed = 0;
		lobby_created result = {0};
		if (!api.completed(utils, pending, &failed))
			goto done;
		if (!api.result(utils, pending, &result, sizeof(result), 513, &failed) || failed || result.result != 1 || !result.lobby) {
			shim_log("lobby: automatic Steam creation failed (I/O %u, EResult %d)", failed, result.result);
			retry_later();
		} else if (pending_generation != generation || !desired[0]) {
			api.leave(mm, result.lobby);
			shim_log("lobby: discarded stale automatic Steam lobby %llu", (unsigned long long)result.lobby);
		} else if (!api.set_data(mm, result.lobby, "invite", desired)) {
			api.leave(mm, result.lobby);
			shim_log("lobby: automatic Steam publication failed: SetLobbyData "
					 "rejected invite");
			retry_later();
		} else {
			published = result.lobby;
			published_generation = generation;
			shim_log("lobby: automatic Steam lobby %llu published for friends", (unsigned long long)published);
		}
		pending = 0;
	}
	if (desired[0] && !published && attempts < 3 && GetTickCount64() >= retry_at) {
		pending_generation = generation;
		attempts++;
		pending = api.create(mm, 1, 256); // Same friends-only settings as MPLobby.
		if (!pending) {
			shim_log("lobby: automatic Steam creation refused (invalid API call)");
			retry_later();
		}
	}
done:
	ReleaseSRWLockExclusive(&lock);
}
