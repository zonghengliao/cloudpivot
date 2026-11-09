#!/usr/bin/env bash
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
mkdir -p /tmp/i5
cd /opt/cloudpivot/backend
cargo build > /tmp/i5/build.log 2>&1
echo "BUILD_RC=$?" >> /tmp/i5/build.log
