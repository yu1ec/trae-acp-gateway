#!/usr/bin/env bash
# Build (if needed) and run the gateway on http://127.0.0.1:8080
# Usage: ./run.sh [extra args, e.g. --port 9000 --workdir /path/to/project]
set -euo pipefail
cd "$(dirname "$0")"

[ -x target/release/trae_acp_gateway ] || cargo build --release
exec ./target/release/trae_acp_gateway "$@"
#exec ./target/release/trae_acp_gateway --port 6666 --workdir ./agent-workdir
