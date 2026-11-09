//! 窗口截屏层：调用 gdi32/user32 现成 API 截取指定窗口客户区为 BGRA 像素。

use crate::winapi::*;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering};

/// Auto 模式下上一次成功拿到有效画面的通路（0=未定 1=dc 2=pw 3=scr）
///
/// 微信小程序（WeChatAppEx）这类 GPU 合成窗口 dc 永远空帧，Auto 若每帧都先试 dc，
/// 等于每帧白跑一次 BitBlt 外加一条「空帧」告警。首次探测成功后在这里记下来，
/// 后续帧直接复用，只有复用的通路也开始空帧时才重新探测另一条。
static AUTO_GOOD: AtomicU8 = AtomicU8::new(0);

/// 冻结帧检测用：上一帧的稀疏指纹，以及「连续多少轮画面完全没变」。
///
/// 有些机器的显卡/驱动组合下 PrintWindow 对 GPU 合成窗口只返回一张陈旧快照
/// （窗口缓存位图），后续调用永远给同一帧。它的统计特征完全正常（既不是黑图也不是
/// 纯色），is_blank 判不出来，Auto 会把这条通路锁死，于是每轮都拿同一张老画面去匹配
/// 模板 —— 结果就是「窗口能吸附、模板永远 0 命中、一条点击都不发」。
/// 这类冻结只能靠跨通路的画面比对识别：连续多轮像素级完全不变，就换一条通路试。
static LAST_FP: AtomicU64 = AtomicU64::new(0);
static SAME_RUN: AtomicU32 = AtomicU32::new(0);

/// 连续多少轮画面完全一致才判定为「疑似冻结」。按 5 秒节拍约 30 秒；
/// 真在跑的游戏有动画/计时器/金币数字，不可能像素级纹丝不动这么久。
const FROZEN_ROUNDS: u32 = 6;

/// 每次「开始」时复位冻结计数：上一次运行留下的计数会误判这次的首帧。
pub fn reset_frozen_watch() {
    SAME_RUN.store(0, Ordering::Relaxed);
    LAST_FP.store(0, Ordering::Relaxed);
}

/// 抽样计算整帧指纹（FNV-1a / 32x32 网格），用于判断画面是否完全没变。
fn frame_fingerprint(bgra: &[u8], w: i32, h: i32) -> u64 {
    if bgra.is_empty() || w <= 0 || h <= 0 {
        return 0;
    }
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let sx = (w / 32).max(1);
    let sy = (h / 32).max(1);
    let mut y = 0;
    while y < h {
        let mut x = 0;
        while x < w {
            let idx = ((y as usize * w as usize + x as usize) * 4) as usize;
            if idx + 2 < bgra.len() {
                let v = bgra[idx] as u64 | (bgra[idx + 1] as u64) << 8 | (bgra[idx + 2] as u64) << 16;
                hash ^= v;
                hash = hash.wrapping_mul(0x100_0000_01b3);
            }
            x += sx;
        }
        y += sy;
    }
    hash
}

/// 通过窗口标题查找顶层窗口句柄（user32: FindWindowW）
/// # Safety
/// 只读查询，不改动窗口。
pub unsafe fn find_window(title: &str) -> Option<HWND> {
    let wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    let hwnd = FindWindowW(std::ptr::null(), wide.as_ptr());
    (hwnd != ptr_null()).then_some(hwnd)
}

/// 客户区矩形：返回 (屏幕原点x, 屏幕原点y, 宽, 高)
/// 原点用 ClientToScreen 取（客户区 (0,0) 的真实屏幕坐标），
/// 不能用 GetWindowRect().left/top —— Win10/11 的不可见缩放边框 + 标题栏会让它偏掉。
/// # Safety
/// hwnd 须有效。
pub unsafe fn client_rect(hwnd: HWND) -> Option<(i32, i32, i32, i32)> {
    let mut cr: RECT = std::mem::zeroed();
    if GetClientRect(hwnd, &mut cr) == 0 {
        return None;
    }
    let mut pt = POINT { x: 0, y: 0 };
    if ClientToScreen(hwnd, &mut pt) == 0 {
        // 退化路径：ClientToScreen 失败时用外框 + 客户区偏移兜底
        let mut wr: RECT = std::mem::zeroed();
        if GetWindowRect(hwnd, &mut wr) == 0 {
            return None;
        }
        return Some((wr.left + cr.left, wr.top + cr.top, cr.right - cr.left, cr.bottom - cr.top));
    }
    Some((pt.x, pt.y, cr.right - cr.left, cr.bottom - cr.top))
}

fn ptr_null() -> *mut core::ffi::c_void {
    std::ptr::null_mut()
}

/// 窗口是否处于最小化（收起）状态。
///
/// 微信小游戏窗口一旦最小化，客户区会被缩成 144x20 这类极小尺寸、窗口矩形移到
/// (-32000,-32000)，此时 dc / pw 两条通路拿到的都是纯黑，属于硬限制——
/// 必须由上层提示用户还原窗口，而不是反复重试抓图。
/// # Safety
/// hwnd 须有效。
pub unsafe fn is_minimized(hwnd: HWND) -> bool {
    !hwnd.is_null() && IsIconic(hwnd) != 0
}

