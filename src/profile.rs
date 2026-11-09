//! 站点参数（target profile）：把「跟具体程序名绑定」的那些值全部挪到外部配置里，
//! 代码本体只保留中性变量名，不再出现任何第三方程序名。
//!
//! 取值优先级（后者覆盖前者）：
//!   1. 内建默认值 —— 全部为空，代表「不依赖任何程序名」
//!   2. exe 同目录的 `config.toml`
//!   3. 环境变量（`MCLICK_*`）
//!
//! 配置文件格式极简，只认 `key = "字符串"` 与 `key = ["a", "b"]` 两种写法，
//! 不引入任何 crates.io 依赖，保持「零外部依赖单文件 exe」的交付约束。

use std::sync::OnceLock;

#[derive(Clone, Debug, Default)]
pub struct Profile {
    /// 未显式指定 `--window` 时使用的默认窗口关键字（GUI 启动自动查找也用它）
    pub default_keyword: String,
    /// 「进程 + 类名」识别失败时的兜底：标题包含该子串即视为游戏本体
    pub fallback_title_hint: String,
    /// 干扰窗口的进程名前缀（小写比较），命中即排除，形如 "somehelpertool"
    pub noise_exe_prefixes: Vec<String>,
    /// 干扰窗口的标题前缀（小写比较），命中即排除
    pub noise_title_prefixes: Vec<String>,
}

static PROFILE: OnceLock<Profile> = OnceLock::new();
static CONFIG_FILE: OnceLock<String> = OnceLock::new();

/// 本次实际读取的配置文件路径（不存在时也返回候选路径，便于报错指路）
pub fn config_file() -> &'static str {
    CONFIG_FILE
        .get()
        .map(|s| s.as_str())
        .unwrap_or("<未初始化>")
}

/// 全局唯一实例；首次访问时惰性加载
pub fn get() -> &'static Profile {
    PROFILE.get_or_init(load)
}

/// 显式初始化（在 main 开头调一次，让加载早于任何窗口扫描）
pub fn init() {
    let _ = get();
}

fn exe_dir() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

fn load() -> Profile {
    let mut p = Profile::default();

    // ---- 2. config.toml ----
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(env_path) = std::env::var("MCLICK_CONFIG") {
        let t = env_path.trim().to_string();
        if !t.is_empty() {
            candidates.push(std::path::PathBuf::from(t));
        }
    }
    candidates.push(exe_dir().join("config.toml"));
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("config.toml"));
    }

    let mut used = String::new();
    for c in candidates {
        if c.is_file() {
            if let Ok(text) = std::fs::read_to_string(&c) {
                apply_toml(&mut p, &text);
                used = c.display().to_string();
            }
            break;
        }
    }
    if used.is_empty() {
        used = exe_dir().join("config.toml").display().to_string();
    }
    let _ = CONFIG_FILE.set(used);

    // ---- 3. 环境变量 ----
    if let Ok(v) = std::env::var("MCLICK_DEFAULT_KEYWORD") {
        if !v.trim().is_empty() {
            p.default_keyword = v.trim().to_string();
        }
    }
    if let Ok(v) = std::env::var("MCLICK_FALLBACK_TITLE_HINT") {
        if !v.trim().is_empty() {
            p.fallback_title_hint = v.trim().to_string();
        }
    }
    if let Ok(v) = std::env::var("MCLICK_NOISE_EXE_PREFIXES") {
        let list = split_list(&v);
        if !list.is_empty() {
            p.noise_exe_prefixes = list;
        }
    }
    if let Ok(v) = std::env::var("MCLICK_NOISE_TITLE_PREFIXES") {
        let list = split_list(&v);
        if !list.is_empty() {
            p.noise_title_prefixes = list;
        }
    }

    // 前缀统一小写，匹配时直接 starts_with
    p.noise_exe_prefixes = p
        .noise_exe_prefixes
        .into_iter()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    p.noise_title_prefixes = p
        .noise_title_prefixes
        .into_iter()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    p.default_keyword = p.default_keyword.trim().to_string();
    p.fallback_title_hint = p.fallback_title_hint.trim().to_string();
    p
}

fn split_list(s: &str) -> Vec<String> {
    s.split([',', ';', '|'])
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect()
}

/// 极简 TOML 解析：只处理一行一个 `key = value`，忽略注释与 [section] 头。
/// 支持的 value：`"字符串"` / `'字符串'` / `["a", "b"]`。
fn apply_toml(p: &mut Profile, text: &str) {
    for raw in text.lines() {
        let line = strip_comment(raw).trim();
        if line.is_empty() || line.starts_with('[') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let key = k.trim().trim_matches('"').trim_matches('\'').to_lowercase();
        let val = v.trim();
        match key.as_str() {
            "default_keyword" | "default-window-keyword" | "window" => {
                p.default_keyword = parse_str(val);
            }
            "fallback_title_hint" | "fallback-title-hint" | "fallback_title" => {
                p.fallback_title_hint = parse_str(val);
            }
            "noise_exe_prefixes" | "noise-exe-prefixes" | "noise_exe" => {
                p.noise_exe_prefixes = parse_arr(val);
            }
            "noise_title_prefixes" | "noise-title-prefixes" | "noise_title" => {
                p.noise_title_prefixes = parse_arr(val);
            }
            _ => {}
        }
    }
}

/// 去掉行内 `#` 注释，但不切字符串内部的 `#`
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_str: Option<u8> = None;
    for (i, b) in bytes.iter().enumerate() {
        match in_str {
            Some(q) => {
                if *b == q {
                    in_str = None;
                }
            }
            None => {
                if *b == b'"' || *b == b'\'' {
                    in_str = Some(*b);
                } else if *b == b'#' {
                    return &line[..i];
                }
            }
        }
    }
    line
}

fn parse_str(v: &str) -> String {
    let t = v.trim().trim_end_matches(',');
    let t = t.trim();
    t.trim_matches('"').trim_matches('\'').trim().to_string()
}

fn parse_arr(v: &str) -> Vec<String> {
    let t = v.trim();
    if !(t.starts_with('[') && t.ends_with(']')) {
        // 容许误写成单个字符串，或逗号分隔的裸串
        let one = parse_str(t);
        if one.is_empty() {
            return Vec::new();
        }
        return split_list(&one);
    }
    let inner = &t[1..t.len() - 1];
    split_list(inner)
        .into_iter()
        .map(|s| s.trim_matches('"').trim_matches('\'').trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// 供 CLI 打印：一眼看清本次生效的参数来源与取值
pub fn describe() -> String {
    let p = get();
    let list = |v: &Vec<String>| {
        if v.is_empty() {
            "(空)".to_string()
        } else {
            v.join(", ")
        }
    };
    let s = |v: &str| {
        if v.is_empty() {
            "(空)".to_string()
        } else {
            v.to_string()
        }
    };
    format!(
        "配置文件: {}\n  default_keyword      = {}\n  fallback_title_hint  = {}\n  noise_exe_prefixes   = {}\n  noise_title_prefixes = {}",
        config_file(),
        s(&p.default_keyword),
        s(&p.fallback_title_hint),
        list(&p.noise_exe_prefixes),
        list(&p.noise_title_prefixes)
    )
}
