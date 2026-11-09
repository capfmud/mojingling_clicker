//! 鼠标点击层：调用 user32 现成 API（SetCursorPos + mouse_event）模拟物理点击，
//! 与游戏客户端自身调用链（user32: SetCursorPos / mouse_event）保持一致。

use crate::winapi::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::sleep;
use std::time::Duration;

/// 用户操作护栏日志的节流时刻（避免每次点击都刷一行）
static LAST_GUARD_MS: AtomicU64 = AtomicU64::new(0);

/// 用户此刻是否正用手在操作（拖动窗口 / 按住左键）。
///
/// 为什么必须判断：前台点击会调 SetCursorPos 把光标抢走、还会 SetForegroundWindow 抢前台。
/// 一旦用户正按住标题栏拖我们的窗口，系统的移动循环会被这两步直接打断，
/// 表现出来就是「我就拖了一下，它自己跑出去了，然后不见了」。
/// 所以只要用户手上按着左键，或窗口正在被拖动，本次物理点击一律让路。
pub unsafe fn user_is_holding_mouse() -> bool {
    if crate::gui::is_dragging() {
        return true;
    }
    GetAsyncKeyState(VK_LBUTTON) < 0
}

/// 节流写一条「因用户操作而跳过点击」的日志
fn note_skip(reason: &str) {
    let now = crate::epoch_ms();
    let last = LAST_GUARD_MS.load(Ordering::SeqCst);
    if now.saturating_sub(last) >= 2000 {
        LAST_GUARD_MS.store(now, Ordering::SeqCst);
        crate::logger::log_warn(format!(
            "[guard] 检测到{reason}，本轮物理点击已让路（不会移动光标、不抢前台）"
        ));
    }
}

/// 将窗口置前（user32: SwitchToThisWindow + SetForegroundWindow）
/// # Safety
/// 会改变前台窗口。
pub unsafe fn activate(hwnd: HWND) {
    if hwnd.is_null() {
        return;
    }
    SwitchToThisWindow(hwnd, 1);
    let _ = SetForegroundWindow(hwnd);
}

/// 将鼠标移到指定屏幕坐标（user32: SetCursorPos）
/// # Safety
/// 会移动物理鼠标光标。
pub unsafe fn move_to(x: i32, y: i32) -> bool {
    SetCursorPos(x, y) != 0
}

/// 在当前鼠标位置按下并抬起左键（user32: mouse_event）
/// # Safety
/// 会触发真实的左键点击。
pub unsafe fn click_now() {
    mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0);
    mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0);
}

/// 一键封装：可选置前窗口 → 移动鼠标 → 左键点击
/// # Safety
/// 同 move_to / click_now。
pub unsafe fn click_at(hwnd: Option<HWND>, x: i32, y: i32, bring_to_front: bool) {
    // 先记住点击前的光标位置：点完立刻 SetCursorPos 还回去。
    // 只影响 --fg-click 的物理模式；但物理路径必须还，否则用户鼠标会被一次次
    // 钉在游戏窗口上 —— 就是「抢鼠标」的直接观感来源。
    let mut prev = POINT { x: 0, y: 0 };
    let had_prev = GetCursorPos(&mut prev) != 0;
    if bring_to_front {
        if let Some(h) = hwnd {
            activate(h);
        } else {
            let _ = SetForegroundWindow(GetForegroundWindow());
        }
    }
    let _ = move_to(x, y);
    click_now();
    if had_prev {
        let _ = SetCursorPos(prev.x, prev.y);
    }
}

// ==================== 后台点击（同款方案：PostMessage） ====================
//
//  P/Invoke 里同时导入了 mouse_event（前台物理点击）和 PostMessage/SendMessage
// （消息式点击）。后者把 WM_LBUTTONDOWN / WM_LBUTTONUP 直接投给游戏窗口，
// 不移动真实鼠标、不抢前台、不影响用户同时干别的 —— 就是「后台点击」。
// 坐标用客户区坐标（和后台找图拿到的 ROI 在同一坐标系里），不需要再转屏幕坐标。