/// 客户区是否落在虚拟屏幕内（未最小化、窗口可见、且与屏幕矩形有交集）。
///
/// scr 通路抓的是屏幕上真实像素：窗口缩成 144x20 隐形壳、移到 (-32000,-32000)、
/// 或被最小化时，抓到的要么全黑、要么是别的窗口。所以在 Auto 里降级到 scr 之前
/// 先做这个判定，不满足就直接跳过，不要拿一帧错的画面去喂模板匹配。
/// # Safety
/// hwnd 须有效；本函数只读查询。
pub unsafe fn client_on_screen(hwnd: HWND) -> bool {
    if hwnd.is_null() || IsIconic(hwnd) != 0 || IsWindowVisible(hwnd) == 0 {
        return false;
    }
    let Some((x, y, w, h)) = client_rect(hwnd) else {
        return false;
    };
    if w <= 0 || h <= 0 {
        return false;
    }
    let (sw, sh) = (
        GetSystemMetrics(SM_CXVIRTUALSCREEN),
        GetSystemMetrics(SM_CYVIRTUALSCREEN),
    );
    if sw <= 0 || sh <= 0 {
        return false;
    }
    let (svx, svy) = (
        GetSystemMetrics(SM_XVIRTUALSCREEN),
        GetSystemMetrics(SM_YVIRTUALSCREEN),
    );
    x < svx + sw && x + w > svx && y < svy + sh && y + h > svy
}

/// 客户区在虚拟屏幕上的位置与尺寸，用于空帧时的诊断日志。
/// # Safety
/// hwnd 须有效；本函数只读查询。
pub unsafe fn client_geometry(hwnd: HWND) -> Option<(i32, i32, i32, i32, i32, i32)> {
    let (x, y, w, h) = client_rect(hwnd)?;
    let (sw, sh) = (
        GetSystemMetrics(SM_CXVIRTUALSCREEN),
        GetSystemMetrics(SM_CYVIRTUALSCREEN),
    );
    let (svx, svy) = (
        GetSystemMetrics(SM_XVIRTUALSCREEN),
        GetSystemMetrics(SM_YVIRTUALSCREEN),
    );
    Some((x, y, w, h, svx + sw, svy + sh))
}

// ==================== 后台抓图（与游戏客户端同款通路） ====================
//
// 抓图不是「截屏」，而是问窗口要绘制表面：
//   GetWindowDC(hwnd) → CreateCompatibleDC/Bitmap → SelectObject → BitBlt → 转像素
//   （.NET 里用 Bitmap.FromHbitmap 取像素；这里用 GetDIBits 等价实现）
// 附加一条 PrintWindow(PW_RENDERFULLCONTENT) 通路，专门对付 GPU 合成的窗口
// （微信小程序运行时 WeChatAppEx 属于这一类，纯 BitBlt 会拿到黑图/旧帧）。
// 两条通路都不需要窗口处于前台，也不会把窗口切到前面 —— 这就是「后台找图」。

/// 抓图方式
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CaptureMode {
    /// 自动：先试同款 GetWindowDC+BitBlt，画面无效再试 PrintWindow
    Auto,
    /// 只用 GetWindowDC + BitBlt（同款方案，对 GDI 绘制的窗口最快最稳）
    WindowDc,
    /// 只用 PrintWindow(PW_RENDERFULLCONTENT → PW_CLIENTONLY)
    PrintWindow,
    /// 直接抓屏幕上「客户区所在那块矩形」的像素：GetDC(NULL) + BitBlt
    /// 微信小程序（WeChatAppEx）这类 GPU 合成窗口，PrintWindow 只能拿到冻结的旧帧，
    /// 画面明明变了却一直返回同一张图 —— 只能走屏幕抓取。
    /// 代价：要求窗口可见且没被别的窗口盖住（--front 会先把窗口置前）。
    Screen,
    /// 只用 D3D 子窗口的 PrintWindow(PW_RENDERFULLCONTENT)
    ///
    /// 微信小程序（WeChatAppEx / 顶层类 Chrome_WidgetWin_0）的真实画面并不画在顶层窗口上，
    /// 而是画在类名为 "Intermediate D3D Window" 的子窗口里：
    /// - 对顶层窗口 dc/pw 都是黑图或冻结旧帧；
    /// - 对这个 D3D 子窗口 PrintWindow(flag=2) 能拿到真实帧（实测亮度均值 70+、暗像素 8%）；
    /// - 且完全后台：不要求窗口可见、不要求不被遮挡、不抢焦点、不动鼠标。
    /// 因此这条通路是微信小程序的正确通路，优先于需要「窗口在最前」的 scr。
    D3d,
}

impl CaptureMode {
    pub fn label(self) -> &'static str {
        match self {
            CaptureMode::Auto => "auto",
            CaptureMode::WindowDc => "dc",
            CaptureMode::PrintWindow => "pw",
            CaptureMode::Screen => "scr",
            CaptureMode::D3d => "d3d",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "auto" | "" => Some(CaptureMode::Auto),
            "dc" | "gdi" | "bitblt" | "tfjl" => Some(CaptureMode::WindowDc),
            "pw" | "printwindow" => Some(CaptureMode::PrintWindow),
            "scr" | "screen" | "desktop" => Some(CaptureMode::Screen),
            "d3d" | "pw2" | "d3dpw2" | "d3d/pw2" | "d3d-pw2" => Some(CaptureMode::D3d),
            _ => None,
        }
    }
    /// 这条通路是否依赖窗口处于最前（被遮挡就抓不到画面）
    pub fn needs_visible(self) -> bool {
        matches!(self, CaptureMode::Screen)
    }
}

/// 一帧画面的有效性与诊断信息
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameStat {
    /// 平均灰度（0~255）
    pub mean: f64,
    /// 单像素最大亮度
    pub max: u8,
    /// 近黑像素占比（灰度 < 12）
    pub dark_ratio: f64,
}

