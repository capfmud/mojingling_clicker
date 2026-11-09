//! Windows 现成 API FFI 层（零依赖，全部手写 extern 声明）
//! 覆盖：kernel32 动态加载、user32 窗口/鼠标、gdi32 截屏。

#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(dead_code)]

use std::os::raw::c_void;

// ---------- 类型 ----------
pub type HWND = *mut c_void;
pub type HDC = *mut c_void;
pub type HGDIOBJ = *mut c_void;
pub type HBITMAP = *mut c_void;
pub type HMODULE = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct RECT {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct POINT {
    pub x: i32,
    pub y: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct BITMAPINFOHEADER {
    pub biSize: u32,
    pub biWidth: i32,
    pub biHeight: i32,
    pub biPlanes: u16,
    pub biBitCount: u16,
    pub biCompression: u32,
    pub biSizeImage: u32,
    pub biXPelsPerMeter: i32,
    pub biYPelsPerMeter: i32,
    pub biClrUsed: u32,
    pub biClrImportant: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct BITMAPINFO {
    pub bmiHeader: BITMAPINFOHEADER,
    pub bmiColors: [u32; 3],
}

// ---------- kernel32 ----------
#[link(name = "kernel32")]
extern "system" {
    pub fn LoadLibraryA(lp_file_name: *const u8) -> HMODULE;
    pub fn GetProcAddress(h_module: HMODULE, lp_proc_name: *const u8) -> *mut c_void;
    /// 当前线程伪句柄（无需 CloseHandle）
    pub fn GetCurrentThread() -> isize;
    /// 设置线程优先级：点击/识别线程用 BELOW_NORMAL，保证界面线程永远先被调度
    pub fn SetThreadPriority(h_thread: isize, n_priority: i32) -> i32;
    pub fn FreeLibrary(h_module: HMODULE) -> i32;
    pub fn SetConsoleCtrlHandler(
        handler_routine: Option<unsafe extern "system" fn(u32) -> i32>,
        add: i32,
    ) -> i32;
}

// ---------- user32 ----------
#[link(name = "user32")]
extern "system" {
    pub fn FindWindowW(lp_class_name: *const u16, lp_window_name: *const u16) -> HWND;
    pub fn GetClientRect(hwnd: HWND, rect: *mut RECT) -> i32;
    pub fn GetWindowRect(hwnd: HWND, rect: *mut RECT) -> i32;
    /// 客户区坐标 → 屏幕坐标。Win10/11 窗口有不可见缩放边框（约 8px）和标题栏，
    /// 直接用 GetWindowRect().left/top 当客户区原点会把点击位置上移约 31px、左移约 8px。
    pub fn ClientToScreen(hwnd: HWND, point: *mut POINT) -> i32;
    pub fn GetForegroundWindow() -> HWND;
    pub fn SetForegroundWindow(hwnd: HWND) -> i32;
    pub fn SwitchToThisWindow(hwnd: HWND, alt_tab: i32);
    pub fn SetCursorPos(x: i32, y: i32) -> i32;
    pub fn mouse_event(dw_flags: u32, dx: i32, dy: i32, dw_data: u32, dw_extra_info: usize);
    pub fn GetDC(hwnd: HWND) -> HDC;
    pub fn ReleaseDC(hwnd: HWND, hdc: HDC) -> i32;
    pub fn PrintWindow(hwnd: HWND, hdc: HDC, flags: u32) -> i32;
}

pub const MOUSEEVENTF_LEFTDOWN: u32 = 0x0002;
pub const MOUSEEVENTF_LEFTUP: u32 = 0x0004;
pub const PW_CLIENTONLY: u32 = 0x00000001;
/// PrintWindow 第三参 PW_RENDERFULLCONTENT：让 Chromium/Electron 这类 GPU 合成窗口
/// （微信小程序运行时 WeChatAppEx 即属此类）也能整帧输出，只给 PW_CLIENTONLY 会得到全黑图。
pub const PW_RENDERFULLCONTENT: u32 = 0x00000002;

// ---------- gdi32 ----------
#[link(name = "gdi32")]
extern "system" {
    pub fn CreateCompatibleDC(hdc: HDC) -> HDC;
    pub fn CreateCompatibleBitmap(hdc: HDC, w: i32, h: i32) -> HBITMAP;
    pub fn SelectObject(hdc: HDC, h: HGDIOBJ) -> HGDIOBJ;
    pub fn DeleteDC(hdc: HDC) -> i32;
    pub fn DeleteObject(ho: HGDIOBJ) -> i32;
    pub fn BitBlt(
        dest: HDC,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        src: HDC,
        sx: i32,
        sy: i32,
        rop: u32,
    ) -> i32;
    pub fn GetDIBits(
        hdc: HDC,
        hbmp: HBITMAP,
        start: u32,
        lines: u32,
        buf: *mut c_void,
        bi: *mut BITMAPINFO,
        usage: u32,
    ) -> i32;
    pub fn GetObjectW(h: HGDIOBJ, size: i32, buf: *mut c_void) -> i32;
}

pub const BI_RGB: u32 = 0;
pub const DIB_RGB_COLORS: u32 = 0;
pub const SRCCOPY: u32 = 0x00CC0020;

// ---------- 窗口枚举 / 进程名（查找句柄用） ----------
pub type HANDLE = *mut c_void;
pub type LPARAM = isize;

#[link(name = "kernel32")]
extern "system" {
    pub fn OpenProcess(access: u32, inherit: i32, pid: u32) -> HANDLE;
    pub fn CloseHandle(h: HANDLE) -> i32;
    pub fn QueryFullProcessImageNameW(
        h: HANDLE,
        flags: u32,
        buf: *mut u16,
        size: *mut u32,
    ) -> i32;
}

#[link(name = "user32")]
extern "system" {
    pub fn EnumWindows(cb: Option<unsafe extern "system" fn(HWND, LPARAM) -> i32>, lp: LPARAM) -> i32;
    /// 枚举某窗口的全部子孙窗口（递归）。微信小程序的 D3D 绘制表面就藏在子窗口里。
    pub fn EnumChildWindows(
        parent: HWND,
        cb: Option<unsafe extern "system" fn(HWND, LPARAM) -> i32>,
        lp: LPARAM,
    ) -> i32;
    pub fn IsWindow(hwnd: HWND) -> i32;
    pub fn IsIconic(hwnd: HWND) -> i32;
    pub fn ShowWindow(hwnd: HWND, cmd: i32) -> i32;
    pub fn IsWindowVisible(hwnd: HWND) -> i32;
    pub fn GetWindowTextW(hwnd: HWND, buf: *mut u16, max: i32) -> i32;
    pub fn GetClassNameW(hwnd: HWND, buf: *mut u16, max: i32) -> i32;
    pub fn GetWindowThreadProcessId(hwnd: HWND, pid: *mut u32) -> u32;
    pub fn GetSystemMetrics(index: i32) -> i32;
    pub fn SetWindowPos(
        hwnd: HWND,
        insert_after: HWND,
        x: i32,
        y: i32,
        cx: i32,
        cy: i32,
        flags: u32,
    ) -> i32;
}

pub const SW_RESTORE: i32 = 9;

// GetSystemMetrics 索引（虚拟屏幕：含多显示器）
pub const SM_XVIRTUALSCREEN: i32 = 76;
pub const SM_YVIRTUALSCREEN: i32 = 77;
pub const SM_CXVIRTUALSCREEN: i32 = 78;
pub const SM_CYVIRTUALSCREEN: i32 = 79;
// ---------- 窗体矩形换算（客户区 ↔ 外框）----------
//
// 为什么需要：模板是按住户区（canvas 像素）裁的，跨机器要「客户区一致」才有意义；
// 而 SetWindowPos 只吃外框尺寸，所以要先把客户区尺寸换算成外框尺寸。

#[link(name = "user32")]
extern "system" {
    /// 取窗口样式（index=GWL_STYLE / GWL_EXSTYLE）
    pub fn GetWindowLongW(hwnd: HWND, index: i32) -> i32;
    /// 按样式把「客户区矩形」扩张成「外框矩形」（原地修改）
    pub fn AdjustWindowRectEx(rect: *mut RECT, style: u32, menu: i32, exstyle: u32) -> i32;
}

pub const GWL_STYLE: i32 = -16;
pub const GWL_EXSTYLE: i32 = -20;

pub const SWP_NOSIZE: u32 = 0x0001;
pub const SWP_NOMOVE: u32 = 0x0002;
pub const SWP_NOZORDER: u32 = 0x0004;
pub const SWP_NOACTIVATE: u32 = 0x0010;

pub const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;

// ---------- 命中测试 / 光标位置（点击护栏用） ----------

#[link(name = "user32")]
extern "system" {
    /// 取当前物理光标位置（屏幕坐标）
    pub fn GetCursorPos(point: *mut POINT) -> i32;
    /// 命中测试：返回屏幕坐标处最上层的窗口句柄
    pub fn WindowFromPoint(point: POINT) -> HWND;
    /// 取祖先窗口：GA_ROOT = 2（最外层顶层窗口）
    pub fn GetAncestor(hwnd: HWND, flags: u32) -> HWND;
    /// 取按键/鼠标键的实时状态：最高位为 1（返回值为负）表示「当前正被按住」
    pub fn GetAsyncKeyState(vkey: i32) -> i16;
}

/// GetAncestor 的 GA_ROOT：沿父链追到顶层窗口
pub const GA_ROOT: u32 = 2;

/// 虚拟键码：鼠标左键
pub const VK_LBUTTON: i32 = 0x01;

/// 取当前进程 ID（不依赖 std 的 process::id 也要能对比）
pub fn current_pid() -> u32 {
    extern "system" {
        fn GetCurrentProcessId() -> u32;
    }
    unsafe { GetCurrentProcessId() }
}


// ==================== 后台抓图 / 后台点击（TFJL 同款 API 面） ====================
//
// TFJL.exe 的原生边界里，抓图链路是：
//   user32: GetWindowDC / GetWindowRect / ReleaseDC  (+ PrintWindow 作兜底)
//   gdi32 : CreateCompatibleDC / CreateCompatibleBitmap / SelectObject / BitBlt / GetDeviceCaps
//   .NET  : Bitmap.FromHbitmap 把 HBITMAP 转成像素（我们这里用 GetDIBits 等价代替）
// 点击链路除了 SetCursorPos + mouse_event，还导入了 PostMessage / SendMessage，
// 也就是「消息式点击」—— 不挪鼠标、不抢前台。这里把两套都补齐。

#[link(name = "user32")]
extern "system" {
    /// 取整个窗口（含非客户区）的设备上下文；配合 BitBlt 可在窗口不被激活时抓取绘制表面
    pub fn GetWindowDC(hwnd: HWND) -> HDC;
    /// 投递窗口消息（后台点击用，不移动真实鼠标、不改变前台窗口）
    pub fn PostMessageW(hwnd: HWND, msg: u32, wparam: usize, lparam: isize) -> i32;
    /// 投递 SendMessage 版本（个别游戏只认同步消息）
    pub fn SendMessageW(hwnd: HWND, msg: u32, wparam: usize, lparam: isize) -> isize;
}

#[link(name = "gdi32")]
extern "system" {
    /// 取设备能力（窗口/屏幕像素尺寸、DPI），用于校验抓图尺寸与缩放
    pub fn GetDeviceCaps(hdc: HDC, index: i32) -> i32;
}

/// BitBlt 的 CAPTUREBLT 扩展：把分层/半透明窗口也一并合成进来
pub const CAPTUREBLT: u32 = 0x4000_0000;
/// 抓图常用组合：SRCCOPY + CAPTUREBLT
pub const SRCCOPY_CAPTUREBLT: u32 = SRCCOPY | CAPTUREBLT;

pub const WM_LBUTTONDOWN: u32 = 0x0201;
pub const WM_LBUTTONUP: u32 = 0x0202;
pub const WM_MOUSEMOVE: u32 = 0x0200;
pub const MK_LBUTTON: usize = 0x0001;

pub const HORZRES: i32 = 8;
pub const VERTRES: i32 = 10;
pub const BITSPIXEL: i32 = 12;
pub const LOGPIXELSX: i32 = 88;

/// 把客户区坐标打包成 lParam（MAKELPARAM 等价实现）
#[inline]
pub fn make_lparam(x: i32, y: i32) -> isize {
    ((y as u16 as isize) << 16) | (x as u16 as isize)
}
