#![windows_subsystem = "windows"]

//! 每5秒点一次魔精灵 —— 单文件交付版（零外部依赖 / 零外部文件 / 带原生窗体）
//!
//! 所有东西都在这一个 exe 里：48 张模板图（258 KB）用 `include_bytes!` 编译进只读数据段，
//! 截屏用 gdi32、点击用 user32、匹配用自己写的多尺度 NCC，窗体用 user32 原生控件，
//! 不依赖 OpenCV / DLL / 模型文件 / 运行库。
//!
//! 双击启动 = 原生 Win32 小窗体（GUI 子系统，不弹控制台黑框）。
//! 带参数启动 = 命令行模式（自动挂到控制台，便于脚本调用）。
//!
//! 用法示例：
//!   mojingling_clicker.exe --window "<标题关键字>" --interval 5 --times 0
//!   mojingling_clicker.exe --coords 960,540 --times 60          # 固定坐标
//!   mojingling_clicker.exe --self-test                          # 自检（无需游戏）
//!   mojingling_clicker.exe --window "<标题关键字>" --dump --times 1    # 存截图排查
//!
//! 安全提示：程序会真实移动鼠标并点击，请确认目标窗口后再运行。

mod assets;
mod capture;
mod click;
mod elevate;
mod ffi;
mod gui;
mod logger;
mod matcher;
mod sizelock;
mod winapi;

use ffi::Engine;
use matcher::{Match, Pyramid};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use winapi::{GetForegroundWindow, HWND, SetConsoleCtrlHandler};

/// 运行标志：任一时间为 false 即让主循环停下（Ctrl+C / 窗体「停止」都会置 false）
pub static RUNNING: AtomicBool = AtomicBool::new(false);

/// 节拍信息（供界面「记时」倒计时显示）：
/// LAST_TICK_MS = 最近一次点击发生的时刻（epoch 毫秒），TICK_SEQ = 已点次数，
/// INTERVAL_MS = 节拍周期（毫秒）。对应原实现里 MissionBall 的「Stopwatch + intervalSec」机制：
/// 到点执行命令后 swRestart() 重置秒表，等下一个周期。
pub static LAST_TICK_MS: AtomicU64 = AtomicU64::new(0);
pub static TICK_SEQ: AtomicU64 = AtomicU64::new(0);
pub static INTERVAL_MS: AtomicU64 = AtomicU64::new(5000);
/// 本次运行开始时刻（epoch 毫秒），日志里用来核对节拍间隔
pub static START_MS: AtomicU64 = AtomicU64::new(0);
/// HIT_AT_MS = 最近一次「点中魔精灵」的时刻（epoch 毫秒），0 表示本轮没命中。
/// 用户规则：5 秒周期的起点是「点到魔精灵那一刻」，不是本轮起手、也不是刷新按钮。
pub static HIT_AT_MS: AtomicU64 = AtomicU64::new(0);

/// 相对本次运行开始已过多少毫秒（日志打点用）
pub fn elapsed_ms() -> u64 {
    let s = START_MS.load(Ordering::Relaxed);
    if s == 0 {
        0
    } else {
        epoch_ms().saturating_sub(s)
    }
}

/// 当前 epoch 毫秒
pub fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

unsafe extern "system" fn ctrl_handler(_ctrl_type: u32) -> i32 {
    RUNNING.store(false, Ordering::SeqCst);
    1
}

#[derive(Clone)]
pub struct Config {
    pub interval_secs: u64,
    pub times: u32,
    pub coords: Option<(i32, i32)>,
    pub window_title: String,
    pub dll_path: Option<PathBuf>,
    pub model_dir: Option<PathBuf>,
    pub bring_to_front: bool,
    // 模板匹配参数
    pub threshold: f64,
    pub scales: Vec<f32>,
    pub roi: Option<(usize, usize, usize, usize)>,
    /// 三张手牌卡牌的查找区域（相对客户区的比例 x,y,w,h，0~1）。
    /// 非空时启用「三格手牌模式」：只在这三个框里找魔精灵，都找不到就点右侧刷新按钮。
    pub cells: Vec<(f32, f32, f32, f32)>,
    /// 三格右侧「刷新」按钮的相对位置（比例 0~1）
    pub refresh: Option<(f32, f32)>,
    /// 离线帧：用一张 BMP 当游戏画面（测试用，不截屏）
    pub offline_bmp: Option<PathBuf>,
    /// 空跑：只打印「会点哪里」，不真的移动鼠标（安全测试用）
    pub dry_run: bool,
    /// 毫秒级节拍（>0 时优先于 interval_secs，用于把周期压进 5 秒以内）
    pub interval_ms: u64,
    /// 刷新换牌后等牌面重绘的时间（毫秒）：默认 200；设为 0 表示刷完立刻抓图（可能抓到旧牌残留）
    pub miss_interval_ms: u64,
    /// 刷新后等查找的最长时间（毫秒）：超过它还没找到就立刻再刷一次。
    /// 实测「刷新到首次匹配到卡片」最慢约 500 ms（--probe-refresh 采样 8 轮），故默认 550 稍长之。
    pub find_wait_ms: u64,
    /// 「没钱」检测区域：相对客户区的比例矩形 x,y,w,h（默认盖住刷新按钮上的价格文字带）。
    /// 钱不够时这块会变红，检测到红就停下来等，不再空刷。
    pub money_roi: Option<(f32, f32, f32, f32)>,
    /// 上面那块区域里「红色像素占比」达到这个百分比就判为没钱（默认 1.2%，即 MONEY_RED_PCT）
    pub money_red_pct: f64,
    /// 没钱时的轮询间隔（毫秒）：每隔这么久看一次红没了没有（默认 400）
    pub money_wait_ms: u64,
    /// 是否启用「没钱等待」（默认开；--no-money-check 关掉）
    pub money_check: bool,
    /// 两次「点刷新」之间的最小间隔（毫秒，默认 1500）：刷太密会把金币刷光还刷不出东西
    pub refresh_gap_ms: u64,
    /// 诊断：采样 N 次打印金钱区域红色占比（手动把金币花光，看数值会不会跳，用来定阈值）
    pub probe_money: u32,
    /// 后台抓图通路：Auto（先 GetWindowDC+BitBlt，空帧再换 PrintWindow）/ dc / pw
    pub capture_mode: capture::CaptureMode,
    /// 后台点击：用 PostMessage 投递鼠标消息，不移动真实鼠标、不抢前台
    pub bg_click: bool,
    /// 诊断：打印两条抓图通路的画面特征后退出（判断本机游戏窗口吃哪条）
    pub probe: bool,
    /// 诊断：测量「点刷新 → 新牌面画完」的耗时，采样 N 轮后退出（0 = 不启用）
    pub probe_refresh: u32,
    /// 标定：扫描底部按钮栏，找出「点一下三格牌面就会变」的刷新按钮坐标后退出（换机标定用）
    pub scan_refresh: bool,
    pub step: usize,
    pub topk: usize,
    pub threads: usize,
    pub dump: bool,
    /// 命中区间下限（0 = 用 threshold）。配 --max-score 用，只扫「灰区」样本
    pub min_score: f64,
    /// 命中区间上限（1.0 及以上 = 不限）。超过它的分数一律不算命中
    pub max_score: f64,
    /// 只采样不点牌：命中后存图 + 刷新换牌，绝不点魔精灵（核验灰区样本用）
    pub sample_only: bool,
    /// 不参与匹配的模板编号（0-based）。用户确认 tpl#03、tpl#09、tpl#11、tpl#12
    /// 同样是魔精灵，但不是这游戏里出现过的皮肤，参与匹配只会制造误报，故默认剔除。
    pub skip_tpl: Vec<usize>,
    /// 「三格齐平」防线：三格全部命中同一张模板且分数极差很小 → 匹配到的是三格共同底纹
    /// （空槽/换牌过渡帧），不是真的有牌。默认开。
    pub bg_guard: bool,
    /// 齐平时的极差阈值（默认 0.25）：三格分数接近就判底纹
    pub bg_spread: f64,
    /// 齐平判定的分数上限（默认 0.93）：最高分已经很高时不再按「齐平」判底纹，
    /// 避免真的抽到两张同款皮肤被误杀
    pub bg_max: f64,
    /// 底纹判据二「拉不开」的阈值（默认 0.20）：最高分与次高分之差小于该值，说明三格分数
    /// 挤在一起（底纹特征），判为共同底纹；真牌是「一格 0.83~0.95、另两格 0.38~0.61」，
    /// 实测落差 0.29~0.53，余量很足。判据不要求三格命中同一张模板。
    pub bg_gap: f64,
    pub self_test: bool,
    /// 强制锁定游戏窗体的**客户区**尺寸（--size WxH）。
    /// 用于跨机器统一模板对齐尺寸：模板按客户区裁，客户区一致匹配尺度才对得上。
    pub size_lock_client: Option<(u32, u32)>,
    /// 诊断：把目标窗体真的改到 --size 尺寸，打印前后尺寸后退出（换机验证能否被改）
    pub size_test: bool,
    /// 详细输出（-g / --verbose）：带它启动 = 打印全部信息（坐标、相似度、耗时等）；
    /// 不带 = 只打印最简状态（点中了几次、还剩多久、钱够不够），不带坐标等细节。
    pub verbose: bool,
    /// 点击前复检（默认开，--no-recheck 关）：命中后要等到节拍到点才点，这期间牌可能已被
    /// 游戏换掉/播动画移位。到点那一刻重新抓一帧确认牌还在、分数还够，再按新坐标点，
    /// 避免「识别到了却点空」。
    pub recheck: bool,
    /// 贴阈值宽限（默认开，--no-borderline-grace 关）：最高分贴在阈值边上（动画中没画完的真牌）
    /// 时先多等 BORDERLINE_GRACE_ROUNDS 轮再刷新，别急着把它刷掉。
    pub borderline_grace: bool,
    /// 刷新效果核对（默认开，--no-refresh-verify 关）：刷新后比对三格区域像素，
    /// 差异过小说明这次刷新没生效（按钮坐标偏了 / 后台点击被忽略），日志告警，避免白花钱一直空刷。
    pub refresh_verify: bool,
    /// 低分兜底点击（默认开，--no-loose-hit 关）：最高分只比命中线低一点点（LOOSE_DROP 以内）、
    /// 且「一格独高」时，照样点出去，不再去刷新换牌 —— 解决「明明有一张卡却视而不见去刷新」。
    pub loose_hit: bool,
    /// 「三格必有一格」模式（默认开，--no-assume-one 关）：游戏规则是刷新后三格手牌里必有
    /// 一张魔精灵，所以分数没到命中线不代表「没有」，而代表「这张牌画得不像模板」。
    /// 开启后取三格最高分那格直接点，不再因为没过阈值去刷新换牌。
    pub assume_one: bool,
    /// 「三格必有一格」模式的最低可信分（默认 0.55，--assume-floor 调）。
    /// 低于它的帧（空槽 / 坐标偏了 / 完全对不上的画面，实测 0.25~0.38）不点，
    /// 免得把三格空白当成牌乱点乱刷。
    pub assume_floor: f64,
}

impl Config {
    /// 命中区间下限：--min-score 优先，否则退回 threshold
    pub fn lo_score(&self) -> f64 {
        if self.min_score > 0.0 {
            self.min_score
        } else {
            self.threshold
        }
    }
    /// 分数是否落在当前命中区间内（默认就是 score >= threshold）
    pub fn hit(&self, score: f64) -> bool {
        score >= self.lo_score() && score <= self.max_score
    }
    /// 命中区间的可读描述，用于日志
    pub fn range_txt(&self) -> String {
        format!("{:.2}~{:.2}", self.lo_score(), self.max_score)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            interval_secs: 5,
            times: 0,
            coords: None,
            window_title: String::new(),
            dll_path: None,
            model_dir: None,
            bring_to_front: false,
            threshold: 0.78,
            scales: vec![1.0, 0.8, 1.25],
            roi: None,
            cells: default_cells(),
            refresh: Some((REFRESH_X, REFRESH_Y)),
            offline_bmp: None,
            dry_run: false,
            interval_ms: 0,
            miss_interval_ms: 100,
            find_wait_ms: 550,
            money_roi: Some(MONEY_ROI),
            money_red_pct: MONEY_RED_PCT,
            money_wait_ms: 400,
            money_check: true,
            refresh_gap_ms: REFRESH_GAP_MS,
            probe_money: 0,
            capture_mode: capture::CaptureMode::Auto,
            // 默认走后台消息式点击：不移动真实鼠标、不抢前台。
            // 想回到物理鼠标（SetCursorPos + mouse_event）用 --fg-click 显式开。
            bg_click: true,
            probe: false,
            probe_refresh: 0,
            scan_refresh: false,
            step: 2,
            topk: 6,
            threads: 0,
            dump: false,
            min_score: 0.0,
            max_score: 1.0,
            sample_only: false,
            skip_tpl: vec![3, 9, 11, 12],
            bg_guard: true,
            bg_spread: 0.25,
            bg_max: 0.93,
            bg_gap: 0.20,
            self_test: false,
            size_lock_client: None,
            size_test: false,
            verbose: false,
            recheck: true,
            borderline_grace: true,
            refresh_verify: true,
            loose_hit: true,
            assume_one: true,
            assume_floor: 0.20,
        }
    }
}

/// 三格手牌（正下方三个方框）的默认查找区域，与三格右侧「刷新」按钮位置。
///
/// 数值来自 2026-11-08 的实测截图（2400x1080，即客户区满幅画面）：
///   第1格 像素 x 628~854   第2格 x 866~1092   第3格 x 1104~1330
///   三格纵向 y 924~1058（每格约 226x134）
///   刷新按钮 像素 (1550, 967)，正落在这三个方框的右侧
/// 这里存「相对比例」，运行时按客户区实际宽高换算 —— 游戏窗口换尺寸也不用改，
/// 但若游戏把卡牌位置整体挪了，需要重新标定（可用 --cells / --refresh 覆盖）。
pub const REFRESH_X: f32 = 0.711538;
pub const REFRESH_Y: f32 = 0.906200;

/// 「没钱」检测区域（相对客户区比例 x,y,w,h）：刷新按钮上的「价格数字」文字带。
///
/// 实测 1040x629 客户区下该区域为 x 702~772 / y 590~616（8 倍放大复核：数字"230"/"500"整串都在带内）。
/// 2026-11-08 采样：钱够时该带红像素 0 个（0.00%），比旧 ROI 干净得多；钱不够时整串数字变红，
/// 占比会直接跳高，所以拿「红像素占比」当判据既灵敏又不误报。用 --money-roi 可以换到别的区域。
/// 旧值 (0.680,0.900,0.080,0.060) 只压到数字下边缘、且把骰子图标上的红点也算进来（恒定 0.22%），已废弃。
pub const MONEY_ROI: (f32, f32, f32, f32) = (0.6750, 0.9380, 0.0673, 0.0413);

/// 「钱没了」判定阈值：MONEY_ROI 内红像素占比（%）达到它就暂停刷新/点击去等钱。
///
/// 预设在 1.2%（不是实测标定值，实战里钱一直够花、采不到变红跳变，按面积推算后取的保守值）：
///   - 该 ROI 在 1040x629 客户区下是 70x26 = 1820 像素，钱够时红像素恒为 0（541 帧连续采样 0.00%）；
///   - 数字整串变红约占 150 像素 ≈ 8%，哪怕只红一位数字也 ≥ 25 像素 ≈ 1.4%；
///   - 取 1.2% 相当于 ~22 像素，略低于「红一位数字」的量级：真变红必触发，同时不受基线 0% 的零星噪点干扰。
/// 2026-11-09 调整：1.2% 太低。实测点中后的按钮高亮 / 牌面闪红能让该区域单帧就冲到 1.2% 以上，
/// 于是判了「没钱」，可 0.4 秒后第一次复查红又掉回去了 —— 纯瞬时误报，却白打断一轮刷新。
/// 现在改为 2.0% 触发，且必须再连续复检 MONEY_CONFIRM_ROUNDS 次都超阈值才认「真没钱」。
/// 实战若发现该进不进（画面明显变红却没停），用 --money-red-pct 往下调到 1.0；若误停，往上调到 3.0。
pub const MONEY_RED_PCT: f64 = 2.0;

/// 「钱回来了」判定阈值：等钱期间红占比低于它才算恢复（%）。
/// 比 MONEY_RED_PCT 低一档形成滞回，避免占比卡在阈值附近时反复「等 → 继续 → 又等」。
pub const MONEY_RED_EXIT_PCT: f64 = 0.5;

/// 没钱判定的连续确认轮数：首帧超阈值后，还要再连抓这么多次、每次都超阈值，才判「真没钱」。
/// 任一轮回落到阈值以下 = 瞬时误报，直接放行继续干活。
pub const MONEY_CONFIRM_ROUNDS: u32 = 3;

/// 上面每轮确认之间的间隔（毫秒）：3 次采样铺开约 0.5 秒，足够穿过按钮高亮 / 闪红。
pub const MONEY_CONFIRM_GAP_MS: u64 = 250;

/// 等钱结束时「钱回来了」的连续确认轮数：连续这么多次低于恢复线才放行，
/// 免得单帧抖动（恰好抓到牌面淡出的暗帧）就又冲出去刷。
pub const MONEY_OK_ROUNDS: u32 = 2;

/// 两次「点刷新」之间的最小间隔（毫秒）。
///
/// 原逻辑：刷新后等 find_wait_ms(550) 或「画面静止」(250ms) 就立刻再刷一次，实测两刷间隔只有
/// 450~570 ms —— 一分钟能刷 100 次以上，一次 15 金币，钱掉得比刷出魔精灵快得多。
/// 现在统一由这个下限兜住：无论「超时未找到」还是「画面静止确认没有」，都得等满它才允许再刷。
/// 取 1500 ms ≈ 每分钟 40 次刷新 × 15 金币 = 600 金币/分钟的上限，给攒钱留出余量；
/// 想更快用 --refresh-gap-ms 1200，想更省用 --refresh-gap-ms 2500。
pub const REFRESH_GAP_MS: u64 = 1500;

/// 「三格齐平」判据二的保护余量：判据二只在「三格都像牌、只是分不开」时才成立。
///
/// 旧实现只看 `最高分 - 次高分 < bg_gap(0.20)`：真牌帧次高分落在 0.38~0.61，一旦
/// 命中格分数被动画/缩放压到 0.79 左右，落差就掉到 0.19 以下，真牌被当成底纹判掉
/// ——表现为「明明有魔精灵却一直不点、继续刷钱」。现在要求次高分也 ≥ 命中线 - 该余量，
/// 真牌另两格（≤0.61）远达不到，底纹帧（三格 0.76~0.93）全部满足，区分度很足。
pub const BG_SECOND_MARGIN: f64 = 0.10;

/// 贴阈值帧的宽限轮数：最高分落在 [阈值-0.08, 阈值) 时，先多等几轮再判「没有」。
/// 真牌入场动画还没画完时分数会被压低，急着刷新等于自己把牌刷掉。
/// 实测牌面 66 ms 开始换、300~367 ms 才画完，2 轮 ×120 ms = 240 ms 盖不住画完那一刻，
/// 故放宽到 4 轮 ×150 ms = 600 ms（宽限期间只重扫不刷新，不花钱）。
pub const BORDERLINE_GRACE_ROUNDS: u32 = 4;

/// 贴阈值帧每轮的等待时间（毫秒）
pub const BORDERLINE_GRACE_MS: u64 = 150;

/// 「低分兜底点击」允许的最低分落差（默认 0.12）。
///
/// 真牌被入场动画 / 窗口缩放 / 半透明过渡压低分数时，最高分会掉到命中线以下一点点
/// （0.66~0.78）。这种情况如果直接刷新，等于花钱把真牌刷掉 —— 用户看到的现象就是
/// 「明明有一张卡却视而不见去刷新」。
///
/// 兜底点击的触发条件很严（三条同时成立），误点风险很低：
///   ① 最高分 ≥ 命中线 - 该落差；
///   ② 最高分 - 次高分 ≥ bg_gap(0.20)，即「一格独高」，底纹帧（三格挤在一起）进不来；
///   ③ 其实也就是底纹守卫已经放行的帧。
/// 代价分析：误点时点空槽/别的牌不消耗刷新金币；而漏打再刷新则必花一次金币。
/// 想完全禁止用 `--no-loose-hit`。
pub const LOOSE_DROP: f64 = 0.12;