impl FrameStat {
    /// 判断是否为「空帧 / 黑图 / 纯色」—— 抓取失败或窗口未渲染时的典型表现
    pub fn is_blank(&self) -> bool {
        self.max < 16 || self.dark_ratio > 0.995
    }
    pub fn describe(&self) -> String {
        format!(
            "mean={:.1} max={} dark={:.1}%{}",
            self.mean,
            self.max,
            self.dark_ratio * 100.0,
            if self.is_blank() { " [空帧]" } else { "" }
        )
    }
}

/// 统计 BGRA 缓冲的亮度特征（抽样步长 4 像素，避免整帧遍历拖慢 5s 节拍）
pub fn frame_stat(bgra: &[u8]) -> FrameStat {
    let px = bgra.len() / 4;
    if px == 0 {
        return FrameStat::default();
    }
    let step = (px / 20000).max(1);
    let (mut sum, mut max, mut dark, mut n) = (0f64, 0u8, 0usize, 0usize);
    let mut i = 0usize;
    while i < px {
        let o = i * 4;
        let b = bgra[o] as u32;
        let g = bgra[o + 1] as u32;
        let r = bgra[o + 2] as u32;
        let gray = ((b + g + r) / 3) as u8;
        sum += gray as f64;
        if gray > max {
            max = gray;
        }
        if gray < 12 {
            dark += 1;
        }
        n += 1;
        i += step;
    }
    FrameStat {
        mean: if n > 0 { sum / n as f64 } else { 0.0 },
        max,
        dark_ratio: if n > 0 { dark as f64 / n as f64 } else { 1.0 },
    }
}

/// 抓取结果：像素 + 实际生效的通路 + 诊断信息
pub struct Capture {
    pub width: i32,
    pub height: i32,
    /// 自上而下、每像素 4 字节 BGRA
    pub bgra: Vec<u8>,
    /// 实际使用的通路："d3d"（D3D 子窗 PrintWindow）、"dc"（GetWindowDC+BitBlt）、
    /// "pw"（PrintWindow）或 "scr"（屏幕抓取）
    pub via: &'static str,
    pub stat: FrameStat,
}

struct GdiFrame {
    window_dc: HDC,
    mem_dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
}

impl GdiFrame {
    /// 建立一张与窗口同尺寸的内存位图，并绑定到内存 DC
    unsafe fn new(hwnd: HWND, w: i32, h: i32) -> Option<Self> {
        let window_dc = GetWindowDC(hwnd);
        if window_dc.is_null() {
            return None;
        }
        let mem_dc = CreateCompatibleDC(window_dc);
        let bmp = CreateCompatibleBitmap(window_dc, w, h);
        if mem_dc.is_null() || bmp.is_null() {
            if !mem_dc.is_null() {
                DeleteDC(mem_dc);
            }
            if !bmp.is_null() {
                DeleteObject(bmp as HGDIOBJ);
            }
            ReleaseDC(hwnd, window_dc);
            return None;
        }
        let old = SelectObject(mem_dc, bmp as HGDIOBJ);
        Some(GdiFrame { window_dc, mem_dc, bmp, old })
    }

    /// 把内存位图读成 BGRA（biHeight 取负 → 自顶向下，无需翻转）
    unsafe fn read_bgra(&self, w: i32, h: i32) -> Option<Vec<u8>> {
        let mut bi: BITMAPINFO = std::mem::zeroed();
        bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bi.bmiHeader.biWidth = w;
        bi.bmiHeader.biHeight = -h;
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        bi.bmiHeader.biCompression = BI_RGB;
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let got = GetDIBits(
            self.mem_dc,
            self.bmp,
            0,
            h as u32,
            buf.as_mut_ptr() as _,
            &mut bi,
            DIB_RGB_COLORS,
        );
        (got != 0).then_some(buf)
    }
}

impl Drop for GdiFrame {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.mem_dc, self.old);
            DeleteObject(self.bmp as HGDIOBJ);
            DeleteDC(self.mem_dc);
            // ReleaseDC 需要原窗口句柄，这里用 DC 自身的窗口句柄反查不安全，
            // 因此改为在调用侧成对释放：见 release_window_dc
        }
    }
}

/// 释放 GetWindowDC 拿到的 DC（必须与创建它的 HWND 配对）
unsafe fn release_window_dc(hwnd: HWND, dc: HDC) {
    if !dc.is_null() {
        ReleaseDC(hwnd, dc);
    }
}

/// 通路 1：GetWindowDC + BitBlt（同款方案）
/// BitBlt 的源原点必须对准客户区在「整个窗口」内的偏移，
/// 否则会把标题栏/边框也拷进来，导致整片 ROI 偏掉十几像素。
unsafe fn grab_by_window_dc(hwnd: HWND, w: i32, h: i32) -> Option<Vec<u8>> {
    let mut ox = 0i32;
    let mut oy = 0i32;
    let mut wr: RECT = std::mem::zeroed();
    let mut pt = POINT { x: 0, y: 0 };
    if GetWindowRect(hwnd, &mut wr) != 0 && ClientToScreen(hwnd, &mut pt) != 0 {
        ox = pt.x - wr.left;
        oy = pt.y - wr.top;
    }
    let f = GdiFrame::new(hwnd, w, h)?;
    let mut ok = BitBlt(f.mem_dc, 0, 0, w, h, f.window_dc, ox, oy, SRCCOPY_CAPTUREBLT) != 0;
    if !ok {
        // 部分显卡驱动不认 CAPTUREBLT，退回纯 SRCCOPY
        ok = BitBlt(f.mem_dc, 0, 0, w, h, f.window_dc, ox, oy, SRCCOPY) != 0;
    }
    let frame = if ok { f.read_bgra(w, h) } else { None };
    let dc = f.window_dc;
    drop(f);
    release_window_dc(hwnd, dc);
    frame
}

