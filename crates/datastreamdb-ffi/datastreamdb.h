/* datastreamdb.h - stable C ABI for datastreamdb (M1).
 *
 * Single source of truth: this file must match `crates/datastreamdb-ffi/src/lib.rs`.
 * `cargo xtask codegen` will generate it (M1 tracks manual sync; see TODO.md).
 *
 * Status codes (int32_t):
 *   0  DATASTREAMDB_OK       success
 *   1  DATASTREAMDB_EINVAL   null pointer, invalid UTF-8, or malformed envelope
 *   2  DATASTREAMDB_ECONFIG  config JSON rejected
 *   3  DATASTREAMDB_EENGINE  engine failure (storage, backend)
 *   4  DATASTREAMDB_EPANIC   caught a Rust panic; instance state is unchanged
 *
 * Threading: handles are Send + Sync; datastreamdb_last_error is thread-local.
 * Linking (Linux): gcc app.c -I. -L<target>/debug -ldatastreamdb_ffi -lpthread -ldl -lm
 */
#ifndef DATASTREAMDB_H
#define DATASTREAMDB_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define DATASTREAMDB_OK 0
#define DATASTREAMDB_EINVAL 1
#define DATASTREAMDB_ECONFIG 2
#define DATASTREAMDB_EENGINE 3
#define DATASTREAMDB_EPANIC 4

/* Opaque handle owning one isolated instance. */
typedef struct datastreamdb_handle datastreamdb_handle_t;

/* Opens an instance from JSON config, e.g. "{}" for in-memory.
 * On success the caller owns *out and must pass it to datastreamdb_close. */
int32_t datastreamdb_open(const char *config_json, datastreamdb_handle_t **out);

/* Executes a JSON request; response bytes go to *out_ptr/*out_len.
 * On success the caller owns the buffer and must free it with
 * datastreamdb_free_bytes. See docs/EMBEDDING.md for the envelope. */
int32_t datastreamdb_execute(const datastreamdb_handle_t *handle, const uint8_t *req,
                       size_t req_len, uint8_t **out_ptr, size_t *out_len);

/* Closes a handle. The last close flushes that instance. */
int32_t datastreamdb_close(datastreamdb_handle_t *handle);

/* Frees a response buffer from datastreamdb_execute. NULL is a no-op. */
void datastreamdb_free_bytes(uint8_t *ptr, size_t len);

/* Describes the last failure on this thread (never NULL). */
const char *datastreamdb_last_error(void);

/* Crate version string (never NULL, static lifetime). */
const char *datastreamdb_version(void);

#ifdef __cplusplus
}
#endif

#endif /* DATASTREAMDB_H */
