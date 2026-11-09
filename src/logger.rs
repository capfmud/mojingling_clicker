//! 日志出口路由。
//!
//! - GUI 模式：写入窗体里的只读日志框（跨线程用 SendMessage，安全）
//! - 控制台模式：写 stdout
//!
//! 全局用 `logln!` 宏，不在业务代码里关心当前是哪种模式。
//!
//! 三档输出：
//!   - `logln!`    始终输出（同时自动前置「时:分:秒.毫秒」时间戳）
//!   - `logvln!`   只在详细模式（启动加 `-g`）输出：坐标、相似度、耗时等细节
//!   - `logsel!`   双份文案：详细模式打第一份，简化模式打第二份（最简状态一句话）
//!
//! 颜色：异常用红色（`loge!`）、警告用橙色（`logw!`）。
//!   - 窗体模式：日志框改用 RichEdit，按行设置字符颜色
//!   - 控制台模式：打开 VT 后走 ANSI 转义；老控制台打开失败则退化为纯文本（不加转义，避免出现乱码）
//!
//! 详细模式默认关（简化输出），加 `-g` / `--verbose` 或环境变量 `MOJINGLING_VERBOSE=1` 打开。

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, AtomicU8, Ordering};
use std::sync::Mutex;

static LOG_HWND: AtomicIsize = AtomicIsize::new(0);

/// GUI 模式的日志投递队列。
///
/// 为什么不用「工作线程 PostMessage 每条日志」：节拍运行时每秒能打出几十行，
/// 每行都是一次跨线程消息 + 一次 RichEdit 插入/滚动/裁剪重排，
/// UI 线程会被这些消息占满，表现就是「点开始后整个窗口卡死、停止和叉号都没反应」。
/// 现在工作线程只往这个有界队列 push（纯内存操作，微秒级），
/// UI 线程用 120ms 定时器成批取走、合并重复行后一次性插入。
static GUI_QUEUE: Mutex<Vec<(String, u32)>> = Mutex::new(Vec::new());
/// 队列满时丢弃的行数（由 UI 线程取走时一并上报）
static GUI_DROPPED: AtomicU64 = AtomicU64::new(0);
/// 队列上限：够放几秒的日志，超出直接丢，绝不反过来阻塞工作线程
const GUI_QUEUE_CAP: usize = 4000;

/// 详细输出开关（-g）
static VERBOSE: AtomicBool = AtomicBool::new(false);
/// 控制台 VT（ANSI）支持状态：0 = 还没试，1 = 可用，2 = 不可用
static VT_STATE: AtomicU8 = AtomicU8::new(0);

/// 正常文字（黑）
pub const COLOR_NORMAL: u32 = 0x0000_0000;
/// 警告（橙黄，COLORREF = 0x00BBGGRR）
pub const COLOR_WARN: u32 = 0x0000_8CFF;
/// 异常（红）
pub const COLOR_ERR: u32 = 0x0000_00FF;

/// 日志级别：决定颜色
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Info,
    Warn,
    Err,
}

impl Level {
    fn color(self) -> u32 {
        match self {
            Level::Info => COLOR_NORMAL,
            Level::Warn => COLOR_WARN,
            Level::Err => COLOR_ERR,
        }
    }
    /// ANSI 转义（控制台用）
    fn ansi(self) -> &'static str {
        match self {
            Level::Info => "",
            Level::Warn => "\x1b[33m",
            Level::Err => "\x1b[1;31m",
        }
    }
}

// ---------- 控制台 VT 支持 ----------

const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
/// ENABLE_VIRTUAL_TERMINAL_PROCESSING
const ENABLE_VT: u32 = 0x0004;

// main.rs 的 ensure_console() 里另有一份 GetStdHandle 声明，签名不同但语义一致
#[allow(clashing_extern_declarations)]
extern "system" {
    fn GetStdHandle(which: u32) -> isize;
    fn GetConsoleMode(handle: isize, mode: *mut u32) -> i32;
    fn SetConsoleMode(handle: isize, mode: u32) -> i32;
}