/// 「三格齐平」判据一的最低分前提：三格分数极差小，还要三格**都**像牌才算共同底纹。
///
/// 旧实现只看「三格同模板 + 极差 < bg_spread(0.25)」：真牌那格 0.82、另两格空槽被同一张
/// 模板承接成 0.60/0.62 时，极差 0.22 < 0.25 → 真牌被当底纹判掉（漏打）。
/// 底纹帧三格都在 0.76~0.93，最低分也高；真牌帧另两格只有 0.38~0.62。
/// 所以补一条「最低分 ≥ 命中线 - BG_SECOND_MARGIN 才算齐平」。
pub fn bg_flat_floor(lo: f64) -> f64 {
    lo - BG_SECOND_MARGIN
}

/// 刷新后「牌面真的变了」的判据：三格区域像素差异低于该百分比 = 这次刷新很可能没生效
/// （刷新按钮坐标偏了、或后台点击被游戏忽略）。此时只告警不阻断，避免白刷金币还以为在换牌。
pub const REFRESH_EFFECT_MIN_PCT: f64 = 0.5;

/// 连续这么多次「刷新了但牌面没变」就停止自动点击（默认 3 次）。
///
/// 这是乱刷钱的最后一道止血阀：如果刷新按钮坐标在当前窗口尺寸/界面下是错的，
/// 程序会一次次点下去、金币一次次扣掉、牌面一动不动，用户却只看到「钱少了」。
/// 与其无限消耗，不如停手并给出明确的排查指引（--scan-refresh 重新标定 / --fg-click）。
pub const REFRESH_BLOCK_ROUNDS: u32 = 3;

/// 刷新后「判这手牌没有」之前的最短观察窗口（毫秒）。
///
/// 这是「空刷」的第一道防线：新牌从开始换到画完实测 300~367 ms，再加上点刷新 →
/// 游戏收到 → 重绘的链路延迟，一帧就判空等于在牌面还没画出来的空档里把钱刷掉。
pub const EMPTY_MIN_OBSERVE_MS: u64 = 1200;

/// 「三格必有一格」模式下，判这手牌没有之前的最短观察窗口（毫秒）。
///
/// 【2026-11-09 新增】规则前提变了：三格手牌里必有一张魔精灵，牌槽为空只等于「没钱」，
/// 而没钱由 ensure_money / confirm_money_red 的红色像素检测负责。
/// 那么 match_cells 走到 Miss 就只剩两种解释：①牌还没画完（抓早了）②坐标偏了/窗口变了。
/// 两种都不该靠「再花金币刷一次」来解决 —— 前者该等，后者该告警。
/// 所以 assume_one 模式下把观察窗口从 1200 ms 抬到 1500 ms，给牌面留足绘制时间。
pub const ASSUME_MISS_OBSERVE_MS: u64 = 1500;

/// 「三格必有一格」模式下，到点复检允许的最低相似度（复检绝对底线）。
///
/// 【2026-11-09 v1.3.3 新增】手牌从「刚刷新」放到「满 5 秒」被点，牌面会因动画/特效被压分：
/// 实测同一张牌刷新后 0.6 秒测得 0.819，放满 5 秒只有 0.626。
/// 旧代码用「命中线 - 0.05」（≈0.730）当复检门槛，于是这张真牌每次都被否掉 → 刷牌 → 空烧金币。
/// 三格必有一格的前提下，复检落空只可能是抓早了或画面在动，不该升级成花金币换牌，
/// 所以这里给一个独立且更宽松的绝对底线，只用来识别「画面完全不对」（窗口遮挡/坐标全偏/纯黑）。
pub const RECHECK_ASSUME_FLOOR: f64 = 0.45;

/// 判「这手牌没有」需要的连续空帧票数（多帧投票，替代单帧判决）。
pub const EMPTY_VOTES_NEED: u32 = 3;

/// 这些空帧至少要跨越这么久（毫秒），避免三帧挤在 60 ms 里等于还是单帧判决。
pub const EMPTY_VOTES_SPAN_MS: u64 = 300;

/// 刷新未被确认生效（牌面几乎没变）时最多再等这么久（毫秒），超时打告警并放行。
///
/// 游戏卡顿时「刷新生效」会晚到：3 秒足够让一次正常刷新画完，超了就不是慢而是失效。
pub const REFRESH_UNSURE_WAIT_MS: u64 = 3000;

/// 等「牌面画完」时的轮询间隔（毫秒）：只抓帧不点击，20 ms 太费 CPU，100 ms 够用。
pub const UNSETTLED_POLL_MS: u64 = 100;


/// 三个方框的相对矩形（x, y, w, h）
pub fn default_cells() -> Vec<(f32, f32, f32, f32)> {
    vec![
        (0.384615, 0.810811, 0.080769, 0.178060), // 第1格（实测 1040x629 客户区：x0=400 y0=510 84x112）
        (0.468269, 0.810811, 0.080769, 0.178060), // 第2格（x0=487，格距 87px）
        (0.551923, 0.810811, 0.080769, 0.178060), // 第3格（x0=574，图标实测左上 582,520）
    ]
}

/// 解析 "x,y,w,h;x,y,w,h;..." 形式的相对矩形列表
fn parse_cells(s: &str) -> Vec<(f32, f32, f32, f32)> {
    let mut out = Vec::new();
    for seg in s.split(';') {
        let p: Vec<f32> = seg
            .split(',')
            .filter_map(|x| x.trim().parse::<f32>().ok())
            .collect();
        if p.len() == 4 && p[2] > 0.0 && p[3] > 0.0 {
            out.push((p[0], p[1], p[2], p[3]));
        }
    }
    out
}

/// 解析 "x,y" 形式的相对坐标
fn parse_point(s: &str) -> Option<(f32, f32)> {
    let p: Vec<f32> = s
        .split(',')
        .filter_map(|x| x.trim().parse::<f32>().ok())
        .collect();
    if p.len() == 2 {
        Some((p[0], p[1]))
    } else {
        None
    }
}

fn parse_scales(s: &str) -> Vec<f32> {
    let mut v: Vec<f32> = s
        .split(',')
        .filter_map(|x| x.trim().parse::<f32>().ok())
        .filter(|x| *x >= 0.3 && *x <= 3.0)
        .collect();
    if v.is_empty() {
        v = vec![1.0];
    }
    v
}

fn parse_roi(s: &str) -> Option<(usize, usize, usize, usize)> {
    let p: Vec<usize> = s.split(',').filter_map(|x| x.trim().parse().ok()).collect();
    if p.len() == 4 && p[2] > 0 && p[3] > 0 {
        Some((p[0], p[1], p[2], p[3]))
    } else {
        None
    }
}

/// 解析 "x,y,w,h" 形式的相对比例矩形（0~1），用于 --money-roi
fn parse_rect(s: &str) -> Option<(f32, f32, f32, f32)> {
    let p: Vec<f32> = s.split(',').filter_map(|x| x.trim().parse().ok()).collect();
    if p.len() == 4 && p[2] > 0.0 && p[3] > 0.0 {
        Some((p[0], p[1], p[2], p[3]))
    } else {
        None
    }
}

/// 解析 "宽x高"（也接受 "宽*高"、"宽,高"），用于 --size 强制客户区尺寸
fn parse_size(s: &str) -> Option<(u32, u32)> {
    let t = s.trim().to_lowercase().replace(['*', ','], "x");
    let p: Vec<u32> = t
        .split('x')
        .filter_map(|x| x.trim().parse::<u32>().ok())
        .collect();
    // 低于 200x200 基本不可能是游戏窗体，判为手抖输错
    if p.len() == 2 && p[0] >= 200 && p[1] >= 200 {
        Some((p[0], p[1]))
    } else {
        None
    }
}

fn parse_args() -> Config {
    let mut cfg = Config::default();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0usize;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--interval" => {
                i += 1;
                cfg.interval_secs = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(5);
                cfg.interval_ms = 0;
            }
            "--interval-ms" => {
                i += 1;
                if let Some(ms) = args.get(i).and_then(|s| s.parse::<u64>().ok()) {
                    cfg.interval_ms = ms.max(200);
                }
            }
            "--miss-interval-ms" => {
                i += 1;
                if let Some(ms) = args.get(i).and_then(|s| s.parse::<u64>().ok()) {
                    cfg.miss_interval_ms = ms;
                }
            }
            "--refresh-gap-ms" => {
                i += 1;
                if let Some(ms) = args.get(i).and_then(|s| s.parse::<u64>().ok()) {
                    cfg.refresh_gap_ms = ms.max(200);
                }
            }
            "--find-wait-ms" => {
                if let Some(v) = args.get(i + 1) {
                    match v.parse::<u64>() {
                        Ok(ms) => cfg.find_wait_ms = ms.max(100),
                        Err(_) => {
                            logln!("--find-wait-ms 需要一个毫秒数（>=100）");
                            std::process::exit(1);
                        }
                    }
                }
            }
            "--times" => {
                i += 1;
                cfg.times = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(0);
            }
            "--money-roi" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    match parse_rect(v) {
                        Some(r) => cfg.money_roi = Some(r),
                        None => logw!("[warn] --money-roi 需要 4 个比例数 x,y,w,h，已保持默认"),
                    }
                }
            }
            "--money-red-pct" => {
                i += 1;
                if let Some(p) = args.get(i).and_then(|s| s.parse::<f64>().ok()) {
                    cfg.money_red_pct = p.max(0.01);
                }
            }
            "--money-wait-ms" => {
                i += 1;
                if let Some(ms) = args.get(i).and_then(|s| s.parse::<u64>().ok()) {
                    cfg.money_wait_ms = ms.max(50);
                }
            }
            "--no-money-check" => cfg.money_check = false,
            "--probe-money" => {
                let n = args.get(i + 1).and_then(|v| v.parse::<u32>().ok()).unwrap_or(10);
                cfg.probe_money = n.max(1);
            }
            "--coords" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    let mut it = v.split(',');
                    if let (Some(x), Some(y)) = (it.next(), it.next()) {
                        if let (Ok(x), Ok(y)) = (x.trim().parse(), y.trim().parse()) {
                            cfg.coords = Some((x, y));
                        }
                    }
                }
            }
            "--window" => {
                i += 1;
                cfg.window_title = args.get(i).cloned().unwrap_or_default();
            }
            "--dll" => {
                i += 1;
                cfg.dll_path = args.get(i).map(PathBuf::from);
            }
            "--model" => {
                i += 1;
                cfg.model_dir = args.get(i).map(PathBuf::from);
            }
            "--front" => cfg.bring_to_front = true,
            "--threshold" => {
                i += 1;
                cfg.threshold = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(0.78);
            }
            "--scales" => {
                i += 1;
                if let Some(s) = args.get(i) {
                    cfg.scales = parse_scales(s);
                }
            }
            "--roi" => {
                i += 1;
                if let Some(s) = args.get(i) {
                    cfg.roi = parse_roi(s);
                }
            }
            "--cells" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    let c = parse_cells(v);
                    if !c.is_empty() {
                        cfg.cells = c;
                    }
                }
            }
            "--refresh" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    cfg.refresh = parse_point(v);
                }
            }
            "--no-cells" => cfg.cells.clear(),
            "--image" => {
                i += 1;
                cfg.offline_bmp = args.get(i).map(PathBuf::from);
            }
            "--dry-run" => cfg.dry_run = true,
            "--min-score" => {
                i += 1;
                cfg.min_score = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(0.0);
            }
            "--max-score" => {
                i += 1;
                cfg.max_score = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(1.0);
            }
            "--sample-only" => {
                cfg.sample_only = true;
                cfg.dump = true;
            }
            "--skip-tpl" => {
                // 0-based 模板编号，逗号分隔；传 none 表示不排除任何模板
                i += 1;
                match args.get(i).map(|s| s.trim().to_ascii_lowercase()) {
                    Some(v) if v == "none" || v.is_empty() => cfg.skip_tpl.clear(),
                    Some(v) => {
                        cfg.skip_tpl = v
                            .split([',', ' ', '，'])
                            .filter_map(|x| x.trim().parse::<usize>().ok())
                            .collect()
                    }
                    None => {}
                }
            }
            "--no-bg-guard" => cfg.bg_guard = false,
            "--no-recheck" => cfg.recheck = false,
            "--no-borderline-grace" => cfg.borderline_grace = false,
            "--no-refresh-verify" => cfg.refresh_verify = false,
            "--no-loose-hit" => cfg.loose_hit = false,
            "--no-assume-one" => cfg.assume_one = false,
            "--assume-one" => cfg.assume_one = true,
            "--assume-floor" => {
                i += 1;
                cfg.assume_floor = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(0.55);
            }
            "--bg-spread" => {
                i += 1;
                cfg.bg_spread = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(0.25);
            }
            "--bg-max" => {
                i += 1;
                cfg.bg_max = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(0.93);
            }
            "--bg-gap" => {
                i += 1;
                cfg.bg_gap = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(0.20);
            }
            "--capture" => {
                i += 1;
                if let Some(m) = args.get(i).and_then(|s| capture::CaptureMode::parse(s)) {
                    cfg.capture_mode = m;
                } else {
                    logw!("[warn] --capture 只接受 auto | d3d | dc | pw | scr，已保持 {}", cfg.capture_mode.label());
                }
            }
            "--bg-click" => cfg.bg_click = true,
            "--fg-click" => {
                // 显式切回前台物理点击（会移动真实鼠标）；默认已不再使用
                cfg.bg_click = false;
            }
            "--background" => {
                cfg.bg_click = true;
                cfg.bring_to_front = false;
            }
            "--scan-refresh" => cfg.scan_refresh = true,
            "--probe" => cfg.probe = true,
            "--step" => {
                i += 1;
                cfg.step = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(2).max(1);
            }
            "--topk" => {
                i += 1;
                cfg.topk = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(6).max(1);
            }
            "--threads" => {
                i += 1;
                cfg.threads = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(0);
            }
            "--dump-dir" => {
                // 指定截图输出目录（可以是 UNC 共享路径），方便把画面拿给别处分析
                i += 1;
                if let Some(v) = args.get(i) {
                    set_dump_dir(v);
                    cfg.dump = true;
                }
            }
            "--size" | "--client-size" => {
                // 强制把游戏窗体客户区固定为 WxH（跨机器统一模板对齐尺寸用）
                match args.get(i + 1).and_then(|v| parse_size(v)) {
                    Some((w, h)) => {
                        cfg.size_lock_client = Some((w, h));
                        crate::sizelock::set_force_client(w as i32, h as i32);
                    }
                    None => {
                        logln!("--size 需要 WxH 形式（如 --size 1040x629）");
                        std::process::exit(1);
                    }
                }
                i += 1;
            }
            "--size-test" => cfg.size_test = true,
            "--dump" => cfg.dump = true,
            "--probe-refresh" => {
                // 后面可跟采样轮数；不跟数字就默认 8 轮
                let n = args
                    .get(i + 1)
                    .and_then(|v| v.parse::<u32>().ok())
                    .unwrap_or(8);
                cfg.probe_refresh = n.max(1);
            }
            "--self-test" => cfg.self_test = true,
            // -g：详细输出（坐标、相似度、耗时全打）；不带 = 只打最简状态
            "-g" | "--verbose" | "-v" => cfg.verbose = true,
            "--help" | "-h" => {
                logln!(
                    "用法: Mclick.exe [选项]\n\
                     \n识别（默认，内置模板，无需任何外部文件）:\n  \
                     --window <标题>     目标窗口标题子串\n  \
                     --threshold <0~1>   相似度阈值（默认 0.78）\n  \
                     --min-score <0~1>   命中区间下限（覆盖 threshold），配 --max-score 只抓灰区\n  \
                     --max-score <0~1>   命中区间上限（默认 1.0，超过它的分数不算命中）\n  \
                     --sample-only       只采样不点牌：灰区命中就存图再刷新（配合上面两个用）\n  \
                     --skip-tpl <a,b,..> 剔除指定模板编号（0-based，默认 3,9,11,12），none = 不剔除\n  \
                     --no-bg-guard       关掉「三格齐平/最高次高分不开 → 判底纹」防线\n  \
                     --no-recheck        关掉「到点点击前复检」（默认开：点之前重抓一帧确认牌还在，防点空）\n  \
                     --no-borderline-grace 关掉「贴阈值宽限」（默认开：分数贴着阈值时多等两轮，防把动画中的真牌刷掉）\n  \
                     --no-refresh-verify 关掉「刷新效果核对」（默认开：刷新后牌面没变会告警，防白刷金币）\n  \
                    --no-loose-hit 关掉「低分兜底点击」（默认开：分数只差命中线一点点且一格独高时照样点，防有卡却去刷新）\n  \
                     --no-assume-one 关掉「三格必有一格」（默认开：三格手牌必有一张魔精灵，取最高分那格直接点，不过阈值也不刷新）\n  \
                     --assume-floor <f> 「三格必有一格」的最低可信分（默认 0.55，低于它视为空槽/坐标偏了，不点）\n  \
                     --bg-spread <f>     齐平极差阈值（默认 0.25，判据一）\n  \
                     --bg-max <f>        底纹判定的分数上限（默认 0.93，高分不再判底纹）\n  \
                     --bg-gap <f>        最高分与次高分的最小落差（默认 0.20，判据二）\n  \
                     --scales <a,b,..>   模板缩放比例（默认 1.0,0.8,1.25）\n  \
                     --roi x,y,w,h       只在客户区指定区域搜索（加速用）\n  \
                     --cells x,y,w,h;x,y,w,h  三格手牌查找区域（相对比例，默认已内置三格）\n  \
                     --refresh x,y       三格右侧「刷新」按钮的相对位置（默认已内置）\n  \
                     --no-cells          关掉三格模式，退回整窗金字塔搜索\n  \
                     --money-roi x,y,w,h 「没钱」检测区域（相对比例，默认盖住刷新价格文字带）\n  \
                     --money-red-pct <n> 红像素占比达到 n% 判为没钱（默认 2.0；还需连续复检 3 次都超阈值才算数）\n  \
                     --money-wait-ms <n> 没钱时每隔 n 毫秒复查一次（默认 400）\n  \
                     --no-money-check    关掉「金币变红就等待」\n  \
                     --probe-money [N]   采样金币区域红占比 N 次（默认 10）后退出，用于定阈值\n  \
                     --image <bmp>       用一张 BMP 当游戏画面做离线测试（不截屏）\n  \
                     --dry-run           空跑：只打印会点哪里，不真的动鼠标\n  \
                     \n后台（与 TFJL 同款通路）:\n  \
                     --background        后台模式 = 后台抓图 + 后台点击（默认即是；显式写一遍更保险）\n  \
                     --fg-click          切回前台物理点击（SetCursorPos+mouse_event），只用于后台消息点不动时排查\n  \
                     --size WxH          把游戏窗体客户区强制成 WxH（如 1040x629，跨机器统一模板对齐尺寸）\n  \
                     --size-test         配合 --size：真的改一次目标窗体尺寸，打印前后客户区后退出\n  \
                     --capture <m>       抓图通路 auto(默认)|d3d(D3D子窗PrintWindow，微信小程序首选，纯后台)|dc(GetWindowDC+BitBlt)|pw(PrintWindow)|scr(屏幕抓取，需窗口不被遮挡)\n  \
                     --interval-ms <n>   毫秒级周期（如 5000）：两次点中魔精灵的目标间隔\n  \
                     --miss-interval-ms <n>  刷新换牌后等牌面重绘的时间（默认 200）\n  \
                     --refresh-gap-ms <n> 两次刷新之间的最小间隔（默认 1500 ms）：刷太密会把金币刷光
  \
                      --find-wait-ms <n>   刷新后等查找的最长时间（默认 550 ms）：到点还没找到就再刷（仍受 refresh-gap 下限）\n  \
                     --bg-click          只把点击改成 PostMessage 消息式（鼠标不移动）\n  \
                     --scan-refresh      扫描底部按钮栏，自动定位「刷新」按钮坐标后退出（换机标定用）\n  \
                     --probe             打印两条抓图通路的画面特征后退出（判断哪条有效）\n  \
                     --probe-refresh [N] 测量「点刷新 → 新牌面画完」耗时，采样 N 轮（默认 8）后退出\n  \
                     --step <N>          粗筛步长（默认 2）\n  \
                     --topk <N>          精修候选数（默认 6）\n  \
                     --threads <N>       匹配线程数（默认自动）\n  \
                     \n其他模式:\n  \
                     --coords x,y        固定坐标模式（不识别，直接点）\n  \
                     --dll <路径>        改用第三方 DLL 识别\n  \
                     --model <路径>      DLL 模式下的模型路径\n  \
                     \n通用:\n  \
                     -g, --verbose       详细输出：打印全部信息（坐标、相似度、耗时等）；不带则只打最简状态\n  \
                     --interval <秒>     每轮间隔（默认 5）\n  \
                     --times <N>         点中魔精灵的次数，0=无限（默认 0）\n  \
                     --front             每次点击前把窗口置前\n  \
                     --dump              把截图存到 temp 目录（排查用）\n  \
                         --dump-dir <目录>    截图存到指定目录（支持 UNC 共享路径）\n  \
                     --self-test         无游戏自检：合成图像验证匹配引擎\n  \
                     --help              显示本帮助"
                );
                std::process::exit(0);
            }
            _ => {}
        }
        i += 1;
    }
    // 环境变量也能开详细输出（GUI 双击场景没地方塞参数）
    let env_verbose = std::env::var_os("MOJINGLING_VERBOSE")
        .map(|v| v != "0" && !v.is_empty())
        .unwrap_or(false);
    logger::set_verbose(cfg.verbose || env_verbose);
    cfg.verbose = logger::verbose();
    cfg
}

