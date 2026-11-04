# 安卓救援宝 DroidRescue

面向维修工程师和极客玩家的安卓投屏与救援工具：

- **低延迟投屏控制**：原生 scrcpy 协议客户端，硬件解码零拷贝上屏（D3D11 → D3D12 共享纹理），声音转到电脑，投屏时可熄灭设备屏幕；鼠标、滚轮、键盘、物理按键（含长按）直接操作。
- **碎屏盲操救援**：设备屏幕看不见、摸不了、USB 调试也关着时，电脑把自己模拟成 USB 键盘和触控笔（Android Open Accessory 2.0 HID），完成解锁、选 USB 模式、打开并授权 USB 调试，然后就能投屏。
- **按机型适配**：救援流程是可编辑的脚本（`rescue-profiles.txt`），按设备型号自动选择；非已知机型自动使用通用机身外形，画面按比例显示。

> English: DroidRescue is a low-latency Android mirroring client and a blind-rescue tool for devices with broken screens and USB debugging turned off (AOA HID keyboard + pen over USB, scripted per model).

## 下载

见 [Releases](https://github.com/HeiSir2014/DroidRescue/releases) 或最新一次 CI 构建的 artifact。使用方法见 [docs/使用说明.txt](docs/使用说明.txt)。

| 平台 | 包 | 说明 |
|---|---|---|
| Windows 10/11 x64 | 单文件 `DroidRescue.exe`（7z / zip） | DirectX 12；硬件解码零拷贝上屏 |
| macOS 12+ Apple Silicon | `DroidRescue.app`（dmg） | Metal + VideoToolbox 硬件解码；救援无需装驱动 |

macOS 版目前是 ad-hoc 签名：首次打开请右键 →「打开」。

## 从源码构建

```sh
pwsh scripts/fetch-vendor.ps1   # Windows：scrcpy v5.0.1 官方包（SHA256 固定）→ adb / scrcpy-server / FFmpeg
sh scripts/fetch-vendor.sh      # macOS：scrcpy v5.0.1 官方包（SHA256 固定）→ adb / scrcpy-server
cargo test
cargo build --release           # Windows: target/release/droidrescue.exe
sh scripts/package-macos.sh     # macOS: dist/DroidRescue-<版本>-macos-arm64.dmg
```

第三方运行时（adb、scrcpy-server、Windows 上的 FFmpeg）不提交到仓库，由脚本从官方 Release 获取并校验哈希，编译时嵌入程序。

平台相关代码：Windows 在 `src/`（`winusb.rs`、`usbdev.rs`、`chrome.rs`、`decoder_mf.rs`），macOS 在 `src/macos/`，接口相同；其余（协议、救援脚本、机型配置、界面）两端共用。

## 致谢

- [scrcpy](https://github.com/Genymobile/scrcpy)：设备端 scrcpy-server 与协议
- Android platform-tools（adb）、FFmpeg
- 字体：Maple Mono（等宽，内置西文子集）；中文使用系统字体
