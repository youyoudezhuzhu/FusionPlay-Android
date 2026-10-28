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

## 6.1 GitHub Actions 云端构建（真实执行）

- 工作流：`.github/workflows/build-fnos.yml`（未改动 `android.yml`）
- 触发：push tag `fnos-v0.1.0` → run
  [40621295962](https://github.com/youyoudezhuzhu/FusionPlay-Android/actions/runs/40621295962)
  **全部步骤 success**：backend 45 tests / core 82+1 tests / MiPlay 93 tests →
  release 构建 → FPK 打包 → check-fpk（86 项）→ simulate-install（63）→
  simulate-upgrade（46）→ artifact 上传 → Release 附件。
- 最终构建（tag `fnos-v0.1.0` → commit `0f4984f`）：
  [run 40622350237](https://github.com/youyoudezhuzhu/FusionPlay-Android/actions/runs/40622350237) **全步骤 success**。
- 构建产物（CI artifact 下载后本地复验）：
  - `fusionplay-fnos.fpk`，3,804,340 B，sha256 `6a721c23739a8a97a4c24d94835413cbdd7d4af8785050a7fbdeadbeb6974d8b`
  - 解包 `BUILD-INFO`：`version=0.1.0 commit=0f4984f built=2026-10-28T02:42:36Z`
  - 结构检查 `check-fpk.sh` 通过；本地 `fnos/dist/fusionplay-fnos.fpk` 已同步该产物。
- Android 回归：同一 commit 上手动触发
  [run 40622350101](https://github.com/youyoudezhuzhu/FusionPlay-Android/actions/runs/40622350101)
  **build-apk success**——`start_audio_external` 再导出问题已修（`#[cfg(not(target_os = "android"))]`），
  Android 路径与 APK 构建均未回归。
- GitHub Release：`fnos-v0.1.0` 已附带 `fusionplay-fnos.fpk`
  <https://github.com/youyoudezhuzhu/FusionPlay-Android/releases/tag/fnos-v0.1.0>
- 说明：GitHub 的 `workflow_dispatch` 仅对默认分支上的工作流显示入口；本工作流已在
  tag push 场景真实跑通，合并 `fnos/fpk` 到默认分支后即可在 Actions 页面手动触发（支持指定 ref/分支）。

## 7. 未验证项（必须真机，不得宣称已完成）

1. iPhone/iPad/macOS 真实 AirPlay 2 投放：播放/暂停/切歌/发送端音量/断线重连。
2. AirPlay 2 与 HomePod 等设备加入苹果原生多房间组（本项目未实现 PTP 多房间采样级同步，**不宣称支持**）。
3. DLNA 真实控制端（手机/第三方 App）发现与播放、Seek、RenderingControl 实际听感。
4. 小米 HyperOS 手机/平板真实妙播投放与鉴权（存在厂商准入限制，不宣称官方认证）。
5. 真实多声卡同时输出、长期漂移补偿效果、手动延迟校准听感。
6. Intel N150 长时间运行 CPU/内存实测（架构按低占用设计，但未做 24h 压测）。
7. 真实浏览器/移动端 UI 渲染（无浏览器环境，仅 jsdom）。

---

## 8. v0.2.0 验证记录（四页面 UI 重构 + 逐设备独立音量曲线）

### 8.1 后端（配置 / 迁移 / API / 音频链路）

`cargo test --manifest-path fnos/backend/Cargo.toml --all-targets`：

| target | 结果 |
|---|---|
| lib unit（配置、迁移、音量曲线、仲裁、引擎、HTTP、日志） | **32 passed / 0 failed** |
| `audio_alsa_smoke`（真实声卡，需手动 `--ignored`） | 0 passed / **1 ignored** |
| `audio_engine_mock`（双设备、漂移、热插拔、缓冲、**逐设备曲线/增益/全局/静音链路**） | **7 passed / 0 failed** |
| `dlna_decode`（流式/解码/接管） | **11 passed / 0 failed** |
| `http_calibration`（设备级曲线 API、重启持久化、v1 导入迁移、非法曲线拒绝） | **4 passed / 0 failed** |

合计 **54 passed / 0 failed / 1 ignored**。

回归：`AirPlayReceiver.Core` **82 + 1** passed（含外部 PCM 钩子测试），
`FusionPlay.MiPlaySdk` **93 passed**；Linux release 构建通过。

关键断言（真实执行）：
- 两个设备保存不同曲线互不覆盖；`GET /api/calibration` 分别返回；重启服务后仍在。
- 旧 v1 配置（全局 `audio.curve`）加载后：`config_version=2`、全局 curve 清空、
  每个无独立曲线的设备获得旧曲线；已有独立曲线的设备不被覆盖；迁移幂等。
- `POST /api/import` 导入 v1 导出同样迁移（修复了此前 import 未调用 migrate 的缺陷）。
- 引擎链路：linear 设备在 sender=100% 输出 0.5，custom(0.5) 设备输出 0.25；
  全局 50% + 设备 −6 dB 后两者约 0.125；全局静音=0；发送端静音（gain=0）=0。
  发送端音量只映射一次（曲线按块计算，块级增益斜坡）。

### 8.2 前端（四页面 / 主题 / 草稿 / 曲线编辑）

前端改为 `index.html + style.css + js/`（8 个模块，无框架/无构建/无 CDN/无外链字体/内联 SVG）。

| 检查 | 结果 |
|---|---|
| `node --check` 8 个 JS | 全部通过 |
| HTML 静态自检（id 引用/资源/图标/脚本） | 148 唯一 id、96 JS 引用 0 缺失、STATIC CHECK OK |
| 真实后端 API 集成（null 音频后端，18090） | **72 / 72** |
| jsdom DOM 集成（fnOS 网关路径 `/app/fusionplay-fnos/`） | **74 / 74**，无 window error / unhandled rejection |
| 开发根路径冒烟（`GATEWAY_BASE` 为空） | 通过 |
| 曲线逻辑（公式与后端一致、端点固定、拖拽/键盘/增删/重置） | **18 / 18** |

覆盖：四页导航、主题三态、设备选择与启停独立、设备切换、每设备独立曲线、自定义编辑、
草稿保存/撤销/跨页保留、离线设备、无媒体空态、断线恢复、设置页日志搜索、v1 导入兼容。

未验证：真实浏览器渲染/截图、360/390/768/1024 实际布局、移动端触摸拖拽、真实发送端。

### 8.3 FPK

| 检查 | 结果 |
|---|---|
| `build-fpk.sh`（manifest/crate/binary 三方版本一致） | ✅ `0.2.0`，x86_64 |
| `check-fpk.sh`（结构、cmd、privilege audio、licenses、前端资源 `www/js/main.js` 等） | ✅ 通过 |
| `simulate-install.sh` | ✅ **63 / 63** |
| `simulate-upgrade.sh`（0.2.0 → 0.2.1 临时包 + 孤儿进程） | ✅ **46 / 46** |

产物：`fnos/dist/fusionplay-fnos.fpk`，sha256 见发布记录。

### 8.4 未验证边界（v0.2.0 仍为真机项）

1. 真实浏览器截图/像素级视觉一致性、移动端布局与触摸曲线编辑。
2. 真实 ALSA 多音箱同时输出与逐设备曲线的**听感**、延迟/漂移校准效果。
3. iPhone/iPad AirPlay 2、真实 DLNA 控制端、HyperOS 小米妙播的投放与音量联动。
4. 真实 fnOS 应用中心安装/升级/卸载与 `/dev/snd` 权限。
5. 24h 长稳、真实 XRUN 注入、多声卡拔插。
