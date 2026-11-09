//! 游戏窗体尺寸锁定（独立模块 + 独立线程）。
//!
//! 职责：把游戏窗体的大小死死钉在程序记录的矩形上。
//! 后台线程每 [`TICK_MS`] 毫秒读一次目标窗体的 `GetWindowRect`，
//! 只要宽高超过程序记录值 1px 以上，立刻用 `SetWindowPos(SWP_NOMOVE)` 改回去。
//!
//! 设计要点：
//! - 只锁**尺寸**，不动位置：位置由 gui 层的吸附逻辑负责，两者互不干扰。
//! - 目标窗体最小化（`IsIconic`）时跳过，避免把用户最小化的游戏反复弹出来。
//! - 线程全局只启一次（[`STARTED`] 守卫），窗体句柄换新用 [`set_target`] 热更新。

use crate::logger;
use crate::winapi::{
    AdjustWindowRectEx, GetClientRect, GetWindowLongW, GetWindowRect, IsIconic, IsWindow,
    SetWindowPos, GWL_EXSTYLE, GWL_STYLE, HWND, RECT, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER,
};

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, Ordering};
use std::thread;
use std::time::Duration;

/// 巡检间隔：120ms，肉眼基本看不出尺寸被改过
pub const TICK_MS: u64 = 120;
/// 客户区校准最多轮数（回读差值修正，通常 2 轮内到位）
pub const CALIBRATE_ROUNDS: u32 = 4;
/// 每轮校准后等窗口处理 WM_SIZE 的时间（毫秒）
pub const CALIBRATE_WAIT_MS: u64 = 45;

/// 锁定的窗体宽度（0 = 尚未记录）
static LOCK_W: AtomicI32 = AtomicI32::new(0);
/// 锁定的窗体高度（0 = 尚未记录）
static LOCK_H: AtomicI32 = AtomicI32::new(0);
/// 被锁定的游戏窗体句柄（0 = 无）
static LOCK_HWND: AtomicIsize = AtomicIsize::new(0);
/// 锁开关：true = 持续把尺寸改回记录值
static LOCK_ON: AtomicBool = AtomicBool::new(true);
/// 线程只启动一次
static STARTED: AtomicBool = AtomicBool::new(false);
/// 累计纠正次数（日志用，便于确认线程确实在干活）
static CORRECTIONS: AtomicI32 = AtomicI32::new(0);

/// 强制锁定的**客户区**尺寸（0 = 不强制，沿用首次见到的窗体尺寸）。
///
/// 跨机器统一尺寸时必须用这个：模板是按客户区（canvas 像素）裁的，
/// 只有客户区一致，模板匹配的尺度才对得上；外框尺寸会随机器边框/DPI 变化，
/// 拿外框去对齐是错的。
static FORCE_CW: AtomicI32 = AtomicI32::new(0);
static FORCE_CH: AtomicI32 = AtomicI32::new(0);

/// 设定「强制客户区尺寸」：此后锁定目标不再是窗体当前大小，而是该客户区尺寸。
pub fn set_force_client(cw: i32, ch: i32) {
    FORCE_CW.store(cw.max(0), Ordering::SeqCst);
    FORCE_CH.store(ch.max(0), Ordering::SeqCst);
}

/// 当前强制的客户区尺寸（未设置返回 None）
pub fn force_client() -> Option<(i32, i32)> {
    let cw = FORCE_CW.load(Ordering::SeqCst);
    let ch = FORCE_CH.load(Ordering::SeqCst);
    if cw > 0 && ch > 0 {
        Some((cw, ch))
    } else {
        None
    }
}

/// 当前是否启用尺寸锁定
pub fn is_on() -> bool {
    LOCK_ON.load(Ordering::SeqCst)
}

/// 切换锁定开关，返回切换后的状态
pub fn toggle() -> bool {
    let on = !LOCK_ON.load(Ordering::SeqCst);
    LOCK_ON.store(on, Ordering::SeqCst);
    on
}

/// 程序记录的锁定尺寸（宽, 高）；未记录时为 None
pub fn locked_size() -> Option<(i32, i32)> {
    let w = LOCK_W.load(Ordering::SeqCst);
    let h = LOCK_H.load(Ordering::SeqCst);
    if w > 0 && h > 0 {
        Some((w, h))
    } else {
        None
    }
}

/// 累计纠正次数
pub fn corrections() -> i32 {
    CORRECTIONS.load(Ordering::SeqCst)
}

/// 热更新被锁定的窗体句柄（游戏重启换句柄时调用，保留已记录的尺寸）
pub fn set_target(hwnd: HWND) {
    LOCK_HWND.store(hwnd as isize, Ordering::SeqCst);
}

/// 放弃当前记录（目标窗口消失时调用）
pub fn clear_target() {
    LOCK_HWND.store(0, Ordering::SeqCst);
}

