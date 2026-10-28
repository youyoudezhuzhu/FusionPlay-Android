# Android Release 签名

## 为什么

1.2.6 及以前的 CI APK 使用 Android **debug key**（每次 CI 重新生成），证书每次都不同，
因此新 APK 无法覆盖安装旧版本，只能先卸载。

从 **1.2.7** 起，所有 release APK 使用**固定的 FusionPlay release keystore**，同一签名，
后续版本可以直接覆盖安装（首次从 1.2.6 升级因签名切换需要卸载一次）。

## 密钥信息

- 别名：`fusionplay`
- 算法：RSA 4096 / PKCS12 / 有效期至 2056
- 证书 SHA-256：
  `48:36:FB:96:C2:EF:76:FD:81:AC:38:54:55:F1:B0:15:4A:F7:7F:AD:AE:1B:45:BB:B7:52:B1:92:49:9D:C1:2F`
- 本地备份（**不在 git 仓库内**）：
  `/vol1/@appdata/deepseek.harness/home/github/artifacts/keys/fusionplay-release.jks`
  以及同目录 `fusionplay-release-credentials.txt`（store/key 密码）。

> 这个 keystore 必须长期保存；丢失后无法再对已安装用户发布覆盖升级，只能更换包名或让用户卸载。
> 不要提交进仓库、不要贴到公开 issue。

## GitHub Actions 配置

仓库 Secrets（Settings → Secrets and variables → Actions）：

| Secret | 内容 |
|---|---|
| `ANDROID_KEYSTORE_BASE64` | `base64 -w0 fusionplay-release.jks` |
| `ANDROID_KEYSTORE_PASSWORD` | store 密码 |
| `ANDROID_KEY_ALIAS` | `fusionplay` |
| `ANDROID_KEY_PASSWORD` | key 密码（与 store 相同） |
| `ANDROID_CERT_SHA256` | 上面的证书 SHA-256（用于 CI 构建后校验） |

工作流 `android.yml` 会：
1. 若配置了 `ANDROID_KEYSTORE_BASE64`，解码到 runner 临时目录并设置 `ANDROID_KEYSTORE_PATH`；
2. Gradle 使用该 keystore 签 release APK；
3. 构建后用 `apksigner` 校验证书 SHA-256 与 `ANDROID_CERT_SHA256` 一致，不一致直接失败；
4. 未配置 secrets（例如普通 fork）时回退 debug 签名并给出 warning，不会静默发布错误签名。

## 本地构建

```bash
export ANDROID_KEYSTORE_PATH=/path/to/fusionplay-release.jks
export ANDROID_KEYSTORE_PASSWORD='...'
export ANDROID_KEY_ALIAS=fusionplay
export ANDROID_KEY_PASSWORD='...'
cd flutter && flutter build apk --release
```

不设置这些变量时，本地构建仍可用 debug key（**不可覆盖 release APK**）。

## 验证

```bash
apksigner verify --print-certs app-release.apk
keytool -list -v -keystore fusionplay-release.jks -alias fusionplay
```

两者证书 SHA-256 应与上表一致。