/// 读程序同目录（或当前工作目录）的 `size.cfg`：只要里面写一行 `宽x高`（如 1040x629），
/// 启动即把游戏窗体客户区强制成该尺寸——双击/快捷方式启动也能生效，不用每次记参数。
fn load_size_cfg() {
    let mut cands: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            cands.push(d.join("size.cfg"));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        cands.push(cwd.join("size.cfg"));
    }
    for p in cands {
        let txt = match std::fs::read_to_string(&p) {
            Ok(v) => v,
            Err(_) => continue,
        };
        // 只认第一行，允许 # 注释行
        let line = txt
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty() && !l.starts_with('#'))
            .unwrap_or("")
            .to_string();
        if line.is_empty() {
            continue;
        }
        match parse_size(&line) {
            Some((w, h)) => {
                crate::sizelock::set_force_client(w as i32, h as i32);
                logln!(
                    "[init] 读到 {}：强制游戏窗体客户区为 {}x{}",
                    p.display(),
                    w,
                    h
                );
                return;
            }
            None => logln!(
                "[init] {} 内容无法解析（要写一行 宽x高，如 1040x629）",
                p.display()
            ),
        }
    }
}

fn main() {
    // 启动横幅先落盘：无论后面是崩了、被关了还是正常退出，日志文件里都有这一条起点
    logger::file_banner("Mclick");
    load_size_cfg();
    // 崩溃/未捕获异常也用红色打出来（默认 panic 信息不进日志框）
    std::panic::set_hook(Box::new(|info| {
        let loc = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "未知位置".to_string());
        let msg = match info.payload().downcast_ref::<&str>() {
            Some(s) => (*s).to_string(),
            None => match info.payload().downcast_ref::<String>() {
                Some(s) => s.clone(),
                None => "未知异常".to_string(),
            },
        };
        loge!("[error] 程序异常终止：{msg}（{loc}）");
        // panic 后进程可能立即结束，这里强制把日志刷到磁盘，否则「无声消失」查不到任何东西
        logger::flush_file();
    }));
    // 提权：非管理员时用 UAC 把自己以管理员身份重新拉起（详见 elevate.rs）。
    // 返回 true 说明新进程已起，当前普通权限进程立刻退出，避免两个实例抢着点。
    if elevate::relaunch_if_needed() {
        logger::flush_file();
        return;
    }
    // 无参数（双击）→ 原生窗体模式，不弹控制台
    // 带参数 → 命令行模式，自动挂一个控制台看日志
    if std::env::var_os("MOJINGLING_VERBOSE")
        .map(|v| v != "0" && !v.is_empty())
        .unwrap_or(false)
    {
        logger::set_verbose(true);
    }
    if std::env::args_os().len() <= 1 {
        gui::run();
        return;
    }
    ensure_console();
    cli_main();
}

/// 命令行模式下挂控制台：优先附加到调用方控制台（cmd / PowerShell 里直接看输出），
/// 失败则新开一个控制台窗口。GUI 子系统程序也就能照常打印日志。
fn ensure_console() {
    const ATTACH_PARENT_PROCESS: u32 = 0xFFFF_FFFF;
    const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF5; // (DWORD)-11
    extern "system" {
        fn GetStdHandle(which: u32) -> *mut std::ffi::c_void;
        fn AttachConsole(process_id: u32) -> i32;
        fn AllocConsole() -> i32;
    }
    unsafe {
        // 已有可用 stdout（重定向到文件/管道，或继承自父控制台）就不要再动，
        // 否则 AllocConsole 会截走输出，导致 `> log.txt` 拿到空文件。
        let h = GetStdHandle(STD_OUTPUT_HANDLE);
        if !h.is_null() && h as isize != -1 {
            return;
        }
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            AllocConsole();
        }
    }
}

/// 采样「金币红了没有」：连续抓 N 帧打印 ROI 红像素占比，用来定 --money-red-pct 阈值。
/// # Safety
/// 涉及 gdi32 截图。
unsafe fn probe_money(cfg: &Config, n: u32) {
    let Some(roi) = cfg.money_roi else {
        logln!("[probe-money] 未配置 money_roi，无法采样");
        return;
    };
    logln!(
        "[probe-money] 采样 {n} 次，阈值 {:.2}%（红占比≥阈值=判为没钱）；区域比例 {:?}，间隔 {} ms",
        cfg.money_red_pct,
        roi,
        cfg.money_wait_ms
    );
    let mut broke = 0u32;
    for i in 1..=n {
        if !RUNNING.load(Ordering::SeqCst) {
            break;
        }
        match grab_frame(cfg) {
            Some(f) => {
                let (pct, tot, rect) = roi_red_pct(&f.bgra, f.w, f.h, roi);
                let is_broke = pct >= cfg.money_red_pct;
                if is_broke {
                    broke += 1;
                }
                logln!(
                    "[probe-money] 第{i}/{n}次：红 {pct:.2}%（阈值 {:.2}%）→ {}；像素区域 {}x{} @({},{}), 采样 {tot} 像素",
                    cfg.money_red_pct,
                    if is_broke { "没钱（会等）" } else { "钱够（继续）" },
                    rect.2,
                    rect.3,
                    rect.0,
                    rect.1
                );
            }
            None => logw!("[probe-money] 第{i}/{n}次：抓图失败"),
        }
        sleep_interruptible(Duration::from_millis(cfg.money_wait_ms.max(50)));
    }
    logln!("[probe-money] 采样结束：{broke}/{n} 次判为没钱");
}

/// --size-test：把目标窗体真的改到 --size 指定的客户区尺寸，打印改前/改后尺寸后退出。
///
/// 换机排查用：一跑就知道那台机器能不能把游戏窗体掰成统一尺寸，
/// 掰不动时会直接算出该用多少 --scales 来救。
unsafe fn run_size_test(cfg: &Config) {
    let (tw, th) = match cfg.size_lock_client {
        Some(v) => (v.0 as i32, v.1 as i32),
        // 没给 --size 时退回 size.cfg 里配的尺寸，方便远程一键验证
        None => match crate::sizelock::force_client() {
            Some(v) => v,
            None => {
                logln!("[size-test] 需要目标尺寸：加 --size WxH（如 --size 1040x629），或在程序目录放 size.cfg");
                return;
            }
        },
    };
    let target = match crate::capture::find_game_window() {
        Some(g) => Some(g),
        None => crate::capture::find_windows(&cfg.window_title)
            .into_iter()
            .next(),
    };
    let w = match target {
        Some(v) => v,
        None => {
            logln!("[size-test] 没找到目标窗口：可用 --window <标题/类名/进程名> 指定");
            return;
        }
    };
    let h = w.hwnd;
    logln!(
        "[size-test] 目标 HWND=0x{:X} 标题=\"{}\" 类=\"{}\" 进程={}",
        w.hwnd as usize,
        w.title,
        w.class,
        if w.exe.is_empty() { "?" } else { w.exe.as_str() }
    );
    logln!(
        "[size-test] 改前：客户区 {:?}  外框 {:?}",
        crate::sizelock::current_client(h),
        crate::sizelock::current_outer(h)
    );
    let set = crate::sizelock::force_client_now(h, tw, th);
    let after = crate::sizelock::current_client(h);
    logln!(
        "[size-test] 改后：客户区 {:?}  外框 {:?}  （程序设定的外框 {:?}）",
        after,
        crate::sizelock::current_outer(h),
        set
    );
    match after {
        Some((cw, ch)) if cw == tw && ch == th => logln!(
            "[size-test] 结论：客户区精确命中 {tw}x{th}，模板按 1.0 尺度即可对齐"
        ),
        Some((cw, ch)) => {
            let s = cw as f32 / tw as f32;
            logln!(
                "[size-test] 结论：客户区只到 {}x{}（目标 {tw}x{th}），该窗体自己限制了尺寸；模板改用 --scales {:.3}",
                cw,
                ch,
                s
            );
        }
        None => logln!("[size-test] 结论：读不到客户区（窗口已关闭或最小化）"),
    }
}

fn cli_main() {
    unsafe {
        SetConsoleCtrlHandler(Some(ctrl_handler), 1);
    }
    RUNNING.store(true, Ordering::SeqCst);
    logvln!(
        "[env] 控制台 ANSI 上色：{}",
        if logger::vt_supported() {
            "可用（异常/警告会显示红色/橙色）"
        } else {
            "不可用（异常/警告只显示纯文本）"
        }
    );

    // --find <关键字>：只查找并显示窗口句柄，找到就退出（不启动点击）
    let raw: Vec<String> = std::env::args().skip(1).collect();
    // --list：列出当前所有可见顶层窗口（含被过滤的噪声窗口标记），用于确认游戏窗体真实标题
    if raw.iter().any(|a| a == "--list") {
        unsafe { report_list(); }
        unsafe {
            SetConsoleCtrlHandler(None, 1);
        }
        return;
    }
    if let Some(pos) = raw.iter().position(|a| a == "--find") {
        let key = raw.get(pos + 1).cloned().unwrap_or_default();
        report_find(&key);
        unsafe {
            SetConsoleCtrlHandler(None, 1);
        }
        return;
    }

    let cfg = parse_args();
    // 后台模式不搬动、不激活目标窗口
    AUTO_MOVE.store(!cfg.bg_click, Ordering::SeqCst);

    if cfg.self_test {
        let (_pass, fail) = self_test();
        if fail > 0 {
            std::process::exit(2);
        }
        return;
    }

    // --size-test：真的把目标窗体改到 --size 尺寸，打印前后尺寸后退出
    if cfg.size_test {
        unsafe { run_size_test(&cfg); }
        unsafe {
            SetConsoleCtrlHandler(None, 1);
        }
        return;
    }

    // --probe：两条后台抓图通路各抓一帧，打印画面特征，判断本机游戏窗口吃哪条通路
    if cfg.probe {
        unsafe { run_probe(&cfg.window_title); }
        unsafe {
            SetConsoleCtrlHandler(None, 1);
        }
        return;
    }

    // 加载内嵌模板库（编译期已打包进 exe）
    let tpls = matcher::load_library(assets::TEMPLATES_BIN);
    if tpls.is_empty() {
        loge!("[error] 内嵌模板库为空或已损坏");
        std::process::exit(1);
    }
    let t0 = Instant::now();
    let levels = matcher::build_levels_ex(&tpls, &cfg.scales, &cfg.skip_tpl);
    let build_ms = t0.elapsed().as_secs_f64() * 1000.0;

    logsel!(
        ("[init] 单文件模式：模板 {} 张（内嵌 {:.1} KB），尺度 {:?}，金字塔 {} 组（{:.0} ms）",
            tpls.len(),
            assets::TEMPLATES_BIN.len() as f64 / 1024.0,
            cfg.scales,
            levels.len(),
            build_ms)
        ("[init] 模板 {} 张 · 阈值 {:.2} · 节拍 {:.1} 秒 · 简洁输出（想看细节请加 -g 启动）",
            tpls.len(),
            cfg.threshold,
            cycle_ms(&cfg) as f64 / 1000.0)
    );
    logvln!("[init] 阈值 {:.2}，粗筛步长 {}，精修候选 {}", cfg.threshold, cfg.step, cfg.topk);
    if let Some((x, y, w, h)) = cfg.roi {
        logvln!("[init] 搜索区域 ROI = ({x},{y}) {w}x{h}");
    }
    if cfg.cells.is_empty() {
        logsel!(
            ("[init] 查找模式：整窗金字塔搜索（三格模式已关闭）")
            ("[init] 查找模式：整窗搜索（三格模式已关闭）")
        );
    } else {
        let list: Vec<String> = cfg
            .cells
            .iter()
            .enumerate()
            .map(|(i, c)| format!("第{}格({:.4},{:.4},{:.4},{:.4})", i + 1, c.0, c.1, c.2, c.3))
            .collect();
        logsel!(
            ("[init] 查找模式：三格手牌（相对比例）{}", list.join(" "))
            ("[init] 查找模式：三格手牌（{} 格）", cfg.cells.len())
        );
        if let Some((fx, fy)) = cfg.refresh {
            logvln!("[init] 刷新按钮相对位置 ({fx:.4},{fy:.4})");
        } else {
            logln!("[init] 未配置刷新按钮：三格都没有时不刷新");
        }
        if cfg.offline_bmp.is_some() {
            logln!("[init] 离线帧模式：{}（不截屏）", if cfg.dry_run { "dry-run 空跑" } else { "真实点击" });
        }
    }

    logsel!(
        ("[init] 后台模式：抓图={}（不激活窗口），点击={}{}",
            cfg.capture_mode.label(),
            if cfg.bg_click { "PostMessage 消息式（鼠标不动、不抢前台）" } else { "物理鼠标（SetCursorPos+mouse_event）" },
            if cfg.bring_to_front { "，点击前置前窗口" } else { "" })
        ("[init] {}（抓图 {} / 点击 {}）",
            if cfg.bg_click { "后台运行：不激活窗口、鼠标不动" } else { "前台运行：会移动鼠标" },
            cfg.capture_mode.label(),
            if cfg.bg_click { "消息式" } else { "物理点击" })
    );
    if cfg.money_check {
        if let Some((fx, fy, fw, fh)) = cfg.money_roi {
            logsel!(
                ("[init] 没钱等待：开（红占比 ≥{:.2}% 判为没钱；区域 {fx:.4},{fy:.4},{fw:.4},{fh:.4}；复查间隔 {} ms）",
                    cfg.money_red_pct, cfg.money_wait_ms)
                ("[init] 没钱等待：开（红占比 ≥{:.2}% 且连续复检 {} 次都超阈值才判没钱，每 {} ms 复查）",
                    cfg.money_red_pct, MONEY_CONFIRM_ROUNDS, cfg.money_wait_ms)
            );
        } else {
            logln!("[init] 没钱等待：开但未配置 money_roi → 实际不生效");
        }
    } else {
        logln!("[init] 没钱等待：关（--no-money-check）");
    }
    logln!(
        "[init] 刷新最小间隔：{} ms（--refresh-gap-ms 调；太小会把金币刷光）",
        cfg.refresh_gap_ms
    );

    // --probe-money：采样金币区域红占比，用来确认「钱红了」的阈值是否合适
    if cfg.probe_money > 0 {
        unsafe { probe_money(&cfg, cfg.probe_money); }
        unsafe {
            SetConsoleCtrlHandler(None, 1);
        }
        return;
    }

    // --scan-refresh：换机标定刷新按钮坐标，扫完退出
    if cfg.scan_refresh {
        unsafe { scan_refresh(&cfg); }
        unsafe {
            SetConsoleCtrlHandler(None, 1);
        }
        return;
    }

    // --probe-refresh：只测刷新重绘耗时，不进入点击循环
    if cfg.probe_refresh > 0 {
        unsafe { probe_refresh(&cfg, &levels, cfg.probe_refresh); }
        unsafe {
            SetConsoleCtrlHandler(None, 1);
        }
        return;
    }

    if cfg.interval_secs == 0 {
        logln!("--interval 必须大于 0");
        std::process::exit(1);
    }
    if cfg.coords.is_none() && cfg.dll_path.is_none() && cfg.window_title.is_empty() {
        logw!("[warn] 未指定 --window，将使用当前前台窗口");
    }

    // DLL 识别模式（可选，保留兼容）
    let engine = cfg.dll_path.as_deref().and_then(|p| match unsafe { Engine::load(p) } {
        Ok(e) => {
            logln!("[init] 已加载: {}", p.display());
            for (name, ok) in e.symbol_report() {
                logln!("        {}: {}", name, if ok { "OK" } else { "缺失" });
            }
            Some(e)
        }
        Err(msg) => {
            loge!("[error] {msg}");
            None
        }
    });
    if let (Some(eng), Some(p)) = (&engine, &cfg.dll_path) {
        unsafe {
            let model = cfg.model_dir.clone().map(|m| m.to_string_lossy().into_owned());
            if let (Some(f), Some(m)) = (eng.init_dectct, model.as_deref()) {
                let cstr = std::ffi::CString::new(m).unwrap();
                let r = f(cstr.as_ptr() as *const u8);
                logln!("[init] initOVDectct({m}) -> {r}");
            } else if let Some(f) = eng.myov_init {
                let m = model.unwrap_or_else(|| {
                    let d = p.parent().unwrap_or(std::path::Path::new("."));
                    d.join("model.xml").to_string_lossy().into_owned()
                });
                let cstr = std::ffi::CString::new(m.clone()).unwrap();
                let r = f(cstr.as_ptr() as *const u8);
                logln!("[init] MYOV_InitModel({m}) -> {r}");
            }
        }
    }

    let total = if cfg.times == 0 { "无限".to_string() } else { cfg.times.to_string() };
    logln!(
        "[init] 节拍 {} ms（{}）点一次魔精灵{}，共 {total} 次（Ctrl+C 退出）",
        cycle_ms(&cfg),
        if cycle_ms(&cfg) < 1000 {
            format!("严格 < {:.2} 秒", cycle_ms(&cfg) as f64 / 1000.0)
        } else {
            format!("{:.2} 秒", cycle_ms(&cfg) as f64 / 1000.0)
        },
        cfg.coords.map(|(x, y)| format!("（固定坐标 {x},{y}）")).unwrap_or_default()
    );

    let (done, hits) = click_loop(&cfg, engine.as_ref(), &levels);
    logln!("[done] 已执行 {done} 轮，命中 {hits} 次，退出。");
}

/// 上一次日志里报告过的目标句柄（句柄变化时才打印，避免刷屏）
static LAST_TARGET: AtomicIsize = AtomicIsize::new(0);

