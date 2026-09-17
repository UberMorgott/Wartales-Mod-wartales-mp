// A stand-in steam.hdll for shimcheck: exports the six legacy P2P natives the
// way hlsteam does (steam_<name> plus the hlp_<name> resolver that returns
// its address), with bodies that must never run. Every call increments a
// counter the check reads through fake_legacy_calls(): the SDR shim is only
// correct if that counter stays at zero.
//
// Built by shim\check.ps1 into <OutDir>\check-steam\steam.hdll.

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>

#include <stdint.h>

#define EXPORT __declspec(dllexport)

static volatile LONG legacy_calls;

// Bodies are padded with real work so MinHook has room for its trampoline
// and the compiler cannot merge them.
EXPORT unsigned char steam_send_p2p_packet(unsigned char *uid, unsigned char *data, int length, int type, int channel) {
	(void)uid; (void)data; (void)length; (void)type; (void)channel;
	InterlockedIncrement(&legacy_calls);
	return 0;
}
EXPORT unsigned char *steam_read_p2p_packet(unsigned char *data, int max_length, uint32_t *length, int channel) {
	(void)data; (void)max_length; (void)length; (void)channel;
	InterlockedIncrement(&legacy_calls);
	return (unsigned char *)"LEGACY";
}
EXPORT unsigned char steam_is_p2p_packet_available(uint32_t *size, int channel) {
	(void)channel;
	InterlockedIncrement(&legacy_calls);
	if (size != NULL)
		*size = 0xDEAD;
	return 1;
}
EXPORT unsigned char steam_accept_p2p_session(unsigned char *uid) {
	(void)uid;
	InterlockedIncrement(&legacy_calls);
	return 1;
}
EXPORT unsigned char steam_close_p2p_session(unsigned char *uid) {
	(void)uid;
	InterlockedIncrement(&legacy_calls);
	return 0;
}
EXPORT void *steam_get_p2p_session_data(unsigned char *uid) {
	(void)uid;
	InterlockedIncrement(&legacy_calls);
	return (void *)&legacy_calls;
}

#define RESOLVER(name, sign_str)                     \
	EXPORT void *hlp_##name(const char **sign) {     \
		if (sign != NULL)                            \
			*sign = sign_str;                        \
		return (void *)&steam_##name;                \
	}
RESOLVER(send_p2p_packet, "bbiiib")
RESOLVER(read_p2p_packet, "biXib")
RESOLVER(is_p2p_packet_available, "Xib")
RESOLVER(accept_p2p_session, "bb")
RESOLVER(close_p2p_session, "bb")
RESOLVER(get_p2p_session_data, "b?")

EXPORT long fake_legacy_calls(void) { return legacy_calls; }
