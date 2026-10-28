# FusionPlay fnOS 版：构建与测试验证记录

> 本文件只记录**实际执行过**的验证及结果；未执行的（真机 iPhone/DLNA 控制端/HyperOS 妙播/多声卡听感）
> 单独列在"未验证项"，绝不以推断代替结论。
> 环境：fnOS NAS（Intel N150，x86_64 Linux），Rust stable，ALSA 1.2.8，无 GUI 浏览器。

## 1. 编译与单元/集成测试

| 项目 | 命令 | 结果 |
|---|---|---|
| 协议核心 Linux 编译 | `cargo check --manifest-path src/AirPlayReceiver.Core/Cargo.toml --all-targets` | ✅ 通过（cpal 0.17 → ALSA） |
| MiPlay SDK Linux 编译 | `cargo check --manifest-path src/FusionPlay.MiPlaySdk/Cargo.toml --all-targets` | ✅ 通过 |
| 核心外部 PCM 钩子 | `cargo test --manifest-path src/AirPlayReceiver.Core/Cargo.toml --test external_output` | ✅ 1 passed：960 帧 f32 到 `PcmOutput`，sender_gain −6.02 dB→0.5、48k/2ch、drop 时 flush |
| 后端全量测试 | `cargo test --manifest-path fnos/backend/Cargo.toml --all-targets` | ✅ 45 passed：lib 28（配置/音量曲线/仲裁/引擎/HTTP/日志轮转）+ audio_engine_mock 6（双设备不同速率、漂移、缓冲上界、flush）+ dlna_decode 11（WAV/chunked/404/超时/接管/stop/file） |
| 协议核心回归 | `cargo test --manifest-path src/AirPlayReceiver.Core/Cargo.toml --lib` | ✅ 82 passed |
| MiPlay SDK 回归 | `cargo test --manifest-path src/FusionPlay.MiPlaySdk/Cargo.toml --lib` | ✅ 93 passed |
| 后端 release | `cargo build --release --manifest-path fnos/backend/Cargo.toml` | ✅ 通过 |

## 2. 真实硬件枚举（ALSA）

`fusionplay-fnos --probe` 在 NAS 上实测输出（节选）：

```
hw:CARD=PCH,DEV=0    | ALC269VC Analog (HDA-Intel - HDA Intel PCH) | rates=[44100, 48000, 96000, 192000] ch=2 online=true
hw:CARD=PCH,DEV=3    | HDMI 0                                        | rates=[44100, 48000, 96000, 192000] ch=2 online=true
hw:CARD=PCH,DEV=7/8/9| HDMI 1/2/3                                    | rates=[44100, 48000, 96000, 192000] ch=2 online=true
hw:CARD=ultra,DEV=0  | USB Audio（极境-ultra）                       | rates=[48000] ch=2 online=true
hw:CARD=Loopback,DEV=0/1 | Loopback PCM (软件环回)                   | rates=[44100] ch=2 online=true
```

- 稳定 ID 形如 `hw:CARD=<cardid>,DEV=<n>`，不依赖显示名称。
- 采样率/声道是通过实际 `hw_params` 协商回读，不是静态表。

## 3. AirPlay 2 广播（真实局域网，无 iPhone）

用 `env -u CI` 启动接收器后 `avahi-browse` 实测：

```
= enp3s0-ovs IPv4 FusionPlayTest _airplay._tcp local
   port = [5000]
   txt = ["pi=…" "deviceid=02:82:4d:ac:ee:e5" "model=AppleTV6,2"
          "features=0x405D4A11,0x1C340" "vv=2" "srcvers=366.0" "protovers=1.1"
          "pk=…" "osvers=15.6" "flags=0x4"]
= enp3s0-ovs IPv4 02824DACEEE5@FusionPlayTest _raop._tcp local
   txt = ["et=0,3,5" "tp=TCP,UDP" "md=0,1,2" "ft=0x405D4A11,0x1C340" "pw=false" "vs=366.0" "vn=65537"]
= … _device-info._tcp registered
```

- 说明：AirPlay 2 发现记录已真实出现在局域网，iPhone/iPad/macOS 的 AirPlay 菜单应可见；
  **实际投放、暂停/切歌、发送端音量仍需真机验证**（见 §7）。
