//! 原生 Win32 窗体（纯 FFI，零外部依赖）。
//!
//! 做成窗体后程序是 GUI 子系统（`windows_subsystem = "windows"`），双击不再弹控制台黑框，
//! 所有日志写进窗体内的只读日志框。命令行用法仍保留（见 `main.rs`：带参数启动时自动挂控制台）。

use crate::logger;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

// ---------- Win32 类型 ----------

type HWND = *mut c_void;
type HINSTANCE = *mut c_void;
type HICON = *mut c_void;
type HCURSOR = *mut c_void;
type HBRUSH = *mut c_void;
type HFONT = *mut c_void;
type HMENU = *mut c_void;
type WPARAM = usize;
type LPARAM = isize;
type LRESULT = isize;
type WndProc = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;

/// SYSTEMTIME（GetLocalTime 的本地时间结构）
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct SysTime {
    year: u16,
    month: u16,
    dow: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    ms: u16,
}

/// RECT（客户区矩形）
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
struct WndClassExW {
    cb_size: u32,
    style: u32,
    lpfn_wndproc: Option<WndProc>,
    cb_cls_extra: i32,
    cb_wnd_extra: i32,
    h_instance: HINSTANCE,
    h_icon: HICON,
    h_cursor: HCURSOR,
    hbr_background: HBRUSH,
    lpsz_menu_name: *const u16,
    lpsz_class_name: *const u16,
    h_icon_sm: HICON,
}

#[repr(C)]
struct Point {
    x: i32,
    y: i32,
}

#[repr(C)]
struct Msg {
    hwnd: HWND,
    message: u32,
    w_param: WPARAM,
    l_param: LPARAM,
    time: u32,
    pt: Point,
}

extern "system" {
    fn RegisterClassExW(wc: *const WndClassExW) -> u16;
    fn CreateWindowExW(
        ex_style: u32,
        class_name: *const u16,
        window_name: *const u16,
        style: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        parent: HWND,
        menu: HMENU,
        instance: HINSTANCE,
        param: *mut c_void,
    ) -> HWND;
    fn DefWindowProcW(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT;
    fn DestroyWindow(hwnd: HWND) -> i32;
    fn ShowWindow(hwnd: HWND, cmd: i32) -> i32;
    fn UpdateWindow(hwnd: HWND) -> i32;
    /// 标记区域失效让它重画（WM_SETREDRAW 关过重绘后必须调它，否则新文字不显示）
    fn InvalidateRect(hwnd: HWND, rect: *const Rect, erase: i32) -> i32;
    fn GetMessageW(msg: *mut Msg, hwnd: HWND, min: u32, max: u32) -> i32;
    fn TranslateMessage(msg: *const Msg) -> i32;
    fn DispatchMessageW(msg: *const Msg) -> LRESULT;
    fn PostQuitMessage(code: i32);
    fn SendMessageW(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT;
    fn SetWindowTextW(hwnd: HWND, text: *const u16) -> i32;
    fn GetWindowTextW(hwnd: HWND, buf: *mut u16, max: i32) -> i32;
    fn MessageBoxW(hwnd: HWND, text: *const u16, caption: *const u16, ty: u32) -> i32;
    fn LoadIconW(instance: HINSTANCE, name: *const u16) -> HICON;
    fn LoadCursorW(instance: HINSTANCE, name: *const u16) -> HCURSOR;
    fn SetProcessDPIAware() -> i32;
    fn EnableWindow(hwnd: HWND, enable: i32) -> i32;
    fn SetFocus(hwnd: HWND) -> HWND;
    fn IsWindow(hwnd: HWND) -> i32;
    /// 是否处于最小化状态（跑出屏幕判定要跳过最小化窗口）
    fn IsIconic(hwnd: HWND) -> i32;
    fn SetTimer(hwnd: HWND, id: usize, elapse: u32, func: *mut c_void) -> usize;
    fn GetLocalTime(st: *mut SysTime);
    fn GetClientRect(hwnd: HWND, r: *mut Rect) -> i32;
    fn KillTimer(hwnd: HWND, id: usize) -> i32;
    fn PostMessageW(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> i32;
    fn GetModuleHandleW(name: *const u16) -> HINSTANCE;
    fn GetSystemMetrics(index: i32) -> i32;
    fn SetWindowPos(hwnd: HWND, after: HWND, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> i32;
    fn GetWindowLongW(hwnd: HWND, index: i32) -> i32;
    fn GetDlgItem(parent: HWND, id: i32) -> HWND;
    fn CreateFontW(
        height: i32,
        width: i32,
        esc: i32,
        orient: i32,
        weight: i32,
        italic: u32,
        underline: u32,
        strike: u32,
        charset: u32,
        out_prec: u32,
        clip_prec: u32,
        quality: u32,
        pitch: u32,
        face: *const u16,
    ) -> HFONT;
    fn LoadLibraryW(name: *const u16) -> usize;
}

// ---------- 常量 ----------

const WM_CREATE: u32 = 0x0001;
const WM_DESTROY: u32 = 0x0002;
const WM_SIZE: u32 = 0x0005;
const WM_CLOSE: u32 = 0x0010;
const WM_COMMAND: u32 = 0x0111;
const WM_TIMER: u32 = 0x0113;
const TIMER_SCAN: usize = 1;
/// 记时刷新定时器（100ms = 0.1 秒精度）
const TIMER_CLOCK: usize = 2;
const CLOCK_MS: u32 = 100;
/// 状态栏时间刷新定时器（1 秒一跳）
const TIMER_STATUS: usize = 3;
const STATUS_MS: u32 = 1000;
/// 自动扫描间隔（毫秒）：游戏晚于本程序启动时，靠它自动发现并锁定
const SCAN_MS: u32 = 1200;
/// 节拍倒计时刷新定时器（与「记时」秒表彻底分开，互不 KillTimer）
const TIMER_BEAT: usize = 4;
/// 日志批量刷新定时器：工作线程只往队列里塞，UI 线程每 120ms 成批插一次
const TIMER_FLUSH: usize = 5;
const FLUSH_MS: u32 = 120;
/// 日志框最多保留的字符数（超过就从头部裁掉，避免 RichEdit 越跑越慢）
const LOG_MAX_CHARS: usize = 20_000;
const LOG_TRIM_CHARS: usize = 10_000;
/// 工作线程 → UI 线程：追加一行日志（异步，避免阻塞 UI 消息泵）
const WM_APP_LOG: u32 = 0x8001;
/// 工作线程 → UI 线程：控件状态变更
const WM_APP_UI: u32 = 0x8002;
/// WM_APP_UI 子命令：工作线程结束，恢复按钮
const UI_EVT_FINISHED: usize = 1;
const WM_GETTEXTLENGTH: u32 = 0x000E;
/// 进入系统「拖动/缩放窗口」模态循环（用户按住标题栏或边框时由系统发出）
const WM_ENTERSIZEMOVE: u32 = 0x0231;
/// 退出系统「拖动/缩放窗口」模态循环（松开鼠标时发出）
const WM_EXITSIZEMOVE: u32 = 0x0232;
/// 虚拟屏幕度量：多屏/窗口跑到屏外检测用
const SM_XVIRTUALSCREEN: i32 = 76;
const SM_YVIRTUALSCREEN: i32 = 77;
const SM_CXVIRTUALSCREEN: i32 = 78;
const SM_CYVIRTUALSCREEN: i32 = 79;
/// WM_SETREDRAW：批量插日志前后关/开重绘，避免逐行重排
const WM_SETREDRAW: u32 = 0x000B;
const WM_SETFONT: u32 = 0x0030;
const EM_SETSEL: u32 = 0x00B1;
const EM_REPLACESEL: u32 = 0x00C2;
const EM_SCROLLCARET: u32 = 0x00B7;
/// EM_SETCHARFORMAT：WM_USER + 68
const EM_SETCHARFORMAT: u32 = 0x0444;
/// SCF_SELECTION：只改选区（插入点）的格式
const SCF_SELECTION: usize = 0x0001;
/// SCF_ALL：改整篇（清空后用来复位默认格式）
const SCF_ALL: usize = 0x0004;
/// CFM_COLOR：只改文字颜色
const CFM_COLOR: u32 = 0x4000_0000;
/// RichEdit 5.0 窗口类（需先加载 msftedit.dll）
const RICHEDIT_CLASS: &str = "RICHEDIT50W";

/// 加载 RichEdit 运行库（只需一次；失败则退回普通 EDIT，仅无颜色）
fn ensure_richedit() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        let name = wide("Msftedit.dll");
        LoadLibraryW(name.as_ptr());
    });
}

/// CHARFORMATW（RichEdit 设字符格式用；字段顺序与大小必须与 Win32 一致）
#[repr(C)]
#[derive(Clone, Copy)]
struct CharFormatW {
    cb_size: u32,
    mask: u32,
    effects: u32,
    height: i32,
    offset: i32,
    color: u32,
    charset: u8,
    pitch_and_family: u8,
    face: [u16; 32],
}

const WS_OVERLAPPED: u32 = 0x0000_0000;
const WS_CAPTION: u32 = 0x00C0_0000;
const WS_SYSMENU: u32 = 0x0008_0000;
const WS_MINIMIZEBOX: u32 = 0x0002_0000;
const WS_CHILD: u32 = 0x4000_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const WS_CLIPCHILDREN: u32 = 0x0200_0000;
const WS_BORDER: u32 = 0x0080_0000;
const WS_VSCROLL: u32 = 0x0020_0000;
const WS_TABSTOP: u32 = 0x0001_0000;
/// 静态控件：显示位图
const SS_BITMAP: u32 = 0x0000_000E;
/// 静态控件消息：设置位图（值 = 0x0172）
const STM_SETIMAGE: u32 = 0x0172;
/// STM_SETIMAGE 的 wParam：位图类型
const IMAGE_BITMAP: usize = 0;
const CW_USEDEFAULT: i32 = 0x8000_0000u32 as i32;

const ES_MULTILINE: u32 = 0x0004;
const ES_AUTOHSCROLL: u32 = 0x0080;
const ES_AUTOVSCROLL: u32 = 0x0040;
const ES_READONLY: u32 = 0x0800;
const BS_PUSHBUTTON: u32 = 0x0000_0000;

const SW_SHOW: i32 = 5;
const IDC_ARROW: *const u16 = 32512usize as *const u16;
const COLOR_BTNFACE: u32 = 15;
const MB_ICONWARNING: u32 = 0x30;

const FW_NORMAL: i32 = 400;
const DEFAULT_CHARSET: u32 = 1;
const CLEARTYPE_QUALITY: u32 = 5;

// 控件 ID（同时作为 CreateWindowExW 的 hMenu 传入，供 GetDlgItem 定位）
const ID_INTERVAL: i32 = 1002;
const ID_START: u16 = 1010;
const ID_STOP: u16 = 1011;
const ID_CLEAR: u16 = 1013;
/// 「吸附」开关按钮
const ID_DOCK: u16 = 1015;
/// 「置顶」开关按钮（游戏窗口 + 本窗口一起置顶）
const ID_TOPMOST: u16 = 1017;
/// 「记时」按钮
const ID_CLOCK_BTN: u16 = 1018;
/// 记时显示
const ID_CLOCK_TXT: i32 = 1019;
const ID_LOG: i32 = 1020;
const ID_STATUS: i32 = 1030;
/// 左上角印章图（STATIC + SS_BITMAP）
const ID_STAMP: i32 = 1031;

// 布局
/// 行 2 最右控件「记时显示」右缘 = 332 + 120，窗口宽度以它为基准收窄
const CLOCK_TXT_RIGHT: i32 = 532;
/// 客户区宽度 = 记时显示右缘 + 右侧留白
const CLIENT_W: i32 = CLOCK_TXT_RIGHT + PAD;
/// Win10/11 可调整边框左右各 8px（外框宽 = 客户区宽 + 该边框）
const FRAME_W: i32 = 16;
const WIN_W: i32 = CLIENT_W + FRAME_W;
const WIN_H: i32 = 540;
const LOG_Y: i32 = 96;
const LOG_H: i32 = 354;
const CONTENT_W: i32 = CLIENT_W - PAD * 2;
/// 左右留白 / 状态栏高度 / 状态栏距窗口底部留白
const PAD: i32 = 14;
const STATUS_H: i32 = 22;
const STATUS_BOTTOM_GAP: i32 = 6;
/// 六个按钮统一尺寸（两行三列对齐）
const BTN_W: i32 = 96;
const BTN_H: i32 = 30;
/// 左上角印章：正方形，边长 = 两排按钮的总高度（行1 顶 14 → 行2 底 84）
const STAMP_SZ: i32 = 70;
/// 印章左上角坐标（与行 1 按钮顶边对齐）
const STAMP_X: i32 = 12;
const STAMP_Y: i32 = 14;
/// 右侧控件整体右移量 = 印章宽 + 间隙
const STAMP_SHIFT: i32 = STAMP_SZ + 10;

// ---------- 全局状态 ----------

static BTN_START: AtomicIsize = AtomicIsize::new(0);
static BTN_STOP: AtomicIsize = AtomicIsize::new(0);
/// 「记时」按钮与计时显示控件
static BTN_CLOCK: AtomicIsize = AtomicIsize::new(0);
static CLOCK_TXT: AtomicIsize = AtomicIsize::new(0);
static STATUS_HWND: AtomicIsize = AtomicIsize::new(0);
/// 主窗口句柄（工作线程据此把界面更新 PostMessage 回 UI 线程）
static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);
/// WM_SIZE 重入保护：吸附改尺寸会再次触发 WM_SIZE，必须挡住递归
static IN_SIZE: AtomicBool = AtomicBool::new(false);
/// 上次自动吸附的时刻，用于给跟随吸附加冷却，避免高频 SetWindowPos 抖动
static LAST_DOCK_MS: AtomicU64 = AtomicU64::new(0);
static BUSY: AtomicBool = AtomicBool::new(false);
static FONT: AtomicIsize = AtomicIsize::new(0);
/// 上次用于查找的关键字（edit 里改显示句柄后仍能按名字重查）
static KEYWORD: Mutex<String> = Mutex::new(String::new());
/// 自动查找到的窗口句柄
static LOCKED: AtomicIsize = AtomicIsize::new(0);
/// 吸附开关：true = 锁定后自动把本窗口贴到游戏窗口右侧
static DOCK_ON: AtomicBool = AtomicBool::new(true);
/// 置顶开关：true = 游戏窗口与本窗口一起保持在最前
static TOPMOST_ON: AtomicBool = AtomicBool::new(true);
/// 上次吸附时游戏窗口的屏幕矩形，用于检测游戏移动/缩放后重新吸附
static DOCK_REF: Mutex<(i32, i32, i32, i32)> = Mutex::new((0, 0, 0, 0));
/// 记时开关：true = 正在计时
static CLOCK_RUN: AtomicBool = AtomicBool::new(false);
/// 已累计的毫秒数（暂停时冻结）
static CLOCK_BASE_MS: AtomicU64 = AtomicU64::new(0);
/// 本次开始计时的起点
static CLOCK_START: Mutex<Option<Instant>> = Mutex::new(None);
/// 用户正在拖动/缩放本窗口：期间禁止吸附搬动窗口、禁止发物理点击，
/// 否则会出现「一拖窗口，程序自己把窗口甩到别处/把自己点没了」。
static DRAGGING: AtomicBool = AtomicBool::new(false);
/// 本窗口被检测到跑到屏幕外的次数（连续 3 次就自动关掉吸附，避免来回搬）
static OFFSCREEN_HITS: AtomicU64 = AtomicU64::new(0);

