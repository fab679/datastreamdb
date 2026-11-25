#!/usr/bin/env bash
# Resumable crates.io publisher for the datastreamdb workspace.
#
# Why this exists:
#   - crates.io limits *new crate* publications. Official policy:
#     a burst of 5 new crates, then 1 new crate every 10 minutes.
#     The 429 response carries an exact "Please try again after ..."
#     timestamp, which accounts for any accumulated burst debt.
#   - `cargo publish` has no workspace mode, so crates must go leaf-first.
#   - a failed run must resume without republishing or burning a slot.
#
# Design:
#   - canonical leaf-first order from docs/PUBLISH.md
#   - skip any crate/version already visible on crates.io
#   - before publishing, verify every internal path dependency is visible
#   - wait 10 minutes after each successful new-crate publish
#   - on 429, sleep until the server-provided retry timestamp, then retry
#   - persist next-allowed time so a restart cannot jump the queue
#
# Usage:
#   tools/publish-crates.sh
#   INITIAL_NOT_BEFORE=2026-11-24T19:17:42Z tools/publish-crates.sh
#   INTERVAL_SECONDS=600 SAFETY_SECONDS=15 tools/publish-crates.sh
#
# State and logs live outside the repo by default:
#   ~/.local/state/datastreamdb-release/
#
set -Eeuo pipefail