/// 消息式点击的移动→按下间隔（毫秒）。
///
/// 为什么必须有间隔：实测对微信版（Chrome_WidgetWin_0 / WeChatAppEx）零间隔连发
/// MOVE+DOWN+UP，游戏画面 diff = 0.00%（完全不响应）；改成移动后停 20~30ms、
/// 按下后停 40~50ms，同一坐标 diff = 90.69%（成功弹窗）。
/// Chromium 的输入管线按帧合并合成消息，DOWN/UP 挤在同一批次会被判为无效点击。
pub const CLICK_MOVE_GAP_MS: u64 = 20;

/// 按下后到抬起的保持时长（毫秒），意义见 CLICK_MOVE_GAP_MS。
pub const CLICK_HOLD_MS: u64 = 45;

/// 后台点击：向窗口投递鼠标移动/按下/抬起消息，三条消息之间留真实间隔。
/// `cx, cy` 是客户区坐标。返回三条消息是否都投递成功。
/// # Safety
/// hwnd 须有效。不移动鼠标、不改变前台窗口。
pub unsafe fn post_click(hwnd: HWND, cx: i32, cy: i32) -> bool {
    if hwnd.is_null() {
        return false;
    }
    let lp = make_lparam(cx, cy);
    let a = PostMessageW(hwnd, WM_MOUSEMOVE, 0, lp) != 0;
    sleep(Duration::from_millis(CLICK_MOVE_GAP_MS));
    let b = PostMessageW(hwnd, WM_LBUTTONDOWN, MK_LBUTTON, lp) != 0;
    sleep(Duration::from_millis(CLICK_HOLD_MS));
    let c = PostMessageW(hwnd, WM_LBUTTONUP, 0, lp) != 0;
    crate::logger::log_info(format!(
        "[post_click] hwnd=0x{:X} 客户区({cx},{cy}) lp=0x{lp:X} move={a} down={b} up={c}",
        hwnd as usize
    ));
    a && b && c
}

/// 后台点击（同步版）：个别游戏只处理 SendMessage 队列里的消息。
/// 同样保留消息间隔，理由见 CLICK_MOVE_GAP_MS。
/// # Safety
/// hwnd 须有效；SendMessage 会阻塞到目标窗口处理完消息。
pub unsafe fn send_click(hwnd: HWND, cx: i32, cy: i32) -> bool {
    if hwnd.is_null() {
        return false;
    }
    let lp = make_lparam(cx, cy);
    SendMessageW(hwnd, WM_MOUSEMOVE, 0, lp);
    sleep(Duration::from_millis(CLICK_MOVE_GAP_MS));
    SendMessageW(hwnd, WM_LBUTTONDOWN, MK_LBUTTON, lp);
    sleep(Duration::from_millis(CLICK_HOLD_MS));
    SendMessageW(hwnd, WM_LBUTTONUP, 0, lp);
    true
}

/// 统一点击入口：后台模式走消息式点击（客户区坐标），
/// 前台模式走物理鼠标（屏幕坐标，需要把客户区坐标 + 客户区屏幕原点换算过去）。
/// # Safety
/// 前台模式会移动真实鼠标并可能切换前台窗口；后台模式不会。
pub unsafe fn click_bg_or_fg(
    hwnd: HWND,
    client_x: i32,
    client_y: i32,
    background: bool,
    bring_to_front: bool,
) -> bool {
    if background {
        return post_click(hwnd, client_x, client_y);
    }
    // 前台物理点击前先让路：用户正在拖窗口/按住左键时，绝不去抢光标和前台
    if user_is_holding_mouse() {
        note_skip("用户正在按住鼠标或拖动窗口");
        return false;
    }
    let (ox, oy) = match crate::capture::client_rect(hwnd) {
        Some((ox, oy, _, _)) => (ox, oy),
        None => (0, 0),
    };
    click_at(Some(hwnd), ox + client_x, oy + client_y, bring_to_front);
    true
}
