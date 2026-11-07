#!/usr/bin/env bash
# tc netem wrapper used by the e2e harness (CALLXL_NETEM=tc) and CI. Needs root.
#   netem.sh probe                                  exit 0 if netem works on $NETEM_DEV
#   netem.sh del                                    remove all impairment
#   netem.sh set <all|v4|v6> <delay_ms> <jitter_ms> <loss_pct> <rate_kbit|0>
# Impairment applies to UDP only (see `set`), per address family or both.
set -euo pipefail
DEV="${NETEM_DEV:-lo}"
cmd="${1:-}"

netem_args() {
  local delay=$1 jitter=$2 loss=$3 rate=$4
  local a="delay ${delay}ms"
  if [[ "$jitter" != "0" ]]; then a+=" ${jitter}ms"; fi
  if [[ "$loss" != "0" && "$loss" != "0.0" ]]; then a+=" loss ${loss}%"; fi
  if [[ "$rate" != "0" ]]; then
    # ~100 ms of queue at this rate (600 B average packets), like a router's tail drop.
    local limit=$(( rate * 1000 / 8 / 10 / 600 ))
    (( limit < 10 )) && limit=10
    a+=" rate ${rate}kbit limit ${limit}"
  fi
  echo "$a"
}

case "$cmd" in
  probe)
    tc qdisc del dev "$DEV" root 2>/dev/null || true
    tc qdisc add dev "$DEV" root netem delay 0ms
    tc qdisc del dev "$DEV" root
    ;;
  del)
    tc qdisc del dev "$DEV" root 2>/dev/null || true
    ;;
  set)
    # Only UDP (ICE/media/data channels) is impaired, never TCP: the harness's control
    # sockets and LAN signaling also run over lo, and a 64 KB loopback TCP segment behind
    # a rate cap stalls the audio (CI run 42343155949: 18-24 % audio loss with no file data
    # flowing yet). The proxy backend likewise impairs only the media's UDP.
    # Layout: prio qdisc whose default band (1:4) is clean; u32 filters steer UDP of the
    # chosen family (all|v4|v6) into the netem band 1:3. Reconfigured in place (`change`)
    # when the layout is already right: re-adding would drop every queued packet.
    fam="$2"; args="$(netem_args "$3" "$4" "$5" "$6")"
    want_v4=0; want_v6=0
    [[ "$fam" == "all" || "$fam" == "v4" ]] && want_v4=1
    [[ "$fam" == "all" || "$fam" == "v6" ]] && want_v6=1
    root="$(tc qdisc show dev "$DEV" root 2>/dev/null || true)"
    filters="$(tc filter show dev "$DEV" parent 1:0 2>/dev/null || true)"
    has_v4=0; has_v6=0
    [[ "$filters" == *"protocol ip pref"* ]] && has_v4=1
    [[ "$filters" == *"protocol ipv6 pref"* ]] && has_v6=1
    if [[ "$root" == *"qdisc prio 1:"* && $has_v4 == "$want_v4" && $has_v6 == "$want_v6" ]]; then
      # shellcheck disable=SC2086
      tc qdisc change dev "$DEV" parent 1:3 handle 30: netem $args
    else
      tc qdisc del dev "$DEV" root 2>/dev/null || true
      tc qdisc add dev "$DEV" root handle 1: prio bands 4 priomap 3 3 3 3 3 3 3 3 3 3 3 3 3 3 3 3
      # shellcheck disable=SC2086
      tc qdisc add dev "$DEV" parent 1:3 handle 30: netem $args
      if (( want_v4 )); then
        tc filter add dev "$DEV" parent 1:0 protocol ip prio 1 u32 match ip protocol 17 0xff flowid 1:3
      fi
      if (( want_v6 )); then
        tc filter add dev "$DEV" parent 1:0 protocol ipv6 prio 2 u32 match ip6 protocol 17 0xff flowid 1:3
      fi
    fi
    ;;
  *)
    echo "usage: $0 probe|del|set <all|v4|v6> <delay_ms> <jitter_ms> <loss_pct> <rate_kbit>" >&2
    exit 2
    ;;
esac
