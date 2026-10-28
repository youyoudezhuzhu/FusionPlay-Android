#!/bin/bash
# fpk-sim-lib.sh — 沙箱模拟安装/升级测试的共享函数（只被 source，不单独执行）。
#
# 安全纪律（总纲 §5.1 / §10）：
#   - 所有 TRIM_* 路径必须落在 mktemp 沙箱内，绝不碰真实 /var/apps、/vol1、/root；
#   - 健康检查端口随机，且断言不等于 manifest.service_port（否则会打到真机上的线上实例）；
#   - 停模拟实例必须带同一组 TRIM_* 环境变量（否则 cmd/main 去错目录找 PID）；
#   - 清理进程只按「沙箱内二进制精确路径」/ 沙箱目录匹配，绝不 pkill 系统或其它应用进程。
# shellcheck shell=bash

SIM_PASS=0
SIM_FAIL=0

sim_ok()   { printf '  \033[32m✅\033[0m %s\n' "$1"; SIM_PASS=$((SIM_PASS + 1)); }
sim_bad()  { printf '  \033[31m❌\033[0m %s\n' "$1"; SIM_FAIL=$((SIM_FAIL + 1)); }
sim_eq() {
    if [ "$1" = "$2" ]; then sim_ok "$3"; else sim_bad "$3（期望 '$1'，实际 '$2'）"; fi
}
sim_ne() {
    if [ "$1" != "$2" ]; then sim_ok "$3"; else sim_bad "$3（不应等于 '$2'）"; fi
}
sim_nonempty() {
    if [ -n "$1" ]; then sim_ok "$2"; else sim_bad "$2（为空）"; fi
}
sim_true() { local msg="$1"; shift; if "$@" >/dev/null 2>&1; then sim_ok "$msg"; else sim_bad "$msg"; fi; }
sim_code() {
    # sim_code <期望> <说明> <curl 参数...>
    local want="$1" msg="$2"; shift 2
    local got; got="$(sim_http_code "$@")"
    if [ "$got" = "$want" ]; then sim_ok "$msg（$got）"; else sim_bad "$msg（期望 $want，实际 $got）"; fi
}
sim_matches() {
    # sim_matches <正则> <实际> <说明>
    if printf '%s' "$2" | grep -qE "$1"; then sim_ok "$3"; else sim_bad "$3（实际 '$2'）"; fi
}
sim_summary() {
    echo
    echo "断言汇总：通过 $SIM_PASS / 失败 $SIM_FAIL"
    [ "$SIM_FAIL" -eq 0 ]
}

sim_require_tools() {
    local missing="" c
    for c in curl tar ss pgrep pkill python3 timeout; do
        command -v "$c" >/dev/null 2>&1 || missing="$missing $c"
    done
    [ -z "$missing" ] || { echo "❌ 缺少测试所需命令:$missing" >&2; exit 1; }
}

sim_sandbox() {
    local base="${FPK_SIM_ROOT:-/tmp}"
    [ -d "$base" ] || base=/tmp
    local d
    d="$(mktemp -d "$base/fusionplay-sim.XXXXXX")" || { echo "❌ mktemp 失败" >&2; exit 1; }
    case "$d" in
        /tmp/*|/var/tmp/*|/vol*/@app*/tmp*|"$HOME"/tmp*) : ;;
        *) echo "❌ 沙箱 $d 不在临时区，拒绝继续（防止污染真实路径）" >&2; exit 1 ;;
    esac
    printf '%s\n' "$d"
}

