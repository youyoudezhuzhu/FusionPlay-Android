# 妙播改名后出现两个实例：原因与修复

## 现象

在 Android 版（电视/平板/手机）把接收器名称改名、切换设备类型后，妙播搜索/设备中心长期显示
**两个 FusionPlay 实例**。

## 原因（两个层面）

1. **Mi-Connect 的 DNS 实例名包含接收器显示名**：
   `FusionPlay(AbCdEf1234)` → 改名后变成 `新名字(AbCdEf1234)`，在 mDNS 里这是两个不同的
   service instance。正常停止会发 TTL=0 的 goodbye，但小米电视/设备中心的 MiPlay 缓存比较激进，
   旧路由可能被保留（SDK 里甚至为此硬编码过一个 retired instance goodbye）。
2. **停止路径**：`MiPlayReceiver` 的 `Drop` 会调用 `controller.stop()` 并注销广播（已存在），
   本次继续保证 Android/fnOS 在重启前完整走 shutdown，不留下旧监听线程。

## 修复（1.2.8 / fnOS 0.2.5）

- SDK 新增 `ReceiverConfig::with_retired_receiver_names(...)`：启动时对**历史接收器名称**
  推导出的 Mi-Connect 实例补发 TTL=0 goodbye（TXT 也用旧名称，便于缓存匹配）。
- Android JNI 与 fnOS 后端在身份目录持久化 `miplay-receiver-names.json`（保留最近 8 个名称），
  每次启动把旧名称传给 SDK。
- 以后改名只会在新名字下广播，旧路由会被主动清理；不会再越积越多。

## 已经出现的两个实例怎么办

修复只能清理「以后改名」产生的旧路由；**已经缓存进电视/设备中心的旧路由**需要清一次：

1. 电视/手机重启，或在系统设置里清除「妙播/设备中心/融合设备」的缓存；
2. 确认同一局域网内没有第二个 FusionPlay 实例在跑（例如 NAS 上的 fnOS 应用也开启了妙播，
   它同样会广播；排查时先停掉 NAS 侧的妙播）；
3. 再搜索，应只剩当前名称的一个实例。

## 验证

- 改名后 `miplay-receiver-names.json` 记录新旧名称，启动日志/事件中有
  `discovery_retired_routes`（goodbye_count > 0）。
- Android 1.2.8 与 fnOS 0.2.5 均包含该修复；fnOS 侧同样持久化名称历史。