/// 通路 2：PrintWindow（PW_RENDERFULLCONTENT → PW_CLIENTONLY）
/// RENDERFULLCONTENT 是 Chromium/Electron 这类 GPU 合成窗口唯一能出真画面的方式。
unsafe fn grab_by_printwindow(hwnd: HWND, w: i32, h: i32) -> Option<Vec<u8>> {
    let f = GdiFrame::new(hwnd, w, h)?;
    // PrintWindow 只画客户区时目标左上角即 (0,0)
    let mut ok = PrintWindow(hwnd, f.mem_dc, PW_RENDERFULLCONTENT) != 0;
    if !ok {
        ok = PrintWindow(hwnd, f.mem_dc, PW_CLIENTONLY) != 0;
    }
    let frame = if ok { f.read_bgra(w, h) } else { None };
    let dc = f.window_dc;
    drop(f);
    release_window_dc(hwnd, dc);
    frame
}

/// 通路 3：屏幕抓取（GetDC(NULL) + BitBlt）
/// 源矩形 = 客户区在屏幕上的位置，尺寸 = 客户区尺寸，因此结果与 dc/pw 通路逐像素对齐。
/// 只对「窗口在最上层且没被遮挡」时有效；被遮挡会抓到遮挡窗口的像素。
unsafe fn grab_by_screen(hwnd: HWND, w: i32, h: i32) -> Option<Vec<u8>> {
    let (sx, sy, _, _) = client_rect(hwnd)?;
    let screen_dc = GetDC(ptr_null());
    if screen_dc.is_null() {
        return None;
    }
    let mem_dc = CreateCompatibleDC(screen_dc);
    let bmp = CreateCompatibleBitmap(screen_dc, w, h);
    if mem_dc.is_null() || bmp.is_null() {
        if !mem_dc.is_null() {
            DeleteDC(mem_dc);
        }
        if !bmp.is_null() {
            DeleteObject(bmp as HGDIOBJ);
        }
        ReleaseDC(ptr_null(), screen_dc);
        return None;
    }
    let old = SelectObject(mem_dc, bmp as HGDIOBJ);
    let mut ok = BitBlt(mem_dc, 0, 0, w, h, screen_dc, sx, sy, SRCCOPY_CAPTUREBLT) != 0;
    if !ok {
        ok = BitBlt(mem_dc, 0, 0, w, h, screen_dc, sx, sy, SRCCOPY) != 0;
    }
    let mut frame: Option<Vec<u8>> = None;
    if ok {
        let mut bi: BITMAPINFO = std::mem::zeroed();
        bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bi.bmiHeader.biWidth = w;
        bi.bmiHeader.biHeight = -h;
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        bi.bmiHeader.biCompression = BI_RGB;
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let got = GetDIBits(
            mem_dc,
            bmp,
            0,
            h as u32,
            buf.as_mut_ptr() as _,
            &mut bi,
            DIB_RGB_COLORS,
        );
        if got != 0 {
            frame = Some(buf);
        }
    }
    SelectObject(mem_dc, old);
    DeleteObject(bmp as HGDIOBJ);
    DeleteDC(mem_dc);
    ReleaseDC(ptr_null(), screen_dc);
    frame
}

unsafe extern "system" fn child_cb(hwnd: HWND, lp: LPARAM) -> i32 {
    let out = &mut *(lp as *mut Vec<HWND>);
    out.push(hwnd);
    1
}

/// 在目标窗口的全部子孙窗口里找「真正的 D3D 绘制表面」。
///
/// 微信小程序（进程 WeChatAppEx.exe、顶层类 Chrome_WidgetWin_0）的画布挂在
/// 类名 "Intermediate D3D Window" 的子窗口上。判定条件：类名含 d3d，尺寸 > 200x150，
/// 多个候选取面积最大的那个。找不到返回 None（普通 Win32 窗口走 dc/pw 即可）。
/// # Safety
/// hwnd 须有效；本函数只读查询。
pub unsafe fn find_d3d_child(hwnd: HWND) -> Option<HWND> {
    if hwnd.is_null() || IsWindow(hwnd) == 0 {
        return None;
    }
    let mut hs: Vec<HWND> = Vec::new();
    EnumChildWindows(hwnd, Some(child_cb), &mut hs as *mut _ as LPARAM);
    let mut best: Option<(HWND, i32)> = None;
    for h in hs {
        let mut cb = [0u16; 256];
        let m = GetClassNameW(h, cb.as_mut_ptr(), cb.len() as i32);
        let class = wide_to_string(&cb[..m.max(0) as usize]).to_lowercase();
        // 只要 d3d 表面类；排除同族的 Chrome_RenderWidgetHostHWND（实测黑图）
        let is_d3d = class.contains("d3d") && class.contains("window")
            && !class.contains("renderwidgethost");
        if !is_d3d {
            continue;
        }
        let Some((_, _, cw, ch)) = client_rect(h) else { continue };
        if cw < 200 || ch < 150 {
            continue;
        }
        let area = cw.saturating_mul(ch);
        if best.map_or(true, |(_, a)| area > a) {
            best = Some((h, area));
        }
    }
    best.map(|(h, _)| h)
}