/// 当前是否处于用户拖动/缩放窗口的过程中（点击层据此暂停物理点击）
pub fn is_dragging() -> bool {
    DRAGGING.load(Ordering::SeqCst)
}

/// 主窗口句柄（未创建时为 0）：点击层用来判断「现在前台是不是我自己」
pub fn main_hwnd() -> isize {
    MAIN_HWND.load(Ordering::SeqCst)
}

/// 启动时自动查找的默认关键字
const TARGET_POS: (i32, i32) = (10, 10);
const DEFAULT_KEYWORD: &str = "塔防精灵";

// ---------- 吸附参数 ----------
/// 吸附时本窗口左边缘与游戏窗口右边界之间的距离（像素）
const DOCK_GAP_X: i32 = 1;
/// 吸附时本窗口顶边与游戏窗口顶边的竖直偏移（0 = 与游戏顶边齐平）
const DOCK_GAP_Y: i32 = 0;
/// GetSystemMetrics 的索引：屏幕宽 / 高
const SM_CXSCREEN: i32 = 0;
const SM_CYSCREEN: i32 = 1;

// ---------- 置顶参数 ----------
/// SetWindowPos 的 hWndInsertAfter 特殊值
const HWND_TOPMOST: isize = -1;
const HWND_NOTOPMOST: isize = -2;
const SWP_NOSIZE: u32 = 0x0001;
const SWP_NOZORDER: u32 = 0x0004;
const SWP_NOMOVE: u32 = 0x0002;
const SWP_NOACTIVATE: u32 = 0x0010;
/// SWP_ASYNCWINDOWPOS：跨进程改窗口位置/层级时异步派发，
/// 不阻塞本程序界面线程（同步等对方窗口线程是「游戏一忙本窗口就假死」的常见原因）
const SWP_ASYNCWINDOWPOS: u32 = 0x4000;
/// GetWindowLongW 的索引：扩展样式
const GWL_EXSTYLE: i32 = -20;
/// 扩展样式位：窗口置顶
const WS_EX_TOPMOST: i32 = 0x0000_0008;

// ---------- 左上角印章位图（GDI 32 位 DIB） ----------

/// 印章像素：70×70，BGRA、自上而下，已与窗口底色(240,240,240)预合成
static STAMP_BGRA: &[u8] = include_bytes!("../assets/stamp_70.bgra");

type HDC = *mut c_void;
type HBITMAP = *mut c_void;

/// BITMAPINFOHEADER
#[repr(C)]
struct BitmapInfoHeader {
    bi_size: u32,
    bi_width: i32,
    bi_height: i32,
    bi_planes: u16,
    bi_bit_count: u16,
    bi_compression: u32,
    bi_size_image: u32,
    bi_x_pels: i32,
    bi_y_pels: i32,
    bi_clr_used: u32,
    bi_clr_important: u32,
}

