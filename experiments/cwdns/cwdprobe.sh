#!/usr/bin/env bash
echo "PROBE pwd_P=$(pwd -P)"
echo "PROBE pwd_logical=$(pwd)"
echo "PROBE PWD_env=${PWD:-<unset>}"
echo "PROBE marker_visible=$(ls -1 . 2>/dev/null | tr '\n' ' ')"