/// 通路 4：D3D 子窗口 + PrintWindow(PW_RENDERFULLCONTENT)
///
/// 抓的是子窗口，但产物必须与「父窗口客户区」逐像素对齐 —— 上层模板 ROI 全部按客户区
/// 比例算，尺寸一变整套坐标就偏。因此：子窗画面按它在客户区内的偏移贴进一张
/// w×h 的零缓冲（未覆盖区域留黑），与 dc/pw/scr 三条通路完全同构。
unsafe fn grab_by_d3d(hwnd: HWND, w: i32, h: i32) -> Option<Vec<u8>> {
    let child = find_d3d_child(hwnd)?;
    let (cx, cy, cw, ch) = client_rect(child)?;
    if cw <= 0 || ch <= 0 {
        return None;
    }
    let (px, py, _, _) = client_rect(hwnd)?;
    let (dx, dy) = (cx - px, cy - py);

    // PrintWindow 只会从 (0,0) 开始画，所以先抓进一张子窗尺寸的临时位图
    let f = GdiFrame::new(child, cw, ch)?;
    let mut ok = PrintWindow(child, f.mem_dc, PW_RENDERFULLCONTENT) != 0;
    if !ok {
        // 少数驱动不认 RENDERFULLCONTENT，退回 PW_CLIENTONLY
        ok = PrintWindow(child, f.mem_dc, PW_CLIENTONLY) != 0;
    }
    let src = if ok { f.read_bgra(cw, ch) } else { None };
    let dc = f.window_dc;
    drop(f);
    release_window_dc(child, dc);
    let src = src?;

    // 完全对齐（最常见：D3D 子窗铺满客户区）就不必搬运
    if dx == 0 && dy == 0 && cw == w && ch == h {
        return Some(src);
    }
    let mut dst = vec![0u8; (w as usize) * (h as usize) * 4];
    for row in 0..ch {
        let ty = dy + row;
        if ty < 0 || ty >= h {
            continue;
        }
        let sx0 = 0.max(-dx);
        let sx1 = cw.min(w - dx);
        if sx1 <= sx0 {
            continue;
        }
        let s_off = ((row as usize) * (cw as usize) + (sx0 as usize)) * 4;
        let d_off = ((ty as usize) * (w as usize) + ((dx + sx0) as usize)) * 4;
        let len = ((sx1 - sx0) as usize) * 4;
        dst[d_off..d_off + len].copy_from_slice(&src[s_off..s_off + len]);
    }
    Some(dst)
}

/// 按指定方式抓取窗口客户区（后台，不激活、不置前、不动鼠标）
/// # Safety
/// hwnd 须有效；本函数只读像素，不改动窗口状态。
pub unsafe fn capture_with(hwnd: HWND, mode: CaptureMode) -> Option<Capture> {
    let (_, _, w, h) = client_rect(hwnd)?;
    if w <= 0 || h <= 0 {
        return None;
    }

    let mk = |bgra: Vec<u8>, via: &'static str| {
        let stat = frame_stat(&bgra);
        Capture { width: w, height: h, bgra, via, stat }
    };

    match mode {
        CaptureMode::WindowDc => {
            let bgra = grab_by_window_dc(hwnd, w, h)?;
            Some(mk(bgra, "dc"))
        }
        CaptureMode::PrintWindow => {
            let bgra = grab_by_printwindow(hwnd, w, h)?;
            Some(mk(bgra, "pw"))
        }
        CaptureMode::Screen => {
            let bgra = grab_by_screen(hwnd, w, h)?;
            Some(mk(bgra, "scr"))
        }
        CaptureMode::D3d => {
            let bgra = grab_by_d3d(hwnd, w, h)?;
            Some(mk(bgra, "d3d"))
        }
        CaptureMode::Auto => {
            // 先走上一次成功的通路；没有记录时按 d3d → dc → pw → scr 顺序探测。
            // 画面是黑图/纯色说明这条通路对这个窗口无效，换下一条并记住有效的那个。
            //
            // d3d 排第一：微信小程序的真实画面只在 D3D 子窗口里，且这条通路不需要窗口
            // 可见/不被遮挡，是唯一「纯后台」的正解。没有 D3D 子窗的普通窗口会直接返回
            // None，代价只是一次 EnumChildWindows。
            //
            // 第四级 scr 是最后的兜底：确有机器的显卡/驱动组合下别的通路都拿不到画面
            // （典型表现是 mean=0 max=0 dark=100%），此时只有抓屏幕真实像素才看得见游戏
            // 画面。它要求窗口必须真实显示在屏幕上，所以先用 client_on_screen 把关。
            let prefer = AUTO_GOOD.load(Ordering::Relaxed);
            let order: [u8; 4] = match prefer {
                1 => [1, 2, 4, 3],
                2 => [2, 4, 1, 3],
                3 => [3, 4, 1, 2],
                4 => [4, 3, 1, 2],
                _ => [4, 1, 2, 3],
            };
            let mut fallback: Option<Capture> = None;
            for which in order {
                if which == 3 && !client_on_screen(hwnd) {
                    continue;
                }
                let got = match which {
                    1 => grab_by_window_dc(hwnd, w, h).map(|b| mk(b, "dc")),
                    2 => grab_by_printwindow(hwnd, w, h).map(|b| mk(b, "pw")),
                    4 => grab_by_d3d(hwnd, w, h).map(|b| mk(b, "d3d")),
                    _ => grab_by_screen(hwnd, w, h).map(|b| mk(b, "scr")),
                };
                let Some(c) = got else { continue };
                if !c.stat.is_blank() {
                    // 冻结帧筛查：画面非空但长期像素级不变，说明这条通路可能只给陈旧快照。
                    // 连续 FROZEN_ROUNDS 轮都没变化，就不再锁死这条通路，继续探测其它通路；
                    // 若其它通路的画面不一样、能跟着游戏动，就会在下面替换掉它。
                    let fp = frame_fingerprint(&c.bgra, w, h);
                    let last = LAST_FP.load(Ordering::Relaxed);
                    let mut same = SAME_RUN.load(Ordering::Relaxed);
                    if fp != 0 && fp == last {
                        if same < u32::MAX {
                            same += 1;
                            SAME_RUN.store(same, Ordering::Relaxed);
                        }
                    } else {
                        LAST_FP.store(fp, Ordering::Relaxed);
                        SAME_RUN.store(0, Ordering::Relaxed);
                        same = 0;
                    }
                    if same >= FROZEN_ROUNDS && same % 5 == 0 {
                        crate::logln!(
                            "[cap] 画面连续 {} 轮完全没变化（via={}）→ 疑似陈旧快照，正在换抓图通路探测",
                            same,
                            c.via
                        );
                        if fallback.is_none() {
                            fallback = Some(c);
                        }
                        continue;
                    }
                    AUTO_GOOD.store(which, Ordering::Relaxed);
                    return Some(c);
                }
                // 三条通路都空帧时，保留信息量更大的那一帧，让上层照常跑并在日志里标记
                let replace = match &fallback {
                    Some(b) => c.stat.max > b.stat.max,
                    None => true,
                };
                if replace {
                    fallback = Some(c);
                }
            }
            fallback
        }
    }
}

