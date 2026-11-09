//! 自动提权（UAC）：启动时若当前不是管理员，就用 ShellExecuteExW("runas") 以管理员身份把自己重新拉起一次。
//!
//! 背景：123 机器上实测「右键 → 以管理员身份运行」才正常，普通权限下点不中/抓不到目标窗口
//! （目标进程若以更高完整性级别运行时，普通权限进程的窗口消息会被 UIPI 直接丢弃）。
//!
//! 设计取舍（为什么不用 manifest requireAdministrator）：
//! - manifest 是编译期写死的：双击、命令行、脚本、计划任务全都会强制弹 UAC，
//!   无人值守/后台场景下 UAC 弹不出来会直接启动失败。
//! - 运行时代码提权可以留后门：设置环境变量 `MCLICK_NO_ELEVATE=1` 即跳过提权（跑 --self-test、
//!   --list 这类纯诊断时不被 UAC 打断），其余场景双击即弹 UAC、点「是」就是管理员。
//! - 用户点了「否」→ ShellExecuteExW 返回 ERROR_CANCELLED(1223)：此时不退出，继续以普通权限跑，
//!   与旧版行为一致，避免「点了否程序直接消失」的困惑。
//! - 提权重启时给子进程打环境变量标记 `MCLICK_ELEVATED=1`，子进程即使检测异常也不会二次重启 → 无递归风险。

use std::ffi::{c_void, OsStr};
use std::os::windows::ffi::OsStrExt;

// 日志宏在 crate 根导出（#[macro_export]），子模块里要显式 use 进来
use crate::{logln, logw};

type Handle = *mut c_void;

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> Handle;
    fn CloseHandle(h: Handle) -> i32;
    fn GetLastError() -> u32;
}

#[link(name = "advapi32")]
extern "system" {
    fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
    fn GetTokenInformation(
        token: Handle,
        class: i32,
        info: *mut c_void,
        len: u32,
        ret_len: *mut u32,
    ) -> i32;
}

#[link(name = "shell32")]
extern "system" {
    fn ShellExecuteExW(info: *mut ShellExecuteInfoW) -> i32;
}

/// SHELLEXECUTEINFOW（x64 布局：4 + 4 + 8 对齐 + 后续指针）
#[repr(C)]
#[allow(non_snake_case)]
struct ShellExecuteInfoW {
    cbSize: u32,
    fMask: u32,
    hwnd: *mut c_void,
    lpVerb: *const u16,
    lpFile: *const u16,
    lpParameters: *const u16,
    lpDirectory: *const u16,
    nShow: i32,
    hInstApp: *mut c_void,
    lpIDList: *mut c_void,
    lpClass: *const u16,
    hkeyClass: *mut c_void,
    dwHotKey: u32,
    hIcon: *mut c_void,
    hProcess: *mut c_void,
}

const TOKEN_QUERY: u32 = 0x0008;
const TOKEN_ELEVATION: i32 = 20; // TokenElevation
const SW_SHOWNORMAL: i32 = 1;
const ERROR_CANCELLED: u32 = 1223;

/// 当前进程是否已以管理员（提升后的）令牌运行。
pub fn is_elevated() -> bool {
    unsafe {
        let mut token: Handle = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation: u32 = 0;
        let mut ret: u32 = 0;
        let ok = GetTokenInformation(
            token,
            TOKEN_ELEVATION,
            &mut elevation as *mut u32 as *mut c_void,
            std::mem::size_of::<u32>() as u32,
            &mut ret,
        );
        CloseHandle(token);
        ok != 0 && elevation != 0
    }
}

/// 环境变量开关（值为空 / "0" 视为关闭）
fn env_flag(name: &str) -> bool {
    std::env::var_os(name)
        .map(|v| {
            let s = v.to_string_lossy().to_string();
            !s.is_empty() && s != "0"
        })
        .unwrap_or(false)
}

/// UTF-16 化（带结尾 0）
fn wide(s: &OsStr) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_wide().collect();
    v.push(0);
    v
}