/// 目标句柄发生变化（或首次定位）时，打印一次定位结果
pub fn report_target_once(hwnd: HWND) {
    let cur = hwnd as isize;
    if LAST_TARGET.swap(cur, Ordering::SeqCst) == cur {
        return;
    }
    unsafe {
        match capture::describe(hwnd) {
            Some(w) => {
                let exe = if w.exe.is_empty() { "?" } else { w.exe.as_str() };
                logsel!(
                    ("[target] 已锁定窗口：HWND=0x{:X}（十进制 {}）标题=\"{}\" 进程={exe} 客户区={}x{}",
                        cur as usize, cur, w.title, w.cw, w.ch)
                    ("[窗口] 已锁定「{}」（{}x{}）", w.title, w.cw, w.ch)
                );
            }
            None => logln!("[target] 句柄 0x{cur:X}（十进制 {cur}）已失效"),
        }
        if AUTO_MOVE.load(Ordering::SeqCst) && capture::move_window_to(hwnd, 10, 10) {
            logln!("[move] 已把目标窗口移到屏幕 (10,10)");
        }
    }
}

/// 命令行 --list：列出当前可见顶层窗口，标注哪些会被当作噪声过滤掉
pub unsafe fn report_list() {
    let all = capture::list_top_windows();
    logln!("[list] 当前可见顶层窗口共 {} 个（* = 会被过滤的非游戏窗口）：", all.len());
    for w in all.iter() {
        let noise = capture::is_noise(&w.title, &w.exe);
        let exe = if w.exe.is_empty() { "?" } else { w.exe.as_str() };
        logln!(
            "{} HWND=0x{:X}（十进制 {}）  标题=\"{}\"  类=\"{}\"  进程={exe}(pid {})  客户区={}x{}",
            if noise { "*" } else { " " },
            w.hwnd as usize,
            w.hwnd as isize,
            w.title,
            w.class,
            w.pid,
            w.cw,
            w.ch
        );
    }
}

/// 窗体「查找窗口」按钮 / 命令行 --find：只查找并显示句柄，不启动点击
pub fn report_find(key: &str) {
    let key = key.trim();
    if key.is_empty() {
        logln!("[find] 请先在「窗口标题」输入框里填关键字（标题片段 / 类名 / 进程 exe 名均可）。");
        return;
    }
    unsafe {
        // 先按「进程 + 类名」识别游戏本体（标题是房间号，不靠标题）
        let game = capture::find_game_window();
        let mut hits = capture::find_windows(key);
        if let Some(g) = game {
            if !hits.iter().any(|x| x.hwnd == g.hwnd) {
                hits.insert(0, g);
            }
        }
        if hits.is_empty() {
            logln!("[find] 没有找到含「{key}」的可见游戏窗口（标题、类名、进程名三者任一命中都算）。");
            logln!("[hint] 游戏本体 = 微信小程序运行时窗口：类名 Chrome_WidgetWin_0、进程 WeChatAppEx.exe；");
            logln!("[hint] 辅助程序 TFJL.exe 的窗口（如「锁0724-2.27.2.50」）会被自动排除。");
            return;
        }
        logln!("[find] 关键字「{key}」命中 {} 个窗口（TFJL.exe 辅助窗口已排除）：", hits.len());
        for (i, w) in hits.iter().enumerate() {
            let exe = if w.exe.is_empty() { "?" } else { w.exe.as_str() };
            logln!(
                "  #{i} HWND=0x{:X}（十进制 {}）  标题=\"{}\"  类=\"{}\"  进程={exe}(pid {})  客户区={}x{}",
                w.hwnd as usize, w.hwnd as isize, w.title, w.class, w.pid, w.cw, w.ch
            );
        }
        let first = &hits[0];
        logln!(
            "[find] 将把 #0 作为点击目标：HWND=0x{:X}（十进制 {}）\"{}\"",
            first.hwnd as usize, first.hwnd as isize, first.title
        );
        LAST_TARGET.store(first.hwnd as isize, Ordering::SeqCst);
    }
}

/// 后台模式下是否仍把目标窗口搬到屏幕 (10,10)；后台模式 = 不打扰，故关闭
pub static AUTO_MOVE: AtomicBool = AtomicBool::new(true);

/// 抓图通路只在「变化时」打一次日志，避免 5 秒节拍把日志刷满
static CAP_VIA_SEEN: AtomicU64 = AtomicU64::new(0);

fn log_capture_once(cap: &capture::Capture) {
    let code = match cap.via {
        "d3d" => 4u64,
        "dc" => 1u64,
        "pw" => 2u64,
        _ => 3u64,
    };
    if CAP_VIA_SEEN.load(Ordering::SeqCst) != code {
        CAP_VIA_SEEN.store(code, Ordering::SeqCst);
        logln!(
            "[cap] 后台抓图通路 = {}（{}），画面 {}",
            cap.via,
            match cap.via {
                "d3d" => "D3D 子窗口 PrintWindow(PW2)，纯后台，窗口可被遮挡/最小化以外都行",
                "dc" => "GetWindowDC+BitBlt，与 TFJL 同款",
                "pw" => "PrintWindow(PW_RENDERFULLCONTENT)",
                _ => "屏幕抓取，要求窗口可见且不被遮挡",
            },
            cap.stat.describe()
        );
    }
}

/// 「每轮都失败」类提示的限流时间戳（毫秒）。
/// 扫描节拍可低到 200ms，同一个失败原因不能被刷成日志洪水，2 秒内只报一条。
static SKIP_LOG_MS: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 记录一次「本轮跳过」：同一原因 2 秒内只打一条，避免刷屏
fn skip_throttled(msg: &str) {
    let now = now_ms();
    if now.saturating_sub(SKIP_LOG_MS.load(Ordering::SeqCst)) >= 2000 {
        SKIP_LOG_MS.store(now, Ordering::SeqCst);
        logw!("[skip] {msg}");
    }
}

/// --probe：把两条后台抓图通路各抓一帧，打印画面特征，判断本机游戏窗口吃哪条
/// # Safety
/// 只读像素，不改动窗口。
unsafe fn run_probe(key: &str) {
    let target = if key.trim().is_empty() {
        capture::find_game_window()
    } else {
        capture::resolve_target(key)
    };
    let Some(w) = target else {
        logln!("[probe] 未找到目标窗口（可先用 --list 看有哪些窗口，用 --window <关键字> 指定）");
        return;
    };
    logln!(
        "[probe] 目标：HWND=0x{:X} \"{}\" 类=\"{}\" 进程={} 客户区={}x{}",
        w.hwnd as usize,
        w.title,
        w.class,
        w.exe,
        w.cw,
        w.ch
    );
    let rows = capture::probe_capture(w.hwnd);
    if rows.is_empty() {
        logln!("[probe] 客户区尺寸无效，抓图无法进行");
        return;
    }
    for (name, stat) in rows {
        match stat {
            Some(st) => logln!("[probe] {name:<24} {}", st.describe()),
            None => logw!("[probe] {name:<24} 抓取失败（API 返回 0）"),
        }
    }
    logln!("[probe] 结论：哪条通路有画面就走哪条。微信小程序（WeChatAppEx）应当只有 d3d 有画面 → 用 --capture d3d（纯后台，不要求窗口置前、不被遮挡）；d3d/dc/pw 全空、只有 scr 有画面 → 用 --capture scr（窗口须可见不被遮挡）。auto 会按 d3d→dc→pw→scr 自动选。");
}

// ======================= 抓画面 =======================

/// 一帧游戏客户区画面（离线帧模式下 target 为空指针、origin 为 (0,0)）
struct Frame {
    w: usize,
    h: usize,
    bgra: Vec<u8>,
    /// 客户区左上角在屏幕上的坐标
    origin: (i32, i32),
    /// 窗口句柄（离线帧模式下为空）
    target: HWND,
}

/// 抓一帧：定位窗口 → 截屏。空帧告警与抓图通路日志只打一次。
/// --image 指定离线帧时直接读 BMP 当画面，不找窗口、不截屏（没开游戏也能验证节奏）。
/// # Safety
/// 涉及 gdi32 截图。
unsafe fn grab_frame(cfg: &Config) -> Option<Frame> {
    if let Some(p) = &cfg.offline_bmp {
        let Some((w, h, bgra)) = capture::read_bmp_bgra(p) else {
            logw!("[skip] 离线帧读取失败（需 24/32 位 BMP）: {}", p.display());
            return None;
        };
        logln!(
            "[frame] 离线帧 {} {}x{}（当作客户区画面，原点(0,0)）",
            p.file_name().map(|v| v.to_string_lossy().into_owned()).unwrap_or_default(),
            w,
            h
        );
        return Some(Frame { w, h, bgra, origin: (0, 0), target: std::ptr::null_mut() });
    }

    // 每轮重查窗口，兼容游戏重启。
    // 关键：必须与启动锁定走同一条解析路径（resolve_target：进程+类名优先、关键字兜底），
    // 否则会出现「启动能锁上、一开跑就一直 [skip]」的不一致 —— 游戏窗口标题是房间号，
    // 根本不含游戏名这几个字，纯关键字匹配永远命中不了。
    let target: HWND = if cfg.window_title.is_empty() {
        GetForegroundWindow()
    } else {
        match capture::resolve_target(&cfg.window_title) {
            Some(w) => {
                // 最小化时微信小游戏会把客户区缩成 144x20 这类极小尺寸、窗口移到 (-32000,-32000)，
                // dc / pw 两条通路都只能拿到纯黑。这是硬限制，直接讲清楚，别让用户以为是识别坏了。
                if capture::is_minimized(w.hwnd) {
                    skip_throttled(&format!(
                        "游戏窗口「{}」处于最小化状态，抓不到画面（最小化时 dc/pw 都是黑图）—— 请先把游戏窗口还原再开始",
                        w.title
                    ));
                    return None;
                }
                // 微信在没真正打开小游戏时会挂一个 144x20 的隐形壳窗口，
                // 拿它抓图永远是空的，不如直接当作「游戏没开」跳过。
                if w.cw < 320 || w.ch < 320 {
                    skip_throttled(&format!(
                        "游戏窗口客户区只有 {}x{}，像是小游戏没真正打开（标题「{}」，进程 {}）",
                        w.cw, w.ch, w.title, w.exe
                    ));
                    return None;
                }
                report_target_once(w.hwnd);
                w.hwnd
            }
            None => {
                skip_throttled(&format!(
                    "未找到游戏窗口（关键字「{}」；优先按 进程 WeChatAppEx.exe + 类名 Chrome_WidgetWin_0 识别）",
                    cfg.window_title
                ));
                return None;
            }
        }
    };
    if target.is_null() {
        logw!("[skip] 无可用窗口");
        return None;
    }

    // 屏幕抓取通路必须让窗口处于最上层：被别的窗口压住时抓到的是遮挡窗口的像素
    if cfg.capture_mode.needs_visible() && !cfg.dry_run {
        crate::winapi::SetForegroundWindow(target);
    }
    let Some(mut cap) = capture::capture_with(target, cfg.capture_mode) else {
        logw!("[skip] 抓图失败（capture={}）", cfg.capture_mode.label());
        return None;
    };
    // Auto 退到屏幕抓取时，窗口必须真的在最前：被别的窗口压住时 scr 抓到的是遮挡窗口的像素。
    // 只在确实不在前台时抬一次，避免每轮都抢焦点。
    if cap.via == "scr" && cfg.capture_mode == capture::CaptureMode::Auto && !cfg.dry_run {
        if GetForegroundWindow() != target {
            crate::winapi::SetForegroundWindow(target);
            std::thread::sleep(std::time::Duration::from_millis(40));
            if let Some(c2) = capture::capture_with(target, capture::CaptureMode::Screen) {
                cap = c2;
            }
        }
    }
    log_capture_once(&cap);
    if cap.stat.is_blank() {
        // 每 440 ms 刷一条同样的告警只会淹掉有用日志，这里跟 skip 共用 2 秒去抖
        let geo = capture::client_geometry(target)
            .map(|(x, y, w, h, sw, sh)| format!("客户区 {w}x{h} @屏幕({x},{y})，虚拟屏幕 {sw}x{sh}"))
            .unwrap_or_else(|| "客户区几何未知".to_string());
        let vis = if capture::is_minimized(target) {
            "窗口已最小化"
        } else if capture::client_on_screen(target) {
            "窗口在屏幕内（若被其他窗口压住，屏幕抓取也会拿到遮挡画面）"
        } else {
            "窗口不在屏幕可见区域或不可见"
        };
        skip_throttled(&format!(
            "抓到空帧（via={} {}）：{geo}，{vis}；auto 会按 d3d→dc→pw→scr 逐条降级，全空请运行 --probe 查看各通路画面",
            cap.via,
            cap.stat.describe()
        ));
    }
    let (cw, ch) = (cap.width.max(1) as usize, cap.height.max(1) as usize);
    let (ox, oy, _, _) = capture::client_rect(target).unwrap_or((0, 0, 0, 0));
    Some(Frame { w: cw, h: ch, bgra: cap.bgra, origin: (ox, oy), target })
}

// ======================= 只找不点 =======================

/// 一次扫描的结果：命中只报告坐标，点不点、什么时候点，全交给 click_loop 决定。
enum Scan {
    /// idx 为格号（1-based；整窗模式与 DLL 模式为 0），cx/cy 为客户区坐标
    Hit { idx: usize, cx: i32, cy: i32, score: f64, frame: Frame },
    /// 这一手牌里没有魔精灵；score 为三格里的最高相似度（用于判断是否已接近阈值）
    Miss { score: f64, frame: Frame },
    /// 窗口/抓图异常，本轮无法判断
    Abort,
}

/// 连续「三格都没找到」的轮数，用于给 miss 日志降噪（非 verbose 下每 20 次打一行）
static MISS_CNT: AtomicU64 = AtomicU64::new(0);

/// 三格手牌模式：只在正下方三个卡牌框里找魔精灵。
/// 返回（格号 1-based、客户区点击坐标、相似度）；本函数不点击、不刷新。
fn match_cells(
    cfg: &Config,
    levels: &[matcher::TplScale],
    f: &Frame,
) -> (Option<(usize, i32, i32, f64)>, f64) {
    let (w, h) = (f.w, f.h);
    let bgra = f.bgra.as_slice();
    // 排查用：把这一帧存成彩色 BMP。采样模式不在这里存（不是灰区帧，会灌爆磁盘），
    // 改由 click_loop 命中灰区时按「分数+格号」命名存盘。
    if cfg.dump && !cfg.sample_only && dump_should_save() {
        let dir = dump_dir();
        let name = format!("cells_{}.bmp", now_stamp());
        let _ = std::fs::write(dir.join(&name), matcher::bgra_to_bmp32(bgra, w, h));
        logln!("[dump] {} {}x{}", dir.join(&name).display(), w, h);
    }
    let mut best: Option<(usize, Match, (usize, usize))> = None;
    // 每一格各自的最佳（模板编号, 分数），用于「三格齐平」判定
    let mut per_cell: Vec<Option<(usize, f64)>> = Vec::with_capacity(cfg.cells.len());
    for (idx, &(fx, fy, fw, fh)) in cfg.cells.iter().enumerate() {
        per_cell.push(None);
        let rx = ((fx * w as f32).round() as i64).clamp(0, w as i64) as usize;
        let ry = ((fy * h as f32).round() as i64).clamp(0, h as i64) as usize;
        let rw = ((fw * w as f32).round() as i64).max(1) as usize;
        let rh = ((fh * h as f32).round() as i64).max(1) as usize;
        let rw = rw.min(w.saturating_sub(rx));
        let rh = rh.min(h.saturating_sub(ry));
        if rw < 16 || rh < 16 {
            logln!("[cell{}] 区域太小或越界，跳过（{rw}x{rh}）", idx + 1);
            continue;
        }
        // 裁出这一格
        let mut crop = vec![0u8; rw * rh * 4];
        for y in 0..rh {
            let src = ((ry + y) * w + rx) * 4;
            let dst = y * rw * 4;
            crop[dst..dst + rw * 4].copy_from_slice(&bgra[src..src + rw * 4]);
        }
        let t0 = Instant::now();
        let pyr = Pyramid::from_bgra(rw, rh, &crop);
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        match matcher::find_best(&pyr, levels, cfg.step, cfg.topk, cfg.threads) {
            Some(m) => {
                logvln!(
                    "[cell{}] 框({rx},{ry}) {rw}x{rh} 最佳 tpl#{:02} 尺度{:.2} 相似度{:.3}{} [{ms:.0} ms]",
                    idx + 1,
                    m.tpl,
                    m.scale,
                    m.score,
                    if cfg.hit(m.score) { " 命中" } else { "" }
                );
                if let Some(slot) = per_cell.last_mut() {
                    *slot = Some((m.tpl, m.score));
                }
                if best.as_ref().map(|b| m.score > b.1.score).unwrap_or(true) {
                    best = Some((idx, m, (rx, ry)));
                }
            }
            None => {
                // 单格无候选：细节只在 -g 下打印，简化模式靠 [换牌] 一行状态即可
                logvln!("[cell{}] 框({rx},{ry}) {rw}x{rh} 无候选 [{ms:.0} ms]", idx + 1)
            }
        }
    }

    let top = best.as_ref().map(|b| b.1.score).unwrap_or(0.0);

    // 「三格齐平」防线：三格各自的最佳都是同一张模板、且分数几乎一样 → 匹配到的是三格的
    // 共同底纹（空牌槽 / 换牌过渡帧的半透明混合画面），不是真的有魔精灵。
    // 实测：空槽/过渡帧上三格齐刷刷给同一张（多为 tpl#11，也有 #12、#07），
    //      典型 0.81/0.88/0.84（极差 0.07），最散的一帧 0.81/0.88/0.76（极差 0.12）、
    //      0.93/0.85/0.76（极差 0.17），故 bg_spread 默认取 0.25 把这些都盖住；
    //      真牌帧是「一高一低」——命中格 0.83~0.95，另外两格 0.38~0.61。
    // 真抽到两张同款皮肤时两张分数都会很高，故用 bg_max 兜住，不误杀。
    // 【2026-11-09 修正】assume_one 模式下整套底纹判据退役。
    //
    // 原因：这里的两个判据（same_flat / tight）一旦成立就 `return (None, sc[0])`，
    // 把下面 loose_hit 与 assume_one「取三格最高分那格直接点」两条路整条短路掉。
    // 而用户澄清的规则是「三格手牌里必有一张魔精灵」，牌槽为空只发生在
    // 「没钱 → 价格变红 → 点不出来」这一种情况，那种情况由 ensure_money /
    // confirm_money_red 的红色像素检测负责，与匹配分数、与三格是否齐平毫无关系。
    //
    // 于是底纹判据在这里的语义变成「用『三格分数挤在一起』来否定『必有一张』」，
    // 直接违反规则前提：真牌 0.82 被动画压低、另两格空槽被 tpl#07/#29 承接到 0.70/0.72 时，
    // gap=0.10 < bg_gap(0.20) 且 second=0.72 ≥ lo_score-0.10=0.68 → 判成底纹 → 去刷新，
    // 这就是「明明有卡却刷掉」的最大来源。
    //
    // 因此：默认（assume_one 开）下完全不走这段；只有显式 --no-assume-one 时才保留，
    // 供「三格不一定有牌」的其他玩法复用。
    if cfg.bg_guard && !cfg.assume_one && per_cell.len() >= 2 {
        let all: Vec<(usize, f64)> = per_cell.iter().filter_map(|x| *x).collect();
        if all.len() == per_cell.len() {
            let detail = all
                .iter()
                .map(|x| format!("{:.3}", x.1))
                .collect::<Vec<_>>()
                .join("/");

            // 判据一（同模板齐平）：三格最佳都是同一张模板且分数几乎一样 → 共同底纹。
            // 但「极差小」必须建立在「三格都像牌」的前提上：真牌那格 0.82、另两格空槽被同一张
            // 模板承接成 0.60/0.62 时极差只有 0.22 < bg_spread(0.25)，只看极差会把真牌判成底纹
            // ——这正是「明明有一张卡却视而不见去刷新」的一大来源。故补最低分前提。
            let mx = all.iter().map(|x| x.1).fold(f64::MIN, f64::max);
            let mn = all.iter().map(|x| x.1).fold(f64::MAX, f64::min);
            let same_flat = all.iter().all(|x| x.0 == all[0].0)
                && mx - mn < cfg.bg_spread
                && mx < cfg.bg_max
                && mn >= bg_flat_floor(cfg.lo_score());

            // 判据二（拉不开）：真牌永远是「一格很高、另外两格很低」，最高分与次高分之间
            // 有 0.29~0.53 的落差；底纹帧三格分数挤在一起，落差只有 0.005~0.10。
            // 这条不要求三格命中同一张模板 —— 剔除 tpl#11/#12 后，底纹会改由 tpl#07/#08/#29/#38
            // 混合承接（例如 c1=#38 0.749 / c2=#29 0.808 / c3=#38 0.803），只靠判据一会漏判。
            let mut sc: Vec<f64> = all.iter().map(|x| x.1).collect();
            sc.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
            let gap = sc[0] - sc[1];
            // 判据二的前提：三格「都像牌」才谈得上分不开。次高分低于 命中线-BG_SECOND_MARGIN
            // 时，另两格其实是空槽/底纹（真牌帧次高分 0.38~0.61），不能用「分不开」把它们一起判掉，
            // 否则真牌分数被动画压低一点点就会被误杀成底纹（漏打的直接来源）。
            let second_is_card = sc[1] >= cfg.lo_score() - BG_SECOND_MARGIN;
            let tight = sc[0] <= cfg.bg_max && gap < cfg.bg_gap && second_is_card;

            if same_flat || tight {
                let why = if same_flat {
                    format!("三格齐中 tpl#{:02}，极差 {:.3}", all[0].0, mx - mn)
                } else {
                    format!("最高/次高只差 {:.3}，分不开", gap)
                };
                logvln!(
                    "[bg] {}（{}）→ 判为共同底纹，不算命中",
                    why,
                    detail
                );
                return (None, sc[0]);
            }
        }
    }

    let Some((idx, m, (rx, ry))) = best else {
        logvln!("[miss] 三格内均无候选");
        return (None, 0.0);
    };
    let cx = rx as i32 + m.x as i32 + m.w as i32 / 2;
    let cy = ry as i32 + m.y as i32 + m.h as i32 / 2;
    if !cfg.hit(m.score) {
        // 次高分：判断「一格独高」还是「三格差不多」。
        let mut sc: Vec<f64> = per_cell.iter().filter_map(|x| *x).map(|x| x.1).collect();
        sc.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        let second = if sc.len() >= 2 { sc[1] } else { 0.0 };
        let gap = m.score - second;

        // 低分兜底：分数只差命中线一点点，且「一格独高」→ 这就是一张被动画/缩放压低的真牌，
        // 直接点出去，别去刷新换牌（刷新要花金币，还可能把真牌刷掉）。
        if cfg.loose_hit
            && m.score < cfg.lo_score()
            && m.score >= cfg.lo_score() - LOOSE_DROP
            && gap >= cfg.bg_gap
        {
            logln!(
                "[loose] 第{}格 {:.3} 只差命中线 {:.2} {:.3}（另格最高 {:.3}，落差 {:.3}）→ 判定为真牌被压低，照样点击，不刷新换牌",
                idx + 1,
                m.score,
                cfg.lo_score(),
                cfg.lo_score() - m.score,
                second,
                gap
            );
            MISS_CNT.store(0, Ordering::SeqCst);
            return (Some((idx + 1, cx, cy, m.score)), top);
        }

        // 「三格必有一格」：游戏规则是刷新后三格手牌里必有一张魔精灵。所以分数没过命中线
        // 不能理解成「这轮没牌」，只能理解成「这张牌画得不像模板」（皮肤/动画/缩放/光照差异）。
        // 既然必有一格，那就取三格最高分那格直接点，绝不因为分数不够去刷新换牌。
        // 只留一条底线 assume_floor：三格全是空槽/坐标偏了/画面完全对不上时（实测 0.25~0.38）
        // 不点，免得对着空白乱点乱刷金币。
        if cfg.assume_one && m.score >= cfg.assume_floor {
            logln!(
                "[assume] 三格必有一格：取第{}格最高分 {:.3}（命中线 {:.2}，另格最高 {:.3}，落差 {:.3}）→ 直接点击，不刷新换牌",
                idx + 1,
                m.score,
                cfg.lo_score(),
                second,
                gap
            );
            MISS_CNT.store(0, Ordering::SeqCst);
            return (Some((idx + 1, cx, cy, m.score)), top);
        }

        // 真·没找到：非 verbose 下也每 20 次打一行，避免「只看到不断刷新、不知道为什么」。
        let n = MISS_CNT.fetch_add(1, Ordering::SeqCst);
        if cfg.verbose || n % 20 == 0 {
            let hint = if cfg.assume_one {
                "，但规则上三格必有一张 → 这不是「没牌」，而是三格区域整体对不上（窗口被遮挡 / 尺寸变了 / 坐标偏了），建议用 --dump 抓帧复核 --cells；若钱已变红则是没钱点了"
            } else if m.score < cfg.lo_score() - 0.20 {
                "，分数差得多：三格区域可能偏了（用 --dump 抓帧复核 --cells），或这张皮肤不在模板库里"
            } else {
                ""
            };
            logln!(
                "[miss] 三格都没找到魔精灵（最高 第{}格 {:.3} 不在区间 {}，另格最高 {:.3}，落差 {:.3}{}）",
                idx + 1,
                m.score,
                cfg.range_txt(),
                second,
                gap,
                hint
            );
        }
        return (None, top);
    }
    MISS_CNT.store(0, Ordering::SeqCst);
    (Some((idx + 1, cx, cy, m.score)), top)
}