/// 兼容旧签名：Auto 方式抓图
/// # Safety
/// hwnd 须有效；本函数只读像素，不改动窗口状态。
pub unsafe fn capture_window_bgra(hwnd: HWND) -> Option<(i32, i32, Vec<u8>)> {
    capture_with(hwnd, CaptureMode::Auto).map(|c| (c.width, c.height, c.bgra))
}

/// 诊断用：把各条通路的抓图特征打出来，判断本机游戏窗口吃哪条通路
/// # Safety
/// hwnd 须有效。
pub unsafe fn probe_capture(hwnd: HWND) -> Vec<(String, Option<FrameStat>)> {
    let mut out = Vec::new();
    if let Some((_, _, w, h)) = client_rect(hwnd) {
        // d3d 通路放第一：微信小程序的正确通路，且结果最好认（有画面、且不依赖窗口可见）
        if find_d3d_child(hwnd).is_some() {
            match grab_by_d3d(hwnd, w, h) {
                Some(b) => out.push(("d3d(D3D子窗PrintWindow)".to_string(), Some(frame_stat(&b)))),
                None => out.push(("d3d(D3D子窗PrintWindow)".to_string(), None)),
            }
        } else {
            out.push(("d3d(D3D子窗PrintWindow)".to_string(), None));
        }
        if let Some(b) = grab_by_window_dc(hwnd, w, h) {
            out.push(("dc(GetWindowDC+BitBlt)".to_string(), Some(frame_stat(&b))));
        } else {
            out.push(("dc(GetWindowDC+BitBlt)".to_string(), None));
        }
        if let Some(b) = grab_by_printwindow(hwnd, w, h) {
            out.push(("pw(PrintWindow)".to_string(), Some(frame_stat(&b))));
        } else {
            out.push(("pw(PrintWindow)".to_string(), None));
        }
        // scr 通路抓屏幕真实像素，窗口必须显示在屏幕上才抓得到，否则报抓取失败
        if client_on_screen(hwnd) {
            match grab_by_screen(hwnd, w, h) {
                Some(b) => out.push(("scr(屏幕抓取)".to_string(), Some(frame_stat(&b)))),
                None => out.push(("scr(屏幕抓取)".to_string(), None)),
            }
        } else {
            out.push(("scr(屏幕抓取)".to_string(), None));
        }
    }
    out
}

// ==================== 窗口枚举与模糊查找 ====================

/// 顶层窗口信息（查找 / 日志显示用）
#[derive(Clone)]
pub struct WinInfo {
    pub hwnd: HWND,
    pub title: String,
    pub class: String,
    pub pid: u32,
    pub exe: String,
    pub cw: i32,
    pub ch: i32,
}

fn wide_to_string(w: &[u16]) -> String {
    let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end])
}

/// 取进程可执行文件名（如 game.exe），取不到返回空串
unsafe fn proc_exe(pid: u32) -> String {
    if pid == 0 {
        return String::new();
    }
    let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
    if h.is_null() {
        return String::new();
    }
    let mut buf = [0u16; 1024];
    let mut size: u32 = buf.len() as u32;
    let mut name = String::new();
    if QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut size) != 0 {
        let full = String::from_utf16_lossy(&buf[..size as usize]);
        name = full.rsplit('\\').next().unwrap_or(&full).to_string();
    }
    CloseHandle(h);
    name
}

/// 读取单个窗口的完整信息
pub unsafe fn describe(hwnd: HWND) -> Option<WinInfo> {
    if hwnd.is_null() || IsWindow(hwnd) == 0 {
        return None;
    }
    let mut tb = [0u16; 512];
    let n = GetWindowTextW(hwnd, tb.as_mut_ptr(), tb.len() as i32);
    let title = wide_to_string(&tb[..n.max(0) as usize]);
    let mut cb = [0u16; 256];
    let m = GetClassNameW(hwnd, cb.as_mut_ptr(), cb.len() as i32);
    let class = wide_to_string(&cb[..m.max(0) as usize]);
    let mut pid: u32 = 0;
    GetWindowThreadProcessId(hwnd, &mut pid);
    let exe = proc_exe(pid);
    let (cw, ch) = match client_rect(hwnd) {
        Some((_, _, w, h)) => (w, h),
        None => (0, 0),
    };
    Some(WinInfo { hwnd, title, class, pid, exe, cw, ch })
}

unsafe extern "system" fn enum_cb(hwnd: HWND, lp: LPARAM) -> i32 {
    let out = &mut *(lp as *mut Vec<HWND>);
    out.push(hwnd);
    1
}