/// 按 Windows 命令行规则给单个参数加引号（反斜杠 + 引号的转义规则）
fn quote_arg(s: &str) -> String {
    if !s.is_empty() && !s.chars().any(|c| c == ' ' || c == '\t' || c == '"') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    let mut bs = 0usize;
    for c in s.chars() {
        match c {
            '\\' => bs += 1,
            '"' => {
                for _ in 0..(bs * 2 + 1) {
                    out.push('\\');
                }
                out.push('"');
                bs = 0;
            }
            _ => {
                for _ in 0..bs {
                    out.push('\\');
                }
                bs = 0;
                out.push(c);
            }
        }
    }
    for _ in 0..(bs * 2) {
        out.push('\\');
    }
    out.push('"');
    out
}

/// 把当前进程的参数（不含程序名）重新拼成命令行字符串
fn params_string() -> String {
    let mut parts: Vec<String> = Vec::new();
    for a in std::env::args_os().skip(1) {
        parts.push(quote_arg(&a.to_string_lossy()));
    }
    parts.join(" ")
}

/// 启动时调用：需要提权就用 UAC 拉起自身的提升副本。
///
/// 返回 `true` = 已经拉起新进程，当前（普通权限）进程应立刻退出；
/// 返回 `false` = 无需重启，继续往下跑（已是管理员 / 用户取消 / 显式跳过）。
pub fn relaunch_if_needed() -> bool {
    if is_elevated() {
        logln!("[init] 权限：管理员");
        return false;
    }
    if env_flag("MCLICK_ELEVATED") {
        logw!("[init] 权限：普通 —— 已尝试过提权（MCLICK_ELEVATED），不重复拉起，继续运行");
        return false;
    }
    if env_flag("MCLICK_NO_ELEVATE") {
        logw!("[init] 权限：普通 —— MCLICK_NO_ELEVATE 已设置，跳过提权");
        return false;
    }

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            logw!("[init] 权限：普通 —— 取自身路径失败（{e}），跳过提权");
            return false;
        }
    };
    let dir = std::env::current_dir().ok();

    // 调试/自测用：只打印将要执行的提权命令行，不真的拉起（不会弹 UAC）
    if env_flag("MCLICK_ELEVATE_DRYRUN") {
        logln!(
            "[init] 提权预演（MCLICK_ELEVATE_DRYRUN）：runas \"{}\" {}",
            exe.display(),
            params_string()
        );
        return false;
    }

    // 宽字符缓冲必须在调用期间存活
    let verb_w = wide(OsStr::new("runas"));
    let file_w = wide(exe.as_os_str());
    let params = params_string();
    let params_w = wide(OsStr::new(&params));
    let dir_w = dir
        .as_ref()
        .map(|d| wide(d.as_os_str()))
        .unwrap_or_else(|| vec![0u16]);

    let mut info = ShellExecuteInfoW {
        cbSize: std::mem::size_of::<ShellExecuteInfoW>() as u32,
        fMask: 0,
        hwnd: std::ptr::null_mut(),
        lpVerb: verb_w.as_ptr(),
        lpFile: file_w.as_ptr(),
        lpParameters: params_w.as_ptr(),
        lpDirectory: dir_w.as_ptr(),
        nShow: SW_SHOWNORMAL,
        hInstApp: std::ptr::null_mut(),
        lpIDList: std::ptr::null_mut(),
        lpClass: std::ptr::null(),
        hkeyClass: std::ptr::null_mut(),
        dwHotKey: 0,
        hIcon: std::ptr::null_mut(),
        hProcess: std::ptr::null_mut(),
    };

    // 给子进程打标：避免万一子进程仍判定为非管理员时再次拉起（递归）
    std::env::set_var("MCLICK_ELEVATED", "1");

    let ok = unsafe { ShellExecuteExW(&mut info) } != 0;
    if ok {
        logln!(
            "[init] 权限：普通 → 已用「管理员」重新拉起自身（{}）后退出",
            exe.display()
        );
        return true;
    }

    std::env::remove_var("MCLICK_ELEVATED");
    let err = unsafe { GetLastError() };
    if err == ERROR_CANCELLED {
        logw!("[init] 权限：普通 —— 用户在 UAC 里点了「否」，继续以普通权限运行（可能导致点不中窗口）");
    } else {
        logw!("[init] 权限：普通 —— 提权拉起失败（错误码 {err}），继续以普通权限运行");
    }
    false
}