/// 控制台是否支持 ANSI 转义（首次调用时尝试打开 VT，结果缓存）
fn vt_ready() -> bool {
    match VT_STATE.load(Ordering::SeqCst) {
        1 => return true,
        2 => return false,
        _ => {}
    }
    let ok = unsafe {
        let h = GetStdHandle(STD_OUTPUT_HANDLE);
        if h == 0 || h == -1 {
            false
        } else {
            let mut mode: u32 = 0;
            if GetConsoleMode(h, &mut mode) == 0 {
                false
            } else if mode & ENABLE_VT != 0 {
                true
            } else {
                SetConsoleMode(h, mode | ENABLE_VT) != 0
            }
        }
    };
    VT_STATE.store(if ok { 1 } else { 2 }, Ordering::SeqCst);
    ok
}

/// 控制台是否支持 ANSI 转义（供诊断）
pub fn vt_supported() -> bool {
    vt_ready()
}

/// 打开/关闭详细输出（-g）
pub fn set_verbose(v: bool) {
    VERBOSE.store(v, Ordering::SeqCst);
}

/// 当前是否详细输出
pub fn verbose() -> bool {
    VERBOSE.load(Ordering::SeqCst)
}

/// 绑定 GUI 日志框句柄（进入窗体模式）
pub fn set_gui_log(h: isize) {
    LOG_HWND.store(h, Ordering::SeqCst);
}

/// 解绑（窗口销毁前调用，避免工作线程往失效句柄发消息）
pub fn clear_gui_log() {
    LOG_HWND.store(0, Ordering::SeqCst);
}

/// 当前日志框句柄（GUI 模式非 0）
pub fn log_hwnd() -> isize {
    LOG_HWND.load(Ordering::SeqCst)
}

// ---------- 文件落盘（窗口「不见了」也能事后取证） ----------
//
// 之前的日志只进 RichEdit：一旦窗口被关掉/进程退出，日志随窗口一起消失，
// 「拖动窗体后程序自己跑了又不见了」这类问题就彻底没法查。
// 现在所有日志同时在 exe 同目录写一份 Mclick_run.log（同步写、无缓冲、单文件上限 2MB 后截断），
// 并额外记录启动横幅与退出原因。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

static LOG_FILE: Mutex<Option<File>> = Mutex::new(None);
static LOG_FILE_BYTES: AtomicU64 = AtomicU64::new(0);
static LOG_FILE_DISABLED: AtomicBool = AtomicBool::new(false);
/// 单文件上限：超过就从头截断（保留最近一段即可）
const LOG_FILE_MAX: u64 = 2_000_000;

/// 日志文件路径：exe 同目录的 Mclick_run.log
pub fn log_file_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.to_path_buf();
    Some(dir.join("Mclick_run.log"))
}

/// 写一行到日志文件（任意线程可调；打开失败后永久静默，绝不影响主流程）
pub fn write_file_line(line: &str) {
    if LOG_FILE_DISABLED.load(Ordering::Relaxed) {
        return;
    }
    let Some(path) = log_file_path() else {
        LOG_FILE_DISABLED.store(true, Ordering::Relaxed);
        return;
    };
    let mut g = match LOG_FILE.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if g.is_none() {
        match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(f) => {
                let sz = f.metadata().map(|m| m.len()).unwrap_or(0);
                LOG_FILE_BYTES.store(sz, Ordering::Relaxed);
                *g = Some(f);
            }
            Err(_) => {
                LOG_FILE_DISABLED.store(true, Ordering::Relaxed);
                return;
            }
        }
    }
    let Some(f) = g.as_mut() else { return };
    // 超限就截断，避免无限膨胀
    if LOG_FILE_BYTES.load(Ordering::Relaxed) > LOG_FILE_MAX {
        if f.set_len(0).is_ok() {
            LOG_FILE_BYTES.store(0, Ordering::Relaxed);
            let _ = f.write_all(b"===== log truncated =====\r\n");
        }
    }
    let mut buf = String::with_capacity(line.len() + 2);
    buf.push_str(line);
    buf.push_str("\r\n");
    if f.write_all(buf.as_bytes()).is_ok() {
        LOG_FILE_BYTES
            .fetch_add(buf.len() as u64, Ordering::Relaxed);
        let _ = f.flush();
    }
}

/// 强制刷盘（退出前调用）
pub fn flush_file() {
    let mut g = match LOG_FILE.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if let Some(f) = g.as_mut() {
        let _ = f.flush();
    }
}

