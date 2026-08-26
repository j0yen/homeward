#!/usr/bin/env bash
# Cross-session holdout eval: gallery = newest-session photo, queries = older-session photos.
set -euo pipefail
EVAL_ROOT=/home/jsy/homeward-eval; X=$EVAL_ROOT/xsession
export HF_HOME=$HOME/.cache/homeward/hf
export PYTHONPATH=/home/jsy/build/homeward/homeward/embed
cd $EVAL_ROOT/workdir   # yolov8n.pt resolves against CWD
for v in "$@"; do
  echo "=== xsession eval variant=$v ==="
  $EVAL_ROOT/venv/bin/homeward-embed eval --dataset-dir $X --embed-variant $v --ks 1,5,20 2>&1 | tee $X/results-$v-$(date +%Y%m%d).txt
  cp $X/eval-results.json $X/eval-results-$v.json
done
