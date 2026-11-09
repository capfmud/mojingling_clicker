//! 构建脚本：把应用图标编进 exe 的资源节，实现「单文件 + 带图标」交付。
//!
//! 设计要点：
//! - 图标 `assets/app.ico` 在编译期通过 Windows SDK 的 rc.exe 编译成 .res 并链接进 exe，
//!   运行时不需要任何外部文件，产物仍是单个 exe。
//! - rc.exe 位置不写死：先看环境变量 RC_EXE，再找 PATH，最后扫描 Windows Kits 取最高版本。
//! - 找不到 rc.exe 时只告警、不中断构建 —— 功能不受影响，只是 exe 用系统默认图标。

use std::path::PathBuf;
use std::process::Command;

fn find_rc() -> Option<PathBuf> {
    // 1) 环境变量覆盖
    if let Ok(p) = std::env::var("RC_EXE") {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    // 2) PATH 里已有的 rc.exe
    if let Ok(out) = Command::new("where").arg("rc.exe").output() {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout);
            if let Some(line) = s.lines().next() {
                let pb = PathBuf::from(line.trim());
                if pb.is_file() {
                    return Some(pb);
                }
            }
        }
    }
    // 3) Windows Kits：取版本号最大的 x64\rc.exe
    let roots = [
        r"C:\Program Files (x86)\Windows Kits\10\bin",
        r"C:\Program Files\Windows Kits\10\bin",
    ];
    let mut best: Option<(Vec<u32>, PathBuf)> = None;
    for root in roots {
        let Ok(rd) = std::fs::read_dir(root) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let rc = e.path().join("x64").join("rc.exe");
            if !rc.is_file() {
                continue;
            }
            let ver: Vec<u32> = name.split('.').filter_map(|x| x.parse().ok()).collect();
            if best.as_ref().map(|(v, _)| ver > *v).unwrap_or(true) {
                best = Some((ver, rc));
            }
        }
    }
    best.map(|(_, p)| p)
}

fn main() {
    println!("cargo:rerun-if-changed=assets/app.ico");
    println!("cargo:rerun-if-env-changed=RC_EXE");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let ico = manifest.join("assets").join("app.ico");
    if !ico.is_file() {
        println!("cargo:warning=assets/app.ico 不存在，跳过图标嵌入");
        return;
    }

    match find_rc() {
        Some(rc) => {
            std::env::set_var("RC_EXE", &rc);
            println!("cargo:warning=图标嵌入使用 rc.exe: {}", rc.display());
        }
        None => {
            println!("cargo:warning=未找到 rc.exe，跳过图标嵌入（exe 仍可正常使用，只是用系统默认图标）");
            return;
        }
    }

    // 把 rc 脚本写进 OUT_DIR，并用绝对路径引用 ico，避免工作目录差异
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let rc_file = out.join("app.rc");
    // rc.exe 的字符串里反斜杠是转义符，必须换成正斜杠，否则路径会被吃掉
    let ico_slash = ico.display().to_string().replace('\\', "/");
    let _ = std::fs::write(&rc_file, format!("1 ICON \"{}\"\n", ico_slash));

    embed_resource::compile(&rc_file, embed_resource::NONE);
}
