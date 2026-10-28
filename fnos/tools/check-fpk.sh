#!/bin/bash
# FPK 交付物结构检查（CI + 本地都用）。失败即非 0 退出。
set -eu
FPK="${1:-$(cd "$(dirname "$0")/.." && pwd)/dist/fusionplay-fnos.fpk}"
[ -f "$FPK" ] || { echo "找不到 $FPK"; exit 1; }
fail=0
LIST="$(tar tzf "$FPK")"
APPLIST="$(tar xzOf "$FPK" app.tgz | tar tzf -)"
has() { [[ $'\n'"$LIST"$'\n' == *$'\n'"$1"$'\n'* ]]; }
has_app() { [[ $'\n'"$APPLIST"$'\n' == *$'\n'"$1"$'\n'* ]]; }
check() { if "$@"; then echo "  ok   $2"; else echo "  FAIL $2"; fail=1; fi; }
echo "检查 $FPK"
check has manifest "含 manifest"
check has cmd/main "含 cmd/main"
for script in install_init install_callback upgrade_init upgrade_callback uninstall_init uninstall_callback config_init config_callback; do
    check has "cmd/$script" "含 cmd/$script"
done
check has config/privilege "含 config/privilege"
check has config/resource "含 config/resource"
check has app.tgz "含 app.tgz（app 目录）"
check has ICON.PNG "含 ICON.PNG"
check has ICON_256.PNG "含 ICON_256.PNG"
check has_app bin/fusionplay-fnos "app.tgz 含 bin/fusionplay-fnos"
check has_app www/index.html "app.tgz 含 www/index.html"
check has_app ui/config "app.tgz 含 ui/config"
check has_app licenses/LICENSE "app.tgz 含 AGPL LICENSE"
# manifest 必填字段
MANIFEST="$(tar xzOf "$FPK" manifest)"
for field in appname version display_name desc source platform service_port; do
    if grep -qE "^${field}[[:space:]]*=" <<<"$MANIFEST"; then echo "  ok   manifest 含 $field"; else echo "  FAIL manifest 缺 $field"; fail=1; fi
done
# 权限位
perms="$(tar tvzf "$FPK" cmd/main | awk '{print $1}')"
[[ "$perms" == *x* ]] && echo "  ok   cmd/main 可执行（$perms）" || { echo "  FAIL cmd/main 不可执行（$perms）"; fail=1; }
# privilege 必须是合法 JSON 且带 audio 组
PRIV="$(tar xzOf "$FPK" config/privilege)"
python3 - "$PRIV" <<'PY' || fail=1
import json,sys
data=json.loads(sys.argv[1])
groups=[g.lower() for g in data.get("join-groups",[])]
if "audio" in groups: print("  ok   privilege join-groups 含 audio（/dev/snd 访问）")
else: print("  FAIL privilege 缺 join-groups audio"); sys.exit(1)
PY
if [ "$fail" -ne 0 ]; then echo "❌ FPK 检查失败"; exit 1; fi
echo "✅ FPK 检查通过"
