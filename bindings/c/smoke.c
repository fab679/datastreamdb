/* smoke.c - reference C consumer: two isolated instances, envelope roundtrip.
 * Build: see bindings/c/README.md. Exits nonzero on any failure. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "datastreamdb.h"

#define CHECK(cond, msg)                                        \
  do {                                                          \
    if (!(cond)) {                                              \
      fprintf(stderr, "FAIL: %s (last_error=%s)\n", msg,        \
              datastreamdb_last_error());                             \
      return 1;                                                 \
    }                                                           \
  } while (0)

/* Executes req and returns a malloc'd NUL-terminated copy of the response. */
static char *exec_copy(const datastreamdb_handle_t *h, const char *req) {
  uint8_t *out = NULL;
  size_t len = 0;
  if (datastreamdb_execute(h, (const uint8_t *)req, strlen(req), &out, &len) !=
      DATASTREAMDB_OK) {
    return NULL;
  }
  char *copy = (char *)malloc(len + 1);
  if (copy == NULL) {
    datastreamdb_free_bytes(out, len);
    return NULL;
  }
  memcpy(copy, out, len);
  copy[len] = '\0';
  datastreamdb_free_bytes(out, len);
  return copy;
}

int main(void) {
  printf("datastreamdb version: %s\n", datastreamdb_version());

  datastreamdb_handle_t *a = NULL;
  datastreamdb_handle_t *b = NULL;
  CHECK(datastreamdb_open("{}", &a) == DATASTREAMDB_OK, "open a");
  CHECK(datastreamdb_open("{\"tenant\": \"b\"}", &b) == DATASTREAMDB_OK, "open b");
  CHECK(a != NULL && b != NULL, "handles non-null");

  char *ping = exec_copy(a, "{\"op\":\"ping\"}");
  CHECK(ping != NULL && strstr(ping, "\"ok\":true") != NULL, "ping a");
  CHECK(strstr(ping, "\"backend\":\"memory\"") != NULL, "ping backend");
  free(ping);

  char *put = exec_copy(a, "{\"op\":\"put\",\"key\":\"n\",\"value\":\"Ada\"}");
  CHECK(put != NULL, "put a");
  free(put);

  char *ga = exec_copy(a, "{\"op\":\"get\",\"key\":\"n\"}");
  char *gb = exec_copy(b, "{\"op\":\"get\",\"key\":\"n\"}");
  CHECK(ga != NULL && strstr(ga, "Ada") != NULL, "a reads Ada");
  CHECK(gb != NULL && strstr(gb, "null") != NULL, "b isolated (null)");
  free(ga);
  free(gb);

  char *bad = exec_copy(a, "{\"op\":\"nope\"}");
  CHECK(bad == NULL, "bad op fails");
  printf("expected error: %s\n", datastreamdb_last_error());

  CHECK(datastreamdb_close(a) == DATASTREAMDB_OK, "close a");
  CHECK(datastreamdb_close(b) == DATASTREAMDB_OK, "close b");
  CHECK(datastreamdb_close(NULL) == DATASTREAMDB_EINVAL, "null close rejected");

  printf("C smoke: PASS\n");
  return 0;
}