/// 整窗匹配模式（--no-cells 或未配置三格时）：整块客户区做金字塔搜索，只返回点击点。
fn match_frame_full(cfg: &Config, levels: &[matcher::TplScale], f: &Frame) -> Option<(i32, i32, f64)> {
    let (w, h) = (f.w, f.h);
    let frame = f.bgra.as_slice();
    // ROI 裁切（可选，降低搜索量）
    let (cx, cy, cw, ch, bgra) = match cfg.roi {
        Some((rx, ry, rw, rh)) => {
            let rw = rw.min(w.saturating_sub(rx.min(w)));
            let rh = rh.min(h.saturating_sub(ry.min(h)));
            if rw == 0 || rh == 0 {
                logw!("[skip] ROI 超出客户区（客户区 {w}x{h}）");
                return None;
            }
            let mut out = vec![0u8; rw * rh * 4];
            for y in 0..rh {
                let src = ((ry + y) * w + rx) * 4;
                let dst = y * rw * 4;
                out[dst..dst + rw * 4].copy_from_slice(&frame[src..src + rw * 4]);
            }
            (rx as i32, ry as i32, rw, rh, out)
        }
        None => (0, 0, w, h, frame.to_vec()),
    };

    let t0 = Instant::now();
    let pyr = Pyramid::from_bgra(cw, ch, &bgra);
    let found: Option<Match> = matcher::find_best(&pyr, levels, cfg.step, cfg.topk, cfg.threads);
    let match_ms = t0.elapsed().as_secs_f64() * 1000.0;

    // 排查用：存下截图
    if cfg.dump && dump_should_save_shot() {
        let dir = dump_dir();
        let name = format!("shot_{}.bmp", now_stamp());
        let _ = std::fs::write(dir.join(&name), matcher::gray_to_bmp(&pyr.full, cw, ch));
        logln!("[dump] {}", dir.join(&name).display());
    }

    let Some(m) = found else {
        logvln!("[miss] 无候选（客户区 {cw}x{ch}, {match_ms:.0} ms）");
        return None;
    };
    if !cfg.hit(m.score) {
        logvln!(
            "[miss] 不在命中区间 {:.3} ∉ {}（最佳 tpl#{:02} 尺度 {:.2} @({},{}) {match_ms:.0} ms）",
            m.score, cfg.range_txt(), m.tpl, m.scale, m.x, m.y
        );
        return None;
    }
    let ccx = cx + m.x as i32 + m.w as i32 / 2;
    let ccy = cy + m.y as i32 + m.h as i32 / 2;
    logvln!(
        "[full] tpl#{:02} 尺度{:.2} 相似度{:.3} 客户区({},{}) {}x{} [{match_ms:.0} ms]",
        m.tpl,
        m.scale,
        m.score,
        cx + m.x as i32,
        cy + m.y as i32,
        m.w,
        m.h
    );
    Some((ccx, ccy, m.score))
}

/// 「只看不点」地扫描一次当前画面。
/// # Safety
/// 涉及截屏。
unsafe fn scan_once(cfg: &Config, engine: Option<&Engine>, levels: &[matcher::TplScale]) -> Scan {
    // 固定坐标模式：不用找目标，永远算命中。
    // 但必须把真实窗口句柄填进 Frame —— 早先这里 target 直接填 null_mut()，
    // 结果后台点击的 PostMessage 全投给空句柄（一条消息都没发出去），
    // 程序却照样报「命中 1 次」，表现就是「日志说点了、画面纹丝不动」。
    if let Some((x, y)) = cfg.coords {
        let target: HWND = if cfg.window_title.is_empty() {
            GetForegroundWindow()
        } else {
            match capture::resolve_target(&cfg.window_title) {
                Some(w) => w.hwnd,
                None => {
                    logw!(
                        "[error] 固定坐标模式：没找到游戏窗口（标题「{}」），后台点击无处可投",
                        cfg.window_title
                    );
                    std::ptr::null_mut()
                }
            }
        };
        let origin = if target.is_null() {
            (0, 0)
        } else {
            match capture::client_rect(target) {
                Some((ox, oy, _, _)) => (ox, oy),
                None => (0, 0),
            }
        };
        let f = Frame { w: 0, h: 0, bgra: Vec::new(), origin, target };
        return Scan::Hit { idx: 0, cx: x, cy: y, score: 1.0, frame: f };
    }

    let Some(f) = grab_frame(cfg) else {
        return Scan::Abort;
    };

    // DLL 识别模式（兼容原有用法）
    if let Some(eng) = engine {
        let dets = eng.predict(&f.bgra, f.w as i32, f.h as i32, 4);
        let best = dets
            .iter()
            .max_by(|a, b| a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal));
        return match best {
            Some(b) if cfg.hit(b.score as f64) => {
                let cx = (b.x + b.w / 2.0) as i32;
                let cy = (b.y + b.h / 2.0) as i32;
                Scan::Hit { idx: 0, cx, cy, score: b.score as f64, frame: f }
            }
            Some(b) => {
                logvln!("[miss] DLL 最高分 {:.3} 不在区间 {}", b.score, cfg.range_txt());
                Scan::Miss { score: b.score as f64, frame: f }
            }
            None => {
                logvln!("[miss] DLL 未识别到目标（图像 {}x{}）", f.w, f.h);
                Scan::Miss { score: 0.0, frame: f }
            }
        };
    }

    // 内置模板匹配（三格手牌 / 整窗）
    let (found, top) = if cfg.cells.is_empty() {
        match match_frame_full(cfg, levels, &f) {
            Some((cx, cy, score)) => (Some((0usize, cx, cy, score)), score),
            None => (None, 0.0),
        }
    } else {
        match_cells(cfg, levels, &f)
    };
    match found {
        Some((idx, cx, cy, score)) => Scan::Hit { idx, cx, cy, score, frame: f },
        None => Scan::Miss { score: top, frame: f },
    }
}

// ======================= 点击 =======================

/// 点完「刷新」后最小保护间隔（毫秒）。
/// 实测：牌面 66 ms 才开始变化、298~382 ms 才画完。旧的 100 ms 会让程序停在
/// 「旧牌淡出 + 新牌淡入」的半透明混合帧上——那帧既不是旧牌也不是新牌，
/// 匹配分数会飘到 0.88~0.92 的灰区（tpl#11 恒 0.885、旧牌 tpl#01 还能给到 0.916），
/// 这是灰区误判的主要原因。取 400 ms 盖住「画完」的上界。
const REFRESH_SETTLE_MS: u64 = 400;

/// 「画面已静止」判定：两帧在牌面区域的差异低于这个百分比就算不再变化
const STABLE_DIFF_PCT: f64 = 0.5;

/// 才刷完这么点时间不做「静止」判断——动画还在跑，静止是假的
const STABLE_MIN_MS: u64 = 250;

/// 采样模式（--sample-only）换牌后等多久再抓下一帧。实测牌面 300~367 ms 才画完，
/// 取 900 ms 留足余量；否则会反复采到同一手牌，把样本采成重复值。
const SAMPLE_SETTLE_MS: u64 = 900;

/// 点一次客户区坐标（dry_run 时只打印不点）。
/// # Safety
/// 会移动鼠标或投递消息。
unsafe fn click_point(cfg: &Config, f: &Frame, cx: i32, cy: i32, tag: &str) {
    let px = f.origin.0 + cx;
    let py = f.origin.1 + cy;
    let label = if tag.is_empty() { String::new() } else { format!("[{tag}] ") };
    logvln!(
        "[click] {label}(+{}ms) 客户区({cx},{cy}) -> {}{}",
        elapsed_ms(),
        if cfg.bg_click { format!("后台消息({cx},{cy})") } else { format!("屏幕({px},{py})") },
        if cfg.dry_run { " [dry-run 未点击]" } else { "" }
    );
    if !cfg.dry_run {
        click::click_bg_or_fg(f.target, cx, cy, cfg.bg_click, cfg.bring_to_front);
    }
}

/// 点一次三格右侧的「刷新」按钮。返回是否配置了刷新按钮。
/// tag 标明这次刷新的触发原因（点中后换牌 / 未命中换牌）。
/// # Safety
/// 会移动鼠标或投递消息。
unsafe fn click_refresh(cfg: &Config, f: &Frame, tag: &str) -> bool {
    let (fx, fy) = match cfg.refresh {
        Some(v) => v,
        None => return false,
    };
    if f.w == 0 || f.h == 0 {
        return false;
    }
    let cx = (fx * f.w as f32).round() as i32;
    let cy = (fy * f.h as f32).round() as i32;
    let px = f.origin.0 + cx;
    let py = f.origin.1 + cy;
    let label = if tag.is_empty() { String::new() } else { format!("-{tag}") };
    logvln!(
        "[refresh{}] (+{}ms) 点刷新 客户区({cx},{cy}) -> {}{}",
        label,
        elapsed_ms(),
        if cfg.bg_click { format!("后台消息({cx},{cy})") } else { format!("屏幕({px},{py})") },
        if cfg.dry_run { " [dry-run 未点击]" } else { "" }
    );
    if !cfg.dry_run {
        click::click_bg_or_fg(f.target, cx, cy, cfg.bg_click, cfg.bring_to_front);
    }
    true
}

// ======================= 没钱等待 =======================

/// ROI 内「红色像素」占比（%），返回 (占比, 总像素, 实际像素矩形)。
/// 红判据：R≥140 且 R-G≥60 且 R-B≥60（BGRA 顺序：b,g,r）。
fn roi_red_pct(
    bgra: &[u8],
    w: usize,
    h: usize,
    roi: (f32, f32, f32, f32),
) -> (f64, usize, (usize, usize, usize, usize)) {
    if w == 0 || h == 0 || bgra.len() < w * h * 4 {
        return (0.0, 0, (0, 0, 0, 0));
    }
    let (fx, fy, fw, fh) = roi;
    let rx = ((fx * w as f32).round().max(0.0) as usize).min(w.saturating_sub(1));
    let ry = ((fy * h as f32).round().max(0.0) as usize).min(h.saturating_sub(1));
    let rw = ((fw * w as f32).round().max(1.0) as usize).min(w - rx);
    let rh = ((fh * h as f32).round().max(1.0) as usize).min(h - ry);
    let (mut red, mut tot) = (0u64, 0u64);
    for y in ry..(ry + rh) {
        let row = y * w;
        for x in rx..(rx + rw) {
            let i = (row + x) * 4;
            let b = bgra[i] as i32;
            let g = bgra[i + 1] as i32;
            let r = bgra[i + 2] as i32;
            if r >= 140 && r - g >= 60 && r - b >= 60 {
                red += 1;
            }
            tot += 1;
        }
    }
    let pct = if tot == 0 { 0.0 } else { red as f64 * 100.0 / tot as f64 };
    (pct, tot as usize, (rx, ry, rw, rh))
}

/// 没钱判定的连续复检：首帧红占比已超阈值，再连抓 MONEY_CONFIRM_ROUNDS 次。
/// 返回 true = 每轮都超阈值（确认真没钱）；false = 中途回落（瞬时误报）或证据不足。
fn confirm_money_red(
    cfg: &Config,
    tag: &str,
    first_pct: f64,
    roi: (f32, f32, f32, f32),
) -> bool {
    let mut red_rounds = 0u32;
    for k in 1..=MONEY_CONFIRM_ROUNDS {
        if !RUNNING.load(Ordering::SeqCst) {
            return false;
        }
        sleep_interruptible(Duration::from_millis(MONEY_CONFIRM_GAP_MS));
        if !RUNNING.load(Ordering::SeqCst) {
            return false;
        }
        match unsafe { grab_frame(cfg) } {
            Some(f) => {
                let (pct, _, _) = roi_red_pct(&f.bgra, f.w, f.h, roi);
                logvln!(
                    "[money] 「{tag}」复检 {}/{}：红 {pct:.2}%（阈值 {:.2}%）",
                    k,
                    MONEY_CONFIRM_ROUNDS,
                    cfg.money_red_pct
                );
                if pct < cfg.money_red_pct {
                    logvln!(
                        "[money] 「{tag}」复检第{}次回落到 {pct:.2}%（首帧 {first_pct:.2}% ≥ {:.2}%）→ 判为瞬时误报，继续干活",
                        k,
                        cfg.money_red_pct
                    );
                    return false;
                }
                red_rounds += 1;
            }
            None => {
                logw!("[money] 「{tag}」复检第{k}次抓图失败（不计入变红次数）");
            }
        }
    }
    let ok = red_rounds >= MONEY_CONFIRM_ROUNDS;
    if !ok {
        logvln!(
            "[money] 「{tag}」复检只拿到 {}/{} 次变红（其余抓图失败）→ 证据不足，按钱够处理",
            red_rounds,
            MONEY_CONFIRM_ROUNDS
        );
    }
    ok
}

/// 没钱就原地等：每 cfg.money_wait_ms 抓一帧复查红没了没有。
/// 等待期间不刷新、不点击、不查找。返回 true = 钱够了继续；false = 被停止。
fn wait_money(cfg: &Config, tag: &str) -> bool {
    let Some(roi) = cfg.money_roi else {
        return true;
    };
    let mut round = 0u32;
    let mut ok_rounds = 0u32;
    // 恢复线取「退出阈值」与「进入阈值的一半」中更小的那个：--money-red-pct 调小时恢复线跟着降，
    // 免得恢复线反而高于进入线导致一直等下去。
    let exit_pct = (cfg.money_red_pct * 0.5).min(MONEY_RED_EXIT_PCT);
    loop {
        if !RUNNING.load(Ordering::SeqCst) {
            return false;
        }
        sleep_interruptible(Duration::from_millis(cfg.money_wait_ms.max(50)));
        if !RUNNING.load(Ordering::SeqCst) {
            return false;
        }
        round += 1;
        match unsafe { grab_frame(cfg) } {
            Some(f) => {
                let (pct, _, rect) = roi_red_pct(&f.bgra, f.w, f.h, roi);
                if pct < exit_pct {
                    // 恢复也要连续确认：单帧偏暗（牌面淡出、切场景）不能当作「钱够了」
                    ok_rounds += 1;
                    if ok_rounds >= MONEY_OK_ROUNDS {
                        logsel!(
                            ("[money] (+{}ms) 「{tag}」等钱结束：连续{}次复查红 {pct:.2}% < 恢复线 {:.2}% → 钱够了，继续干活",
                                elapsed_ms(), ok_rounds, exit_pct)
                            ("[有钱] 「{tag}」等钱结束（第{}次复查）→ 立即刷新并查找", round)
                        );
                        return true;
                    }
                    logvln!(
                        "[money] 「{tag}」恢复确认 {}/{}：第{}次复查红 {pct:.2}% < 恢复线 {:.2}%",
                        ok_rounds,
                        MONEY_OK_ROUNDS,
                        round,
                        exit_pct
                    );
                } else {
                    if ok_rounds > 0 {
                        logvln!(
                            "[money] 「{tag}」恢复被打断：第{}次复查又红到 {pct:.2}% ≥ 恢复线 {:.2}% → 重新计数",
                            round,
                            exit_pct
                        );
                    }
                    ok_rounds = 0;
                    logvln!(
                        "[money] 「{tag}」还没钱：第{}次复查红 {pct:.2}% ≥ 恢复线 {:.2}%（区域 {}x{}）→ 再等 {} ms，期间不刷新不点击",
                        round,
                        exit_pct,
                        rect.2,
                        rect.3,
                        cfg.money_wait_ms
                    );
                }
            }
            None => {
                logw!("[money] 「{tag}」第{round}次复查抓图失败 → 继续等");
            }
        }
    }
}

