<!-- markdownlint-disable MD033 -->
# Mclick · 魔精灵点击器 / Magic Sprite Clicker

![Rust](https://img.shields.io/badge/Rust-edition%202021-orange)
![Platform](https://img.shields.io/badge/Platform-Windows%2010%20%2F%2011-blue)
![Dependencies](https://img.shields.io/badge/runtime%20deps-0-brightgreen)
![License](https://img.shields.io/badge/License-MIT-green)

Single-file Windows auto-clicker that locates the **Magic Sprite** card in a 3-slot hand and clicks it — no OpenCV, no DLL, no model files, no runtime.

单文件 Windows 自动点击器：在三格手牌中定位 **魔精灵** 并点击 —— 不依赖 OpenCV / DLL / 模型 / 运行库。

**[English](#english) · [简体中文](#简体中文)**

---

## English

### Overview

Mclick is a self-contained Windows auto-clicker for a tower-defense mini-game. It captures the game window, finds the target card among the three hand slots, clicks it, and clicks the **Refresh** button when the target is not present.

Everything is compiled into a single executable:

- **48 unique card templates** (deduplicated from 53 frames) live in the read-only data section (`.rdata`)
- **The app icon** is linked into the PE resource section at build time
- **The image matcher** is hand-written multi-scale NCC — no external CV library
- `[dependencies]` in `Cargo.toml` is **empty**

Copy `Mclick.exe` to any Windows 10/11 machine, double-click, done.

| Item | Value |
|---|---|
| Binary | `Mclick.exe` (single file, ~785 KB) |
| Version | v1.2.0 |
| Default tick | **5 s** (restarts from the moment the card was clicked) |
| Window title | 魔精灵点击器 |
| Templates | 48 unique (`assets/templates.bin`, 258 KB) |
| Runtime dependencies | none |

### Features

- **Single file, zero install** — no VC++ redistributable, no DLL sidecar, no model or image folder.
- **Two front-ends** — native Win32 GUI for daily use, full CLI for scripting and debugging.
- **Background capture & click** — five capture paths (`auto` / `d3d` / `dc` / `pw` / `scr`) and `PostMessage`-based clicking, so the target window does not need focus.
- **Three-slot hand recognition** — slot regions are expressed as relative ratios, so they survive window resizing.
- **Common-background guard** — two independent criteria stop the anti-false-positive problem of clicking an empty slot whose shared background resembles a card. Validated offline on a 37-frame gray-band sample set: **0 false positives**.
- **"Out of gold" detection** — stops refreshing when the coin area turns red, avoiding draining the in-game currency.
- **Window size lock** — optional `--size WxH` to force the game client area so that templates stay aligned across machines.
- **Self-test** — synthetic-image test that validates the matcher without a game running and without moving the mouse (currently 6/6 PASS).
- **Auto elevation** — re-launches with UAC prompt on demand, with a recursion guard.

### Quick start

```bat
:: GUI: just double-click Mclick.exe
Mclick.exe

:: Recognition mode (CLI): find the Magic Sprite and click it
Mclick.exe --window "<窗口标题关键字>" --interval 5 --times 0

:: Fixed coordinate mode, no recognition
Mclick.exe --coords 960,540 --times 60

:: Validate the matcher only (no screenshot, no mouse movement)
Mclick.exe --self-test

:: Dump screenshots and relax the threshold for diagnosis
Mclick.exe --window "<窗口标题关键字>" --dump --threshold 0.7 --times 1
```

GUI workflow: type a substring of the game window title (default = built-in game-window auto-detection, empty = foreground window) → set the interval (default 5 s) → press **开始 / Start**. The program keeps clicking the target card when it is present, and presses **刷新 / Refresh** when it is not.

Run `Mclick.exe --help` for the complete list of options.

> **The program really moves the mouse and clicks.** Confirm the target window and use a small `--times` value for the first run.

### CLI reference

#### Recognition

| Option | Description | Default |
|---|---|---|
| `--window <title>` | Target window title substring; empty = foreground window | foreground |
| `--threshold <0~1>` | Similarity threshold, higher is stricter | `0.78` |
| `--min-score <0~1>` | Lower bound of the hit band (overrides `--threshold`) | — |
| `--max-score <0~1>` | Upper bound of the hit band; scores above it count as a miss | `1.0` |
| `--sample-only` | Sample only: on a gray-band hit, save the frame and refresh | off |
| `--skip-tpl <a,b,..>` | Exclude template indices (0-based); `none` = exclude nothing | `3,9,11,12` |
| `--no-bg-guard` | Disable the common-background guard | off |
| `--no-recheck` | Disable the pre-click re-check | on |
| `--no-borderline-grace` | Disable the borderline grace rounds | on |
| `--no-refresh-verify` | Disable the post-refresh pixel verification | on |
| `--bg-spread <f>` | Criterion 1 — max-min spread of the same template across slots | `0.25` |
| `--bg-max <f>` | Score ceiling above which the background guard does not apply | `0.93` |
| `--bg-gap <f>` | Criterion 2 — minimum gap between best and runner-up score | `0.20` |
| `--scales <a,b,..>` | Template scale factors, to absorb resolution differences | `1.0,0.8,1.25` |
| `--roi x,y,w,h` | Search only inside this client-area region (speed-up) | whole window |
| `--cells x,y,w,h;x,y,w,h` | Hand-slot regions as relative ratios | built-in three slots |
| `--refresh x,y` | Relative position of the **Refresh** button | built-in |
| `--no-cells` | Disable the 3-slot mode, fall back to whole-window pyramid search | off |
| `--money-roi x,y,w,h` | "Out of gold" detection region (relative ratios) | built-in |
| `--money-red-pct <n>` | Red-pixel ratio (%) that counts as out-of-gold (3 consecutive confirmations required) | `2.0` |
| `--money-wait-ms <n>` | Re-check interval while out of gold | `400` |
| `--no-money-check` | Disable the out-of-gold check | off |
| `--probe-money [N]` | Print the red ratio of the coin area N times, then exit | `10` |
| `--image <bmp>` | Offline test on a BMP frame instead of a live screen capture | — |
| `--dry-run` | Print what would be clicked without touching the mouse | off |

#### Background mode

| Option | Description | Default |
|---|---|---|
| `--background` | Background capture + background click (already the default) | on |
| `--bg-click` | Click via `PostMessage` only; the cursor never moves | on (background) |
| `--fg-click` | Switch to foreground physical clicking (`SetCursorPos` + `mouse_event`) | off |
| `--capture <m>` | Capture path: `auto` / `d3d` / `dc` / `pw` / `scr` | `auto` |
| `--size WxH` | Force the game **client area** to this size (e.g. `1040x629`) | not forced |
| `--size-test` | Actually resize the target window once, print before/after, exit | — |
| `--interval-ms <n>` | Millisecond tick between two successful clicks | `5000` |
| `--miss-interval-ms <n>` | Wait for the hand to repaint after a refresh | `200` |
| `--refresh-gap-ms <n>` | Minimum gap between two refreshes (too dense burns gold) | `1500` |
| `--find-wait-ms <n>` | Longest wait for a match after a refresh | `550` |
| `--scan-refresh` | Scan the bottom button bar, locate **Refresh**, print and exit | — |
| `--probe` | Print features of both capture paths and exit | — |
| `--probe-refresh [N]` | Measure refresh-to-repaint latency over N rounds | `8` |
| `--step <n>` | Coarse-search stride on the 1/8 pyramid level | `2` |
| `--topk <n>` | Candidates forwarded to the fine search | `6` |
| `--threads <n>` | Matching threads | auto |

#### Misc

| Option | Description | Default |
|---|---|---|
| `--coords x,y` | Fixed-position mode; no recognition | — |
| `--dll <path>` | Use a third-party recognition DLL (legacy) | — |
| `--model <path>` | Model path for the DLL mode (legacy) | — |
| `--interval <s>` | Seconds between rounds in the legacy loop | `5` |
| `--times <N>` | Number of clicks, `0` = unlimited | `0` |
| `--front` | Bring the window to the foreground before each click | off |
| `--dump` | Save screenshots to `%TEMP%\mojingling_dump` | off |
| `--dump-dir <dir>` | Save screenshots to a given directory (UNC supported) | — |
| `-g`, `--verbose` | Verbose output: coordinates, scores, timings | off |
| `--self-test` | Synthetic-image self-test of the matching engine | — |
| `--help` | Show help | — |

### How it works

#### Single-file packaging

| Content | Technique | Size |
|---|---|---|
| Card templates | `assets/templates.bin` → `include_bytes!` → `.rdata` | 258 KB |
| App icon | `assets/app.ico` → `build.rs` → `rc.exe` → PE resource section | 14 KB |
| In-window stamp | `assets/stamp_70.bgra` → `include_bytes!` → `CreateDIBSection` → `STM_SETIMAGE` | 19 KB |
| Screen capture | gdi32 `PrintWindow` / `BitBlt` + `GetDIBits` | — |
| Clicking | user32 `PostMessage` (background) / `SetCursorPos` + `mouse_event` (foreground) | — |
| Image matching | hand-written multi-scale NCC, hand-rolled pyramid downsampling | ~30 KB |

`embed-resource` is a **build-time** dependency only; it never enters the final binary.

#### Recognition pipeline

Three-stage search. NCC (normalised cross-correlation) is insensitive to global brightness and contrast changes, which matters because the card art sits on a changing background.

1. **1/8-scale coarse scan** (stride 2) — every template × every scale takes its best position.
2. **1/4-scale re-rank + fine search** (±5 cells) — the 1/8 level is too weak to discriminate, so candidates must be re-ranked before top-k is taken, otherwise the correct match gets pushed out.
3. **Full-resolution refinement** (±6 px) — pixel-accurate coordinates.

The 48 templates are the result of clustering 53 raw frames by downscaled NCC ≥ 0.93, so different animation frames of the same card collapse into one template — a hit on any frame counts as a hit.

A single round on a 1600×900 capture takes roughly 13 ms on the reference machine.

#### Three-slot hand and the common-background guard

The three hand slots are located by relative ratios, so a window resize does not require re-tuning. Each slot is searched independently; the best-scoring slot is clicked.

The hard case is the **transition / empty-slot frame**: no Magic Sprite is present, yet the background shared by all three slots gives some templates a score of 0.75–0.93 — high enough to pass a plain threshold. Two criteria run against it; either one flags "common background":

| Criterion | Condition | Default |
|---|---|---|
| 1 — same template, flat scores | best match in all three slots is the same template, spread < `--bg-spread`, and best score < `--bg-max` | 0.25 / 0.93 |
| 2 — scores indistinguishable | gap between best and runner-up < `--bg-gap`, and best score ≤ `--bg-max` | 0.20 / 0.93 |

Criterion 2 is not optional. After excluding templates `#03 / #09 / #11 / #12` (Magic-Sprite frames that do not match the in-game skin), the background is picked up by a *mixture* of `#07 / #08 / #29 / #38` instead of a single template — so criterion 1 alone would miss it.

Offline replay over a 37-frame gray-band sample set:

| Configuration | Hits | Judged background | False positives |
|---|---|---|---|
| Exclude 11/12, criterion 1 only | 34 | 3 | **31 frames mis-clicked** |
| Exclude 11/12, criteria 1 + 2 | 8 | 29 | **0** |

All 8 hits are real-card shapes ("one slot 0.854–0.950, the other two 0.376–0.610"). The 29 background frames have a best/runner-up gap of 0.004–0.099, while the 8 real cards have 0.29–0.53 — the default 0.20 sits in the middle of the margin on both sides.

Related options: `--no-bg-guard` (disable entirely), `--bg-spread`, `--bg-gap`, `--bg-max`, `--skip-tpl` (default `3,9,11,12`, `none` = keep every template).

**Residual risk**: if a real card scores in the 0.78–0.93 gray band while the other two slots also exceed 0.75, criterion 2 may reject it. Tighten with `--bg-gap 0.12`, or disable the guard and control everything with the threshold.

#### Miss protection: re-check before the click, grace when borderline

Both are on by default. They target the two ways a *real* card gets skipped, which shows up as "often misses" in play:

| Guard | What it does | Why |
|---|---|---|
| **Pre-click re-check** (`--no-recheck` to disable) | Right before pressing, grab one more frame and re-locate the card. If it is gone or its score dropped below `--threshold`, skip the click and refresh instead. If it moved to another slot, click the new position. | The program locates the card, then holds until the beat. During that hold the game may already have played it, or its entry animation may have moved it. Clicking the old coordinates makes the log say "clicked" while the screen does not change. |
| **Borderline grace** (`--no-borderline-grace` to disable) | When the best score sits just under the threshold (`threshold - 0.08`) and the search timeout is up, wait up to 2 more rounds of 120 ms instead of refreshing immediately. | A card in its fading-in animation scores 0.70–0.77 for a moment. Refreshing at that instant throws away a real card — and costs coins. |

#### Refresh verification: did the refresh actually work?

`--no-refresh-verify` disables it. After every refresh the program compares the three card slots with the frame taken before the click. If the pixel difference is below `0.5 %`, the cards did not change, which means either the refresh button coordinates are off for this window size or the background click was ignored. The log then says:

```
[refresh] 刷新后牌面几乎没变（差异 0.12% < 0.50%）→ 这次刷新可能没生效
```

This is the "burns coins without ever changing the cards" symptom. Re-calibrate with `--scan-refresh`, or fall back to `--fg-click` if background messages are being ignored.

#### Capture paths

| Mode | Mechanism | When to use |
|---|---|---|
| `auto` | Probe and pick the best available path | default |
| `d3d` | `PrintWindow` on the D3D child window | mini-program / WeChat windows, pure background |
| `dc` | `GetWindowDC` + `BitBlt` | classic GDI windows |
| `pw` | `PrintWindow` | windows that honour the WM_PRINT contract |
| `scr` | Screen copy | last resort; the window must not be occluded |

#### Out-of-gold detection

The coin area is sampled for red pixels. When the red ratio exceeds `--money-red-pct` for three consecutive checks, the tool treats it as "out of gold" and pauses refreshing, re-checking every `--money-wait-ms`. Calibrate the threshold on your machine with `--probe-money`.

### Build from source

```bat
cargo build --release --offline
```

- Requires a Rust toolchain (edition 2021). No crates.io runtime dependencies, so `--offline` works as long as the cargo cache already holds `embed-resource`.
- `build.rs` locates `rc.exe` in this order: the `RC_EXE` environment variable → `PATH` → the highest version under `Windows Kits\10\bin`. If it is missing, the build only warns; the executable still works, without a custom icon.
- Output: `target\release\Mclick.exe`.

**Template format** — `[u32 count] + count × (u16 w, u16 h, w×h grayscale)`. To swap in a different card, repack your images into `assets/templates.bin` in this format; no code change is needed. The self-test picks the first / middle / last template dynamically, so it adapts to any library length.

### Environment variables

| Variable | Effect |
|---|---|
| `MCLICK_NO_ELEVATE=1` | Skip the UAC self-elevation (useful for self-test and remote debugging) |
| `MCLICK_ELEVATE_DRYRUN=1` | Print the elevation command line without launching it |
| `MCLICK_ELEVATED=1` | Internal marker set on the elevated child process (prevents an elevation loop) |
| `MOJINGLING_VERBOSE=1` | Verbose logging, same as `-g` |
| `MOJINGLING_COLORTEST=1` | Window colour self-check |

Log file: `Mclick_run.log` next to the executable.

### Project layout

```
Cargo.toml        crate `mojingling_clicker`, binary `Mclick`
build.rs          icon embedding (auto-discovers rc.exe)
assets/
  templates.bin   48 Magic-Sprite templates, clustered from 53 frames (258 KB)
  app.ico         horse-head icon
  stamp_70.bgra   70×70 BGRA in-window stamp
src/
  main.rs         CLI parsing, main loop, self-test
  gui.rs          native Win32 window
  matcher.rs      multi-scale NCC (1/8 coarse → 1/4 re-rank → full-res refine)
  capture.rs      capture paths (auto/d3d/dc/pw/scr)
  click.rs        clicking
  sizelock.rs     window client-area size lock
  elevate.rs      self-elevation
  logger.rs       logging
  ffi.rs, winapi.rs, assets.rs   FFI and embedded-asset glue
```

### Troubleshooting

| Symptom | Fix |
|---|---|
| Nothing is clicked, log shows low scores | Lower `--threshold` (e.g. `0.7`), or check `--dump` frames to confirm the capture path works |
| Captured frame is black | Try `--capture d3d`, then `--capture pw`, then `--capture scr`; run `--probe` to compare paths |
| Background clicks have no effect | Fall back to `--fg-click` to confirm the coordinates are right, then find a working `--capture` mode |
| False clicks on empty slots | Tighten the guard: `--bg-gap 0.25`, `--bg-max 0.88` |
| Real cards rejected as background | Loosen to `--bg-gap 0.12`, or use `--no-bg-guard` with a higher `--threshold` |
| Refreshing drains gold too fast | Raise `--refresh-gap-ms`; verify `--probe-money` threshold |
| Templates do not align on another machine | Pin the client area with `--size 1040x629` (test with `--size-test` first) |
| Elevation prompt keeps appearing | Set `MCLICK_NO_ELEVATE=1` while debugging |

### Known limitations

- Templates were captured at one fixed resolution. Large window-size differences need `--scales`.
- Matching is grayscale; heavy in-game effects lower the similarity. Lower `--threshold` in that case.
- `--dll` / `--model` are kept only for backward compatibility with the legacy third-party DLL mode; they are unnecessary in single-file mode.
- Windows-only (Win32 / GDI). No Linux or macOS support.

### Compared with Bclick (Ice Sprite build)

| Item | Mclick | Bclick |
|---|---|---|
| Target | Magic Sprite (48 templates) | Ice Sprite (23 templates) |
| Template size | 258 KB | 124 KB |
| Default tick | 5 s | 3 s |
| Excluded templates | `3, 9, 11, 12` | none |
| Icon | black horse head silhouette | blue-purple lotus |
| Env prefix | `MCLICK_*` | `BCLICK_*` |
| Log / dump | `Mclick_run.log` / `mojingling_dump` | `Bclick_run.log` / `bclick_dump` |

The engine, capture paths, elevation, size lock and 3-slot recognition are identical between the two builds.

### Disclaimer

This project is released **for educational and research purposes only**. It exists as a technical demonstration of Win32/GDI programming, screenshot-based local image recognition and single-binary resource embedding. It is not intended for, and must not be used for, commercial purposes, monetisation, boosting or account-trading services, or any activity that disrupts the fairness or normal operation of a game or online service.

1. **No affiliation.** This is an unofficial, independent project. It is not affiliated with, endorsed by, sponsored by or approved by the developer, publisher or operator of any game or platform referenced in this repository. All game names, artwork, trademarks and related intellectual property remain the property of their respective owners.
2. **Third-party terms.** Automating a game client may violate that game's Terms of Service or EULA and may lead to warnings, suspensions or permanent account bans. You are solely responsible for reviewing and complying with all applicable terms of service, platform rules and local laws before running this software.
3. **Assumption of risk.** You use this software entirely at your own risk. The author accepts no liability for account penalties, loss of in-game currency, items or progress, data loss, hardware or software damage, or any direct or indirect damages arising from the use or misuse of this project.
4. **No warranty.** The software is provided "as is", without warranty of any kind, express or implied, including but not limited to the warranties of merchantability, fitness for a particular purpose and non-infringement.
5. **Redistribution.** Reuploads, forks and derivative works are welcome under the terms of the MIT License, provided this disclaimer is retained and the project is not presented as an endorsement of the prohibited use cases above.
6. **Takedown.** If any rights holder believes that content in this repository infringes their rights, please open an issue or contact the author. Verified requests will be handled promptly, including removal of the relevant content.

### License

MIT — Copyright © 2026 capfmud — see [LICENSE](LICENSE).

---

## 简体中文

### 项目简介

Mclick 是一个自包含的 Windows 自动点击器，用于某小程序游戏场景：截取游戏窗口，在三格手牌中定位目标卡牌，点中它；三格里都没有目标时就点右侧 **刷新**。

所有资源都编进同一个可执行文件：

- **48 张去重后的卡牌模板**（由 53 帧聚类而来）编进只读数据段（`.rdata`）
- **应用图标**在构建期链接进 PE 资源节
- **图像匹配**是自写的多尺度 NCC，不依赖任何 CV 库
- `Cargo.toml` 的 `[dependencies]` 是**空的**

把 `Mclick.exe` 拷到任意 Windows 10/11 机器双击即可运行。

| 项目 | 值 |
|---|---|
| 产物 | `Mclick.exe`（单文件，约 785 KB） |
| 版本 | v1.3.1 |
| 默认节拍 | **5 秒**（从「点中卡牌那一刻」重新起算） |
| 窗口标题 | 魔精灵点击器 |
| 模板数 | 48 张（`assets/templates.bin`，258 KB） |
| 运行时依赖 | 无 |

### 功能特性

- **单文件、免安装** —— 不装 VC++ 运行库、不放 DLL、不放模型或图片目录。
- **双前端** —— 日常用原生 Win32 小窗体，脚本与排查用完整命令行。
- **后台抓图 + 后台点击** —— 五种抓图通路（`auto` / `d3d` / `dc` / `pw` / `scr`），点击走 `PostMessage`，目标窗口不需要获得焦点。
- **三格手牌识别** —— 格子位置用相对比例表达，换窗口尺寸不用改参数。
- **共同底纹防线** —— 两条独立判据，专门对付「空槽底纹像卡牌、直接按阈值判就会误点」的问题。37 帧灰区样本离线回放：**0 误报**。
- **「没钱」检测** —— 金币区域变红就暂停刷新，避免把游戏内货币刷干。
- **窗口尺寸锁定** —— 可选 `--size WxH` 强制游戏客户区尺寸，让模板在不同机器上对齐。
- **自检** —— 合成图像验证匹配引擎，不需要开游戏、不动鼠标（当前 6/6 PASS）。
- **自动提权** —— 需要时按需弹 UAC 重启自身，并带去重标记避免递归提权。

### 快速开始

```bat
:: 图形界面：直接双击 Mclick.exe
Mclick.exe

:: 识别模式（命令行）：找魔精灵并点击
Mclick.exe --window "<窗口标题关键字>" --interval 5 --times 0

:: 固定坐标模式，不做识别
Mclick.exe --coords 960,540 --times 60

:: 只验证匹配引擎（不截屏、不动鼠标）
Mclick.exe --self-test

:: 识别不准时：存截图 + 降阈值排查
Mclick.exe --window "<窗口标题关键字>" --dump --threshold 0.7 --times 1
```

图形界面流程：填游戏窗口标题关键字（默认自动识别游戏本体窗口，留空则取前台窗口）→ 设置间隔（默认 5 秒）→ 点 **开始**。程序会循环识别，找到目标就点，没有就点右侧 **刷新**。

`Mclick.exe --help` 可查看全部参数。

> **程序会真实移动鼠标并点击。** 运行前先确认目标窗口，`--times` 先设小一点试跑。

### 命令行参数

#### 识别相关

| 参数 | 说明 | 默认 |
|---|---|---|
| `--window <标题>` | 目标窗口标题子串；留空 = 当前前台窗口 | 前台窗口 |
| `--threshold <0~1>` | 相似度阈值，越高越严格 | `0.78` |
| `--min-score <0~1>` | 命中区间下限（覆盖 `--threshold`） | — |
| `--max-score <0~1>` | 命中区间上限，超过它的分数不算命中 | `1.0` |
| `--sample-only` | 只采样不点牌：灰区命中就存图再刷新 | 关 |
| `--skip-tpl <a,b,..>` | 剔除指定模板编号（0-based）；`none` = 不剔除 | `3,9,11,12` |
| `--no-bg-guard` | 关闭共同底纹防线 | 关 |
| `--no-recheck` | 关闭「到点点击前复检」 | 开 |
| `--no-borderline-grace` | 关闭「贴阈值宽限」 | 开 |
| `--no-refresh-verify` | 关闭「刷新后牌面像素核对」 | 开 |
| `--bg-spread <f>` | 判据一：同一模板三格分数的极差阈值 | `0.25` |
| `--bg-max <f>` | 分数上限，超过它不再判底纹 | `0.93` |
| `--bg-gap <f>` | 判据二：最高分与次高分的最小落差 | `0.20` |
| `--scales <a,b,..>` | 模板缩放比例，覆盖不同分辨率 | `1.0,0.8,1.25` |
| `--roi x,y,w,h` | 只在客户区指定区域搜索（加速） | 全窗 |
| `--cells x,y,w,h;x,y,w,h` | 三格手牌区域（相对比例） | 内置三格 |
| `--refresh x,y` | **刷新** 按钮的相对位置 | 内置 |
| `--no-cells` | 关闭三格模式，退回整窗金字塔搜索 | 关 |
| `--money-roi x,y,w,h` | 「没钱」检测区域（相对比例） | 内置 |
| `--money-red-pct <n>` | 红像素占比达到 n% 判为没钱（需连续复检 3 次） | `2.0` |
| `--money-wait-ms <n>` | 没钱时的复查间隔 | `400` |
| `--no-money-check` | 关闭「金币变红就等待」 | 关 |
| `--probe-money [N]` | 采样金币区域红占比 N 次后退出，用于定阈值 | `10` |
| `--image <bmp>` | 用一张 BMP 当游戏画面做离线测试（不截屏） | — |
| `--dry-run` | 空跑：只打印会点哪里，不动鼠标 | 关 |

#### 后台模式

| 参数 | 说明 | 默认 |
|---|---|---|
| `--background` | 后台抓图 + 后台点击（本来就是默认） | 开 |
| `--bg-click` | 只把点击改成 `PostMessage` 消息式，鼠标不移动 | 开（后台） |
| `--fg-click` | 切回前台物理点击（`SetCursorPos` + `mouse_event`） | 关 |
| `--capture <m>` | 抓图通路：`auto` / `d3d` / `dc` / `pw` / `scr` | `auto` |
| `--size WxH` | 把游戏**客户区**强制成该尺寸（如 `1040x629`） | 不强制 |
| `--size-test` | 配合 `--size`：真的改一次窗体尺寸，打印前后后退出 | — |
| `--interval-ms <n>` | 两次点中之间的毫秒级节拍 | `5000` |
| `--miss-interval-ms <n>` | 刷新换牌后等牌面重绘的时间 | `200` |
| `--refresh-gap-ms <n>` | 两次刷新之间的最小间隔（刷太密会把金币刷光） | `1500` |
| `--find-wait-ms <n>` | 刷新后等查找的最长时间 | `550` |
| `--scan-refresh` | 扫描底部按钮栏，定位**刷新**按钮后退出（换机标定） | — |
| `--probe` | 打印两条抓图通路的画面特征后退出 | — |
| `--probe-refresh [N]` | 测量「点刷新 → 新牌面画完」耗时，采样 N 轮 | `8` |
| `--step <n>` | 1/8 图粗筛步长 | `2` |
| `--topk <n>` | 进入精搜的候选数 | `6` |
| `--threads <n>` | 匹配线程数 | 自动 |

#### 其他与通用

| 参数 | 说明 | 默认 |
|---|---|---|
| `--coords x,y` | 固定坐标模式，不做识别 | — |
| `--dll <路径>` | 改用第三方识别 DLL（旧用法） | — |
| `--model <路径>` | DLL 模式下的模型路径（旧用法） | — |
| `--interval <秒>` | 旧主循环的每轮间隔 | `5` |
| `--times <N>` | 点击次数，`0` = 无限 | `0` |
| `--front` | 每次点击前把窗口置前 | 关 |
| `--dump` | 截图存到 `%TEMP%\mojingling_dump` | 关 |
| `--dump-dir <目录>` | 截图存到指定目录（支持 UNC 共享路径） | — |
| `-g`, `--verbose` | 详细输出：坐标、相似度、耗时 | 关 |
| `--self-test` | 无游戏自检：合成图像验证匹配引擎 | — |
| `--help` | 显示帮助 | — |

### 实现原理

#### 单文件是怎么做到的

| 内容 | 做法 | 体积 |
|---|---|---|
| 卡牌模板 | `assets/templates.bin` → `include_bytes!` → `.rdata` | 258 KB |
| 应用图标 | `assets/app.ico` → `build.rs` → `rc.exe` → PE 资源节 | 14 KB |
| 窗内印章 | `assets/stamp_70.bgra` → `include_bytes!` → `CreateDIBSection` → `STM_SETIMAGE` | 19 KB |
| 截图 | gdi32 `PrintWindow` / `BitBlt` + `GetDIBits` | — |
| 点击 | user32 `PostMessage`（后台）/ `SetCursorPos` + `mouse_event`（前台） | — |
| 图像匹配 | 自写多尺度 NCC，图像金字塔手写降采样 | ~30 KB |

`embed-resource` 只在**构建期**使用，不会进入最终二进制。

#### 识别流程

三级搜索。NCC（归一化互相关）对整体亮度/对比度变化不敏感，这一点很关键，因为卡面背后是不断变化的背景。

1. **1/8 图全图粗筛**（步长 2）—— 每个模板 × 每个尺度取最优位置；
2. **1/4 图重排 + 精搜**（±5 格）—— 1/8 层判别力偏弱，必须先重排再取 top-k，否则正确答案会被挤出；
3. **原图精定位**（±6 px）—— 拿到像素级坐标。

48 张模板是把 53 帧原始画面按 1/4 降采样 NCC ≥ 0.93 聚类后的结果，同一张卡的不同动画帧收敛成一张模板 —— 命中任意一帧即算识别成功。

参考机上 1600×900 截图单轮约 13 ms。

#### 三格手牌与共同底纹防线

三格手牌按相对比例定位，换窗口尺寸不用改参数；每格单独做金字塔搜索，取分数最高的一格去点。

难点在**换牌过渡帧 / 空槽帧**：三格里没有任何目标卡牌，但三格共同底纹会让某几张模板拿到 0.75~0.93 的高分，直接按阈值判就会误点。两条判据，任一触发即判「共同底纹」：

| 判据 | 条件 | 默认 |
|---|---|---|
| 一：同模板齐平 | 三格最佳是同一张模板，且极差 < `--bg-spread`，且最高分 < `--bg-max` | 0.25 / 0.93 |
| 二：拉不开 | 最高分与次高分之差 < `--bg-gap`，且最高分 ≤ `--bg-max` | 0.20 / 0.93 |

判据二不可省：把 `#03 / #09 / #11 / #12`（魔精灵里非本游戏皮肤的帧）剔除后，底纹会改由 `#07 / #08 / #29 / #38` **混合承接**，三格不再是同一张模板，只靠判据一会漏判。

37 帧灰区样本离线回放：

| 配置 | 命中 | 判底纹 | 误报 |
|---|---|---|---|
| 剔 11/12，仅判据一 | 34 | 3 | **31 帧误点** |
| 剔 11/12，判据一 + 判据二 | 8 | 29 | **0** |

8 帧命中全部是「一格 0.854~0.950、另两格 0.376~0.610」的真牌形态；29 帧底纹的最高分与次高分落差只有 0.004~0.099，而真牌那 8 帧落差 0.29~0.53 —— 默认阈值 0.20 落在两侧余量中间。

相关参数：`--no-bg-guard`（全关）、`--bg-spread`、`--bg-gap`、`--bg-max`、`--skip-tpl`（默认 `3,9,11,12`，`none` = 不剔除任何模板）。

**残余风险**：真牌分数落在 0.78~0.93 灰区、且另两格也拿到 0.75 以上时，可能被判据二误杀。可用 `--bg-gap 0.12` 收紧，或 `--no-bg-guard` 后单用阈值控制。

#### 防漏打：点击前复检 + 贴阈值宽限

两项默认开启，针对「真牌被跳过」的两种典型情形，也就是实际使用中最常见的「经常漏打」：

| 防线 | 做什么 | 为什么 |
|---|---|---|
| **点击前复检**（`--no-recheck` 关闭） | 到点真正按下之前，重新抓一帧再找一次。牌已经不在、或分数掉到 `--threshold` 以下 → 这一次不点，直接换牌；牌换到了别的格 → 按新坐标点。 | 程序是「先找到牌，再等节拍到点」才点。等待期间游戏可能已经自动出牌、入场动画也可能让牌位移，拿等待前的坐标去点，结果就是日志说点了、画面没变化。 |
| **贴阈值宽限**（`--no-borderline-grace` 关闭） | 最高分贴着阈值（`threshold - 0.08`）且查找超时时，先多等最多 2 轮 × 120 ms，不立刻刷新。 | 牌在淡入动画里会短暂只有 0.70~0.77。这一瞬间刷新，既丢掉真牌，又白花一次金币。 |

#### 防乱刷钱：刷新效果核对

`--no-refresh-verify` 关闭。每次刷新后，拿刷新前的帧和刷新后三格区域做像素比对；差异低于 `0.5%` 说明牌面根本没换，可能是刷新按钮坐标在当前窗口尺寸下偏了，或后台点击被游戏忽略，日志会明确告警：

```
[refresh] 刷新后牌面几乎没变（差异 0.12% < 0.50%）→ 这次刷新可能没生效
```

这正是「金币一直掉、牌一直不换」的形态。用 `--scan-refresh` 重新标定刷新按钮，或改 `--fg-click` 用前台点击试探后台消息是否被忽略。

#### 抓图通路

| 模式 | 机制 | 适用场景 |
|---|---|---|
| `auto` | 探测后选可用通路 | 默认 |
| `d3d` | 对 D3D 子窗 `PrintWindow` | 小程序 / 微信类窗口，纯后台 |
| `dc` | `GetWindowDC` + `BitBlt` | 经典 GDI 窗口 |
| `pw` | `PrintWindow` | 实现了 WM_PRINT 的窗口 |
| `scr` | 屏幕拷贝 | 兜底；窗口不能被遮挡 |

#### 「没钱」检测

对金币区域采样红像素占比。连续三次检查都超过 `--money-red-pct`，判定为「没钱」，暂停刷新并按 `--money-wait-ms` 复查。阈值可用 `--probe-money` 在本机标定。

### 从源码构建

```bat
cargo build --release --offline
```

- 需要 Rust 工具链（edition 2021）。零 crates.io 运行时依赖，只要 cargo 缓存里已有 `embed-resource`，`--offline` 也能编译。
- `build.rs` 定位 `rc.exe` 的顺序：环境变量 `RC_EXE` → `PATH` → `Windows Kits\10\bin` 下最高版本。找不到只告警不中断，exe 仍可用，只是没有自定义图标。
- 产物：`target\release\Mclick.exe`。

**模板格式**：`[u32 count] + count × (u16 w, u16 h, w×h 灰度)`。换一张卡只需把同尺寸灰度图按此格式重打包成 `assets/templates.bin`，代码无需改动。自检用例按库长度动态取首/中/末三张，不写死编号。

### 环境变量

| 变量 | 作用 |
|---|---|
| `MCLICK_NO_ELEVATE=1` | 跳过 UAC 提权（跑自检、远程调试时用） |
| `MCLICK_ELEVATE_DRYRUN=1` | 只打印提权命令行，不真的拉起 |
| `MCLICK_ELEVATED=1` | 提权后子进程的内部标记（防止递归提权） |
| `MOJINGLING_VERBOSE=1` | 详细日志，等同 `-g` |
| `MOJINGLING_COLORTEST=1` | 窗口颜色自检 |

日志文件：exe 同目录的 `Mclick_run.log`。

### 目录结构

```
Cargo.toml        crate 名 mojingling_clicker，bin 名 Mclick
build.rs          图标嵌入（自动查找 rc.exe）
assets/
  templates.bin   48 张魔精灵模板，由 53 帧聚类而来（258 KB）
  app.ico         马头图标
  stamp_70.bgra   70×70 BGRA 窗内印章
src/
  main.rs         命令行解析 + 主循环 + 自检
  gui.rs          原生 Win32 窗体
  matcher.rs      多尺度 NCC（1/8 粗筛 → 1/4 重排 → 原图精定位）
  capture.rs      抓图通路（auto/d3d/dc/pw/scr）
  click.rs        点击
  sizelock.rs     窗口客户区尺寸锁定
  elevate.rs      自动提权
  logger.rs       日志
  ffi.rs, winapi.rs, assets.rs   FFI 与内嵌资源胶水
```

### 排查手册

| 现象 | 处理 |
|---|---|
| 完全不点，日志里分数很低 | 下调 `--threshold`（如 `0.7`），或用 `--dump` 看截图确认抓图通路是否正常 |
| 截出来是黑图 | 依次试 `--capture d3d` → `pw` → `scr`；用 `--probe` 对比两条通路 |
| 后台点击没反应 | 先用 `--fg-click` 确认坐标正确，再换 `--capture` 通路 |
| 空槽也会误点 | 收紧防线：`--bg-gap 0.25`、`--bg-max 0.88` |
| 真牌被判成底纹 | 放宽到 `--bg-gap 0.12`，或用 `--no-bg-guard` 提高阈值单独控制 |
| 刷新太费金币 | 调大 `--refresh-gap-ms`；用 `--probe-money` 校准阈值 |
| 换机器后模板对不上 | 用 `--size 1040x629` 锁客户区尺寸（先用 `--size-test` 验证能不能改） |
| 反复弹 UAC | 调试时设 `MCLICK_NO_ELEVATE=1` |

### 已知边界

- 模板来自固定分辨率截图；窗口尺寸差异很大时需用 `--scales` 补尺度。
- 灰度匹配，游戏内特效遮挡严重会拉低相似度，此时可适当下调 `--threshold`。
- `--dll` / `--model` 仅为兼容旧的三方 DLL 模式保留，单文件模式下不需要。
- 仅支持 Windows（Win32 / GDI），没有 Linux / macOS 版本。

### 与 Bclick（冰精灵版）的区别

| 项目 | Mclick（魔精灵） | Bclick（冰精灵） |
|---|---|---|
| 识别目标 | 魔精灵（48 张模板） | 冰精灵（23 张模板） |
| 模板体积 | 258 KB | 124 KB |
| 默认节拍 | 5 秒 | 3 秒 |
| 剔除模板 | `3, 9, 11, 12` | 无 |
| 图标 | 黑色马头剪影 | 蓝紫莲花 |
| 环境变量前缀 | `MCLICK_*` | `BCLICK_*` |
| 日志 / dump | `Mclick_run.log` / `mojingling_dump` | `Bclick_run.log` / `bclick_dump` |

引擎、抓图通路、提权、尺寸锁定、三格手牌识别在两者之间完全一致。

### 免责声明

本项目**仅供学习、研究与技术交流使用**，用于演示 Win32/GDI 编程、基于截图的本地图像识别以及单文件资源内嵌等实现方案，不作为任何形式的成品工具对外提供或运营。严禁将其用于商业用途、盈利目的、代练代打、账号交易，或任何破坏游戏公平性与正常运营的行为。

1. **非官方声明**：本项目为个人独立作品，与本仓库中提及的游戏之开发方、发行方、运营方均无任何关联，亦未获得其授权、认可或赞助。相关游戏名称、美术素材、商标及其他知识产权归各自权利人所有。
2. **合规使用**：对游戏客户端进行自动化操作可能违反该游戏的服务条款或用户协议，可能导致警告、临时封禁乃至永久封号。使用前请自行确认并遵守相关服务条款、平台规则及所在地法律法规。
3. **风险自担**：使用本软件产生的全部风险由使用者自行承担。作者不对账号处罚、游戏内货币/道具/进度损失、数据丢失、软硬件损坏，以及因使用或误用本项目引起的任何直接或间接损失承担责任。
4. **无任何担保**：本软件按"现状"提供，不附带任何明示或暗示的担保，包括但不限于适销性、特定用途适用性及不侵权担保。
5. **转载与分发**：欢迎在 MIT 许可证条款下转载、派生与二次开发，但须完整保留本免责声明，且不得将本项目用于宣传上述被禁止的用途。
6. **侵权处理**：若任何权利人认为本仓库内容侵犯其合法权益，可通过 Issue 或邮件联系作者，核实后将及时处理，包括删除相关内容。

### 许可证

MIT —— 版权所有 © 2026 capfmud，详见 [LICENSE](LICENSE)。