sim_under() { case "$1" in "$2"/*) return 0 ;; *) return 1 ;; esac; }
sim_require_under() {
    # sim_require_under <变量名> <路径> <沙箱>
    sim_under "$2" "$3" || { echo "❌ $1=$2 不在沙箱 $3 内，拒绝继续" >&2; exit 1; }
}

sim_manifest_get() {
    # sim_manifest_get <manifest 文件> <key>
    grep -m1 -E "^[[:space:]]*$2[[:space:]]*=" "$1" 2>/dev/null \
        | sed 's/^[^=]*=//' | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' \
              -e 's/^"//' -e 's/"$//'
}

sim_pick_port() {
    # 随机空闲端口，且避开 manifest 里声明的服务端口与 fnOS 系统端口
    python3 - "$1" <<'PY'
import socket, sys
banned = {18080, 5666, 5667, 8000, 22, 445, 2049, 5005, 5006}
declared = int(sys.argv[1]) if len(sys.argv) > 1 and sys.argv[1].isdigit() else 0
banned.add(declared)
for _ in range(100):
    s = socket.socket()
    try:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    finally:
        s.close()
    if port not in banned:
        print(port)
        sys.exit(0)
sys.exit("无法分配测试端口")
PY
}

sim_port_listeners() {
    # 监听该端口的 socket 数（只数 listen 状态）
    [ -n "${1:-}" ] || { echo 0; return 0; }
    ss -H -lnt 2>/dev/null | awk '{print $4}' | grep -cE "[:.]$1\$" || true
}
sim_wait_port_free() {
    local port="$1" tries="${2:-25}" i
    for i in $(seq 1 "$tries"); do
        [ "$(sim_port_listeners "$port")" = "0" ] && return 0
        sleep 0.2
    done
    return 1
}
sim_wait_gone() {
    local pid="$1" tries="${2:-40}" i
    for i in $(seq 1 "$tries"); do
        kill -0 "$pid" 2>/dev/null || return 0
        sleep 0.2
    done
    return 1
}

sim_http_code() { curl -s -o /dev/null -w '%{http_code}' --max-time 2 "$@" 2>/dev/null || true; }
sim_health_json() { curl -fsS --max-time 2 "http://127.0.0.1:$1/healthz" 2>/dev/null; }
sim_health_version() {
    sim_health_json "$1" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("version",""))' 2>/dev/null \
        || true
}
sim_wait_health() {
    local port="$1" tries="${2:-80}" i
    for i in $(seq 1 "$tries"); do
        curl -fsS --max-time 1 "http://127.0.0.1:$port/healthz" >/dev/null 2>&1 && return 0
        sleep 0.25
    done
    return 1
}
sim_wait_health_down() {
    local port="$1" tries="${2:-40}" i
    for i in $(seq 1 "$tries"); do
        [ "$(sim_http_code "http://127.0.0.1:$port/healthz")" = "200" ] || return 0
        sleep 0.25
    done
    return 1
}

sim_pid_of() {
    local f="$1" pid=""
    [ -f "$f" ] && pid="$(tr -dc '0-9' < "$f" 2>/dev/null)"
    printf '%s' "$pid"
}
sim_alive() { [ -n "${1:-}" ] && kill -0 "$1" 2>/dev/null; }
sim_pids_of() {
    # 精确匹配本沙箱内的二进制路径（排除当前 shell）
    pgrep -f -- "$1" 2>/dev/null | grep -v "^$$\$" || true
}
sim_pids_count() {
    local pids; pids="$(sim_pids_of "$1")"
    [ -z "$pids" ] && { echo 0; return; }
    printf '%s\n' "$pids" | grep -c . || true
}
sim_proc_env() {
    # sim_proc_env <pid> <ENV_NAME>
    local pid="$1" name="$2"
    [ -r "/proc/$pid/environ" ] || return 0
    tr '\0' '\n' < "/proc/$pid/environ" 2>/dev/null | sed -n "s/^${name}=//p" | head -1
}
sim_proc_exe_hash() {
    local pid="$1"
    sha256sum "/proc/$pid/exe" 2>/dev/null | awk '{print $1}'
}
sim_file_hash() { sha256sum "$1" 2>/dev/null | awk '{print $1}'; }

sim_cleanup_sandbox_processes() {
    # 只清理命令行里带沙箱路径的进程（沙箱路径是 mktemp 唯一的）
    [ -n "${1:-}" ] || return 0
    pkill -f -- "$1" 2>/dev/null || true
}