/// 读取目标窗体当前的**外框**尺寸（不做还原/激活，避免副作用）
unsafe fn outer_size(hwnd: HWND) -> Option<(i32, i32)> {
    if hwnd.is_null() || IsWindow(hwnd) == 0 {
        return None;
    }
    if IsIconic(hwnd) != 0 {
        return None;
    }
    let mut r = RECT::default();
    if GetWindowRect(hwnd, &mut r) == 0 {
        return None;
    }
    let w = r.right - r.left;
    let h = r.bottom - r.top;
    if w <= 0 || h <= 0 {
        return None;
    }
    Some((w, h))
}

/// 读取目标窗体当前客户区尺寸（诊断/验证用，只读）
pub unsafe fn current_client(hwnd: HWND) -> Option<(i32, i32)> {
    client_size(hwnd)
}

/// 读取目标窗体当前外框尺寸（诊断/验证用，只读）
pub unsafe fn current_outer(hwnd: HWND) -> Option<(i32, i32)> {
    outer_size(hwnd)
}

/// 读取目标窗体当前的**客户区**尺寸（canvas 实际像素，不做还原/激活）
unsafe fn client_size(hwnd: HWND) -> Option<(i32, i32)> {
    if hwnd.is_null() || IsWindow(hwnd) == 0 {
        return None;
    }
    let mut r = RECT::default();
    if GetClientRect(hwnd, &mut r) == 0 {
        return None;
    }
    let w = r.right - r.left;
    let h = r.bottom - r.top;
    if w <= 0 || h <= 0 {
        return None;
    }
    Some((w, h))
}

/// 把「客户区尺寸」按目标窗体自身的样式换算成「外框尺寸」。
///
/// `SetWindowPos` 只吃外框尺寸，而模板对齐关心的是客户区，
/// 所以必须先走一趟 `AdjustWindowRectEx`。
pub unsafe fn client_to_outer(hwnd: HWND, cw: i32, ch: i32) -> Option<(i32, i32)> {
    if hwnd.is_null() || IsWindow(hwnd) == 0 || cw <= 0 || ch <= 0 {
        return None;
    }
    let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
    let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
    let mut r = RECT {
        left: 0,
        top: 0,
        right: cw,
        bottom: ch,
    };
    if AdjustWindowRectEx(&mut r, style, 0, ex) == 0 {
        return None;
    }
    let w = r.right - r.left;
    let h = r.bottom - r.top;
    if w <= 0 || h <= 0 {
        None
    } else {
        Some((w, h))
    }
}

/// 立刻把目标窗体尺寸改成锁定值（不等巡检 tick），最小化时不动。
pub unsafe fn apply_once(hwnd: HWND) -> bool {
    let (tw, th) = match locked_size() {
        Some(v) => v,
        None => return false,
    };
    if hwnd.is_null() || IsWindow(hwnd) == 0 || IsIconic(hwnd) != 0 {
        return false;
    }
    SetWindowPos(
        hwnd,
        std::ptr::null_mut(),
        0,
        0,
        tw,
        th,
        SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
    ) != 0
}

/// 以指定**客户区**尺寸为准强制锁定：换算外框 → 逐轮校准 → 立即应用。
///
/// 为什么要「逐轮校准」而不是一次算完：`AdjustWindowRectEx` 对带 DWM 扩展边框
/// 或自绘边框的窗口（Chromium/微信小程序、Win11 记事本等）算出的客户区会偏几像素到几十像素，
/// 一次设定往往拿不到精确客户区。这里设定后回读 `GetClientRect`，按差值修正外框再设一次，
/// 最多 [`CALIBRATE_ROUNDS`] 轮；同时把最终外框尺寸留在 [`LOCK_W`]/[`LOCK_H`] 上，
/// 后续由巡检线程持续维持。返回外框 (宽, 高)。
pub unsafe fn force_client_now(hwnd: HWND, cw: i32, ch: i32) -> Option<(i32, i32)> {
    let (mut ow, mut oh) = client_to_outer(hwnd, cw, ch)?;
    let mut settled = false;
    for round in 1..=CALIBRATE_ROUNDS {
        LOCK_W.store(ow, Ordering::SeqCst);
        LOCK_H.store(oh, Ordering::SeqCst);
        set_target(hwnd);
        if !apply_once(hwnd) {
            logger::log(format!(
                "[lock] 第 {round} 轮 SetWindowPos 外框 {ow}x{oh} 失败（窗口可能拒绝改尺寸）"
            ));
            break;
        }
        thread::sleep(Duration::from_millis(CALIBRATE_WAIT_MS));
        let (rcw, rch) = match client_size(hwnd) {
            Some(v) => v,
            None => break,
        };
        if rcw == cw && rch == ch {
            settled = true;
            logger::log(format!(
                "[lock] 客户区已锁定 {cw}x{ch}（外框 {ow}x{oh}，第 {round} 轮校准到位）"
            ));
            break;
        }
        let (dw, dh) = (cw - rcw, ch - rch);
        logger::log(format!(
            "[lock] 第 {round} 轮回读客户区 {rcw}x{rch} ≠ 目标 {cw}x{ch}，外框修正 {ow}x{oh} → {}x{}",
            ow + dw,
            oh + dh
        ));
        ow += dw;
        oh += dh;
    }
    if !settled {
        let real = match client_size(hwnd) {
            Some((w, h)) => (w, h),
            None => (0, 0),
        };
        if real.0 > 0 {
            let s = real.0 as f32 / cw as f32;
            logger::log(format!(
                "[lock] 警告：客户区只做到 {}x{}（目标 {cw}x{ch}），该窗体自身限制了尺寸。\
                 模板按目标尺寸裁的会对不上，可改用 --scales {:.3} 让模板跟着缩放",
                real.0, real.1, s
            ));
            // 保留最接近目标的那一组外框尺寸，交给巡检线程维持
            LOCK_W.store(ow, Ordering::SeqCst);
            LOCK_H.store(oh, Ordering::SeqCst);
            set_target(hwnd);
        }
    }
    Some((ow, oh))
}

