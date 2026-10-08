# RyukinLedger

原神六种资源（原石、摩拉、创世结晶、纠缠之缘、相遇之缘、无主的星辉）的**每日收支账本**。

数据靠抓包获得：Rust 写的无界面内核 `irminsul` 解析游戏的网络流量，把余额变化追加进
`ledger.jsonl`；WPF 界面只读这个文件，负责展示、图表和托盘常驻。

界面另外显示**原粹树脂**。它和上面六种资源不是一回事：没有收支，所以不进 `ledger.jsonl`、
不参与图表和抽数换算，只出现在内核的 `status.json` 里（字段 `original_resin` 与
`original_resin_last_increase_at`），界面把它摆在抓包状态条左边。

树脂会自己恢复（每 8 分钟 +1，上限 200），而游戏不会为这个 +1 发任何包，所以卡片上的数是**推算**
出来的：内核最后知道的数值，加上从那之后落下的拍数。内核一旦看到过一次 +1，就把那次的时间写进
`original_resin_last_increase_at`；之后"落了几拍"就是两个时间点之间有几个 8 分钟边界 —— 减法，
不是估计，所以内核停了以后界面照样算得准。还没看到过 +1 时（只有登录同步值）可能少 1。

内核没在跑的时候，数字后面跟一个 `?`：没有人在确认这个数，如果这期间在游戏里花过树脂，它会偏高。

```
RyukinLedger/
├─ src/irminsul/               抓包内核（Rust，无界面，需要管理员权限）
├─ src/RyukinLedger.App/       WPF 界面（托盘常驻 + 图表）
├─ assets/                     立绘与货币图标（可选，构建时原样复制到输出目录）
└─ tools/                      构建与修复脚本
```

## 构建与运行

### 构建

一条命令把内核和界面都编译好，放进同一个目录：

```powershell
pwsh -ExecutionPolicy Bypass -File .\tools\build.ps1
```

产出：

```
dist\app\RyukinLedger.exe          界面
dist\app\RyukinLedger.dll
dist\app\RyukinLedger.deps.json
dist\app\RyukinLedger.runtimeconfig.json
dist\app\irminsul.exe              抓包内核
dist\app\assets\                   立绘与图标（有才会复制）
```

| 开关                     | 说明                                                         |
| ------------------------ | ------------------------------------------------------------ |
| `-Mode <形态>`           | 打包形态，见下表。默认 `FrameworkDependent`                  |
| `-ReadyToRun`            | 预先编译（R2R）：启动更快，文件更大。会自动带上运行时标识    |
| `-RuntimeIdentifier`     | 默认 `win-x64`；`SelfContained` / `SingleFile` / `-ReadyToRun` 需要它 |
| `-Clean`                 | 先删掉输出目录                                               |
| `-Offline`               | 给 cargo 加 `--offline`（依赖已在缓存里、没有网络时用）      |
| `-SkipCore` / `-SkipApp` | 只构建其中一边                                               |
| `-Configuration Debug`   | 构建 Debug 而不是 Release                                    |
| `-Destination <目录>`    | 换输出目录，默认 `dist\app`                                  |
| `-Quiet`                 | 不打印                                                       |

| `-Mode`                      | 说明                                                 |
| ---------------------------- | ---------------------------------------------------- |
| `FrameworkDependent`（默认） | 不打包运行时                                         |
| `SelfContained`              | 把 .NET 运行时一起放进文件夹，无运行时的机器也能运行 |
| `SingleFile`                 | 自包含单文件                                         |

### 运行

运行 `dist\app\RyukinLedger.exe`。第一次启动会弹 UAC——内核需要管理员权限才能抓包，
界面不需要，这是两个独立的进程。

关掉窗口不等于退出，只是缩进托盘；真正退出走托盘的右键菜单。

**内核要在游戏完成登录握手之前就开着**，否则会话密钥推导不出来。进了游戏才发现没开的话，
退回登录界面重新登录即可。

### 界面的命令行参数

| 参数            | 说明                                   |
| --------------- | -------------------------------------- |
| `--no-core`     | 只启动界面，不拉起抓包内核（不弹 UAC） |
| `--core <路径>` | 临时指定 `irminsul.exe` 的位置         |

### 发布

```powershell
pwsh -ExecutionPolicy Bypass -File .\tools\publish.ps1
```

三种形态各构建一次、各压成一个 zip，放进 `dist\release\`：

```
dist\release\RyukinLedger-0.1.0-win-x64-FrameworkDependent.zip
dist\release\RyukinLedger-0.1.0-win-x64-SelfContained.zip
dist\release\RyukinLedger-0.1.0-win-x64-SingleFile.zip
dist\release\SHA256SUMS.txt
```

| 开关 | 说明 |
| --- | --- |
| `-Version <号>` | 默认取 `Directory.Build.props` 里的版本 |
| `-Modes <形态...>` | 只打其中几种，默认三种全打 |
| `-NoAssets` | 不把 `assets\` 放进压缩包 |
| `-KeepFolders` | 压缩之后保留未压缩的目录 |
| `-Offline` | 透传给 `build.ps1` |

## 数据在哪

`%LOCALAPPDATA%\RyukinLedger\`

| 路径 | 谁写 | 说明 |
| --- | --- | --- |
| `data\ledger.jsonl` | 内核 | 账本，追加写，一行一条 JSON |
| `data\status.json` | 内核 | 状态、心跳与原粹树脂 |
| `data\log\` | 内核 | 每日滚动日志 |
| `control\stop.request` | 界面 | 停止请求，内容是目标会话 id |
| `settings.json` | 界面 | 界面设置 |

## 来源与许可

内核修改自 [konkers/irminsul](https://github.com/konkers/irminsul) v0.2.2（MIT，© 2025 Erik Gilling）。原始许可原文保留在 `src\irminsul\LICENSE`。
