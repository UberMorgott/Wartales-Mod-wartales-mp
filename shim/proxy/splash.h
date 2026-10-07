// The start-up status window shown while the bytecode is patched (splash.c).
#ifndef WARTALES_MP_SPLASH_H
#define WARTALES_MP_SPLASH_H

#include <windows.h>

// splash_begin shows the status window (after a short delay) from its own
// thread. Only inside Wartales.exe; a no-op on later calls.
void splash_begin(HINSTANCE inst);

// splash_patched reports the patch outcome: the window switches to "starting
// the game" and closes once the game's own window is visible. ok == FALSE
// also shows a message box naming the log.
void splash_patched(BOOL ok);

// console_detach releases an empty console window Wartales.exe (a console
// subsystem program) was given by its launcher, when no other process shares
// it. The game's output never goes there (hl_sys_print is hooked into the log).
void console_detach(void);

#endif