REPO=${REPO:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)}
STATE_DIR=${STATE_DIR:-$HOME/.local/state/datastreamdb-release}
INTERVAL_SECONDS=${INTERVAL_SECONDS:-600}
SAFETY_SECONDS=${SAFETY_SECONDS:-15}
API=${API:-https://crates.io/api/v1/crates}
USER_AGENT=${USER_AGENT:-datastreamdb-release/0.1}
INITIAL_NOT_BEFORE=${INITIAL_NOT_BEFORE:-}

# Canonical dependency order from docs/PUBLISH.md. Already-published
# crates are included so the script is idempotent and can verify the
# whole release at the end.
CRATES=(
  datastreamdb-proto
  datastreamdb-acl
  datastreamdb-codec
  datastreamdb-algo
  datastreamdb-audit
  datastreamdb-lex
  datastreamdb-types
  datastreamdb-tok
  datastreamdb-x
  datastreamdb-chunker
  datastreamdb-enc
  datastreamdb-schema
  datastreamdb-store
  datastreamdb-posting
  datastreamdb-backup
  datastreamdb-dql
  datastreamdb-task
  datastreamdb-query
  datastreamdb-graphql
  datastreamdb-core
  datastreamdb-ffi
  datastreamdb-filestore
  datastreamdb-raft
  datastreamdb-worker
  datastreamdb-frontend
  datastreamdb-xidmap
  datastreamdb-loader
  datastreamdb-server
)

mkdir -p "$STATE_DIR"
LOG=$STATE_DIR/publish.log
STATE=$STATE_DIR/next-allowed
STATUS=$STATE_DIR/status.json
METADATA=$STATE_DIR/metadata.json
LAST_OUTPUT=$STATE_DIR/last-cargo-output.log
LOCK=$STATE_DIR/publish.lock

exec 9>"$LOCK"
if ! flock -n 9; then
  echo "another publish-crates.sh is already running (lock: $LOCK)" >&2
  exit 1
fi

log() {
  printf '[%s] %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" "$*" | tee -a "$LOG"
}

die() {
  log "FATAL: $*"
  exit 1
}

write_status() {
  local phase=$1 crate=${2:-} next=${3:-} tmp="$STATUS.tmp.$$"
  jq -n \
    --arg phase "$phase" \
    --arg crate "$crate" \
    --arg next "$next" \
    --arg updatedAt "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" \
    '{phase:$phase, crate:$crate, nextAllowed:$next, updatedAt:$updatedAt}' >"$tmp"
  mv -f "$tmp" "$STATUS"
}

save_next() {
  local tmp="$STATE.tmp.$$"
  printf '%s\n' "$1" >"$tmp"
  mv -f "$tmp" "$STATE"
}

load_next() {
  if [[ -s "$STATE" ]]; then
    cat "$STATE"
  elif [[ -n "$INITIAL_NOT_BEFORE" ]]; then
    date -u -d "$INITIAL_NOT_BEFORE" +%s
  else
    date -u +%s
  fi
}

crate_version() {
  jq -r --arg c "$1" '.packages[] | select(.name == $c) | .version' "$METADATA" | head -n1
}

crate_deps() {
  jq -r --arg c "$1" \
    '.packages[] | select(.name == $c) | .dependencies[] | select(.path != null) | .name' \
    "$METADATA" | sort -u
}

api_status() {
  local crate=$1 version=$2
  curl -sS -o /dev/null -w '%{http_code}' \
    -H "User-Agent: $USER_AGENT" \
    --connect-timeout 10 --max-time 30 \
    --retry 3 --retry-delay 2 --retry-all-errors \
    "$API/$crate/$version" || true
}

is_published() {
  local crate=$1 version=$2 code
  code=$(api_status "$crate" "$version")
  [[ "$code" == "200" ]]
}

wait_for_visibility() {
  local crate=$1 version=$2 deadline=$(( $(date -u +%s) + 900 ))
  while (( $(date -u +%s) < deadline )); do
    if is_published "$crate" "$version"; then
      return 0
    fi
    log "waiting for $crate $version to appear in the crates.io API"
    sleep 10
  done
  return 1
}

wait_until() {
  local target=$1 now remaining
  while :; do
    now=$(date -u +%s)
    remaining=$(( target - now ))
    if (( remaining <= 0 )); then
      return 0
    fi
    # Sleep in short chunks so the log shows progress and SIGINT is responsive.
    if (( remaining > 60 )); then
      log "rate-limit wait: $(( remaining / 60 ))m $(( remaining % 60 ))s remaining (next slot $(date -u -d "@$target" '+%Y-%m-%dT%H:%M:%SZ'))"
      sleep 60
    else
      sleep "$remaining"
    fi
  done
}

parse_retry_epoch() {
  local file=$1 stamp epoch
  stamp=$(grep -oE 'after (Mon|Tue|Wed|Thu|Fri|Sat|Sun), [0-9]{1,2} [A-Z][a-z]{2} [0-9]{4} [0-9]{2}:[0-9]{2}:[0-9]{2} GMT' "$file" 2>/dev/null \
    | tail -n1 | sed 's/^after //') || true
  [[ -n "$stamp" ]] || return 1
  epoch=$(date -u -d "$stamp" +%s 2>/dev/null) || return 1
  printf '%s\n' "$epoch"
}

publish_one() {
  local crate=$1 version=$2 rc
  : >"$LAST_OUTPUT"
  log "publishing $crate $version"
  set +e
  cargo publish -p "$crate" --locked 2>&1 | tee -a "$LOG" | tee "$LAST_OUTPUT"
  rc=${PIPESTATUS[0]}
  set -e
  return "$rc"
}

preflight() {
  cd "$REPO"
  cargo metadata --no-deps --format-version 1 >"$METADATA"
  log "workspace metadata: $(jq '.packages | length' "$METADATA") packages"
}

main() {
  cd "$REPO"
  log "===== publish run started ====="
  log "repo=$REPO interval=${INTERVAL_SECONDS}s safety=${SAFETY_SECONDS}s"
  preflight

  local next_allowed crate version dep dep_version code rc retry_epoch now transient_retries
  next_allowed=$(load_next)
  write_status "waiting" "" "$(date -u -d "@$next_allowed" '+%Y-%m-%dT%H:%M:%SZ')"
  log "next new-crate slot: $(date -u -d "@$next_allowed" '+%Y-%m-%dT%H:%M:%SZ')"

  for crate in "${CRATES[@]}"; do
    version=$(crate_version "$crate")
    [[ -n "$version" ]] || die "no workspace version for $crate"

    if is_published "$crate" "$version"; then
      log "skip $crate $version (already on crates.io)"
      continue
    fi

    # A parent cannot be packaged until all of its path dependencies are
    # resolvable from crates.io at the workspace version.
    while read -r dep; do
      [[ -n "$dep" ]] || continue
      dep_version=$(crate_version "$dep")
      [[ -n "$dep_version" ]] || die "no workspace version for $dep (dependency of $crate)"
      if ! is_published "$dep" "$dep_version"; then
        die "$crate needs $dep $dep_version, which is not visible on crates.io yet"
      fi
    done < <(crate_deps "$crate")

    transient_retries=0
    while :; do
      wait_until "$next_allowed"
      write_status "publishing" "$crate" "$(date -u -d "@$next_allowed" '+%Y-%m-%dT%H:%M:%SZ')"

      if is_published "$crate" "$version"; then
        log "skip $crate $version (appeared before publish)"
        break
      fi

      rc=0
      publish_one "$crate" "$version" || rc=$?

      if (( rc == 0 )); then
        # Persist the slot before the visibility wait. If this process dies
        # while waiting, a restart must not publish the next crate immediately.
        now=$(date -u +%s)
        next_allowed=$(( now + INTERVAL_SECONDS + SAFETY_SECONDS ))
        save_next "$next_allowed"
        write_status "waiting" "" "$(date -u -d "@$next_allowed" '+%Y-%m-%dT%H:%M:%SZ')"
        if ! wait_for_visibility "$crate" "$version"; then
          die "$crate $version published but did not become visible within 15 minutes"
        fi
        log "published $crate $version; next slot $(date -u -d "@$next_allowed" '+%Y-%m-%dT%H:%M:%SZ')"
        break
      fi

      # A rerun can race with an earlier upload. Treat "already exists" as done.
      if grep -qiE 'already (exists|uploaded)|is already uploaded|crate version .* already' "$LAST_OUTPUT"; then
        log "skip $crate $version (cargo reports it already exists)"
        break
      fi

      retry_epoch=$(parse_retry_epoch "$LAST_OUTPUT" || true)
      if grep -qE '429 Too Many Requests|published too many new crates' "$LAST_OUTPUT"; then
        if [[ -z "${retry_epoch:-}" ]]; then
          retry_epoch=$(( $(date -u +%s) + INTERVAL_SECONDS ))
          log "429 without a parseable retry timestamp; backing off ${INTERVAL_SECONDS}s"
        else
          log "429: server says retry after $(date -u -d "@$retry_epoch" '+%Y-%m-%dT%H:%M:%SZ')"
        fi
        # Add a small safety margin; never move the slot backwards and
        # never allow a zero/negative wait to cause a tight retry loop.
        now=$(date -u +%s)
        retry_epoch=$(( retry_epoch + SAFETY_SECONDS ))
        if (( retry_epoch < now + 60 )); then
          retry_epoch=$(( now + 60 ))
        fi
        if (( retry_epoch > next_allowed )); then
          next_allowed=$retry_epoch
        fi
        save_next "$next_allowed"
        write_status "rate-limited" "$crate" "$(date -u -d "@$next_allowed" '+%Y-%m-%dT%H:%M:%SZ')"
        continue
      fi

      if grep -qiE 'spurious network error|network failure|timed out|connection reset|Could not resolve host|status 5[0-9][0-9]' "$LAST_OUTPUT"; then
        transient_retries=$(( transient_retries + 1 ))
        if (( transient_retries > 5 )); then
          die "$crate $version failed after $transient_retries transient retries; see $LAST_OUTPUT"
        fi
        log "transient failure for $crate; retry $transient_retries/5 in 60s"
        sleep 60
        continue
      fi

      if grep -qE 'no matching package named|failed to select a version|failed to prepare local package' "$LAST_OUTPUT"; then
        die "$crate could not resolve a dependency from crates.io; see $LAST_OUTPUT"
      fi

      die "$crate $version failed; see $LAST_OUTPUT"
    done
  done

  log "===== verifying full release ====="
  local missing=0
  for crate in "${CRATES[@]}"; do
    version=$(crate_version "$crate")
    if is_published "$crate" "$version"; then
      log "ok $crate $version"
    else
      log "MISSING $crate $version"
      missing=$(( missing + 1 ))
    fi
  done
  if (( missing > 0 )); then
    die "$missing crate(s) still missing"
  fi

  write_status "complete" "" ""
  log "===== publish run complete: all ${#CRATES[@]} crates visible ====="
}

main "$@"
