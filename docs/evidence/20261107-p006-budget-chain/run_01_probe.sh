#!/usr/bin/env bash
set -uo pipefail
cd /opt/cloudpivot/docs/evidence/20261107-p006-budget-chain
source ./00_env.sh
psqlf ./01_probe.sql