/// 启动横幅：每次进程启动写一行，方便在日志里切分「哪一次运行」
pub fn file_banner(tag: &str) {
    let pid = std::process::id();
    let path = log_file_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<无>".to_string());
    write_file_line(&format!(
        "\r\n===== [{}] {tag} 启动 pid={pid} 日志={path} =====",
        stamp()
    ));
}

// ---------- 时间戳 ----------
/// Win32 SYSTEMTIME（GUID 定义顺序，字段名换成 snake 只为消警告，布局必须原样）
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct SystemTime {
    year: u16,
    month: u16,
    day_of_week: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    milliseconds: u16,
}

extern "system" {
    fn GetLocalTime(lp_system_time: *mut SystemTime);
}

/// 本地时间「时:分:秒.毫秒」，如 `14:07:33.412`
pub fn stamp() -> String {
    let mut st = SystemTime::default();
    unsafe { GetLocalTime(&mut st) };
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        st.hour, st.minute, st.second, st.milliseconds
    )
}

pub fn log(msg: impl Into<String>) {
    log_level(Level::Info, msg);
}

/// 提示级（正常颜色）
#[allow(dead_code)]
pub fn log_info(msg: impl Into<String>) {
    log_level(Level::Info, msg);
}

/// 警告级（橙色）
pub fn log_warn(msg: impl Into<String>) {
    log_level(Level::Warn, msg);
}

/// 异常级（红色）
pub fn log_err(msg: impl Into<String>) {
    log_level(Level::Err, msg);
}

/// 统一出口：按级别决定颜色（GUI 走 RichEdit 着色，控制台走 ANSI）
pub fn log_level(level: Level, msg: impl Into<String>) {
    let msg: String = msg.into();
    // 每行统一前置时间戳（几点几分几秒几毫秒）
    let line = format!("[{}] {msg}", stamp());
    // 落盘优先：不管窗口还在不在，这一行都要能留下
    write_file_line(&format!("{:?} {line}", level));
    let h = LOG_HWND.load(Ordering::SeqCst);
    if h == 0 {
        let esc = level.ansi();
        if esc.is_empty() || !vt_ready() {
            println!("{line}");
        } else {
            // 控制台支持 VT：整行上色后复位
            println!("{esc}{line}\x1b[0m");
        }
    } else {
        // GUI 模式：只入队，不碰任何窗口句柄、不发任何跨线程消息
        push_gui_line(line.clone(), level.color());
        let _ = line;
    }
}

/// 入队一行待显示日志（纯内存操作，任意线程可调，永不阻塞）
fn push_gui_line(line: String, color: u32) {
    let mut q = match GUI_QUEUE.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    if q.len() >= GUI_QUEUE_CAP {
        GUI_DROPPED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    q.push((line, color));
}

/// UI 线程定时调用：取走累积的日志行与丢弃计数
pub fn take_gui_lines() -> (Vec<(String, u32)>, u64) {
    let mut q = match GUI_QUEUE.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    let dropped = GUI_DROPPED.swap(0, Ordering::Relaxed);
    (std::mem::take(&mut *q), dropped)
}

/// 统一的日志宏：自动走窗体或控制台
#[macro_export]
macro_rules! logln {
    () => { $crate::logger::log(String::new()) };
    ($($arg:tt)*) => { $crate::logger::log(format!($($arg)*)) };
}

/// 警告级日志（橙色）
#[macro_export]
macro_rules! logw {
    ($($arg:tt)*) => { $crate::logger::log_warn(format!($($arg)*)) };
}

/// 异常级日志（红色）
#[macro_export]
macro_rules! loge {
    ($($arg:tt)*) => { $crate::logger::log_err(format!($($arg)*)) };
}

/// 只在详细模式（-g）下输出的日志：坐标、相似度、耗时等细节
#[macro_export]
macro_rules! logvln {
    ($($arg:tt)*) => {{
        if $crate::logger::verbose() {
            $crate::logln!($($arg)*);
        }
    }};
}

/// 双份文案日志：详细模式打第一段，简化模式打第二段
#[macro_export]
macro_rules! logsel {
    (($($v:tt)*) ($($b:tt)*)) => {{
        if $crate::logger::verbose() {
            $crate::logln!($($v)*);
        } else {
            $crate::logln!($($b)*);
        }
    }};
}
