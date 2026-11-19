#!/bin/sh
# A forked node whose upstream refuses the genesis fetch: its own reason goes
# to stderr and it exits before ever reporting readiness.
echo 'Error: server returned an error response: error code 429' >&2
exit 1
