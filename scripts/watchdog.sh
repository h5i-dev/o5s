#!/usr/bin/env bash
# Nudge idle agent sessions to keep going. Runs in its own tmux session "pico2peta-watchdog".
# A session counts as idle when its status line lacks "esc to interrupt" on two checks in a row.
# To stop nudging one role: touch /mnt/scratch/pico2peta/stop-<session>
INTERVAL=${INTERVAL:-300}
LOG=/mnt/scratch/pico2peta/watchdog.log
declare -A idle
while true; do
  for s in pico2peta pico2peta-net; do
    tmux has-session -t "$s" 2>/dev/null || continue
    [ -e "/mnt/scratch/pico2peta/stop-$s" ] && continue
    if tmux capture-pane -t "$s" -p | tail -5 | grep -q 'esc to interrupt'; then
      idle[$s]=0
    else
      idle[$s]=$(( ${idle[$s]:-0} + 1 ))
      if [ "${idle[$s]}" -ge 2 ]; then
        echo "$(date -u +%FT%TZ) nudge $s" >> "$LOG"
        tmux send-keys -t "$s" -l "Watchdog: you appear idle. If any milestone or goal in your task file or docs/PLAN.md is unfinished, continue with the next one now, and don't stop to report. If everything really is complete and verified, write that in your progress doc and reply with only DONE."
        sleep 1; tmux send-keys -t "$s" Enter
        idle[$s]=0
      fi
    fi
  done
  sleep "$INTERVAL"
done
