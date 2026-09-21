// tips.h: the C ABI of wartales-tips (E:\DEV\Wartales\tips, Rust), linked
// statically into winmm.dll. It rewrites the HashLink bytecode structurally
// (inserts opcodes, appends a function and a type) so the items in the
// new-game starting-troop preview get hover tooltips. The output image is a
// different size from the input.
#ifndef WARTALES_TIPS_H
#define WARTALES_TIPS_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// wartales_tips_patch patches image[0..len]. Returns 0 on success with
// *out/*out_len set to a buffer owned by the library (release it with
// wartales_tips_free); returns 1 on failure with *out left NULL and a
// NUL-terminated reason copied into err (truncated to err_len). Never unwinds.
int32_t wartales_tips_patch(const uint8_t *image, size_t len, uint8_t **out, size_t *out_len,
	uint8_t *err, size_t err_len);

// wartales_tips_free releases a buffer returned by wartales_tips_patch; p/len
// must be exactly what it returned (p may be NULL).
void wartales_tips_free(uint8_t *p, size_t len);

#ifdef __cplusplus
}
#endif

#endif
