#!/bin/zsh
# Whole-Chrome CPU (renderer + GPU + browser processes) for one prototype URL.
# Usage: cpu.sh <url> [extra shot.mjs flags]   → prints % of one core over 10 s.
N=/opt/homebrew/bin/node
DIR=${0:A:h}
cputotal() { ps -A -o time=,command= | grep 'fs-shot-' | grep -v grep | awk '{split($1,a,":"); if (length(a)==3) t+=a[1]*3600+a[2]*60+a[3]; else t+=a[1]*60+a[2]} END {printf "%.3f", t+0}'; }
$N $DIR/shot.mjs "$1" /tmp/cpu-shot.png --wait 1500 --perf ${PERF:-16} ${@:2} >/dev/null &
pid=$!
sleep ${PRE:-5}
a=$(cputotal); sleep 10; b=$(cputotal)
wait $pid
echo "scale=1; ($b - $a) * 10" | bc