- ⚠️ 坑：`CI` 环境变量存在时 shairplay 会跳过 mDNS 注册 → 接收器不可被发现。
  `cmd/main` 启动前必须 `unset CI`（已验证 `env -u CI` 后立即出现）。

## 4. DLNA 端到端（真实 DMR + 真实 HTTP 媒体流）

在本机启动 `fusionplay-fnos`（null 音频后端，避免噪声），用 SOAP 模拟控制端：

1. `GET http://<nas>:<dlna_port>/description.xml` → `MediaRenderer:1`，`friendlyName=FusionPlay`。
2. `SetAVTransportURI`（3 秒 44.1 kHz/2ch PCM WAV，经本机 HTTP 提供）→ HTTP 200。
3. `Play` → HTTP 200。
4. 观察 `/api/status`：

```
dlna.detail = 播放完成：media.wav · 44.1 kHz → 48 kHz · 已解码 132300 帧（推送 143929 帧） · 529244 字节
audio.master_frames = 143929
errors = []
```

- 结论：DLNA 控制面（SSDP/AVTransport）+ 媒体 HTTP 拉流 + symphonia 解码 + 重采样到 48k stereo + 统一引擎入口全链路真实跑通。
- 另经 SSDP `M-SEARCH` 验证：`ST: urn:schemas-upnp-org:device:MediaRenderer:1` 有响应，
  `LOCATION: http://192.168.31.50:<port>/description.xml`。
- 限制：HTTPS/HLS/视频/206 Range-Seek 不支持（明确报错，见 §7）。

## 5. 管理 API / 网关

- `GET /healthz` → `{"status":"ok","version":"0.1.0","uptime":N}`。
- `GET /api/status` 显示三协议状态：`airplay ready(port 5000)`、`dlna listening(随机端口)`、`miplay running`。
- Unix socket 与 TCP 同时可用（fnOS 网关用 unix socket）。
- 网关前缀：`GET /app/fusionplay-fnos` → `307 + Location: /app/fusionplay-fnos/`；
  带斜杠后的静态资源/API 正常（web-ui 242 项断言覆盖）。
- 前端：`node --check` 通过；jsdom 集成/API 契约断言通过（详见 web-ui 报告）。

## 6. FPK 打包

| 检查 | 结果 |
|---|---|
| `./fnos/tools/build-fpk.sh` | ✅ 生成 `fnos/dist/fusionplay-fnos.fpk` |
| `./fnos/tools/check-fpk.sh` | ✅ manifest / 9 个 cmd / config/privilege+resource / ICON* / app.tgz(bin+www+ui+licenses) / 可执行位 / privilege join-groups=audio 全通过 |
| manifest 版本 == 二进制 `--version` == `/healthz` | ✅ 0.1.0（构建期守卫） |
| `./fnos/tools/simulate-install.sh` | ✅ 63/63 断言：安装→启动→healthz→config_callback 重启→stop/status=3→PID/socket/端口清理→卸载保留数据→非法参数 |
| `./fnos/tools/simulate-upgrade.sh` | ✅ 46/46 断言：正常升级（upgrade_init 停旧+清端口，upgrade_callback 起新，/healthz 报新版本 0.1.1，/proc/pid/exe 哈希=新二进制，配置/HOME 保留，仅 1 进程）＋ 无 PID 文件孤儿进程升级清理 |
| FPK 体积 | 3.7 MB（含 x86_64 单二进制 + 静态 UI + 许可证） |

## 7. 未验证项（必须真机，不得宣称已完成）

1. iPhone/iPad/macOS 真实 AirPlay 2 投放：播放/暂停/切歌/发送端音量/断线重连。
2. AirPlay 2 与 HomePod 等设备加入苹果原生多房间组（本项目未实现 PTP 多房间采样级同步，**不宣称支持**）。
3. DLNA 真实控制端（手机/第三方 App）发现与播放、Seek、RenderingControl 实际听感。
4. 小米 HyperOS 手机/平板真实妙播投放与鉴权（存在厂商准入限制，不宣称官方认证）。
5. 真实多声卡同时输出、长期漂移补偿效果、手动延迟校准听感。
6. Intel N150 长时间运行 CPU/内存实测（架构按低占用设计，但未做 24h 压测）。
7. 真实浏览器/移动端 UI 渲染（无浏览器环境，仅 jsdom）。
