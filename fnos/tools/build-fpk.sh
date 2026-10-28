#!/bin/bash
# 构建 fnOS FPK：Rust release 二进制 + 静态前端 + fnpack。
# 用法：./fnos/tools/build-fpk.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"        # fnos/
REPO="$(cd "$ROOT/.." && pwd)"
STAGE_ROOT="${APP_BUILD_ROOT:-/tmp/fusionplay-fnos-build}"
STAGE="$STAGE_ROOT/fusionplay-fnos"
FNPACK="$ROOT/tools/fnpack-linux-amd64"

[ -x "$FNPACK" ] || { echo "缺少 fnpack: $FNPACK"; exit 1; }
export PATH="$HOME/.cargo/bin:/usr/local/cargo/bin:$PATH"
command -v cargo >/dev/null || { echo "缺少 cargo"; exit 1; }

echo "▶ cargo build (x86_64 Linux release)"
cargo build --release --manifest-path "$REPO/fnos/backend/Cargo.toml"

echo "▶ 组装暂存目录 $STAGE"
rm -rf "$STAGE"
mkdir -p "$STAGE"
cp -r "$ROOT/package/." "$STAGE/"
mkdir -p "$STAGE/app/bin" "$STAGE/app/www" "$STAGE/app/licenses"
cp "$REPO/fnos/backend/target/release/fusionplay-fnos" "$STAGE/app/bin/fusionplay-fnos"
cp -r "$ROOT/frontend/." "$STAGE/app/www/"
cp "$REPO/LICENSE" "$STAGE/app/licenses/LICENSE" 2>/dev/null || true
cp "$REPO/THIRD_PARTY_NOTICES.md" "$STAGE/app/licenses/THIRD_PARTY_NOTICES.md" 2>/dev/null || true
cat > "$STAGE/app/licenses/SOURCE.txt" <<TXT
FusionPlay fnOS FPK — 本包内二进制由仓库源码构建：
https://github.com/youyoudezhuzhu/FusionPlay-Android （AGPL-3.0-only）
对应源码 tag/commit 见构建产物附带的 BUILD-INFO。
TXT
printf 'version=%s\ncommit=%s\nbuilt=%s\n' \
    "$(grep -m1 '^version' "$STAGE/manifest" | cut -d= -f2 | tr -d ' ')" \
    "$(git -C "$REPO" rev-parse --short HEAD 2>/dev/null || echo unknown)" \
    "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" > "$STAGE/app/BUILD-INFO"
chmod +x "$STAGE"/cmd/* "$STAGE/app/bin/"* 2>/dev/null || true

echo "▶ fnpack build"
( cd "$STAGE" && "$FNPACK" build )
mkdir -p "$ROOT/dist"
FPK="$(ls -t "$STAGE"/*.fpk 2>/dev/null | head -1)"
[ -n "$FPK" ] || { echo "未生成 .fpk"; exit 1; }
cp -f "$FPK" "$ROOT/dist/fusionplay-fnos.fpk"
echo "✅ 产出: $ROOT/dist/fusionplay-fnos.fpk"
