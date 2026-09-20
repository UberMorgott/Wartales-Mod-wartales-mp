// Desired Steam advertisement, queued by the authenticated bridge reader and
// applied exclusively on the game thread during SteamAPI_RunCallbacks.
#ifndef WARTALES_MP_LOBBY_H
#define WARTALES_MP_LOBBY_H
#include <stddef.h>
int lobby_set_invite(const unsigned char *value, size_t len);
void lobby_pump(void);
#endif