/// 枚举所有可见的顶层窗口
pub unsafe fn list_top_windows() -> Vec<WinInfo> {
    let mut hs: Vec<HWND> = Vec::new();
    EnumWindows(Some(enum_cb), &mut hs as *mut _ as LPARAM);
    let mut v = Vec::with_capacity(hs.len());
    for h in hs {
        if IsWindowVisible(h) == 0 {
            continue;
        }
        if let Some(w) = describe(h) {
            v.push(w);
        }
    }
    v
}

/// 关键字拆分：支持 `游戏名|子串` / 逗号 / 空格分隔的“或”组合，任一命中即算。
/// 注意：不再做任何进程名自动别名展开 —— 辅助程序不是游戏本体。
fn expand_keywords(key: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in key
        .split(|c| matches!(c, '|' | ',' | '，' | ';' | '；' | ' ' | '\t'))
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty())
    {
        if !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

/// 干扰窗口过滤：这些不是游戏本体窗体，查找时必须剔除。
/// - 进程辅助程序，它的窗口标题形如「锁0724-2.27.2.50」，不是游戏本体
/// - 系统外壳类窗口
/// 注意：不能按「锁」开头的标题过滤 —— 游戏本体的窗口标题恰恰是房间号「锁0724」。
pub fn is_noise(title: &str, exe: &str) -> bool {
    let e = exe.to_lowercase();
    if e.starts_with("tfjl") || e.starts_with("towerdefense") {
        return true;
    }
    let t = title.trim();
    const SHELL: [&str; 4] = [
        "Program Manager",
        "MarvisShadowCompanion",
        "Windows",
        "MSCTFIME UI",
    ];
    SHELL.iter().any(|s| t.eq_ignore_ascii_case(s))
}

/// 游戏窗口的最小客户区：小于这个尺寸的一律视为隐形壳窗口，不能作为抓图目标
const MIN_GAME_W: i32 = 320;
const MIN_GAME_H: i32 = 320;

/// 定位「游戏本体」窗口（实测结论，不依赖标题）：
/// 游戏跑在微信小程序运行时里 —— 类名 `Chrome_WidgetWin_0`、进程 `WeChatAppEx.exe`、
/// 标题是房间号（如「锁0724」），画面上才是游戏画面。
/// 优先顺序：① 小程序运行时窗口里「够大」的那个（客户区至少 320x320，排除 144x20 这类幽灵壳窗口）② 标题含游戏名的窗口
pub unsafe fn find_game_window() -> Option<WinInfo> {
    let all = list_top_windows();
    let mut best: Option<WinInfo> = None;
    let mut best_area: i64 = -1;
    // 兜底：尺寸不够格时退回「面积最大」，避免一个候选都没有
    let mut fallback: Option<WinInfo> = None;
    let mut fallback_area: i64 = -1;
    for w in all.iter() {
        if is_noise(&w.title, &w.exe) {
            continue;
        }
        let cls = w.class.to_lowercase();
        let exe = w.exe.to_lowercase();
        if cls == "chrome_widgetwin_0" && exe.contains("wechatappex") {
            let area = w.cw as i64 * w.ch as i64;
            if area > fallback_area {
                fallback_area = area;
                fallback = Some(w.clone());
            }
            // 运行时会挂一个标题为游戏名、客户区 144x20 的隐形壳窗口，
            // 只按标题或只按面积排序都可能锁到它，抓图会全空。先卡尺寸门槛。
            if w.cw < MIN_GAME_W || w.ch < MIN_GAME_H {
                continue;
            }
            if area > best_area {
                best_area = area;
                best = Some(w.clone());
            }
        }
    }
    if best.is_some() {
        return best;
    }
    if fallback.is_some() {
        return fallback;
    }
    all.iter()
        .filter(|w| !is_noise(&w.title, &w.exe))
        .find(|w| w.title.contains("塔防精灵"))
        .cloned()
}

/// 统一目标解析：默认关键字（游戏名 / 空）优先走「进程+类名」识别游戏本体，
/// 其余关键字走标题/类名/进程名模糊匹配。
pub unsafe fn resolve_target(key: &str) -> Option<WinInfo> {
    let k = key.trim();
    if k.is_empty() || k.contains("塔防精灵") {
        if let Some(w) = find_game_window() {
            return Some(w);
        }
    }
    find_windows(k).into_iter().next()
}

/// 按关键字模糊查找窗口（忽略大小写）：
/// 标题包含 / 类名包含 / 进程 exe 名包含 三者任一命中。
/// 排序优先级：exe 名命中 > 标题命中 > 类名命中。
pub unsafe fn find_windows(key: &str) -> Vec<WinInfo> {
    let toks = expand_keywords(key);
    if toks.is_empty() {
        return Vec::new();
    }
    let mut hit: Vec<(u8, WinInfo)> = Vec::new();
    for w in list_top_windows() {
        if is_noise(&w.title, &w.exe) {
            continue;
        }
        let tl = w.title.to_lowercase();
        let cl = w.class.to_lowercase();
        let el = w.exe.to_lowercase();
        let mut best: Option<u8> = None;
        for t in &toks {
            let rank = if el.contains(t.as_str()) {
                0u8
            } else if tl.contains(t.as_str()) {
                1
            } else if cl.contains(t.as_str()) {
                2
            } else {
                continue;
            };
            best = Some(match best {
                Some(b) if b <= rank => b,
                _ => rank,
            });
        }
        if let Some(r) = best {
            hit.push((r, w));
        }
    }
    hit.sort_by(|a, b| a.0.cmp(&b.0));
    hit.into_iter().map(|(_, w)| w).collect()
}

/// 窗口在屏幕上的外框矩形：返回 (left, top, right, bottom)
/// # Safety
/// hwnd 须有效。
pub unsafe fn window_rect(hwnd: HWND) -> Option<(i32, i32, i32, i32)> {
    if hwnd.is_null() || IsWindow(hwnd) == 0 {
        return None;
    }
    if IsIconic(hwnd) != 0 {
        ShowWindow(hwnd, SW_RESTORE);
    }
    let mut wr: RECT = std::mem::zeroed();
    if GetWindowRect(hwnd, &mut wr) == 0 {
        return None;
    }
    let (l, t, r, b) = (wr.left, wr.top, wr.right, wr.bottom);
    // 丢弃明显无效的矩形（屏幕外的隐藏/占位窗口）
    if r - l <= 0 || b - t <= 0 {
        return None;
    }
    Some((l, t, r, b))
}

const DWMWA_EXTENDED_FRAME_BOUNDS: u32 = 9;

#[link(name = "dwmapi")]
extern "system" {
    fn DwmGetWindowAttribute(hwnd: HWND, attr: u32, pv: *mut core::ffi::c_void, cb: u32) -> i32;
}

// 异步版 ShowWindow：跨进程还原窗口时不会等对方窗口线程，避免拖住本程序界面线程
#[link(name = "user32")]
extern "system" {
    fn ShowWindowAsync(hwnd: HWND, cmd: i32) -> i32;
}

/// 窗口在屏幕上“肉眼可见”的边界：返回 (left, top, right, bottom)。
/// Win10/11 的顶层窗口外框（GetWindowRect）左右各带 7~8px 不可见的缩放边框，
/// 用它算贴合会让视觉缝隙比预期大十几像素，所以吸附一律改用 DWM 扩展帧边界。
/// DWM 取不到时退化为外框矩形。
/// # Safety
/// hwnd 须有效。
pub unsafe fn visual_rect(hwnd: HWND) -> Option<(i32, i32, i32, i32)> {
    let outer = window_rect(hwnd)?;
    let (ol, ot, orr, ob) = outer;
    let mut r: RECT = std::mem::zeroed();
    let hr = DwmGetWindowAttribute(
        hwnd,
        DWMWA_EXTENDED_FRAME_BOUNDS,
        &mut r as *mut RECT as *mut core::ffi::c_void,
        std::mem::size_of::<RECT>() as u32,
    );
    if hr != 0 {
        return Some(outer);
    }
    // 夹到外框范围内，避免动画/DPI 过渡期的瞬时异常值
    let l = r.left.max(ol);
    let t = r.top.max(ot);
    let rr = r.right.min(orr);
    let b = r.bottom.min(ob);
    if rr - l <= 0 || b - t <= 0 {
        return Some(outer);
    }
    Some((l, t, rr, b))
}

/// 把窗口整体移到屏幕坐标 (x, y)（左上角对齐，尺寸不变）
/// 返回是否成功
pub unsafe fn move_window_to(hwnd: HWND, x: i32, y: i32) -> bool {
    if hwnd.is_null() || IsWindow(hwnd) == 0 {
        return false;
    }
    // 最小化的窗口 GetWindowRect 恒为 (-32000,-32000)，且 SetWindowPos 只改还原位置、屏幕上看不到，
    // 必须先把窗口还原成正常状态，位置才会真正生效
    if IsIconic(hwnd) != 0 {
        ShowWindowAsync(hwnd, SW_RESTORE);
    }
    // SWP_ASYNCWINDOWPOS：目标窗口属于别的进程（微信小程序），
    // 同步 SetWindowPos 要等对方窗口线程处理完才返回，对方一忙就会把调用方（界面线程）拖死。
    // 加上这个标志后由系统异步投递，界面线程立刻返回。
    SetWindowPos(
        hwnd,
        std::ptr::null_mut(),
        x,
        y,
        0,
        0,
        SWP_NOSIZE | SWP_NOZORDER | 0x4000,
    ) != 0
}

/// 读取 BMP 文件为 BGRA（自上而下，每像素 4 字节）。
/// 仅支持 24/32 位未压缩位图；用于「离线帧」测试：把一张静态截图当成
/// 游戏窗口画面喂给匹配引擎，不截屏、不需要游戏在运行。
pub fn read_bmp_bgra(path: &std::path::Path) -> Option<(usize, usize, Vec<u8>)> {
    let data = std::fs::read(path).ok()?;
    if data.len() < 54 || &data[0..2] != b"BM" {
        return None;
    }
    let u32at = |o: usize| -> u32 {
        u32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]])
    };
    let i32at = |o: usize| -> i32 {
        i32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]])
    };
    let off = u32at(10) as usize;
    let w = i32at(18);
    let h = i32at(22);
    let bpp = u16::from_le_bytes([data[28], data[29]]) as usize;
    let comp = u32at(30);
    if comp != 0 || (bpp != 24 && bpp != 32) || w == 0 || h == 0 {
        return None;
    }
    let w = w.unsigned_abs() as usize;
    let hn = h.unsigned_abs() as usize;
    let bottom_up = h > 0;
    let bytes = bpp / 8;
    let stride = (w * bytes + 3) & !3usize;
    if off + stride * hn > data.len() {
        return None;
    }
    let mut out = vec![0u8; w * hn * 4];
    for y in 0..hn {
        let sy = if bottom_up { hn - 1 - y } else { y };
        let row = off + sy * stride;
        for x in 0..w {
            let p = row + x * bytes;
            if p + 2 >= data.len() {
                return None;
            }
            let d = (y * w + x) * 4;
            out[d] = data[p];
            out[d + 1] = data[p + 1];
            out[d + 2] = data[p + 2];
            out[d + 3] = if bytes == 4 { data[p + 3] } else { 255 };
        }
    }
    Some((w, hn, out))
}