/// 把**首次**见到的目标窗体尺寸写入程序（已记录过则保持不变，保证尺寸稳定），
/// 返回最终生效的锁定尺寸。
///
/// 若已通过 [`set_force_client`] 指定了强制客户区尺寸，则完全不看窗体当前大小，
/// 直接按目标尺寸写入并立即应用（这样最小化/被改过尺寸的窗体也能一次掰正）。
pub unsafe fn lock_current(hwnd: HWND) -> Option<(i32, i32)> {
    set_target(hwnd);
    if let Some((cw, ch)) = force_client() {
        return match locked_size() {
            Some(v) => {
                apply_once(hwnd);
                Some(v)
            }
            None => force_client_now(hwnd, cw, ch),
        };
    }
    if let Some((w, h)) = locked_size() {
        return Some((w, h));
    }
    let (w, h) = outer_size(hwnd)?;
    LOCK_W.store(w, Ordering::SeqCst);
    LOCK_H.store(h, Ordering::SeqCst);
    logger::log(format!("[lock] 已记录游戏窗体尺寸 {w}x{h}，后台线程开始锁定"));
    Some((w, h))
}

/// 手动以窗体当前尺寸重新设定锁定值（"以当前大小为准"）
pub unsafe fn relock_now(hwnd: HWND) -> Option<(i32, i32)> {
    let (w, h) = outer_size(hwnd)?;
    LOCK_W.store(w, Ordering::SeqCst);
    LOCK_H.store(h, Ordering::SeqCst);
    set_target(hwnd);
    logger::log(format!("[lock] 锁定尺寸已更新为当前大小 {w}x{h}"));
    Some((w, h))
}

/// 启动锁定线程（幂等，重复调用无副作用）
pub fn ensure_thread() {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawn = thread::Builder::new()
        .name("size-lock".to_string())
        .spawn(|| {
            logger::log(format!(
                "[lock] 尺寸锁定线程已启动（每 {TICK_MS}ms 巡检一次）"
            ));
            loop {
                thread::sleep(Duration::from_millis(TICK_MS));
                if !LOCK_ON.load(Ordering::SeqCst) {
                    continue;
                }
                let h = LOCK_HWND.load(Ordering::SeqCst) as HWND;
                if h.is_null() {
                    continue;
                }
                let (tw, th) = match locked_size() {
                    Some(v) => v,
                    None => continue,
                };
                let (w, ht) = match unsafe { outer_size(h) } {
                    Some(v) => v,
                    None => continue,
                };
                // 1px 容差：避免 DPI 取整导致来回抖动
                if (w - tw).abs() <= 1 && (ht - th).abs() <= 1 {
                    continue;
                }
                let ok = unsafe {
                    SetWindowPos(
                        h,
                        std::ptr::null_mut(),
                        0,
                        0,
                        tw,
                        th,
                        SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                    )
                } != 0;
                let n = CORRECTIONS.fetch_add(1, Ordering::SeqCst) + 1;
                if ok {
                    // 只在前几次和每 50 次记一行，避免刷屏
                    if n <= 5 || n % 50 == 0 {
                        logger::log(format!(
                            "[lock] 游戏窗体尺寸被改成 {w}x{ht}，已锁回 {tw}x{th}（第 {n} 次纠正）"
                        ));
                    }
                } else if n <= 5 || n % 50 == 0 {
                    logger::log(format!(
                        "[lock] 锁回 {tw}x{th} 失败（当前 {w}x{ht}，可能权限不足）"
                    ));
                }
            }
        });
    if spawn.is_err() {
        STARTED.store(false, Ordering::SeqCst);
        logger::log("[lock] 尺寸锁定线程启动失败");
    }
}
