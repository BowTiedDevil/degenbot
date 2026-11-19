#!/bin/sh
# The readiness wait checks its deadline between stdout lines, so a stand-in
# that never reports readiness must keep emitting complete lines — a child that
# prints nothing parks the wait forever instead of letting the deadline fire.
while true; do
    echo .
    sleep 0.05
done