/// BITMAPINFO（单色表占位；32 位真彩不用色表）
#[repr(C)]
struct BitmapInfo {
    header: BitmapInfoHeader,
    colors: [u32; 3],
}

#[link(name = "gdi32")]
extern "system" {
    fn CreateDIBSection(
        hdc: HDC,
        pbmi: *const BitmapInfo,
        usage: u32,
        bits: *mut *mut c_void,
        section: *mut c_void,
        offset: u32,
    ) -> HBITMAP;
}

/// 把编入二进制的印章像素做成 HBITMAP，供 STATIC(SS_BITMAP) 显示
unsafe fn make_stamp_bitmap() -> HBITMAP {
    let bi = BitmapInfo {
        header: BitmapInfoHeader {
            bi_size: std::mem::size_of::<BitmapInfoHeader>() as u32,
            bi_width: STAMP_SZ,
            // 负数 = 自上而下，行序与 STAMP_BGRA 一致
            bi_height: -STAMP_SZ,
            bi_planes: 1,
            bi_bit_count: 32,
            bi_compression: 0, // BI_RGB
            bi_size_image: (STAMP_SZ * STAMP_SZ * 4) as u32,
            bi_x_pels: 0,
            bi_y_pels: 0,
            bi_clr_used: 0,
            bi_clr_important: 0,
        },
        colors: [0; 3],
    };
    let mut bits: *mut c_void = std::ptr::null_mut();
    let hbm = CreateDIBSection(
        std::ptr::null_mut(),
        &bi as *const BitmapInfo,
        0, // DIB_RGB_COLORS
        &mut bits,
        std::ptr::null_mut(),
        0,
    );
    if !hbm.is_null() && !bits.is_null() && STAMP_BGRA.len() == (STAMP_SZ * STAMP_SZ * 4) as usize {
        std::ptr::copy_nonoverlapping(STAMP_BGRA.as_ptr(), bits as *mut u8, STAMP_BGRA.len());
    }
    hbm
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe fn set_text(hwnd: HWND, s: &str) {
    let w = wide(s);
    if !hwnd.is_null() {
        SetWindowTextW(hwnd, w.as_ptr());
    }
}

unsafe fn get_text(parent: HWND, id: i32) -> String {
    let h = GetDlgItem(parent, id);
    if h.is_null() {
        return String::new();
    }
    let mut buf = [0u16; 512];
    let n = GetWindowTextW(h, buf.as_mut_ptr(), buf.len() as i32);
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// 往日志框追加一行（黑色，普通信息）
#[allow(dead_code)]
pub fn append_log(hwnd: isize, text: &str) {
    append_log_colored(hwnd, text, 0x0000_0000);
}

/// 跨线程日志负载：工作线程只负责投递，真正插入 RichEdit 由 UI 线程执行
pub struct LogPayload {
    hwnd: isize,
    text: String,
    color: u32,
}

/// 往日志框追加一行并指定文字颜色（COLORREF = 0x00BBGGRR）
///
/// 跨线程安全且**不阻塞**：以前这里用 SendMessage 同步插 RichEdit，
/// 工作线程每轮多次调用会把 UI 消息泵堵死（表现为整个窗口卡死、点停止/叉号无反应），
/// 现在改成 PostMessageW 把负载投递到主窗口，由 UI 线程自己插入。
pub fn append_log_colored(hwnd: isize, text: &str, color: u32) {
    unsafe {
        if hwnd == 0 || IsWindow(hwnd as HWND) == 0 {
            return;
        }
        let main = MAIN_HWND.load(Ordering::SeqCst);
        if main != 0 {
            let p = Box::new(LogPayload {
                hwnd,
                text: text.to_string(),
                color,
            });
            let raw = Box::into_raw(p);
            if PostMessageW(
                main as HWND,
                WM_APP_LOG,
                raw as usize as WPARAM,
                0,
            ) != 0
            {
                return;
            }
            // 投递失败（消息队列满/窗口已销毁）→ 回收内存
            drop(Box::from_raw(raw));
        }
        // 兜底：主窗口尚未建立（WM_CREATE 早期）或投递失败时直接同线程插入
        append_log_sync(hwnd, text, color);
    }
}

/// UI 线程定时调用：把工作线程攒下的日志一次插进 RichEdit。
///
/// 三个要点，缺一个都会重新出现「点开始就卡死」：
///   1. 整批只在关闭重绘的状态下插入，最后再开重绘 → 一次重排而不是每行一次；
///   2. 连续重复的行合并成「（重复 ×N）」，节拍循环刷屏时行数骤降；
///   3. 裁剪只在超过上限时做一次，不是每行都测长度。
unsafe fn flush_logs() {
    let (lines, dropped) = logger::take_gui_lines();
    let h = logger::log_hwnd();
    if h == 0 {
        return;
    }
    let hw = h as HWND;
    if IsWindow(hw) == 0 {
        return;
    }
    if lines.is_empty() && dropped == 0 {
        return;
    }

    // 合并连续的重复行 + 同色合并
    let mut runs: Vec<(u32, String)> = Vec::new();
    let mut prev: Option<(u32, String)> = None;
    let mut rep: usize = 0;
    let mut flush_prev = |prev: Option<(u32, String)>, rep: usize, runs: &mut Vec<(u32, String)>| {
        if let Some((c, t)) = prev {
            let text = if rep > 0 {
                format!("{t}   （重复 ×{}）", rep + 1)
            } else {
                t
            };
            runs.push((c, text));
        }
    };
    for (text, color) in lines {
        if let Some((pc, pt)) = &prev {
            if *pc == color && *pt == text {
                rep += 1;
                continue;
            }
        }
        flush_prev(prev.take(), rep, &mut runs);
        rep = 0;
        prev = Some((color, text));
    }
    flush_prev(prev.take(), rep, &mut runs);
    if dropped > 0 {
        runs.push((
            logger::COLOR_WARN,
            format!("（日志过快，已丢弃 {dropped} 行）"),
        ));
    }

    let redraw_off = 0u32; // WM_SETREDRAW wParam=FALSE
    SendMessageW(hw, WM_SETREDRAW, redraw_off as usize, 0);

    // 裁剪：只在超上限时做一次
    let len = SendMessageW(hw, WM_GETTEXTLENGTH, 0, 0) as usize;
    if len > LOG_MAX_CHARS {
        let empty = wide("");
        SendMessageW(hw, EM_SETSEL, 0, LOG_TRIM_CHARS as isize);
        SendMessageW(hw, EM_REPLACESEL, 0, empty.as_ptr() as LPARAM);
    }

    for (color, text) in runs {
        let end = SendMessageW(hw, WM_GETTEXTLENGTH, 0, 0) as usize;
        SendMessageW(hw, EM_SETSEL, end, end as LPARAM);
        set_char_color(hw, color);
        let line = format!("{}\r\n", text.replace("\r\n", "\n").replace('\n', "\r\n"));
        let w = wide(&line);
        SendMessageW(hw, EM_REPLACESEL, 0, w.as_ptr() as LPARAM);
    }
    let redraw_on = 1u32;
    SendMessageW(hw, WM_SETREDRAW, redraw_on as usize, 0);
    SendMessageW(hw, EM_SCROLLCARET, 0, 0);
    // 重绘必须显式要求，否则插入的内容留在「无效但不重画」状态
    repaint_log(hw);
}

/// 真正把一行文字插进 RichEdit，只能在 UI 线程调用
pub fn append_log_sync(hwnd: isize, text: &str, color: u32) {
    unsafe {
        let h = hwnd as HWND;
        if h.is_null() || IsWindow(h) == 0 {
            return;
        }
        // 防无限增长：超 4 万字符就砍掉前 2 万
        let len = SendMessageW(h, WM_GETTEXTLENGTH, 0, 0) as usize;
        if len > 40_000 {
            let empty = wide("");
            SendMessageW(h, EM_SETSEL, 0, 20_000);
            SendMessageW(h, EM_REPLACESEL, 0, empty.as_ptr() as LPARAM);
        }
        let end = SendMessageW(h, WM_GETTEXTLENGTH, 0, 0) as usize;
        SendMessageW(h, EM_SETSEL, end, end as LPARAM);
        // 先设定插入点处的字符颜色，再插入文本，则该行按颜色渲染
        set_char_color(h, color);
        // 多行 RichEdit/EDIT 需要 \r\n 换行
        let line = format!("{}\r\n", text.replace("\r\n", "\n").replace('\n', "\r\n"));
        let w = wide(&line);
        SendMessageW(h, EM_REPLACESEL, 0, w.as_ptr() as LPARAM);
        SendMessageW(h, EM_SCROLLCARET, 0, 0);
        repaint_log(h);
    }
}

/// 设置插入点（当前选区）的文字颜色
unsafe fn set_char_color(h: HWND, color: u32) {
    let mut cf = CharFormatW {
        cb_size: std::mem::size_of::<CharFormatW>() as u32,
        mask: CFM_COLOR,
        effects: 0,
        height: 0,
        offset: 0,
        color,
        charset: 0,
        pitch_and_family: 0,
        face: [0u16; 32],
    };
    SendMessageW(
        h,
        EM_SETCHARFORMAT,
        SCF_SELECTION,
        &mut cf as *mut CharFormatW as LPARAM,
    );
}

/// 通知 RichEdit 立刻重画。
///
/// 为什么必须有它：批量插入时先 `WM_SETREDRAW(FALSE)` 关重绘、插完再开，
/// 但「开重绘」**不会**让控件自己把这段区域重画一遍 —— 尤其当新文本不足一屏、
/// EM_SCROLLCARET 也没引起滚动时，屏幕就一直是空白的旧画面，
/// 直到用户用鼠标框选（选区反色会强制重画）才突然看见文字。
unsafe fn repaint_log(h: HWND) {
    if h.is_null() || IsWindow(h) == 0 {
        return;
    }
    InvalidateRect(h, std::ptr::null(), 1);
    UpdateWindow(h);
}

unsafe fn set_status(text: &str) {
    let h = STATUS_HWND.load(Ordering::SeqCst);
    if h != 0 {
        set_text(h as HWND, text);
    }
}

/// 星期几中文
fn weekday_cn(dow: u16) -> &'static str {
    match dow {
        0 => "周日",
        1 => "周一",
        2 => "周二",
        3 => "周三",
        4 => "周四",
        5 => "周五",
        _ => "周六",
    }
}

/// 状态栏文字：运行中节拍倒计时 + 窗口锁定成功 + 吸附 + 置顶 + 当前年月日时分秒星期
fn status_text() -> String {
    // 运行中在状态栏显示「已点次数 + 已运行时长」。
    // 注意这里不去算「还剩几秒」：节拍周期在候选逻辑里会被临时改写（找牌窗口/刷新保护等
    // 都会复用 INTERVAL_MS），拿它算出来的倒计时是假数字，会误导人。
    let beat_txt = if crate::RUNNING.load(Ordering::SeqCst) {
        let start = crate::START_MS.load(Ordering::SeqCst);
        let seq = crate::TICK_SEQ.load(Ordering::SeqCst);
        let run_s = if start == 0 {
            0
        } else {
            crate::epoch_ms().saturating_sub(start) / 1000
        };
        format!("#{seq} 已运行 {run_s}s  ")
    } else {
        String::new()
    };
    let lock_txt = if LOCKED.load(Ordering::SeqCst) != 0 {
        "窗口锁定成功"
    } else {
        "未锁定窗口"
    };
    let dock_txt = if DOCK_ON.load(Ordering::SeqCst) {
        "吸附开"
    } else {
        "吸附关"
    };
    let top_txt = if TOPMOST_ON.load(Ordering::SeqCst) {
        "置顶开"
    } else {
        "置顶关"
    };
    let mut st = SysTime::default();
    unsafe { GetLocalTime(&mut st) };
    format!(
        "{beat_txt}{lock_txt}  {dock_txt}  {top_txt}  {:04}-{:02}-{:02} {:02}:{:02}:{:02} {}",
        st.year,
        st.month,
        st.day,
        st.hour,
        st.minute,
        st.second,
        weekday_cn(st.dow)
    )
}

/// 刷新状态栏（锁定状态/开关/时间）
unsafe fn refresh_status() {
    set_status(&status_text());
}

/// 按当前客户区重排「日志框 + 状态栏」：状态栏贴窗口底部、宽度延伸到控件右缘，
/// 日志框高度自适应填充上方剩余空间（窗口高度跟随游戏窗体变化时同步伸缩）
unsafe fn layout(hwnd: HWND) {
    let mut cr = Rect::default();
    if GetClientRect(hwnd, &mut cr) == 0 {
        return;
    }
    let cw = cr.right - cr.left;
    let ch = cr.bottom - cr.top;
    if cw <= 0 || ch <= 0 {
        return;
    }
    let content_w = cw - PAD * 2;
    let st_y = ch - STATUS_H - STATUS_BOTTOM_GAP;
    let log_h = (st_y - 8 - LOG_Y).max(80);
    let lg = logger::log_hwnd();
    if lg != 0 {
        SetWindowPos(
            lg as HWND,
            std::ptr::null_mut(),
            PAD,
            LOG_Y,
            content_w,
            log_h,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
    let st = STATUS_HWND.load(Ordering::SeqCst);
    if st != 0 {
        SetWindowPos(
            st as HWND,
            std::ptr::null_mut(),
            PAD,
            st_y,
            content_w,
            STATUS_H,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

/// 按「运行中/空闲」切换按钮可用性与状态文字
unsafe fn set_ui_running(running: bool) {
    let start = BTN_START.load(Ordering::SeqCst) as HWND;
    let stop = BTN_STOP.load(Ordering::SeqCst) as HWND;
    if !start.is_null() {
        EnableWindow(start, if running { 0 } else { 1 });
    }
    if !stop.is_null() {
        EnableWindow(stop, if running { 1 } else { 0 });
    }
    refresh_status();
}

/// 工作线程（点击循环 / 自检）结束时回收界面状态。
/// 注意：这里跑在工作线程上，绝不能直接碰控件（EnableWindow/SetWindowText 都是跨线程同步消息），
/// 统一 PostMessage 回 UI 线程执行。
pub fn worker_finished() {
    BUSY.store(false, Ordering::SeqCst);
    unsafe {
        let main = MAIN_HWND.load(Ordering::SeqCst);
        if main != 0 && IsWindow(main as HWND) != 0 {
            PostMessageW(main as HWND, WM_APP_UI, UI_EVT_FINISHED, 0);
        }
    }
}

unsafe fn mk(parent: HWND, class: &str, text: &str, style: u32, x: i32, y: i32, w: i32, h: i32, id: u16) -> HWND {
    let cls = wide(class);
    let txt = wide(text);
    let ctl = CreateWindowExW(
        0,
        cls.as_ptr(),
        txt.as_ptr(),
        WS_CHILD | WS_VISIBLE | style,
        x,
        y,
        w,
        h,
        parent,
        id as usize as HMENU,
        GetModuleHandleW(std::ptr::null()),
        std::ptr::null_mut(),
    );
    let font = FONT.load(Ordering::SeqCst);
    if !ctl.is_null() && font != 0 {
        SendMessageW(ctl, WM_SETFONT, font as WPARAM, 1);
    }
    ctl
}

unsafe fn build_controls(hwnd: HWND) {
    let face = wide("Microsoft YaHei UI");
    let f = CreateFontW(
        -15, 0, 0, 0, FW_NORMAL, 0, 0, 0, DEFAULT_CHARSET, 0, 0, CLEARTYPE_QUALITY, 0, face.as_ptr(),
    );
    FONT.store(f as isize, Ordering::SeqCst);

    // 左上角印章（「謐」白文印）：正方形，高度恰好占满两排按钮
    let stamp = mk(hwnd, "STATIC", "", SS_BITMAP, STAMP_X, STAMP_Y, STAMP_SZ, STAMP_SZ, ID_STAMP as u16);
    let hbm = make_stamp_bitmap();
    if !stamp.is_null() && !hbm.is_null() {
        SendMessageW(stamp, STM_SETIMAGE, IMAGE_BITMAP, hbm as LPARAM);
    }


    // 行 1：开始 / 停止 / 清空 + 间隔（秒）；六个按钮统一 96x30，两行三列对齐
    let b = mk(hwnd, "BUTTON", "开始", BS_PUSHBUTTON | WS_TABSTOP, 12 + STAMP_SHIFT, 14, BTN_W, BTN_H, ID_START);
    BTN_START.store(b as isize, Ordering::SeqCst);
    let b = mk(hwnd, "BUTTON", "停止", BS_PUSHBUTTON | WS_TABSTOP, 116 + STAMP_SHIFT, 14, BTN_W, BTN_H, ID_STOP);
    BTN_STOP.store(b as isize, Ordering::SeqCst);
    EnableWindow(b, 0);
    mk(hwnd, "BUTTON", "清空", BS_PUSHBUTTON | WS_TABSTOP, 220 + STAMP_SHIFT, 14, BTN_W, BTN_H, ID_CLEAR);
    mk(hwnd, "STATIC", "间隔(秒)", 0, 332 + STAMP_SHIFT, 20, 58, 22, 0);
    mk(hwnd, "EDIT", "5", WS_BORDER | ES_AUTOHSCROLL | WS_TABSTOP, 396 + STAMP_SHIFT, 17, 46, 24, ID_INTERVAL as u16);

    // 行 2：置顶 / 吸附 / 记时（点一次开始计时，再点暂停）
    mk(hwnd, "BUTTON", "置顶 开", BS_PUSHBUTTON | WS_TABSTOP, 12 + STAMP_SHIFT, 54, BTN_W, BTN_H, ID_TOPMOST);
    mk(hwnd, "BUTTON", "吸附 开", BS_PUSHBUTTON | WS_TABSTOP, 116 + STAMP_SHIFT, 54, BTN_W, BTN_H, ID_DOCK);
    let cb = mk(hwnd, "BUTTON", "记时 开始", BS_PUSHBUTTON | WS_TABSTOP, 220 + STAMP_SHIFT, 54, BTN_W, BTN_H, ID_CLOCK_BTN);
    BTN_CLOCK.store(cb as isize, Ordering::SeqCst);
    let ct = mk(hwnd, "STATIC", "0.0 秒", 0, 332 + STAMP_SHIFT, 58, 120, 22, ID_CLOCK_TXT as u16);
    CLOCK_TXT.store(ct as isize, Ordering::SeqCst);

    // 日志框（多行只读；用 RichEdit 以支持按行着色：异常红、警告橙）
    let log = mk(
        hwnd,
        RICHEDIT_CLASS,
        "",
        WS_BORDER | WS_VSCROLL | ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL | WS_TABSTOP,
        12,
        LOG_Y,
        CONTENT_W,
        LOG_H,
        ID_LOG as u16,
    );
    logger::set_gui_log(log as isize);

    // 状态栏：锁定状态 / 吸附 / 置顶 / 当前时间（宽度与上方控件右缘对齐，贴窗口底部）
    let st = mk(hwnd, "STATIC", "", 0, PAD, LOG_Y + LOG_H + 8, CONTENT_W - 4, STATUS_H, ID_STATUS as u16);
    STATUS_HWND.store(st as isize, Ordering::SeqCst);
    refresh_status();
    layout(hwnd);

    logger::log(format!("魔精灵点击器 v{}  —  单文件版，无需任何外部文件", env!("CARGO_PKG_VERSION")));
    logger::log("用法：启动即自动查找并锁定游戏窗口，确认「间隔(秒)」后点「开始」即可。");
    logger::log("提示：循环为无限次数，点「停止」可随时中断；点「记时 开始」开始计时。");
    // 临时：颜色自检（MOJINGLING_COLORTEST=1）
    if std::env::var_os("MOJINGLING_COLORTEST").is_some() {
        logger::log_err("[error] 颜色自检：异常行应为红色");
        logger::log_warn("[warn] 颜色自检：警告行应为橙色");
        logger::log("[info] 颜色自检：普通行应为黑色");
    }
}

/// 「开始」按钮
unsafe fn on_start(hwnd: HWND) {
    if BUSY.load(Ordering::SeqCst) {
        return;
    }
    // 界面已去掉「窗口标题」输入框：统一按记住的关键字定位（兼容游戏重启后句柄变化）
    let title = {
        let kw = KEYWORD.lock().map(|g| g.clone()).unwrap_or_default();
        if kw.trim().is_empty() {
            DEFAULT_KEYWORD.to_string()
        } else {
            kw
        }
    };
    let interval_s = get_text(hwnd, ID_INTERVAL);

    let interval: u64 = match interval_s.trim().parse() {
        Ok(v) if v > 0 => v,
        _ => {
            let t = wide("间隔必须是大于 0 的整数。");
            let c = wide("参数错误");
            MessageBoxW(hwnd, t.as_ptr(), c.as_ptr(), MB_ICONWARNING);
            return;
        }
    };
    // 界面已去掉「次数」输入框：固定无限循环，靠「停止」中断
    let times: u32 = 0;
    // 界面已去掉「阈值」输入框：固定回退值 0.78
    let threshold: f64 = 0.78;

    let cfg = crate::Config {
        interval_secs: interval,
        times,
        window_title: title.trim().to_string(),
        threshold,
        ..Default::default()
    };

    BUSY.store(true, Ordering::SeqCst);
    // 运行标志必须在这里（UI 线程、spawn 之前）置位：
    // 以前放在工作线程第一行，用户刚点开始就点停止会被线程里的 store(true) 覆盖，停止彻底失灵。
    crate::RUNNING.store(true, Ordering::SeqCst);
    set_ui_running(true);
    // 记时与点击节拍联动：点「开始」即刻启动倒计时（写在状态栏，不碰秒表标签）
    crate::TICK_SEQ.store(0, Ordering::SeqCst);
    crate::INTERVAL_MS.store(interval.saturating_mul(1000), Ordering::SeqCst);
    crate::LAST_TICK_MS.store(crate::epoch_ms(), Ordering::SeqCst);
    SetTimer(hwnd, TIMER_BEAT, CLOCK_MS, std::ptr::null_mut());
    refresh_beat();
    logger::log(format!(
        "──────── 开始：窗口「{}」间隔 {} 秒，次数 {}，阈值 {:.2} ────────",
        if cfg.window_title.is_empty() { "前台窗口".to_string() } else { cfg.window_title.clone() },
        cfg.interval_secs,
        if cfg.times == 0 { "无限".to_string() } else { cfg.times.to_string() },
        cfg.threshold
    ));
    logger::log(format!(
        "点击方式：{}（{}）",
        if cfg.bg_click { "后台消息 PostMessage" } else { "前台物理鼠标 SetCursorPos+mouse_event" },
        if cfg.bg_click { "鼠标不动、不抢前台" } else { "会移动真实鼠标" }
    ));
    crate::spawn_worker(cfg);
}

/// 「停止」按钮：置停止标志 + 立刻回收界面状态，不等工作线程跑完当前轮
unsafe fn on_stop(hwnd: HWND) {
    let was_running = crate::RUNNING.swap(false, Ordering::SeqCst);
    // 倒计时定时器立刻停掉，秒表不受影响（两者独立）
    KillTimer(hwnd, TIMER_BEAT);
    // 界面立刻回到空闲态：开始可点、停止置灰。以前要等 worker 跑完当前轮才恢复，
    // 用户看到按钮纹丝不动就以为「点了没反应」。
    BUSY.store(false, Ordering::SeqCst);
    set_ui_running(false);
    refresh_beat();
    refresh_clock();
    if was_running {
        logger::log("[ui] 已停止：循环会在 100ms 内退出当前等待。");
    } else {
        logger::log("[ui] 当前未在运行。");
    }
}

/// 「自检」按钮
/// 把文本写进指定子控件（按控件 ID 定位）
unsafe fn set_ctl_text(parent: HWND, id: i32, text: &str) {
    let w = wide(text);
    SetWindowTextW(GetDlgItem(parent, id), w.as_ptr());
}

// ---------- 吸附：把本程序窗口贴到游戏窗口右侧 ----------

/// 把本程序主窗口吸附到 `game` 窗口右侧（右侧空间不足时退到左侧）。
/// 返回实际落点 (x, y)；取不到目标矩形时返回 None。
unsafe fn dock_to_game(hwnd: HWND, game: HWND) -> Option<(i32, i32)> {
    if hwnd.is_null() || hwnd == game {
        return None;
    }
    // 拖动/缩放进行中：绝不移动窗口（系统移动循环里 SetWindowPos 会让窗口「自己跑」）
    if DRAGGING.load(Ordering::SeqCst) {
        return None;
    }
    // 外框矩形：用于记录跟随基准
    let (gl, gt, gr, gb) = crate::capture::window_rect(game)?;
    // 可见边界矩形：用于真正算贴合位置（排除 Win10/11 不可见的缩放边框）
    let (vgl, vgt, vgr, vgb) = crate::capture::visual_rect(game).unwrap_or((gl, gt, gr, gb));
    let (sl, st, sr, sb) = match crate::capture::window_rect(hwnd) {
        Some(r) => r,
        None => (0, 0, WIN_W, WIN_H),
    };
    // 本窗口「外框 → 可见内容」的偏移：SetWindowPos 定位的是外框，必须扣掉自身边框
    let (vl, vt, vr, vb) = crate::capture::visual_rect(hwnd).unwrap_or((sl, st, sr, st + WIN_H));
    let inset_l = vl - sl;
    let inset_t = vt - st;
    let self_vis_w = (vr - vl).max(1);
    let self_vis_h = (vb - vt).max(1);
    let self_w = if sr - sl > 0 { sr - sl } else { WIN_W };
    // 高度对齐游戏窗体：本窗口「可见高度」= 目标「可见高度」
    // 注意 SetWindowPos 传的是外框高度，补偿量必须是「外框高 - 可见高」而非「可见高 - 客户区高」
    let self_outer_h = if sb - st > 0 { sb - st } else { WIN_H };
    let frame_h = (self_outer_h - self_vis_h).max(0);
    let want_vis_h = (vgb - vgt).max(240);
    let want_outer_h = want_vis_h + frame_h;
    let mut sw = GetSystemMetrics(SM_CXSCREEN);
    let mut sh = GetSystemMetrics(SM_CYSCREEN);
    if sw <= 0 {
        sw = 1920;
    }
    if sh <= 0 {
        sh = 1080;
    }

    // 首选：贴在目标右侧，本窗口「可见左边缘」= 目标「可见右边界」+ DOCK_GAP_X
    let mut x = vgr + DOCK_GAP_X - inset_l;
    let mut side = "右侧";
    if x + inset_l + self_vis_w > sw {
        // 右侧放不下 → 改贴左侧
        let lx = vgl - DOCK_GAP_X - self_vis_w - inset_l;
        side = "左侧";
        x = if lx >= 0 { lx } else { (sw - self_w).max(0) };
    }
    // 竖直方向：可见上边缘对齐目标可见上边缘，并保证不超出屏幕底部（按新高度算）
    let y = (vgt + DOCK_GAP_Y - inset_t)
        .min(sh - want_vis_h - inset_t)
        .max(0);

    if !crate::capture::move_window_to(hwnd, x, y) {
        logger::log(format!("[dock] 吸附到{side}失败（可能权限受限）"));
        return None;
    }
    // 高度拉到与游戏窗体等高（会触发 WM_SIZE → 日志框/状态栏重排）
    // 只有偏差超过 2px 才真的动尺寸：否则每轮吸附都把取整误差写回去，尺寸在 1px 之间来回跳，
    // WM_SIZE 被反复触发，界面跟着抖到卡死。
    if (want_outer_h - self_outer_h).abs() > 2 {
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            0,
            0,
            self_w,
            want_outer_h,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
    if let Ok(mut g) = DOCK_REF.lock() {
        *g = (gl, gt, gr, gb);
    }
    logger::log(format!(
        "[dock] 已吸附到目标窗口{side}：本窗口 ({x},{y})，目标可见边界 ({vgl},{vgt})-({vgr},…)，视觉缝隙 {DOCK_GAP_X}px（已扣除系统不可见边框）"
    ));
    Some((x, y))
}

// ---------- 置顶：让游戏窗口与本程序窗口一起保持最前 ----------

/// 设置 / 取消单个窗口的置顶状态（不改位置、尺寸、激活状态）
unsafe fn set_topmost(h: HWND, on: bool) {
    if h.is_null() || IsWindow(h) == 0 {
        return;
    }
    let after = (if on { HWND_TOPMOST } else { HWND_NOTOPMOST }) as usize as HWND;
    SetWindowPos(
        h,
        after,
        0,
        0,
        0,
        0,
        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS,
    );
}

unsafe fn is_topmost(h: HWND) -> bool {
    if h.is_null() || IsWindow(h) == 0 {
        return false;
    }
    (GetWindowLongW(h, GWL_EXSTYLE) & WS_EX_TOPMOST) != 0
}

/// 把游戏窗口与本窗口一起置顶；本窗口最后设置，保证压在游戏窗口之上
unsafe fn apply_topmost(me: HWND, game: HWND) -> bool {
    if !TOPMOST_ON.load(Ordering::SeqCst) {
        return false;
    }
    set_topmost(game, true);
    set_topmost(me, true);
    true
}

/// 巡检：游戏窗口或本窗口被系统 / 其它程序取消置顶时补回来
unsafe fn keep_topmost(me: HWND, game: HWND) {
    if !TOPMOST_ON.load(Ordering::SeqCst) {
        return;
    }
    if !is_topmost(game) || !is_topmost(me) {
        apply_topmost(me, game);
    }
}

/// 按关键字查找窗口，并把首个命中的句柄写进左上角「窗口标题」edit。
/// 返回命中的句柄（未命中返回 None）。
unsafe fn lookup_and_show(hwnd: HWND, key: &str) -> Option<isize> {
    lookup_and_show_q(hwnd, key, false)
}

/// 同上，`quiet=true` 时未命中不写日志（供后台轮询使用，避免刷屏）
unsafe fn lookup_and_show_q(hwnd: HWND, key: &str, quiet: bool) -> Option<isize> {
    let mut key = key.trim().to_string();
    // edit 里已经是句柄文本时，用上次记住的关键字重查
    if key.to_lowercase().starts_with("0x") {
        key = KEYWORD.lock().map(|g| g.clone()).unwrap_or_default();
    }
    if key.is_empty() {
        logger::log("[find] 请先在「窗口标题」里填关键字（标题片段 / 类名 / 进程 exe 名均可）。");
        return None;
    }
    if let Ok(mut g) = KEYWORD.lock() {
        *g = key.clone();
    }

    // 关键：游戏窗体的标题是房间号（如「锁0724」），不含游戏名，
    // 因此默认目标改用「进程 WeChatAppEx + 类名 Chrome_WidgetWin_0」识别，
    // 关键字命中只作为补充来源。
    let mut hits: Vec<crate::capture::WinInfo> = Vec::new();
    if let Some(g) = crate::capture::find_game_window() {
        hits.push(g);
    }
    for w in crate::capture::find_windows(&key) {
        if !hits.iter().any(|x| x.hwnd == w.hwnd) {
            hits.push(w);
        }
    }
    if hits.is_empty() {
        if quiet {
            return None;
        }
        logger::log(format!(
            "[find] 没有找到含「{key}」的可见窗口。游戏本体判定＝进程 WeChatAppEx.exe + 类名 Chrome_WidgetWin_0。"
        ));
        refresh_status();
        return None;
    }

    logger::log(format!("[find] 关键字「{key}」命中 {} 个窗口：", hits.len()));
    for (i, w) in hits.iter().enumerate() {
        let exe = if w.exe.is_empty() { "?" } else { w.exe.as_str() };
        logger::log(format!(
            "  #{i} HWND=0x{:X}（十进制 {}）  标题=\"{}\"  类=\"{}\"  进程={exe}(pid {})  客户区={}x{}",
            w.hwnd as usize,
            w.hwnd as isize,
            w.title,
            w.class,
            w.pid,
            w.cw,
            w.ch
        ));
    }

    let first = &hits[0];
    let h = first.hwnd as isize;
    LOCKED.store(h, Ordering::SeqCst);
    // 尺寸锁定：记录游戏窗体当前外框尺寸，交给 sizelock 模块的后台线程持续钉住
    let locked_size = crate::sizelock::lock_current(first.hwnd);
    // 把目标窗体挪到屏幕左上角 (10,10)，尺寸不变
    let (tx, ty) = TARGET_POS;
    let moved = crate::capture::move_window_to(first.hwnd, tx, ty);
    if moved {
        logger::log(format!("[move] 已把目标窗口移到屏幕 ({tx},{ty})"));
    } else {
        logger::log(format!("[move] 移动目标窗口到 ({tx},{ty}) 失败（可能权限或窗口受限）"));
    }
    // 吸附：把本程序窗口贴到目标窗口右侧（开关关掉时不动）
    let docked = if DOCK_ON.load(Ordering::SeqCst) {
        dock_to_game(hwnd, first.hwnd)
    } else {
        None
    };
    // 置顶：游戏窗口与本窗口一起保持最前（开关关掉时不动）
    let topped = apply_topmost(hwnd, first.hwnd);
    // 移动后重新取一次窗口信息：最小化的窗口客户区在还原前是 0x0，还原后才是真实尺寸
    let (title, cw, ch) = match crate::capture::describe(first.hwnd) {
        Some(w) => (w.title, w.cw, w.ch),
        None => (first.title.clone(), first.cw, first.ch),
    };
    let dock_txt = match docked {
        Some((dx, dy)) => format!("  吸附({dx},{dy})"),
        None => {
            if DOCK_ON.load(Ordering::SeqCst) {
                "  未吸附".to_string()
            } else {
                "  吸附已关".to_string()
            }
        }
    };
    let lock_txt = match locked_size {
        Some((w, hh)) => {
            // --size 指定了强制客户区尺寸时，状态栏把靶尺寸一并显示出来，
            // 免得看到外框数字对不上就以为没生效
            let forced = match crate::sizelock::force_client() {
                Some((fcw, fch)) => format!("(客户区{fcw}x{fch})"),
                None => String::new(),
            };
            if crate::sizelock::is_on() {
                format!("  锁尺寸{w}x{hh}{forced}")
            } else {
                format!("  尺寸{w}x{hh}(未锁)")
            }
        }
        None => "  锁尺寸失败".to_string(),
    };
    // 状态栏只显示「窗口锁定成功 + 吸附 + 置顶 + 时间」，细节写日志
    logger::log(format!(
        "[status] 已把句柄 0x{:X}（十进制 {h}）写入状态栏：「{title}」 {cw}x{ch}{}{dock_txt}{lock_txt}{}",
        h as usize,
        if moved { format!("  已移至({tx},{ty})") } else { "  移动失败".to_string() },
        if topped { "  置顶" } else { "  仅本窗口" }
    ));
    refresh_status();
    Some(h)
}

// ---------- 拖动护栏：用户拖动窗口时不许任何代码去搬它 ----------

/// 用户拖动/缩放开始时进入静默让路状态：冻结所有自动搬窗行为。
/// 注意：这里**不写日志**。拖动是高频日常操作，刷日志只会把真正有用的信息顶掉。
unsafe fn on_enter_sizemove(hwnd: HWND) {
    if DRAGGING.swap(true, Ordering::SeqCst) {
        return;
    }
    // 拖动期间把吸附冷却刷新，松手后也不会立刻被搬走
    LAST_DOCK_MS.store(crate::epoch_ms(), Ordering::SeqCst);
    let _ = hwnd;
}

/// 退出拖动/缩放：恢复正常，并检查窗口是否被放到屏幕外（同样不写日志）
unsafe fn on_exit_sizemove(hwnd: HWND) {
    let _was = DRAGGING.swap(false, Ordering::SeqCst);
    LAST_DOCK_MS.store(crate::epoch_ms(), Ordering::SeqCst);
    // 松手后自检一次：被拖到屏幕外就拉回来，否则用户会以为「窗口不见了」
    guard_onscreen(hwnd);
}

/// 把本窗口从屏幕外拉回可见区域。
/// 判定依据：标题栏横向中点 + 顶部 12px 那一点是否落在虚拟屏幕内——这一点是用户唯一能抓回窗口的地方。
unsafe fn guard_onscreen(hwnd: HWND) {
    if hwnd.is_null() || IsWindow(hwnd) == 0 || IsIconic(hwnd) != 0 {
        return;
    }
    let (l, t, r, b) = match crate::capture::window_rect(hwnd) {
        Some(v) => v,
        None => return,
    };
    if r - l <= 0 || b - t <= 0 {
        return;
    }
    let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
    let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
    let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1);
    let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1);
    let probe_x = (l + r) / 2;
    let probe_y = t + 12;
    let reachable =
        probe_x >= vx && probe_x < vx + vw && probe_y >= vy && probe_y < vy + vh;
    if reachable {
        OFFSCREEN_HITS.store(0, Ordering::SeqCst);
        return;
    }
    let hits = OFFSCREEN_HITS.fetch_add(1, Ordering::SeqCst) + 1;
    let w = (r - l).min(vw);
    let h = (b - t).min(vh);
    let nx = vx + (vw - w) / 2;
    let ny = vy + (vh - h) / 3;
    logger::log_warn(format!(
        "[guard] 本窗口已跑到屏幕外（({l},{t})-({r},{b})，第 {hits} 次），已拉回 ({nx},{ny})"
    ));
    crate::capture::move_window_to(hwnd, nx, ny);
    LAST_DOCK_MS.store(crate::epoch_ms(), Ordering::SeqCst);
    // 连续多次跑出屏幕：大概率是吸附算出来的位置本身有问题，直接关掉吸附让它老实待着
    if hits >= 3 && DOCK_ON.load(Ordering::SeqCst) {
        DOCK_ON.store(false, Ordering::SeqCst);
        set_ctl_text(hwnd, ID_DOCK as i32, "吸附 关");
        logger::log_warn("[guard] 连续多次跑到屏幕外，已自动关闭吸附，可手动点「吸附」重新开启");
    }
}

/// 只有目标矩形发生变化时才移动，避免和用户手动拖动本窗口打架。
/// 跟随吸附：游戏窗口被拖动/缩放后，把本窗口重新贴回它旁边。
unsafe fn follow_dock(hwnd: HWND, game: HWND) {
    if !DOCK_ON.load(Ordering::SeqCst) {
        return;
    }
    // 用户正在拖窗口：这时候任何 SetWindowPos 都会和系统的移动循环打架，
    // 表现就是「手一拖，窗口自己窜出去」，所以拖动期间一律不动。
    if DRAGGING.load(Ordering::SeqCst) {
        return;
    }
    // 冷却：跟随吸附最多 300ms 动一次，避免 SetWindowPos ↔ WM_SIZE 高频互踢
    let now = crate::epoch_ms();
    if now.saturating_sub(LAST_DOCK_MS.load(Ordering::SeqCst)) < 300 {
        return;
    }
    let rect = match crate::capture::window_rect(game) {
        Some(r) => r,
        None => return,
    };
    let last = DOCK_REF.lock().map(|g| *g).unwrap_or((0, 0, 0, 0));
    if rect == last {
        return;
    }
    LAST_DOCK_MS.store(now, Ordering::SeqCst);
    dock_to_game(hwnd, game);
}

/// 后台自动扫描：未锁定 / 已锁定的窗口被关掉时，按关键字重新查找并锁定。
/// 目标：游戏晚于程序启动也能自动接管，无需手动点按钮。
unsafe fn autoscan(hwnd: HWND) {
    // 用户正在拖本窗口：不扫描、不吸附、不置顶，全部让给用户
    if DRAGGING.load(Ordering::SeqCst) {
        return;
    }
    let cur = LOCKED.load(Ordering::SeqCst);
    if cur != 0 {
        if IsWindow(cur as HWND) != 0 {
            follow_dock(hwnd, cur as HWND);
            keep_topmost(hwnd, cur as HWND);
            return; // 已锁定且窗口健在
        }
        LOCKED.store(0, Ordering::SeqCst);
        crate::sizelock::clear_target();
        logger::log("[auto] 已锁定的窗口已关闭，继续扫描新窗口…");
    }
    let mut key = KEYWORD.lock().map(|g| g.clone()).unwrap_or_default();
    if key.is_empty() || key.to_lowercase().starts_with("0x") {
        key = DEFAULT_KEYWORD.to_string();
    }
    lookup_and_show_q(hwnd, &key, true);
}

/// 「吸附」按钮：开关吸附（开＝锁定后自动把本窗口贴到游戏窗口右侧并跟随）
unsafe fn on_dock(hwnd: HWND) {
    let on = !DOCK_ON.load(Ordering::SeqCst);
    DOCK_ON.store(on, Ordering::SeqCst);
    set_ctl_text(hwnd, ID_DOCK as i32, if on { "吸附 开" } else { "吸附 关" });
    refresh_status();
    if !on {
        logger::log("[dock] 吸附已关闭，本窗口可自由拖动");
        return;
    }
    let cur = LOCKED.load(Ordering::SeqCst);
    if cur != 0 && IsWindow(cur as HWND) != 0 {
        match dock_to_game(hwnd, cur as HWND) {
            Some((x, y)) => logger::log(format!("[dock] 吸附已开启，本窗口贴到 ({x},{y})")),
            None => logger::log("[dock] 吸附已开启，但取不到目标窗口矩形"),
        }
    } else {
        logger::log("[dock] 吸附已开启，锁定目标窗口后会自动贴靠");
    }
}

/// 「置顶」按钮：开关「游戏窗口 + 本窗口」的置顶
unsafe fn on_topmost(hwnd: HWND) {
    let on = !TOPMOST_ON.load(Ordering::SeqCst);
    TOPMOST_ON.store(on, Ordering::SeqCst);
    set_ctl_text(hwnd, ID_TOPMOST as i32, if on { "置顶 开" } else { "置顶 关" });
    refresh_status();
    let cur = LOCKED.load(Ordering::SeqCst);
    let game = cur as HWND;
    if !on {
        set_topmost(game, false);
        set_topmost(hwnd, false);
        logger::log("[top] 置顶已关闭，两个窗口恢复正常层叠");
        return;
    }
    if cur != 0 && IsWindow(game) != 0 {
        apply_topmost(hwnd, game);
        logger::log("[top] 置顶已开启：游戏窗口与本窗口都保持最前");
    } else {
        set_topmost(hwnd, true);
        logger::log("[top] 置顶已开启（本窗口），锁定游戏窗口后会一起置顶");
    }
}

// ---------- 记时 ----------

/// 当前记时毫秒数（运行中实时递增，暂停后冻结）
fn clock_now_ms() -> u64 {
    let base = CLOCK_BASE_MS.load(Ordering::SeqCst);
    if !CLOCK_RUN.load(Ordering::SeqCst) {
        return base;
    }
    let started = CLOCK_START.lock().ok().and_then(|g| *g);
    match started {
        Some(t) => base + t.elapsed().as_millis() as u64,
        None => base,
    }
}

/// 毫秒转「秒」文本，保留 1 位小数；超 1 分钟显示「分:秒.十分位」
fn clock_text(ms: u64) -> String {
    let secs = ms as f64 / 1000.0;
    if secs >= 60.0 {
        format!("{}:{:04.1} 秒", ms / 60_000, (ms % 60_000) as f64 / 1000.0)
    } else {
        format!("{secs:.1} 秒")
    }
}

/// 把秒表当前值刷新到「记时」旁边那个 STATIC 上。
/// 只负责秒表——节拍倒计时改由 `refresh_beat()` 写到状态栏，
/// 二者不再抢同一个标签，也不会互相覆盖（这是原来「秒表跑到计时标签里」的根因）。
unsafe fn refresh_clock() {
    let h = CLOCK_TXT.load(Ordering::SeqCst) as HWND;
    if h.is_null() {
        return;
    }
    set_text(h, &clock_text(clock_now_ms()));
}

/// 节拍倒计时刷新：实在状态栏，运行中每 100ms 调一次
unsafe fn refresh_beat() {
    refresh_status();
}

/// 「记时」按钮：点一次从 0 开始计时并实时显示，再点停止（显示冻结值），再点重新计时
unsafe fn on_clock(hwnd: HWND) {
    let btn = BTN_CLOCK.load(Ordering::SeqCst) as HWND;
    if CLOCK_RUN.load(Ordering::SeqCst) {
        let total = clock_now_ms();
        CLOCK_RUN.store(false, Ordering::SeqCst);
        KillTimer(hwnd, TIMER_CLOCK);
        if let Ok(mut g) = CLOCK_START.lock() {
            *g = None;
        }
        CLOCK_BASE_MS.store(total, Ordering::SeqCst);
        if !btn.is_null() {
            set_text(btn, "记时 开始");
        }
        refresh_clock();
        logger::log(format!("[clock] 记时停止，用时 {}", clock_text(total)));
    } else {
        CLOCK_BASE_MS.store(0, Ordering::SeqCst);
        if let Ok(mut g) = CLOCK_START.lock() {
            *g = Some(Instant::now());
        }
        CLOCK_RUN.store(true, Ordering::SeqCst);
        if !btn.is_null() {
            set_text(btn, "记时 停止");
        }
        SetTimer(hwnd, TIMER_CLOCK, CLOCK_MS, std::ptr::null_mut());
        refresh_clock();
        logger::log("[clock] 记时开始");
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER => {
            match wp {
                TIMER_SCAN => autoscan(hwnd),
                TIMER_CLOCK => refresh_clock(),
                TIMER_BEAT => refresh_beat(),
                TIMER_STATUS => {
                    refresh_status();
                    // 每秒自检一次：窗口是否被搬到屏幕外（拖动/吸附都可能造成）
                    guard_onscreen(hwnd);
                }
                // 日志批量落地：一次插入，代价可控
                TIMER_FLUSH => flush_logs(),
                _ => {}
            }
            0
        }
        WM_ENTERSIZEMOVE => {
            // 用户按住标题栏/边框：系统进入模态移动循环，这里必须冻结一切自动搬窗与物理点击
            on_enter_sizemove(hwnd);
            0
        }
        WM_EXITSIZEMOVE => {
            on_exit_sizemove(hwnd);
            0
        }
        WM_CREATE => {
            // 先登记主窗口句柄：build_controls 里打日志要靠它异步投递
            MAIN_HWND.store(hwnd as isize, Ordering::SeqCst);
            build_controls(hwnd);
            // 启动横幅之类先落地一次，剩下的交给定时器批量刷
            flush_logs();
            SetTimer(hwnd, TIMER_FLUSH, FLUSH_MS, std::ptr::null_mut());
            0
        }
        WM_APP_LOG => {
            // 工作线程投递过来的日志行，在这里（UI 线程）真正插入 RichEdit
            if wp != 0 {
                let p = Box::from_raw(wp as *mut LogPayload);
                append_log_sync(p.hwnd, &p.text, p.color);
            }
            0
        }
        WM_APP_UI => {
            if wp == UI_EVT_FINISHED {
                set_ui_running(false);
                KillTimer(hwnd, TIMER_BEAT);
                refresh_beat();
                refresh_clock();
            }
            0
        }
        WM_SIZE => {
            // 最小化时宽/高为 0，此时重排会把控件算成负尺寸，直接跳过
            let w = (lp & 0xFFFF) as i32;
            let h = ((lp >> 16) & 0xFFFF) as i32;
            if w <= 0 || h <= 0 {
                return 0;
            }
            // 重入保护：吸附里改窗口尺寸会再次派发 WM_SIZE，若不挡住就会无限递归，
            // UI 线程死在递归里 → 整个窗口卡死、停止按钮和右上角叉号全部无响应。
            if !IN_SIZE.swap(true, Ordering::SeqCst) {
                // 窗口尺寸变化后重排日志框/状态栏（日志区自适应、状态栏贴底）
                layout(hwnd);
                IN_SIZE.store(false, Ordering::SeqCst);
            }
            0
        }
        WM_COMMAND => {
            let id = (wp & 0xFFFF) as u16;
            match id {
                ID_START => on_start(hwnd),
                ID_STOP => on_stop(hwnd),
                ID_DOCK => on_dock(hwnd),
                ID_TOPMOST => on_topmost(hwnd),
                ID_CLOCK_BTN => on_clock(hwnd),
                ID_CLEAR => {
                    let h = logger::log_hwnd();
                    if h != 0 {
                        let hw = h as HWND;
                        unsafe {
                            if IsWindow(hw) != 0 {
                                // 1) 先把还没插进控件的日志丢掉，否则清完它们又补回来
                                let _ = logger::take_gui_lines();
                                // 2) 关重绘再删，避免 RichEdit 逐段重排闪烁
                                SendMessageW(hw, WM_SETREDRAW, 0, 0);
                                let empty = wide("");
                                SendMessageW(hw, EM_SETSEL, 0, -1);
                                SendMessageW(hw, EM_REPLACESEL, 0, empty.as_ptr() as LPARAM);
                                // 3) 复位默认字符格式：否则下一行会沿用被删掉那段（例如灰色/红色）
                                let mut cf = CharFormatW {
                                    cb_size: std::mem::size_of::<CharFormatW>() as u32,
                                    mask: CFM_COLOR,
                                    effects: 0,
                                    height: 0,
                                    offset: 0,
                                    color: logger::COLOR_NORMAL,
                                    charset: 0,
                                    pitch_and_family: 0,
                                    face: [0u16; 32],
                                };
                                SendMessageW(
                                    hw,
                                    EM_SETCHARFORMAT,
                                    SCF_ALL,
                                    &mut cf as *mut CharFormatW as LPARAM,
                                );
                                SendMessageW(hw, WM_SETREDRAW, 1, 0);
                                // 4) 插入点归零 + 显式重绘：清空后的画面必须马上刷新，
                                //    不能等用户框选才把内容「刷」出来
                                SendMessageW(hw, EM_SETSEL, 0, 0);
                                repaint_log(hw);
                            }
                        }
                    }
                }
                _ => {}
            }
            0
        }
        WM_CLOSE => {
            // 取证：窗口是被「谁」关掉的，写清楚再退出，避免下次又是「莫名其妙不见了」
            logger::log_warn(format!(
                "[exit] 收到关闭请求 WM_CLOSE（用户点叉 / 系统关机 / Alt+F4 都可能触发），开始退出"
            ));
            logger::flush_file();
            // 先让工作线程退出，再关窗
            crate::RUNNING.store(false, Ordering::SeqCst);
            KillTimer(hwnd, TIMER_SCAN);
            KillTimer(hwnd, TIMER_STATUS);
            KillTimer(hwnd, TIMER_CLOCK);
            KillTimer(hwnd, TIMER_BEAT);
            KillTimer(hwnd, TIMER_FLUSH);
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            // 解绑日志框，避免工作线程继续往已销毁的控件投递
            logger::clear_gui_log();
            MAIN_HWND.store(0, Ordering::SeqCst);
            logger::log("[exit] 窗口已销毁，消息循环退出");
            logger::flush_file();
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// 启动窗体（阻塞直到窗口关闭）
pub fn run() {
    unsafe {
        SetProcessDPIAware();
        // UI 线程提到 ABOVE_NORMAL：点击线程是 BELOW_NORMAL、匹配子线程是 NORMAL，
        // 这样无论底层多忙，窗口消息泵都排在前面，永远能响应点击/拖动/关闭。
        crate::winapi::SetThreadPriority(crate::winapi::GetCurrentThread(), 1); // THREAD_PRIORITY_ABOVE_NORMAL
        // 日志框用 RichEdit（可着色），必须先加载 msftedit.dll
        ensure_richedit();
        // 尺寸锁定线程独立于 UI 线程运行，程序一起来就待命
        crate::sizelock::ensure_thread();
        let inst = GetModuleHandleW(std::ptr::null());
        let cls_name = wide("MojinglingClickerWindow");

        let wc = WndClassExW {
            cb_size: std::mem::size_of::<WndClassExW>() as u32,
            style: 0,
            lpfn_wndproc: Some(wndproc),
            cb_cls_extra: 0,
            cb_wnd_extra: 0,
            h_instance: inst,
            // 图标取自 exe 资源节（build.rs 已嵌入 1 号图标）
            h_icon: LoadIconW(inst, 1usize as *const u16),
            h_cursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            hbr_background: COLOR_BTNFACE as usize as HBRUSH,
            lpsz_menu_name: std::ptr::null(),
            lpsz_class_name: cls_name.as_ptr(),
            h_icon_sm: LoadIconW(inst, 1usize as *const u16),
        };
        RegisterClassExW(&wc);

        // 标题带版本号：拷到别的机器上一眼就能确认跑的是不是最新那份，
        // 不用再靠文件大小/时间戳猜。版本号取自 Cargo.toml，编译期定死。
        let title = wide(&format!("魔精灵点击器 v{}", env!("CARGO_PKG_VERSION")));
        let hwnd = CreateWindowExW(
            0,
            cls_name.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            WIN_W,
            WIN_H,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            inst,
            std::ptr::null_mut(),
        );
        if hwnd.is_null() {
            let t = wide("窗口创建失败。");
            let c = wide("错误");
            MessageBoxW(std::ptr::null_mut(), t.as_ptr(), c.as_ptr(), MB_ICONWARNING);
            return;
        }

        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);

        // 启动即自动查找目标窗口，并把句柄填到左上角 edit
        logger::log(format!("[auto] 正在自动查找窗口「{DEFAULT_KEYWORD}」…"));
        if lookup_and_show(hwnd, DEFAULT_KEYWORD).is_none() {
            logger::log("[auto] 暂未找到游戏窗体，已开启后台扫描，游戏打开后会自动锁定。");
        }
        // 开启后台扫描：游戏晚于本程序启动，或锁定的窗口被关掉，都能自动重新接管
        SetTimer(hwnd, TIMER_SCAN, SCAN_MS, std::ptr::null_mut());
        // 状态栏时间每秒刷新
        SetTimer(hwnd, TIMER_STATUS, STATUS_MS, std::ptr::null_mut());
        SetFocus(GetDlgItem(hwnd, ID_START as i32));

        let mut msg = Msg {
            hwnd: std::ptr::null_mut(),
            message: 0,
            w_param: 0,
            l_param: 0,
            time: 0,
            pt: Point { x: 0, y: 0 },
        };
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