/// 刷新前的「钱够不够」检查：真红（连续复检都红）就等钱；只是瞬时闪红就放行。
/// 返回 true = 可以继续；false = 被停止。
fn ensure_money(cfg: &Config, f: &Frame, tag: &str) -> bool {
    if !cfg.money_check {
        return true;
    }
    let Some(roi) = cfg.money_roi else {
        return true;
    };
    let (pct, _, rect) = roi_red_pct(&f.bgra, f.w, f.h, roi);
    if pct < cfg.money_red_pct {
        // 钱够时这块本该是 0.00%；一旦出现非零（≥ 阈值的 1/4）就记一行，当作「快要变红」的信号留痕，
        // 方便日后回看日志判断阈值定得对不对。
        if pct >= cfg.money_red_pct * 0.25 {
            logvln!(
                "[money] 「{tag}」价格带微红 {pct:.2}% < 阈值 {:.2}%（区域 {}x{}）→ 仍按钱够处理",
                cfg.money_red_pct,
                rect.2,
                rect.3
            );
        }
        return true;
    }
    // 单帧超阈值不算数：先连续复检，全部超阈值才认「真没钱」
    if !confirm_money_red(cfg, tag, pct, roi) {
        if !RUNNING.load(Ordering::SeqCst) {
            return false;
        }
        return true;
    }
    logsel!(
        ("[money] 「{tag}」价格数字变红：红 {pct:.2}% ≥ 阈值 {:.2}%，且复检 {} 次都红（区域 {}x{}）→ 暂停刷新/点击，等钱",
            cfg.money_red_pct, MONEY_CONFIRM_ROUNDS, rect.2, rect.3)
        ("[没钱] 价格变红（{pct:.2}% ≥ {:.2}%，复检确认）→ 暂停刷新，原地等钱", cfg.money_red_pct)
    );
    wait_money(cfg, tag)
}

// ======================= 刷新重绘诊断 =======================

/// 三格区域的并集包围盒（客户区像素坐标）：判断「这一帧牌面变没变」只看这块
fn cells_bbox(cfg: &Config, w: usize, h: usize) -> Option<(usize, usize, usize, usize)> {
    if cfg.cells.is_empty() || w == 0 || h == 0 {
        return None;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, 0f32, 0f32);
    for &(fx, fy, fw, fh) in &cfg.cells {
        x0 = x0.min(fx);
        y0 = y0.min(fy);
        x1 = x1.max(fx + fw);
        y1 = y1.max(fy + fh);
    }
    let rx = ((x0 * w as f32).floor().max(0.0) as usize).min(w.saturating_sub(1));
    let ry = ((y0 * h as f32).floor().max(0.0) as usize).min(h.saturating_sub(1));
    let rw = (((x1 - x0) * w as f32).round().max(1.0) as usize).min(w - rx);
    let rh = (((y1 - y0) * h as f32).round().max(1.0) as usize).min(h - ry);
    if rw == 0 || rh == 0 {
        None
    } else {
        Some((rx, ry, rw, rh))
    }
}

/// 两帧画面在 bbox 内的差异百分比（像素三通道差之和 > 24 记为「变了」）
fn bbox_diff_pct(a: &[u8], b: &[u8], w: usize, bbox: (usize, usize, usize, usize)) -> f64 {
    let (rx, ry, rw, rh) = bbox;
    if a.len() != b.len() || w == 0 {
        return 100.0;
    }
    let rows = b.len() / (w * 4);
    let (mut diff, mut total) = (0u64, 0u64);
    for y in ry..(ry + rh).min(rows) {
        let row = y * w;
        for x in rx..(rx + rw).min(w) {
            let i = (row + x) * 4;
            let d = (a[i] as i32 - b[i] as i32).abs()
                + (a[i + 1] as i32 - b[i + 1] as i32).abs()
                + (a[i + 2] as i32 - b[i + 2] as i32).abs();
            if d > 24 {
                diff += 1;
            }
            total += 1;
        }
    }
    if total == 0 {
        0.0
    } else {
        diff as f64 * 100.0 / total as f64
    }
}

/// 三格里最高的相似度（静默版，不打每格日志，供诊断/复核用）→ (格号 0-based, 分数)
fn best_cell_score(cfg: &Config, levels: &[matcher::TplScale], f: &Frame) -> (usize, f64) {
    let (w, h) = (f.w, f.h);
    let bgra = f.bgra.as_slice();
    let mut best = (0usize, 0.0f64);
    for (idx, &(fx, fy, fw, fh)) in cfg.cells.iter().enumerate() {
        let rx = ((fx * w as f32).round() as i64).clamp(0, w as i64) as usize;
        let ry = ((fy * h as f32).round() as i64).clamp(0, h as i64) as usize;
        let rw = ((fw * w as f32).round() as i64).max(1) as usize;
        let rh = ((fh * h as f32).round() as i64).max(1) as usize;
        let (rw, rh) = (rw.min(w.saturating_sub(rx)), rh.min(h.saturating_sub(ry)));
        if rw < 16 || rh < 16 {
            continue;
        }
        let mut crop = vec![0u8; rw * rh * 4];
        for y in 0..rh {
            let src = ((ry + y) * w + rx) * 4;
            let dst = y * rw * 4;
            crop[dst..dst + rw * 4].copy_from_slice(&bgra[src..src + rw * 4]);
        }
        let pyr = Pyramid::from_bgra(rw, rh, &crop);
        if let Some(m) = matcher::find_best(&pyr, levels, cfg.step, cfg.topk, cfg.threads) {
            if m.score > best.1 {
                best = (idx, m.score);
            }
        }
    }
    best
}

/// 诊断：点一次刷新，然后每 ~15 ms 抓一帧，测出新牌面「开始变化 / 画完 / 第一次能匹配到」的时刻。
///
/// 结论用于定「刷新后等待查找的最长时间」——它是「卡牌真实存在时，刷新到能稳定认出来」
/// 的上界，取略大于它的值就能既不空等、也不误判。
///
/// # Safety
/// 会真实点击刷新按钮、会截屏。
unsafe fn probe_refresh(cfg: &Config, levels: &[matcher::TplScale], rounds: u32) {
    logln!(
        "[probe-refresh] 采样 {rounds} 轮：每轮点一次刷新，然后每 ~15 ms 抓一帧，测新牌面开始变化 / 画完 / 首次匹配到的时刻"
    );
    if cfg.dry_run {
        logln!("[probe-refresh] dry-run 下不会真点刷新，测不到真实重绘耗时；请去掉 --dry-run 再跑");
        return;
    }
    let (mut fc_list, mut st_list) = (Vec::<u64>::new(), Vec::<u64>::new());
    let mut hit_list = Vec::<u64>::new();
    for r in 1..=rounds {
        if !RUNNING.load(Ordering::SeqCst) {
            break;
        }
        let Some(f0) = grab_frame(cfg) else {
            logw!("[probe-refresh] #{r} 抓图失败，跳过");
            continue;
        };
        let Some(bbox) = cells_bbox(cfg, f0.w, f0.h) else {
            logln!("[probe-refresh] 未配置三格区域，无法诊断");
            return;
        };
        if !click_refresh(cfg, &f0, "probe") {
            logln!("[probe-refresh] 未配置刷新按钮，无法诊断");
            return;
        }
        let t0 = epoch_ms();
        let base = f0.bgra.clone(); // 刷新前那一帧，作为「变没变」的基准
        let mut prev = f0;
        let (mut first_change, mut stable, mut first_hit) = (None::<u64>, None::<u64>, None::<u64>);
        let limit_ms = 1500u64;
        let mut trace: Vec<String> = Vec::new();
        while epoch_ms().saturating_sub(t0) < limit_ms {
            std::thread::sleep(Duration::from_millis(15));
            let Some(f) = grab_frame(cfg) else { continue };
            let since = epoch_ms().saturating_sub(t0);
            // 与「刷新前那一帧」比：判断新牌有没有开始画
            let d_base = bbox_diff_pct(&base, &f.bgra, f.w, bbox);
            // 与「上一帧」比：判断画面是否已经画完、不再动
            let d_prev = bbox_diff_pct(&prev.bgra, &f.bgra, f.w, bbox);
            let (idx, sc) = best_cell_score(cfg, levels, &f);
            if trace.len() < 14 {
                trace.push(format!("{since}ms:变{d_base:.1}% 抖{d_prev:.1}% 分{sc:.3}"));
            }
            if first_change.is_none() && d_base >= 2.0 {
                first_change = Some(since);
            }
            if first_change.is_some() && stable.is_none() && d_prev < 0.5 {
                stable = Some(since);
            }
            if first_hit.is_none() && sc >= cfg.threshold {
                let _ = idx;
                first_hit = Some(since);
            }
            prev = f;
            if let Some(st) = stable {
                if since > st + 150 {
                    break;
                }
            }
        }
        let fc = first_change.map(|v| format!("{v}ms")).unwrap_or_else(|| "未变化".into());
        let st = stable.map(|v| format!("{v}ms")).unwrap_or_else(|| "未稳定".into());
        let hit = first_hit.map(|v| format!("{v}ms")).unwrap_or_else(|| "这手牌无魔精灵".into());
        logln!("[probe-refresh] #{r} 开始变化 {fc}｜画面画完 {st}｜首次匹配到 {hit}");
        logln!("               采样曲线 {}", trace.join(" | "));
        if let Some(v) = first_change {
            fc_list.push(v);
        }
        if let Some(v) = stable {
            st_list.push(v);
        }
        if let Some(v) = first_hit {
            hit_list.push(v);
        }
        std::thread::sleep(Duration::from_millis(600));
    }

    let stat = |name: &str, v: &Vec<u64>| {
        if v.is_empty() {
            logln!("[probe-refresh] {name}: 无有效样本");
        } else {
            let avg = v.iter().sum::<u64>() as f64 / v.len() as f64;
            logln!(
                "[probe-refresh] {name}: {} 个样本，最小 {} ms / 平均 {:.0} ms / 最大 {} ms",
                v.len(),
                v.iter().min().unwrap(),
                avg,
                v.iter().max().unwrap()
            );
        }
    };
    stat("开始变化", &fc_list);
    stat("画面画完", &st_list);
    stat("首次匹配到魔精灵", &hit_list);
    if let Some(mx) = st_list.iter().max() {
        logln!(
            "[probe-refresh] 建议：把「刷新后等待查找的最长时间」设为 {} ms 左右（实测最大 {mx} ms + 余量），到点还没找到就立刻再刷新",
            mx + 100
        );
    }
}

// ======================= 刷新按钮标定（换机用） =======================

/// 扫描底部按钮栏，找出「点一下三格牌面就会变、但整窗不大变」的那个坐标，打印客户区坐标与相对比例。
/// 换电脑后游戏窗口尺寸/宽高比一变，刷新按钮的相对比例就会偏，用这个命令重新标定：
///   mojingling_clicker.exe --window <标题关键字> --scan-refresh
/// # Safety
/// 会真实移动鼠标并点击底部栏若干位置。
unsafe fn scan_refresh(cfg: &Config) {
    logln!("[scan-refresh] 开始扫描底部按钮栏，定位真正的「刷新」按钮（会真实点击，请保持游戏窗口可见、不要手动干预）");
    if cfg.dry_run {
        logln!("[scan-refresh] dry-run 下不会真点，扫不到结果；请去掉 --dry-run 再跑");
        return;
    }
    let Some(f0) = grab_frame(cfg) else {
        loge!("[error] [scan-refresh] 抓图失败：先用 --probe 确认抓图通路（pw / dc / scr）");
        return;
    };
    let (w, h) = (f0.w, f0.h);
    let target = f0.target;
    let Some(bbox) = cells_bbox(cfg, w, h) else {
        logln!("[scan-refresh] 未配置三格区域（--cells），无法判断牌面是否变化");
        return;
    };
    logln!(
        "[scan-refresh] 客户区 {w}x{h}，三格区域 x{}..{} y{}..{}",
        bbox.0,
        bbox.0 + bbox.2,
        bbox.1,
        bbox.1 + bbox.3
    );

    // 单点探测：抓基准帧 → 点一下 → 等重绘 → 抓帧，返回（三格变化%, 整窗变化%）
    let probe = |cx: i32, cy: i32| -> Option<(f64, f64)> {
        let base = grab_frame(cfg)?;
        click::click_bg_or_fg(target, cx, cy, cfg.bg_click, cfg.bring_to_front);
        std::thread::sleep(Duration::from_millis(1000));
        let f2 = grab_frame(cfg)?;
        if f2.w != base.w || f2.h != base.h {
            return None;
        }
        let cells = bbox_diff_pct(&base.bgra, &f2.bgra, f2.w, bbox);
        let win = bbox_diff_pct(&base.bgra, &f2.bgra, f2.w, (0, 0, f2.w, f2.h));
        Some((cells, win))
    };
    // 刷新的特征：三格牌被换掉（变化大），而三格只占整窗一小块（整窗变化小得多）
    let looks_refresh = |cells: f64, win: f64| -> bool {
        cells >= 20.0 && win >= 0.5 && cells / win.max(0.5) >= 1.4
    };

    let y0 = REFRESH_Y;
    let mut cands: Vec<(f32, f64)> = Vec::new(); // (x 相对比例, 三格变化) 供第二轮细化
    let mut best: Option<(f32, i32, i32, f64, f64)> = None;

    // 第一轮：当前常数所在行，步长 0.02 粗扫
    let mut x = 0.55f32;
    while x <= 0.99 {
        let cx = (x * w as f32).round() as i32;
        let cy = (y0 * h as f32).round() as i32;
        if let Some((cells, win)) = probe(cx, cy) {
            logln!("[scan-refresh] 粗扫 ({x:.4},{y0:.4}) 客户区({cx},{cy}) → 三格 {cells:.1}% / 整窗 {win:.1}%");
            if looks_refresh(cells, win) {
                if best.map(|b| cells > b.3).unwrap_or(true) {
                    best = Some((x, cx, cy, cells, win));
                }
            }
            cands.push((x, cells));
        } else {
            logw!("[scan-refresh] 客户区({cx},{cy}) 抓图失败，跳过");
        }
        x += 0.02;
    }
    if best.is_some() {
        logln!("[scan-refresh] 粗扫已命中，跳过细化");
    } else {
        // 第二轮：取三格变化最大的几处，在 ±0.03 内以 0.006 步长细化
        cands.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let seeds: Vec<f32> = cands.iter().take(3).map(|c| c.0).collect();
        logln!("[scan-refresh] 粗扫未命中，改在三格变化最大的 {} 处细化（步长 0.006）", seeds.len());
        'seed: for sx in seeds {
            let mut k = -5i32;
            while k <= 5 {
                let x = sx + k as f32 * 0.006;
                if x >= 0.50 && x <= 0.995 {
                    let cx = (x * w as f32).round() as i32;
                    let cy = (y0 * h as f32).round() as i32;
                    if let Some((cells, win)) = probe(cx, cy) {
                        logln!("[scan-refresh] 细化 ({x:.4},{y0:.4}) 客户区({cx},{cy}) → 三格 {cells:.1}% / 整窗 {win:.1}%");
                        if looks_refresh(cells, win) {
                            if best.map(|b| cells > b.3).unwrap_or(true) {
                                best = Some((x, cx, cy, cells, win));
                            }
                            break 'seed;
                        }
                    }
                }
                k += 1;
            }
        }
    }

    match best {
        Some((fx, cx, cy, cells, win)) => {
            logln!(
                "[scan-refresh] 命中：客户区({cx},{cy})，相对比例 ({fx:.6},{y0:.6})，三格变化 {cells:.1}% / 整窗 {win:.1}%"
            );
            logln!("[scan-refresh] 直接用：--refresh {cx},{cy}（按相对比例换算，窗口缩放也适用）");
            logln!("[scan-refresh] 固化进程序：把 REFRESH_X 改成 {fx:.6}、REFRESH_Y 改成 {y0:.6} 后重新编译");
        }
        None => {
            logln!("[scan-refresh] 本次没扫到符合刷新特征的按钮：可能窗口被遮挡 / 抓图是空帧 / 刷新按钮不在底部栏；可用 --refresh x,y 手动指定，或换 --capture dc 再试");
        }
    }
}

// ======================= 节拍 =======================

/// 实际节拍周期（毫秒）：--interval-ms 优先，否则 --interval 秒 × 1000
pub fn cycle_ms(cfg: &Config) -> u64 {
    if cfg.interval_ms > 0 {
        cfg.interval_ms
    } else {
        cfg.interval_secs.max(1).saturating_mul(1000)
    }
}

/// 点击前复检：等到「该点」的那一刻，重新抓一帧确认这张牌还在、分数还够。
///
/// 为什么需要：命中之后要一直按住等到节拍到点（默认最长一个周期），这期间游戏可能出牌、
/// 播入场动画、玩家自己点掉这张牌。拿等待之前那一帧的坐标去点，就是「日志说点中了、
/// 实际点了个空」——这是漏打最隐蔽的一类来源。
///
/// 返回 Some((格位, cx, cy, score, frame))：可以点，且用复检到的新坐标与新格位；
/// 返回 None：这张牌已经不该点了，调用方应改走「刷新换牌」。
///
/// 【v1.3.4】返回值补上格位：复检可能发现牌挪到了另一格，原来只回传坐标，
/// 调用方日志仍打印初判格位（实测出现「第142次（第3格…点(453,563)）」而 453,563 是第1格），
/// 坐标是对的，但人工核对日志时会误判成点错格。
unsafe fn recheck_target(
    cfg: &Config,
    levels: &[matcher::TplScale],
    idx: usize,
    old_score: f64,
) -> Option<(usize, i32, i32, f64, Frame)> {
    let f = match unsafe { grab_frame(cfg) } {
        Some(f) => f,
        None => {
            logw!("[recheck] 到点复检抓图失败 → 放弃本次点击，按未命中处理");
            return None;
        }
    };
    let found = if cfg.cells.is_empty() {
        match_frame_full(cfg, levels, &f).map(|(x, y, s)| (0usize, x, y, s))
    } else {
        match_cells(cfg, levels, &f).0
    };
    let (nidx, nx, ny, ns) = match found {
        Some(v) => v,
        None => {
            logln!(
                "[recheck] 到点复检：牌已经不在了（原 第{idx}格 {old_score:.3}）→ 放弃这次点击，按未命中处理"
            );
            return None;
        }
    };
    // 复检容差：旧实现硬卡「命中线 - 0.05」，会把「低分兜底命中」（[loose]，0.66~0.78）一律否掉，
    // 于是刚认出来的真牌又被改判成刷新（还是那个「有卡却去刷新」）。现在改成：
    // 允许落到命中线 - LOOSE_DROP，同时不允许比等待前掉超过 0.05。
    //
    // 【2026-11-09 修正】assume_floor 已从 0.55 降到 0.20（空槽只等于没钱，由红色像素检测管），
    // 所以这里不能再直接跟 assume_floor 走，否则复检门槛会松到 0.20、
    // 「等点期间牌被换掉」这种真该拦的情况就拦不住了。
    // 改成一个独立的复检底线 0.45：既有足够余量容纳被动画压低的真牌，
    // 又能识别出「这一格已经不是原来那张牌」。
    //
    // 【v1.3.3 修正】上面 0.45 的底线曾被下一行 `floor = hard_floor.max((lo-0.05).min(old-0.05))`
    // 整个吃掉：assume_one 下 hard_floor=0.45，但那个 max 又把它顶回 0.730，
    // 于是「手牌放满 5 秒被动画压分」的真牌（实测 0.626）每次复检都被否决 → 空刷金币。
    // 现在 assume_one 下直接采用 hard_floor，不再参与「掉分不超过 0.05」的比较。
    let hard_floor = if cfg.assume_one {
        cfg.assume_floor.max(RECHECK_ASSUME_FLOOR).min(cfg.lo_score() - LOOSE_DROP)
    } else {
        cfg.lo_score() - LOOSE_DROP
    };
    let floor = if cfg.assume_one {
        hard_floor
    } else {
        hard_floor.max((cfg.lo_score() - 0.05).min(old_score - 0.05))
    };
    if ns < floor {
        logln!(
            "[recheck] 到点复检：相似度从 {old_score:.3} 掉到 {ns:.3}（低于本轮容差 {floor:.3}）→ 放弃这次点击"
        );
        return None;
    }
    if nidx != idx {
        logln!("[recheck] 到点复检：目标移到第{nidx}格（原第{idx}格）→ 按新位置 ({nx},{ny}) 点");
    } else {
        logvln!("[recheck] 到点复检通过：第{nidx}格 {ns:.3}（等待前 {old_score:.3}），点 ({nx},{ny})");
    }
    Some((nidx, nx, ny, ns, f))
}

