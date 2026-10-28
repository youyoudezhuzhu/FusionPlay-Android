# AirPlay 音响身份（audio-only）说明

## 问题

`shairplay` 以 `hls` feature 构建时（Android 与 fnOS 都启用），mDNS 的
`features` 会额外带上：

- bit 0 — AirPlay video v1
- bit 4 — HTTP Live Streaming

同时 `model` 固定为 `AppleTV6,2`。iPhone/iPad 因此把接收器归类为
**Apple TV / 视频目标**，在控制中心/播放器里会提供“视频投放 / 镜像”，
但 FusionPlay 实际只输出音频。

## 修复

`vendor/shairplay` 新增 `RaopServerBuilder::audio_only(bool)`：

- `model` / `am` / `_device-info` 改为 `AirPort10,115`（第二代 AirPort Express，
  纯音频 AirPlay 2 接收器）；
- `_airplay._tcp` 与 `_raop._tcp` 的 `features`/`ft` 清除 bit 0（video v1）与
  bit 4（HLS），保留音频、AP2 buffered audio、PTP 等位；
- audio-only 时 `_device-info._tcp` 不再发布 `osxvers`（避免被识别为电脑）；
- AP1 回退 profile 的 features 从 `0x7`（video/photo）改为 `0x200`（audio）；
- 未开启 `audio_only` 的调用方（桌面 headless 默认）行为完全不变。

启用位置：

- **fnOS**：`fnos/backend/src/protocols.rs` 的 `Settings { airplay_audio_only: true }`。
- **Android**：`src/AirPlayReceiver.Core/src/android_jni.rs` 的 `Settings` 同样设为
  `true`（FusionPlay 是音频接收器，不再向 iPhone 提供视频/镜像目标）。

## 验证

- shairplay 单测：audio-only profile 的 model/features/device-info 断言 + 原有
  171 个测试全部通过（`--features hls`）。
- 真机 mDNS（`env -u CI` 启动 fnOS 服务后 `avahi-browse`）：

```
_airplay._tcp: model=AirPort10,115
              features=0x405D4A00,0x1C340     # bit0/bit4 已清除
              protovers=1.1 vv=2 pk=… pi=…
_raop._tcp:    am=AirPort10,115 ft=0x405D4A00,0x1C340
```

- AirPlay 音频（AP1/AP2 buffered audio）接收路径未改动；HLS/视频接收代码仍编译，
  只是不再对外广播，iPhone 不会再把它当视频目标。

## 回退

把 `audio_only(false)`（shairplay builder）或对应 `Settings.airplay_audio_only = false`
即可恢复 Apple TV 视频能力广播；Android 的 AirPlay 视频/HLS 功能本身未被删除。
