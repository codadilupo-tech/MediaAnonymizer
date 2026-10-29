/*
 * C ABI of MediaAnonymizer for the mobile apps (crates/mobile/src/lib.rs).
 *
 * Strings are NUL-terminated UTF-8. Every `char *` returned must be released
 * with mediaanon_string_free(). Fallible calls return JSON:
 *   {"ok": <value>}  or  {"error": {"kind": "...", "message": "..."}}
 * `options` is the JSON form of mediaanon_core::Options.
 * All calls are blocking and thread-safe.
 */
#ifndef MEDIAANON_H
#define MEDIAANON_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

char *mediaanon_version(void);
void mediaanon_string_free(char *s);

uint64_t mediaanon_cancel_new(void);
void mediaanon_cancel(uint64_t handle);
void mediaanon_cancel_free(uint64_t handle);

/* {"ok": {"format": "JPEG", "findings": [{"block", "detail", "sensitive"}]}} */
char *mediaanon_inspect(const char *path);
/* {"ok": <report>}; `cancel` is a handle or 0. */
char *mediaanon_process(const char *options, const char *input, const char *output, uint64_t cancel);
/* Writes a JPEG preview to `output`. {"ok": {"faces", "width", "height"}} */
char *mediaanon_preview(const char *options, const char *input, const char *output, uint32_t max_side);

#ifdef __cplusplus
}
#endif

#endif /* MEDIAANON_H */