/// 刷新之后：等牌面重绘，并核对「这次刷新到底生效了没有」。
///
/// 判据是刷新前后三格区域的像素差异：差异过小说明牌面根本没换（刷新按钮坐标偏了、
/// 或后台 PostMessage 被游戏忽略）。这种情况程序会一直刷、钱一直掉、却什么也没换到，
/// 正是「乱刷钱」的典型形态，所以这里留一条明确的告警日志。
/// 返回牌面差异百分比；未启用核对、dry-run 或抓图失败返回 -1。
fn settle_after_refresh(cfg: &Config, before: &Frame) -> f64 {
    let ms = cfg.miss_interval_ms.max(REFRESH_SETTLE_MS);
    sleep_interruptible(Duration::from_millis(ms));
    if !cfg.refresh_verify || cfg.dry_run || !RUNNING.load(Ordering::SeqCst) {
        return -1.0;
    }
    let Some(bb) = cells_bbox(cfg, before.w, before.h) else {
        return -1.0;
    };
    let Some(nf) = (unsafe { grab_frame(cfg) }) else {
        return -1.0;
    };
    if nf.w != before.w || nf.h != before.h || nf.bgra.len() != before.bgra.len() {
        return -1.0;
    }
    let d = bbox_diff_pct(&before.bgra, &nf.bgra, before.w, bb);
    if d < REFRESH_EFFECT_MIN_PCT {
        logw!(
            "[refresh] 刷新后牌面几乎没变（差异 {d:.2}% < {REFRESH_EFFECT_MIN_PCT:.2}%）→ 这次刷新可能没生效：核对刷新按钮坐标（--scan-refresh 重新标定），或改 --fg-click 用前台点击"
        );
    } else {
        logvln!("[refresh] 刷新生效：三格区域牌面差异 {d:.2}%");
    }
    d
}

/// 按「刷新前基准帧」重算三格差异（用当前这一帧，不额外抓图）。
///
/// 用于复核「刷新当时看起来没变，但可能只是游戏卡顿、牌面晚画完」的情况：
/// 定位阶段每帧都会顺手调一次，牌面一画完就能立刻放行判空。
fn tri_diff_pct_vs_base(cfg: &Config, base: &(Vec<u8>, usize, usize), cur: &Frame) -> f64 {
    if !cfg.refresh_verify || cfg.dry_run || !RUNNING.load(Ordering::SeqCst) {
        return -1.0;
    }
    let (base_bgra, base_w, base_h) = base;
    if cur.w != *base_w || cur.h != *base_h || cur.bgra.len() != base_bgra.len() {
        return -1.0;
    }
    let Some(bb) = cells_bbox(cfg, *base_w, *base_h) else {
        return -1.0;
    };
    bbox_diff_pct(base_bgra, &cur.bgra, *base_w, bb)
}

/// 刷新记账：回答一个问题——「这次刷新，牌面到底换了没有？」
///
/// 空刷的根因就是这个没人回答：刷新没生效时牌面还是旧牌，旧牌已经判过「没有」，
/// 再判当然还是没有 → 一两秒后又刷一次，金币白掉、牌一张没换。
/// 所以「判这手牌没有」之前，必须先确认这一次刷新是真的生效了。
struct RefreshBook {
    /// 本次刷新是否已确认让牌面变了
    effective: bool,
    /// 刷新前的基准帧（bgra, w, h）；没开核对时为 None
    base: Option<(Vec<u8>, usize, usize)>,
    /// 连续「刷了但牌面没变」的次数
    bad: u32,
}

impl RefreshBook {
    fn new() -> Self {
        Self { effective: true, base: None, bad: 0 }
    }

    /// 点刷新之前调用：留基准帧，并把状态先置为「未确认生效」。
    fn before_refresh(&mut self, cfg: &Config, f: &Frame) {
        self.base = if cfg.refresh_verify && !cfg.dry_run {
            Some((f.bgra.clone(), f.w, f.h))
        } else {
            None
        };
        // 没开核对 / dry-run 时无从判断，保持不阻塞（与旧行为一致，不制造死等）
        self.effective = self.base.is_none();
    }

    /// 刷新之后调用，参数是 settle_after_refresh 的返回值（<0 表示判定不了）。
    fn after_refresh(&mut self, d: f64) {
        if d < 0.0 {
            self.effective = true;
            return;
        }
        if d >= REFRESH_EFFECT_MIN_PCT {
            self.effective = true;
            self.bad = 0;
        } else {
            self.effective = false;
            self.bad += 1;
        }
    }

    /// 定位阶段每帧复核：牌面后来画完了，就把「未确认生效」改判为生效。
    fn recheck_late(&mut self, cfg: &Config, f: &Frame) -> f64 {
        let Some(base) = self.base.as_ref() else {
            return -1.0;
        };
        let d = tri_diff_pct_vs_base(cfg, base, f);
        if d >= REFRESH_EFFECT_MIN_PCT {
            self.effective = true;
            self.bad = 0;
        }
        d
    }

    fn blocked(&self) -> bool {
        self.bad >= REFRESH_BLOCK_ROUNDS
    }
}

const BAD_REFRESH_MSG: &str = "刷新后牌面都没变 → 已停止自动点击，不再继续消耗金币。请核对刷新按钮坐标（--scan-refresh 重新标定），或改 --fg-click 确认后台点击是否被游戏忽略";

/// 主循环：一「手」牌的节奏（按用户定的规则）
///
///   1. 点中魔精灵的那一刻开始计时（默认 5 秒）；
///   2. 点中后立刻点刷新换牌，然后马上抓图查找；
///   3. 找到了 → 记下坐标但先不点，等计时到点才点（等待期间不刷新，否则这张牌会被换掉）；
///   4. 没找到 → 立刻刷新再找，一直刷到找出魔精灵为止；
///   5. 若刷新查找已经用掉 5 秒还没找到，那就继续找，找到就点，下一次 5 秒从那次点中重新起算。
///
/// 返回（查找次数, 点中次数）
pub fn click_loop(cfg: &Config, engine: Option<&Engine>, levels: &[matcher::TplScale]) -> (u32, u32) {
    let period_ms = cycle_ms(cfg).max(200);
    // 每次开始清空冻结帧计数，避免上一次运行的陈旧指纹影响本轮首帧判定
    crate::capture::reset_frozen_watch();
    INTERVAL_MS.store(period_ms, Ordering::SeqCst);
    TICK_SEQ.store(0, Ordering::SeqCst);
    START_MS.store(epoch_ms(), Ordering::SeqCst);
    LAST_TICK_MS.store(epoch_ms(), Ordering::SeqCst);
    HIT_AT_MS.store(0, Ordering::SeqCst);
    // 刷新后最小保护间隔：不低于 REFRESH_SETTLE_MS，避免抓到旧牌残留像素
    let settle_ms = cfg.miss_interval_ms.max(REFRESH_SETTLE_MS);
    logsel!(
        ("[beat] 节奏 {:.1} 秒：点中那一刻起计时 → 立刻刷新换牌 → 立刻抓图找；找到先按住不点，等满 {:.1} 秒再点",
            period_ms as f64 / 1000.0, period_ms as f64 / 1000.0)
        ("[开始] 节拍 {:.1} 秒一次（没找到/等钱超过 {:.1} 秒时，一找到就立即点）",
            period_ms as f64 / 1000.0, period_ms as f64 / 1000.0)
    );
    logvln!(
        "[beat] 刷新后查找窗口 {} ms（到点没找到就立刻再刷）；另加「牌面静止仍然没有」立即再刷（静止要求 ≥{STABLE_MIN_MS} ms 且两帧差异 <{STABLE_DIFF_PCT}%，且连续 {EMPTY_VOTES_NEED} 帧空票、跨度 ≥{EMPTY_VOTES_SPAN_MS} ms）；判空前至少观察 {} ms；刷新后最小保护 {settle_ms} ms；底纹防线：{}；三格必有一格：{}（底线分 {:.2}）",
        cfg.find_wait_ms,
        if cfg.assume_one { ASSUME_MISS_OBSERVE_MS } else { EMPTY_MIN_OBSERVE_MS },
        if cfg.bg_guard && !cfg.assume_one { "开" } else { "关" },
        if cfg.assume_one { "开" } else { "关" },
        cfg.assume_floor
    );
    logvln!(
        "[beat] 刷新最小间隔 {} ms（--refresh-gap-ms）：两刷之间至少隔这么久，防止把金币刷光",
        cfg.refresh_gap_ms
    );

    let mut scans: u32 = 0; // 查找次数
    let mut clicks: u32 = 0; // 点中次数
    let mut refreshes: u32 = 0; // 刷新次数
    let mut due: u64 = 0; // 下一次允许点击的时刻（epoch ms；0 = 找到就点）
    let mut prev_click: u64 = 0; // 上一次点中的时刻
    let mut last_refresh: u64 = 0; // 上一次刷新的时刻（等待窗口从这里起算）
    let mut prev_miss_bgra: Option<Vec<u8>> = None; // 本手牌上一帧画面，用于「静止确认没有」
    let mut grace_round: u32 = 0; // 贴阈值帧已经宽限了几轮（防把动画中的真牌刷掉）
    let mut book = RefreshBook::new(); // 刷新记账：这次刷新到底生效了没有（判空的前提）
    let mut empty_votes: u32 = 0; // 连续「确认没有」的空帧票数（多帧投票，替代单帧判决）
    let mut empty_vote_at: u64 = 0; // 第一张空票的时刻（要求票数跨越足够长的时间）

    while RUNNING.load(Ordering::SeqCst) && (cfg.times == 0 || clicks < cfg.times) {
        scans += 1;
        TICK_SEQ.store(scans as u64, Ordering::SeqCst);
        let scan = unsafe { scan_once(cfg, engine, levels) };
        if !RUNNING.load(Ordering::SeqCst) {
            break;
        }

        match scan {
            Scan::Hit { idx, cx, cy, score, frame: f } => {
                let where_txt = if idx == 0 { "整窗".to_string() } else { format!("第{idx}格") };

                // 采样模式（--sample-only）：只存图 + 立刻刷新换牌，绝不点牌。
                // 用来把落在灰区的帧抓成样本，供人工核验「这几格到底有没有魔精灵」。
                if cfg.sample_only {
                    if cfg.dump {
                        let dir = dump_dir();
                        let name =
                            format!("gray_s{:.3}_c{}_t{}_{}.bmp", score, idx, scans, now_stamp());
                        let path = dir.join(&name);
                        let _ = std::fs::write(&path, matcher::bgra_to_bmp32(&f.bgra, f.w, f.h));
                        logln!(
                            "[sample] #{clicks} {where_txt} 相似度 {score:.3}（区间 {}）→ {}",
                            cfg.range_txt(),
                            path.display()
                        );
                    }
                    if !ensure_money(cfg, &f, "采样换牌") {
                        break;
                    }
                    clicks += 1;
                    if cfg.times > 0 && clicks >= cfg.times {
                        break;
                    }
                    // 采样模式不参与判空：重置记账与投票，避免后续定位阶段被卡在「未确认生效」
                    book = RefreshBook::new();
                    empty_votes = 0;
                    empty_vote_at = 0;
                    unsafe { click_refresh(cfg, &f, "采样换牌") };
                    last_refresh = epoch_ms();
                    prev_miss_bgra = None;
                    // 采样必须等牌面真正画完再抓下一帧：settle_ms 只有 100 ms，
                    // 那时新牌还没重绘，会反复采到同一手牌（实测多帧像素差为 0）。
                    let wait_ms = settle_ms.max(SAMPLE_SETTLE_MS);
                    INTERVAL_MS.store(wait_ms, Ordering::SeqCst);
                    LAST_TICK_MS.store(epoch_ms(), Ordering::SeqCst);
                    sleep_interruptible(Duration::from_millis(wait_ms));
                    continue;
                }

                // 1) 到点判定：
                //    - 还没到点 → 先按住不点（等待期间不刷新，避免把已找到的这张牌换掉）
                //    - 已经超期（这手牌查得比一个周期还久，通常是没找到/等钱耽搁的）→ 不再等，立刻点
                let now = epoch_ms();
                let overdue = due > 0 && now >= due;
                if !overdue && due > now {
                    let wait = due - now;
                    logsel!(
                        ("[hold] 第{scans}次查找：{where_txt}命中（相似度{score:.3}），距到点还差 {:.0} ms → 按住不点，等点", wait as f64)
                        ("[等点] 已找到，距到点还差 {:.1} 秒 → 按住不点", wait as f64 / 1000.0)
                    );
                    INTERVAL_MS.store(wait.max(1), Ordering::SeqCst);
                    LAST_TICK_MS.store(now, Ordering::SeqCst);
                    sleep_interruptible(Duration::from_millis(wait));
                    if !RUNNING.load(Ordering::SeqCst) {
                        break;
                    }
                } else if overdue {
                    let late = now.saturating_sub(due);
                    logsel!(
                        ("[hold] 第{scans}次查找：{where_txt}命中（相似度{score:.3}），已超期 {late} ms → 不再等，立刻点")
                        ("[超期] 已超过一个周期 {:.1} 秒，有钱且找到了，直接点", late as f64 / 1000.0)
                    );
                }

                // 2) 到点了 → 点之前再确认这张牌还在（等待期间可能已被游戏换掉/玩家点掉），
                //    复检通过就按复检到的新坐标点；复检落空则这一下不算点中，直接换牌。
                // 【v1.3.4】多接一个格位：复检可能把目标挪到别的格（如初判第3格、复检第1格），
                // 后面的点中日志要按这个最终格位打印，否则坐标与文案会自相矛盾。
                let (final_idx, cx, cy, score, f) = if cfg.recheck {
                    match unsafe { recheck_target(cfg, levels, idx, score) } {
                        Some(v) => v,
                        // 【v1.3.3】三格必有一格：牌一定在手牌里，复检落空只说明「抓早了 / 画面在动」。
                        // 旧行为是花金币刷掉这手牌（实测 25 次落空 → 25 次空刷，金币白烧），
                        // 这里改为按初判坐标直接点：点不中不扣钱，点中就赚，且不会触发刷新换牌。
                        None if cfg.assume_one => {
                            grace_round = 0;
                            logln!(
                                "[recheck] (+{}ms) 复检落空 → 三格必有一格：不刷牌，直接按初判坐标 ({cx},{cy}) 点（相似度 {score:.3}）",
                                elapsed_ms()
                            );
                            (idx, cx, cy, score, f)
                        }
                        None => {
                            grace_round = 0;
                            let now2 = epoch_ms();
                            let since2 = now2.saturating_sub(last_refresh);
                            if since2 < cfg.refresh_gap_ms {
                                sleep_interruptible(Duration::from_millis(
                                    cfg.refresh_gap_ms - since2,
                                ));
                                if !RUNNING.load(Ordering::SeqCst) {
                                    break;
                                }
                            }
                            if !ensure_money(cfg, &f, "复检落空换牌") {
                                break;
                            }
                            refreshes += 1;
                            logln!(
                                "[recheck] (+{}ms) 复检落空 → 刷第 {} 次换牌（这一下不计入点中）",
                                elapsed_ms(),
                                refreshes
                            );
                            book.before_refresh(cfg, &f);
                            empty_votes = 0;
                            empty_vote_at = 0;
                            unsafe { click_refresh(cfg, &f, "复检落空") };
                            last_refresh = epoch_ms();
                            prev_miss_bgra = None;
                            INTERVAL_MS.store(settle_ms.max(1), Ordering::SeqCst);
                            LAST_TICK_MS.store(epoch_ms(), Ordering::SeqCst);
                            let d = settle_after_refresh(cfg, &f);
                            book.after_refresh(d);
                            if book.blocked() {
                                logw!(
                                    "[refresh] 连续 {} 次{}",
                                    book.bad,
                                    BAD_REFRESH_MSG
                                );
                                break;
                            }
                            continue;
                        }
                    }
                } else {
                    (idx, cx, cy, score, f)
                };

                // 【v1.3.4】点中日志的格位以最终格位为准（复检移位后不再沿用初判文案）。
                let where_txt = if final_idx == 0 {
                    "整窗".to_string()
                } else {
                    format!("第{final_idx}格")
                };

                // 3) 点中魔精灵，下一个周期从这一下重新起算
                let clk_at = epoch_ms();
                HIT_AT_MS.store(clk_at, Ordering::SeqCst);
                unsafe { click_point(cfg, &f, cx, cy, "魔精灵") };
                clicks += 1;
                let gap = if prev_click == 0 { 0 } else { clk_at.saturating_sub(prev_click) };
                prev_click = clk_at;
                due = clk_at.saturating_add(period_ms);
                LAST_TICK_MS.store(clk_at, Ordering::SeqCst);
                INTERVAL_MS.store(period_ms, Ordering::SeqCst);
                logsel!(
                    ("[hit] 第 {clicks} 次点中{where_txt}（相似度{score:.3}，点客户区({cx},{cy})，与上次点中相隔 {gap} ms），{:.1} 秒计时自此刻起算", period_ms as f64 / 1000.0)
                    ("[点中] 第 {clicks} 次（{where_txt}，相似度 {score:.3}，点({cx},{cy})），下一次 {:.1} 秒后", period_ms as f64 / 1000.0)
                );
                if !RUNNING.load(Ordering::SeqCst) {
                    break;
                }

                // 3) 点中后立刻刷新换牌（先确认钱够：金币变红就停下等钱，红了没了再刷）
                if !ensure_money(cfg, &f, "点中后换牌") {
                    break;
                }
                refreshes += 1;
                book.before_refresh(cfg, &f);
                empty_votes = 0;
                empty_vote_at = 0;
                unsafe { click_refresh(cfg, &f, "点中后换牌") };
                last_refresh = epoch_ms();
                prev_miss_bgra = None; // 换牌了，旧帧不再是基准
                grace_round = 0;
                INTERVAL_MS.store(settle_ms.max(1), Ordering::SeqCst);
                LAST_TICK_MS.store(epoch_ms(), Ordering::SeqCst);
                let d = settle_after_refresh(cfg, &f);
                book.after_refresh(d);
                if book.blocked() {
                    logw!("[refresh] 连续 {} 次{}", book.bad, BAD_REFRESH_MSG);
                    break;
                }
            }

            Scan::Miss { score, frame: f } => {
                // 没找到，但「现在没找到」不等于「这手牌没有」——新牌可能还没画完。
                // 两条路都会立刻再刷一次：
                //   A. 刷完已经等满 cfg.find_wait_ms（实测牌面 66 ms 开始换、300~367 ms 画完、
                //      233 ms 就能匹配到卡片，450 ms 是这个「刷新后找到卡牌」的上界的余量版）
                //   B. 画面已经静止（≥250 ms 且两帧差异 <0.5%）却仍然没有 → 确认这手牌确实没有
                let (fw, fh) = (f.w, f.h);
                let now = epoch_ms();
                let since = now.saturating_sub(last_refresh);
                let quiet = match (prev_miss_bgra.as_ref(), cells_bbox(cfg, fw, fh)) {
                    (Some(p), Some(bb)) if p.len() == f.bgra.len() => {
                        bbox_diff_pct(p, &f.bgra, fw, bb) < STABLE_DIFF_PCT
                    }
                    _ => false,
                };
                let since_txt = if last_refresh == 0 { "首次".to_string() } else { format!("{since} ms") };
                // 分数已经贴到阈值边上的（可能是被动画压低的真卡牌），不轻易判「没有」，只等超时
                let borderline = score >= cfg.lo_score() - 0.08;
                // 刷新最小间隔：不管「超时未找到」还是「画面静止确认没有」，两刷之间都不许短于它。
                // 没有这条时实测两刷间隔只有 450~570 ms，一分钟刷上百次，金币根本跟不上。
                let gap_min = cfg.refresh_gap_ms;

                // ── 防线 1：这次刷新还没被确认生效（牌面几乎没变）→ 牌还是旧牌，绝不能判空。
                //    旧牌早就被判过一次「没有」，再判当然还是没有，这就是空刷本体：
                //    钱扣了、牌没换，一两秒后又刷一次。
                if !book.effective {
                    let late = if since >= EMPTY_MIN_OBSERVE_MS {
                        book.recheck_late(cfg, &f)
                    } else {
                        -1.0
                    };
                    if book.effective {
                        logvln!(
                            "[refresh] (+{}ms) 牌面后来画完了（三格差异 {late:.2}%）→ 恢复判空",
                            elapsed_ms()
                        );
                    } else if since < REFRESH_UNSURE_WAIT_MS {
                        empty_votes = 0;
                        empty_vote_at = 0;
                        prev_miss_bgra = Some(f.bgra);
                        LAST_TICK_MS.store(now, Ordering::SeqCst);
                        INTERVAL_MS.store(UNSETTLED_POLL_MS, Ordering::SeqCst);
                        logvln!(
                            "[wait] 刷新后 {since} ms 牌面还没变化（差异 {late:.2}%）→ 不判空，继续等牌画完（上限 {REFRESH_UNSURE_WAIT_MS} ms）"
                        );
                        sleep_interruptible(Duration::from_millis(UNSETTLED_POLL_MS));
                        continue;
                    } else {
                        logw!(
                            "[refresh] 刷新后 {since} ms 牌面仍几乎没变（差异 {late:.2}% < {REFRESH_EFFECT_MIN_PCT:.2}%）→ 这回刷新可能没生效（按钮坐标偏了 / 后台点击被忽略）。按超时放行本次；连续 {REFRESH_BLOCK_ROUNDS} 次不生效会直接停手"
                        );
                        book.effective = true; // 放行本次，交给下面的超时刷新 + 止血阀
                    }
                }

                // ── 防线 2：多帧投票。单帧判决会把「牌面正在重绘的空档」直接当成没牌。
                if quiet && !borderline {
                    if empty_votes == 0 {
                        empty_vote_at = now;
                    }
                    empty_votes = empty_votes.saturating_add(1);
                } else {
                    empty_votes = 0;
                    empty_vote_at = 0;
                }
                let vote_ok = empty_votes >= EMPTY_VOTES_NEED
                    && now.saturating_sub(empty_vote_at) >= EMPTY_VOTES_SPAN_MS;

                // ── 防线 3：最短观察窗口 + 票数门槛。
                //    「画面静止确认没有」要求 1.2 秒之后且票数达标；
                //    「超时未找到」同样要求票数达标，只在画面一直在动（票永远凑不齐）时
                //    用 find_wait + 1.2 秒的兜底放行，避免死等不刷。
                // 【2026-11-09 修正】assume_one 模式下「三格必有一张」是硬规则，
                // Miss 只可能是「抓早了，牌还没画完」或「坐标偏了」，
                // 所以观察窗口从 EMPTY_MIN_OBSERVE_MS(1200) 抬到 ASSUME_MISS_OBSERVE_MS(1500)，
                // 把「牌没画完就花钱再刷一次」这个空刷源头再收紧一档。
                let observe_floor = if cfg.assume_one {
                    ASSUME_MISS_OBSERVE_MS
                } else {
                    EMPTY_MIN_OBSERVE_MS
                };
                let hard_deadline = cfg
                    .find_wait_ms
                    .max(gap_min)
                    .saturating_add(EMPTY_VOTES_SPAN_MS * 4);
                let by_timeout = since >= cfg.find_wait_ms.max(gap_min).max(observe_floor)
                    && (vote_ok || since >= hard_deadline);
                let by_quiet = since >= observe_floor.max(gap_min)
                    && quiet
                    && !borderline
                    && vote_ok;

                // 分数贴着阈值：先给两轮宽限再看，别把「动画还没画完的真牌」当成没有刷掉。
                // 宽限期间不刷新（不花钱），只重新抓一帧重扫，分数升上去就会自然命中并点击。
                if cfg.borderline_grace && by_timeout && borderline && grace_round < BORDERLINE_GRACE_ROUNDS {
                    grace_round += 1;
                    logvln!(
                        "[grace] 最高分 {score:.3} 贴着阈值 {:.2}（宽限 {}/{}）→ 先不刷新，等牌画完再看",
                        cfg.lo_score(),
                        grace_round,
                        BORDERLINE_GRACE_ROUNDS
                    );
                    INTERVAL_MS.store(BORDERLINE_GRACE_MS, Ordering::SeqCst);
                    LAST_TICK_MS.store(now, Ordering::SeqCst);
                    sleep_interruptible(Duration::from_millis(BORDERLINE_GRACE_MS));
                    continue;
                }

                if by_timeout || by_quiet {
                    grace_round = 0;
                    let why = if by_timeout { "超时未找到" } else { "画面静止确认没有" };
                    // 金币变红 = 没钱刷新了：先停下等钱（等钱期间不刷新不点击不查找），红了没了再刷
                    if !ensure_money(cfg, &f, why) {
                        break;
                    }
                    refreshes += 1;
                    if since > 60_000 {
                        // 启动后的第一次刷新没有「上一次」，别把 epoch 差打印成人看的间隔
                        logln!(
                            "[find] (+{}ms) {why} → 刷第 {} 次（首刷，之后两刷最小间隔 {} ms）",
                            elapsed_ms(),
                            refreshes,
                            cfg.refresh_gap_ms
                        );
                    } else {
                        logln!(
                            "[find] (+{}ms) 刷新后已等 {since_txt}，{why} → 刷第 {} 次（两刷间隔 {} ms，下限 {} ms）",
                            elapsed_ms(),
                            refreshes,
                            since,
                            cfg.refresh_gap_ms
                        );
                    }
                    book.before_refresh(cfg, &f);
                    empty_votes = 0;
                    empty_vote_at = 0;
                    unsafe { click_refresh(cfg, &f, why) };
                    last_refresh = epoch_ms();
                    prev_miss_bgra = None;
                    INTERVAL_MS.store(settle_ms.max(1), Ordering::SeqCst);
                    LAST_TICK_MS.store(epoch_ms(), Ordering::SeqCst);
                    let d = settle_after_refresh(cfg, &f);
                    book.after_refresh(d);
                    if book.blocked() {
                        logw!("[refresh] 连续 {} 次{}", book.bad, BAD_REFRESH_MSG);
                        break;
                    }
                } else {
                    // 新牌还在画 / 还没到最小刷新间隔：不刷新，但也别每 20~30 ms 狂抓一帧把 CPU 吃满
                    prev_miss_bgra = Some(f.bgra);
                    LAST_TICK_MS.store(now, Ordering::SeqCst);
                    let gap_left = gap_min.saturating_sub(since);
                    if gap_left > 0 {
                        let nap = gap_left.min(200);
                        INTERVAL_MS.store(nap.max(1), Ordering::SeqCst);
                        sleep_interruptible(Duration::from_millis(nap));
                    }
                }
            }

            Scan::Abort => {
                sleep_interruptible(Duration::from_millis(500));
            }
        }
    }
    (scans, clicks)
}

