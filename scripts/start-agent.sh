#!/usr/bin/env bash
# Launch (or re-attach to) the autonomous Claude Code worker in tmux.
# tmux runs server-side, so the session survives SSH disconnects.
#   Start/attach: scripts/start-agent.sh
#   Detach:       Ctrl-b d
#   Re-attach:    tmux attach -t pico2peta
set -euo pipefail
# Usage: scripts/start-agent.sh [core|net]   (default: core)
ROLE=${1:-core}
SCRATCH=/mnt/scratch/pico2peta
case "$ROLE" in
  core) SESSION=pico2peta     REPO=/home/ec2-user/Dev/pico2peta     TARGET=$SCRATCH/cargo-target     TASK=docs/AGENT_TASK.md ;;
  net)  SESSION=pico2peta-net REPO=/home/ec2-user/Dev/pico2peta-net TARGET=$SCRATCH/cargo-target-net TASK=docs/AGENT_TASK_NET.md ;;
  *) echo "unknown role $ROLE" >&2; exit 1 ;;
esac

if tmux has-session -t "$SESSION" 2>/dev/null; then
  exec tmux attach -t "$SESSION"
fi

PROMPT="Read $TASK and docs/DESIGN_BRIEF.md, then carry out the task autonomously. Start with goal 1."

tmux new-session -d -s "$SESSION" -c "$REPO" -x 220 -y 50 \
  -e CARGO_TARGET_DIR="$TARGET" \
  -e TMPDIR="$SCRATCH/tmp"
tmux set-option -t "$SESSION" history-limit 100000
tmux send-keys -t "$SESSION" \
  "claude --model claude-opus-5-5 --dangerously-skip-permissions \"$PROMPT\"" Enter
echo "Started tmux session '$SESSION'. Attach with: tmux attach -t $SESSION"
