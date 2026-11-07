#!/usr/bin/env bash
# tc netem wrapper used by the e2e harness (CALLXL_NETEM=tc) and CI. Needs root.
#   netem.sh probe                                  exit 0 if netem works on $NETEM_DEV
#   netem.sh del                                    remove all impairment
#   netem.sh set <all|v4|v6> <delay_ms> <jitter_ms> <loss_pct> <rate_kbit|0>
# Family-specific impairment: prio qdisc whose default band is clean and a
# `protocol ip|ipv6` filter steering that family into a netem child qdisc.
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
    fam="$2"; args="$(netem_args "$3" "$4" "$5" "$6")"
    tc qdisc del dev "$DEV" root 2>/dev/null || true
    if [[ "$fam" == "all" ]]; then
      # shellcheck disable=SC2086
      tc qdisc add dev "$DEV" root netem $args
    else
      proto=ip; [[ "$fam" == "v6" ]] && proto=ipv6
      tc qdisc add dev "$DEV" root handle 1: prio bands 4 priomap 3 3 3 3 3 3 3 3 3 3 3 3 3 3 3 3
      # shellcheck disable=SC2086
      tc qdisc add dev "$DEV" parent 1:3 handle 30: netem $args
      tc filter add dev "$DEV" parent 1:0 protocol "$proto" prio 1 u32 match u32 0 0 flowid 1:3
    fi
    ;;
  *)
    echo "usage: $0 probe|del|set <all|v4|v6> <delay_ms> <jitter_ms> <loss_pct> <rate_kbit>" >&2
    exit 2
    ;;
esac