/// 可被「停止」中断的等待（100 ms 粒度轮询，避免点了停止还要干等一整轮）
fn sleep_interruptible(dur: Duration) {
    let mut slept = Duration::ZERO;
    while slept < dur {
        if !RUNNING.load(Ordering::SeqCst) {
            return;
        }
        let step = std::cmp::min(Duration::from_millis(100), dur - slept);
        std::thread::sleep(step);
        slept += step;
    }
}

/// 窗体模式的工作线程：跑完一轮循环后把按钮状态还原
pub fn spawn_worker(cfg: Config) {
    std::thread::spawn(move || {
        // 线程优先级降到 BELOW_NORMAL：抓图/金字塔匹配是 CPU 密集活，
        // 用默认优先级时会把 8 个核抢满，界面线程排不上队 → 窗口看着像卡死。
        unsafe {
            crate::winapi::SetThreadPriority(crate::winapi::GetCurrentThread(), -1); // THREAD_PRIORITY_BELOW_NORMAL
        }
        // 注意：RUNNING 由调用方（on_start，UI 线程）在 spawn 之前置位，
        // 这里绝不能再 store(true)，否则会覆盖用户在这几百微秒内点的「停止」。
        let tpls = matcher::load_library(assets::TEMPLATES_BIN);
        if tpls.is_empty() {
            loge!("[error] 内嵌模板库为空或已损坏");
            gui::worker_finished();
            return;
        }
        let levels = matcher::build_levels_ex(&tpls, &cfg.scales, &cfg.skip_tpl);
        logln!(
            "[init] 模板 {} 张（内嵌 {:.1} KB），尺度 {:?}，金字塔 {} 组，阈值 {:.2}",
            tpls.len(),
            assets::TEMPLATES_BIN.len() as f64 / 1024.0,
            cfg.scales,
            levels.len(),
            cfg.threshold
        );
        let (done, hits) = click_loop(&cfg, None, &levels);
        logln!("[done] 已执行 {done} 轮，命中 {hits} 次。");
        gui::worker_finished();
    });
}

/// `--dump-dir <目录>` 指定的截图目录（程序启动时定死，之后只读）
static DUMP_DIR_OVERRIDE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

fn set_dump_dir(p: &str) {
    let _ = DUMP_DIR_OVERRIDE.set(PathBuf::from(p));
}

/// dump 限频：前 5 张全存，之后每 50 张存一张。
/// 长跑排查时一轮一帧 BMP 会把磁盘灌满（915x747 一张就 2.7MB），限频后既留了现场又不爆盘。
fn dump_should_save_shot() -> bool {
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    n <= 5 || n % 50 == 0
}

fn dump_should_save() -> bool {
    static DUMP_SEQ: AtomicU32 = AtomicU32::new(0);
    let n = DUMP_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    n <= 5 || n % 50 == 0
}

fn dump_dir() -> PathBuf {
    let mut d = match DUMP_DIR_OVERRIDE.get() {
        Some(p) => p.clone(),
        None => {
            let mut t = std::env::temp_dir();
            t.push("mojingling_dump");
            t
        }
    };
    if std::fs::create_dir_all(&d).is_err() {
        // 指定目录建不出来（权限/共享掉线），退回系统临时目录，别让排查功能哑火
        d = std::env::temp_dir();
        d.push("mojingling_dump");
        let _ = std::fs::create_dir_all(&d);
    }
    d
}

fn now_stamp() -> String {
    // 用系统时间生成时间戳（零依赖）。带毫秒：秒级精度会让同秒内的多帧互相覆盖。
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}{:03}", now.as_secs(), now.subsec_millis())
}

// ---------- 自检：无游戏环境验证匹配引擎 ----------

pub fn self_test() -> (u32, u32) {
    logln!("=== 自检模式：合成图像验证匹配引擎（不截屏、不点击鼠标）===");
    let tpls = matcher::load_library(assets::TEMPLATES_BIN);
    logln!("[lib] 内嵌模板 {} 张，{:.1} KB", tpls.len(), assets::TEMPLATES_BIN.len() as f64 / 1024.0);
    if tpls[0].w != 64 || tpls[0].h != 86 {
        loge!("[error] 模板尺寸异常: {}x{}", tpls[0].w, tpls[0].h);
        return (0, 1);
    }

    let t0 = Instant::now();
    let levels = matcher::build_levels(&tpls, &[1.0, 0.8, 1.25]);
    logln!("[lib] 金字塔 {} 组，构建 {:.0} ms", levels.len(), t0.elapsed().as_secs_f64() * 1000.0);

    let (w, h) = (1600usize, 900usize);
    let mut pass = 0;
    let mut fail = 0;

    // 用例 1：库内模板原样贴入，应 score≈1.0 且定位准确
    for (ti, pos) in [(0usize, (400usize, 700usize)), (24, (960, 780)), (47, (120, 120))] {
        let canvas = synth_canvas(w, h, 2024u64 + ti as u64);
        let canvas = blit(&canvas, w, h, pos.0, pos.1, &tpls[ti].px, tpls[ti].w, tpls[ti].h);
        let pyr = Pyramid::from_gray(w, h, canvas);
        let t1 = Instant::now();
        let m = matcher::find_best(&pyr, &levels, 2, 6, 0);
        let ms = t1.elapsed().as_secs_f64() * 1000.0;
        match m {
            Some(m) => {
                let dx = (m.x as i64 - pos.0 as i64).abs();
                let dy = (m.y as i64 - pos.1 as i64).abs();
                let ok = dx <= 3 && dy <= 3 && m.score >= 0.95;
                if ok { pass += 1 } else { fail += 1 }
                logln!(
                    "[{}] 用例{} 贴入({},{}) 命中({},{}) 偏差({dx},{dy}) score={:.3} tpl#{:02} 尺度{:.2} [{ms:.0} ms]",
                    if ok { "PASS" } else { "FAIL" }, ti, pos.0, pos.1, m.x, m.y, m.score, m.tpl, m.scale
                );
            }
            None => {
                fail += 1;
                logln!("[FAIL] 用例{} 无任何候选", ti);
            }
        }
    }

    // 用例 2：排除自身帧，用其他帧匹配（模拟游戏动画帧与模板库不同帧）
    {
        let ti = 10usize;
        let pos = (700usize, 640usize);
        let canvas = synth_canvas(w, h, 777);
        let canvas = blit(&canvas, w, h, pos.0, pos.1, &tpls[ti].px, tpls[ti].w, tpls[ti].h);
        let pyr = Pyramid::from_gray(w, h, canvas);
        let others: Vec<matcher::TplScale> = levels.iter().filter(|t| t.idx != ti).map(|t| matcher::TplScale {
            idx: t.idx, scale: t.scale, fw: t.fw, fh: t.fh, full: t.full.clone(), sumf: t.sumf, varf: t.varf,
            w4: t.w4, h4: t.h4, p4: t.p4.clone(), sum4: t.sum4, var4: t.var4,
            w8: t.w8, h8: t.h8, p8: t.p8.clone(), sum8: t.sum8, var8: t.var8,
        }).collect();
        let m = matcher::find_best(&pyr, &others, 2, 8, 0);
        match m {
            Some(m) => {
                let dx = (m.x as i64 - pos.0 as i64).abs();
                let dy = (m.y as i64 - pos.1 as i64).abs();
                let ok = dx <= 6 && dy <= 6;
                if ok { pass += 1 } else { fail += 1 }
                logln!(
                    "[{}] 跨帧匹配（排除 tpl#{ti}）命中({},{}) 偏差({dx},{dy}) score={:.3} 用 tpl#{:02}",
                    if ok { "PASS" } else { "FAIL" }, m.x, m.y, m.score, m.tpl
                );
            }
            None => { fail += 1; logln!("[FAIL] 跨帧匹配无候选"); }
        }
    }

    // 用例 3：模板放大 1.25 倍贴入，应由 1.25 尺度命中
    {
        let ti = 30usize;
        let pos = (1100usize, 200usize);
        let scale = 1.25f32;
        let nw = (tpls[ti].w as f32 * scale).round() as usize;
        let nh = (tpls[ti].h as f32 * scale).round() as usize;
        let mut big = vec![0u8; nw * nh];
        for y in 0..nh {
            let sy = (y * tpls[ti].h) / nh;
            for x in 0..nw {
                let sx = (x * tpls[ti].w) / nw;
                big[y * nw + x] = tpls[ti].px[sy * tpls[ti].w + sx];
            }
        }
        let canvas = synth_canvas(w, h, 999);
        let canvas = blit(&canvas, w, h, pos.0, pos.1, &big, nw, nh);
        let pyr = Pyramid::from_gray(w, h, canvas);
        let m = matcher::find_best(&pyr, &levels, 2, 6, 0);
        match m {
            Some(m) => {
                let dx = (m.x as i64 - pos.0 as i64).abs();
                let dy = (m.y as i64 - pos.1 as i64).abs();
                let ok = dx <= 5 && dy <= 5 && m.score >= 0.9 && (m.scale - scale).abs() < 0.01;
                if ok { pass += 1 } else { fail += 1 }
                logln!(
                    "[{}] 1.25倍缩放贴入({},{}) 命中({},{}) 偏差({dx},{dy}) score={:.3} tpl#{:02} 尺度{:.2}",
                    if ok { "PASS" } else { "FAIL" }, pos.0, pos.1, m.x, m.y, m.score, m.tpl, m.scale
                );
            }
            None => { fail += 1; logln!("[FAIL] 缩放用例无候选"); }
        }
    }

    // 用例 4：空白画布不应误报
    {
        let canvas = synth_canvas(w, h, 1);
        let pyr = Pyramid::from_gray(w, h, canvas);
        let m = matcher::find_best(&pyr, &levels, 2, 6, 0);
        let score = m.map(|x| x.score).unwrap_or(0.0);
        let ok = score < 0.7;
        if ok { pass += 1 } else { fail += 1 }
        logln!("[{}] 空白画布误报检查 score={score:.3}（应 < 0.70）", if ok { "PASS" } else { "FAIL" });
    }

    logln!("=== 自检结果：PASS {pass} / FAIL {fail} ===");
    (pass, fail)
}

/// 合成背景画布：渐变 + 确定性伪噪声（自检用，避免读外部图片）
fn synth_canvas(w: usize, h: usize, seed0: u64) -> Vec<u8> {
    let mut out = vec![0u8; w * h];
    let mut seed = seed0.wrapping_mul(6364136223846793005).wrapping_add(1);
    for y in 0..h {
        for x in 0..w {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let noise = (((seed >> 33) % 41) as i32) - 20;
            let base = 70 + (x * 50 / w) as i32 + (y * 30 / h) as i32;
            out[y * w + x] = (base + noise).clamp(0, 255) as u8;
        }
    }
    out
}

/// 把模板贴到画布指定位置
fn blit(canvas: &[u8], cw: usize, ch: usize, x: usize, y: usize, px: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out = canvas.to_vec();
    for ty in 0..h {
        let dy = y + ty;
        if dy >= ch {
            break;
        }
        for tx in 0..w {
            let dx = x + tx;
            if dx >= cw {
                break;
            }
            out[dy * cw + dx] = px[ty * w + tx];
        }
    }
    out
}
