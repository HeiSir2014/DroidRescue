# Changelog

格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号遵循 [SemVer](https://semver.org/lang/zh-CN/)。
发布：把 `Cargo.toml` 的版本改好、在这里写好对应小节，然后推送 `vX.Y.Z` 标签；CI 构建两个平台、附上 SHA256SUMS，建立 **草稿** Release（说明取自本文件）。在真机上试过附件后，到 GitHub 上点「Publish」。

## [0.2.0] - 2026-11-04

改名为 **安卓救援宝 DroidRescue**（原 padctl），首个公开版本。

### 新增
- **macOS 版**（Apple 芯片，macOS 12+）：VideoToolbox 硬件解码后零拷贝交给 Metal 渲染（与 Windows 的 D3D 零拷贝对等）、原生标题栏；救援无需安装驱动、无需管理员权限；锁屏密码用钥匙串中的密钥加密。
- **任意电脑 / USB 口 / USB 模式都能救援**：救援用的 USB 接口写在机型配置里（`usb = VID:PID:MI`）；开始救援时自动为新接口安装 WinUSB（只请求一次管理员授权，主程序本身不提权）。
- 新电脑上没通过 adb 见过的设备，按 USB 接口自动识别机型，「一键救援」可直接用。
- **设备体检**（06 救援 → 高级）：列出 USB 接口、驱动、adb 状态，可复制报告。
- 亮色 / 暗色主题，默认跟随系统，可手动切换。
- 应用图标（exe、窗口、.app）；exe 带版本信息。
- 救援流程按机型脚本化（`rescue-profiles.txt`），内置 vivo Pad3 与通用 Android。
- 不连接设备时也能先输入锁屏密码；电源键支持长按。

### 改进
- 投屏画面按比例显示，适配任意机型（手机竖屏不再被拉伸）。
- 窗口最小宽度等于控制栏宽度（投屏时另留画面区域），不再出现内容溢出。
- 旧版 padctl 的密码、机型配置、窗口位置自动迁移。

### 修复
- 切换 USB 模式后误连仍残留的旧设备（「AOA 请求 57 失败」）。
- 第二台电脑上 adb 模式接口没有驱动导致救援中断。

## [0.1.1] - 2026-11-04

- vivo Pad3 一键救援：解锁 → 连接办公套件 → 允许 USB 调试，全程盲操作；文案更正（电脑无需安装 vivo 软件）。

## [0.1.0] - 2026-11-03

- 首个内部版本（padctl）：scrcpy 协议低延迟投屏（零拷贝）、声音转发、熄屏投屏、AOA 键盘/触控笔救援。

[0.2.0]: https://github.com/HeiSir2014/DroidRescue/releases/tag/v0.2.0
