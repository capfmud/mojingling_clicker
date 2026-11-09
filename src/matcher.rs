//! 纯 Rust 多尺度模板匹配引擎（零外部依赖，全部手写）
//!
//! 三级搜索（step 见 `find_best`）：
//!   ① 1/8 图全图粗筛（步长 2）
//!   ② 1/4 图重排 + 精搜（±5 格）：1/8 层的 NCC 判别力偏弱，必须先重排再取 top-k
//!   ③ 原图精定位（±6 px）
//!
//! 相似度用 NCC（归一化互相关）：对整体亮度/对比度线性变化不敏感，
//! 因此「模板比截图亮/暗」也能稳定命中。
//!
//! 之所以不用 OpenCV/DLL，是因为目标交付物是**单个 exe**：
//! 模板像素在编译期用 `include_bytes!` 打包进可执行文件，运行时零外部文件。

// ---------- 模板库（assets/templates.bin 的解析结果） ----------

pub struct RawTemplate {
    pub idx: usize,
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

/// 解析内嵌模板库：`[u32 count]` + count × (`u16 w`,`u16 h`,`w*h` 灰度)
pub fn load_library(bin: &[u8]) -> Vec<RawTemplate> {
    if bin.len() < 4 {
        return Vec::new();
    }
    let n = u32::from_le_bytes([bin[0], bin[1], bin[2], bin[3]]) as usize;
    let mut off = 4usize;
    let mut out = Vec::with_capacity(n);
    for idx in 0..n {
        if off + 4 > bin.len() {
            break;
        }
        let w = u16::from_le_bytes([bin[off], bin[off + 1]]) as usize;
        let h = u16::from_le_bytes([bin[off + 2], bin[off + 3]]) as usize;
        off += 4;
        let need = w * h;
        if w == 0 || h == 0 || off + need > bin.len() {
            break;
        }
        out.push(RawTemplate { idx, w, h, px: bin[off..off + need].to_vec() });
        off += need;
    }
    out
}

// ---------- 图像金字塔 ----------

pub struct Pyramid {
    pub w: usize,
    pub h: usize,
    pub full: Vec<u8>,
    pub w4: usize,
    pub h4: usize,
    pub p4: Vec<u8>,
    pub w8: usize,
    pub h8: usize,
    pub p8: Vec<u8>,
}

impl Pyramid {
    /// 由 BGRA 截图构造灰度金字塔（BGR 加权亮度，整数近似 BT.601）
    pub fn from_bgra(w: usize, h: usize, bgra: &[u8]) -> Self {
        let mut full = vec![0u8; w * h];
        for i in 0..(w * h) {
            let b = bgra[i * 4] as u32;
            let g = bgra[i * 4 + 1] as u32;
            let r = bgra[i * 4 + 2] as u32;
            full[i] = ((r * 77 + g * 150 + b * 29) >> 8) as u8;
        }
        Self::from_gray(w, h, full)
    }

    pub fn from_gray(w: usize, h: usize, full: Vec<u8>) -> Self {
        // 1/2 层只作为降采样中间量，匹配不使用（1/8 粗筛 → 1/4 重排 → 原图精定位）
        let (w2, h2, p2) = down2(&full, w, h);
        let (w4, h4, p4) = down2(&p2, w2, h2);
        let (w8, h8, p8) = down2(&p4, w4, h4);
        Self { w, h, full, w4, h4, p4, w8, h8, p8 }
    }
}

fn down2(src: &[u8], w: usize, h: usize) -> (usize, usize, Vec<u8>) {
    let nw = w / 2;
    let nh = h / 2;
    if nw == 0 || nh == 0 {
        return (0, 0, Vec::new());
    }
    let mut out = vec![0u8; nw * nh];
    for y in 0..nh {
        let r0 = (y * 2) * w;
        let r1 = r0 + w;
        for x in 0..nw {
            let i = x * 2;
            let s = src[r0 + i] as u32
                + src[r0 + i + 1] as u32
                + src[r1 + i] as u32
                + src[r1 + i + 1] as u32;
            out[y * nw + x] = (s >> 2) as u8;
        }
    }
    (nw, nh, out)
}

// ---------- 模板（某个缩放尺度下的金字塔 + 统计量） ----------

pub struct TplScale {
    pub idx: usize,
    pub scale: f32,
    pub fw: usize,
    pub fh: usize,
    pub full: Vec<u8>,
    pub sumf: f64,
    pub varf: f64,
    pub w4: usize,
    pub h4: usize,
    pub p4: Vec<u8>,
    pub sum4: f64,
    pub var4: f64,
    pub w8: usize,
    pub h8: usize,
    pub p8: Vec<u8>,
    pub sum8: f64,
    pub var8: f64,
}

/// 返回 (ΣT, Σ(T-T̄)²)
fn stats(px: &[u8]) -> (f64, f64) {
    let n = px.len() as f64;
    let mut s = 0f64;
    let mut s2 = 0f64;
    for &v in px {
        let f = v as f64;
        s += f;
        s2 += f * f;
    }
    (s, s2 - s * s / n)
}

fn resize_nearest(px: &[u8], w: usize, h: usize, nw: usize, nh: usize) -> Vec<u8> {
    let mut out = vec![0u8; nw * nh];
    for y in 0..nh {
        let sy = (y * h) / nh;
        for x in 0..nw {
            let sx = (x * w) / nw;
            out[y * nw + x] = px[sy * w + sx];
        }
    }
    out
}

pub fn build_scale(t: &RawTemplate, scale: f32) -> Option<TplScale> {
    let fw = (t.w as f32 * scale).round() as usize;
    let fh = (t.h as f32 * scale).round() as usize;
    if fw < 8 || fh < 8 {
        return None;
    }
    let full = if (scale - 1.0).abs() < 1e-3 {
        t.px.clone()
    } else {
        resize_nearest(&t.px, t.w, t.h, fw, fh)
    };
    let (sumf, varf) = stats(&full);
    let (w2, h2, p2) = down2(&full, fw, fh);
    let (w4, h4, p4) = down2(&p2, w2, h2);
    let (sum4, var4) = stats(&p4);
    let (w8, h8, p8) = down2(&p4, w4, h4);
    let (sum8, var8) = stats(&p8);
    Some(TplScale {
        idx: t.idx,
        scale,
        fw,
        fh,
        full,
        sumf,
        varf,
        w4,
        h4,
        p4,
        sum4,
        var4,
        w8,
        h8,
        p8,
        sum8,
        var8,
    })
}

// ---------- 核心：NCC ----------

#[inline]
fn ncc(
    scr: &[u8],
    sw: usize,
    x: usize,
    y: usize,
    tpl: &[u8],
    tw: usize,
    th: usize,
    sum_t: f64,
    var_t: f64,
) -> f64 {
    let mut s: u32 = 0;
    let mut s2: u32 = 0;
    let mut dot: u32 = 0;
    let mut i = y * sw + x;
    let mut k = 0usize;
    for _ in 0..th {
        let mut ii = i;
        for tx in 0..tw {
            let v = scr[ii] as u32;
            s += v;
            s2 += v * v;
            dot += v * (tpl[k + tx] as u32);
            ii += 1;
        }
        i += sw;
        k += tw;
    }
    let n = (tw * th) as f64;
    let mean = s as f64 / n;
    let var = s2 as f64 - s as f64 * mean;
    if var <= 1e-9 || var_t <= 1e-9 {
        return 0.0;
    }
    (dot as f64 - sum_t * mean) / (var.sqrt() * var_t.sqrt())
}

// ---------- 候选与结果 ----------

#[derive(Clone, Copy, Default)]
pub struct Cand {
    pub score: f64,
    pub x8: usize,
    pub y8: usize,
    pub tpl: usize,
    pub scale: f32,
}

pub struct Match {
    pub score: f64,
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
    pub tpl: usize,
    pub scale: f32,
}

/// 在 1/8 图上粗筛单个模板
fn coarse(img: &Pyramid, t: &TplScale, step: usize) -> Option<Cand> {
    let tw = t.w8;
    let th = t.h8;
    if tw == 0 || th == 0 || tw > img.w8 || th > img.h8 {
        return None;
    }
    let max_x = img.w8 - tw;
    let max_y = img.h8 - th;
    let mut best = f64::NEG_INFINITY;
    let (mut bx, mut by) = (0usize, 0usize);
    let step = step.max(1);
    let mut y = 0usize;
    while y <= max_y {
        let mut x = 0usize;
        while x <= max_x {
            let s = ncc(&img.p8, img.w8, x, y, &t.p8, tw, th, t.sum8, t.var8);
            if s > best {
                best = s;
                bx = x;
                by = y;
            }
            x += step;
        }
        y += step;
    }
    Some(Cand { score: best, x8: bx, y8: by, tpl: t.idx, scale: t.scale })
}

/// L1 精修：在 1/4 图上以粗筛坐标（×2）为中心 ±5 格内搜索。
/// 返回 (x4, y4, score4)。这一步同时用于「给候选重新排序」和「给 L2 定中心」。
///
/// 搜索基准坐标保持不可变，命中坐标单独记录，否则中心会随命中漂移。
fn refine_l1(img: &Pyramid, t: &TplScale, x8: usize, y8: usize) -> (usize, usize, f64) {
    let mut best = f64::NEG_INFINITY;
    let mut bx = x8 * 2;
    let mut by = y8 * 2;
    if t.w4 == 0 || t.h4 == 0 || t.w4 > img.w4 || t.h4 > img.h4 {
        return (bx, by, best);
    }
    let max_x = img.w4 - t.w4;
    let max_y = img.h4 - t.h4;
    let cx = bx.min(max_x);
    let cy = by.min(max_y);
    for dy in -5i32..=5 {
        let y = cy as i32 + dy;
        if y < 0 || y as usize > max_y {
            continue;
        }
        for dx in -5i32..=5 {
            let x = cx as i32 + dx;
            if x < 0 || x as usize > max_x {
                continue;
            }
            let sc = ncc(&img.p4, img.w4, x as usize, y as usize, &t.p4, t.w4, t.h4, t.sum4, t.var4);
            if sc > best {
                best = sc;
                bx = x as usize;
                by = y as usize;
            }
        }
    }
    (bx, by, best)
}

/// L2 精修：在原图上以 1/4 图坐标（×4）为中心 ±6 px 内搜索。
/// 返回 (x, y, score)（原图像素坐标）。
fn refine_l2(img: &Pyramid, t: &TplScale, x4: usize, y4: usize, fallback: f64) -> (usize, usize, f64) {
    let mut fx = x4 * 4;
    let mut fy = y4 * 4;
    if t.fw == 0 || t.fh == 0 || t.fw > img.w || t.fh > img.h {
        return (fx, fy, fallback);
    }
    let max_x = img.w - t.fw;
    let max_y = img.h - t.fh;
    let cx = fx.min(max_x);
    let cy = fy.min(max_y);
    let mut best = f64::NEG_INFINITY;
    for dy in -6i32..=6 {
        let y = cy as i32 + dy;
        if y < 0 || y as usize > max_y {
            continue;
        }
        for dx in -6i32..=6 {
            let x = cx as i32 + dx;
            if x < 0 || x as usize > max_x {
                continue;
            }
            let sc = ncc(&img.full, img.w, x as usize, y as usize, &t.full, t.fw, t.fh, t.sumf, t.varf);
            if sc > best {
                best = sc;
                fx = x as usize;
                fy = y as usize;
            }
        }
    }
    (fx, fy, if best.is_finite() { best } else { fallback })
}

/// 全图搜索：多尺度 + 多模板，返回最高分命中
///
/// - `step`：L0 粗筛步长（1/8 图像素，默认 2）
/// - `topk`：进入精修的候选数（默认 6）
/// - `threads`：并行线程数（0 = 自动）
pub fn find_best(
    img: &Pyramid,
    levels: &[TplScale],
    step: usize,
    topk: usize,
    threads: usize,
) -> Option<Match> {
    if levels.is_empty() || img.w8 == 0 || img.h8 == 0 {
        return None;
    }
    let nth = if threads == 0 {
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)
    } else {
        threads
    }
    .clamp(1, 32);

    // 粗筛：按模板分块并行
    let chunk = (levels.len() + nth - 1) / nth;
    let mut cands: Vec<Cand> = Vec::new();
    if chunk == 0 {
        return None;
    }
    std::thread::scope(|s| {
        let handles: Vec<_> = levels
            .chunks(chunk)
            .map(|part| {
                s.spawn(move || {
                    let mut local: Vec<Cand> = Vec::with_capacity(part.len());
                    for t in part {
                        if let Some(c) = coarse(img, t, step) {
                            local.push(c);
                        }
                    }
                    local
                })
            })
            .collect();
        for h in handles {
            match h.join() {
                Ok(mut v) => cands.append(&mut v),
                Err(_) => {}
            }
        }
    });

    if cands.is_empty() {
        return None;
    }

    // 二级打分重排：1/8 图的 NCC 判别力偏弱（模板只有 8x10），
    // 直接用它的分数排序会把正确答案挤出 top-k。这里先把每个候选在 1/4 图上
    // 精搜一遍（±5 格），用 1/4 的分数重排，再取 top-k 进原图精定位。
    let find_tpl = |idx: usize, scale: f32| -> Option<&TplScale> {
        levels.iter().find(|t| t.idx == idx && (t.scale - scale).abs() < 1e-3)
    };
    for c in cands.iter_mut() {
        if let Some(t) = find_tpl(c.tpl, c.scale) {
            let (x4, y4, s4) = refine_l1(img, t, c.x8, c.y8);
            c.x8 = x4;
            c.y8 = y4;
            if s4.is_finite() {
                c.score = s4;
            }
        }
    }
    cands.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    cands.truncate(topk.max(1));

    // 原图精定位：取 NCC 最高者
    let mut best: Option<Match> = None;
    for c in &cands {
        let Some(t) = find_tpl(c.tpl, c.scale) else { continue };
        let (x, y, sc) = refine_l2(img, t, c.x8, c.y8, c.score);
        let m = Match { score: sc, x, y, w: t.fw, h: t.fh, tpl: t.idx, scale: t.scale };
        let better = match &best {
            None => true,
            Some(b) => m.score > b.score,
        };
        if better {
            best = Some(m);
        }
    }
    best
}

/// 构造所有模板 × 所有尺度的匹配层
pub fn build_levels(tpls: &[RawTemplate], scales: &[f32]) -> Vec<TplScale> {
    build_levels_ex(tpls, scales, &[])
}

/// 同上，但可以剔除指定编号（0-based 下标）的模板，让它们彻底不参与匹配
pub fn build_levels_ex(tpls: &[RawTemplate], scales: &[f32], skip: &[usize]) -> Vec<TplScale> {
    let mut out = Vec::with_capacity(tpls.len() * scales.len());
    for (i, t) in tpls.iter().enumerate() {
        if skip.contains(&i) {
            continue;
        }
        for &s in scales {
            if let Some(lv) = build_scale(t, s) {
                out.push(lv);
            }
        }
    }
    out
}

/// 8bit 灰度 BMP 导出（排查用，手写文件头避免引入图像库）
/// BGRA 帧 -> 32 位彩色 BMP（排查用，保留颜色便于肉眼定位）
pub fn bgra_to_bmp32(px: &[u8], w: usize, h: usize) -> Vec<u8> {
    let row = w * 4;
    let data_len = row * h;
    let file_len = 54 + data_len;
    let mut out = Vec::with_capacity(file_len);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(file_len as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(h as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(data_len as u32).to_le_bytes());
    out.extend_from_slice(&2835i32.to_le_bytes());
    out.extend_from_slice(&2835i32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    for y in (0..h).rev() {
        out.extend_from_slice(&px[y * row..(y + 1) * row]);
    }
    out
}

pub fn gray_to_bmp(px: &[u8], w: usize, h: usize) -> Vec<u8> {
    let row = (w + 3) / 4 * 4;
    let data_len = row * h;
    let file_len = 54 + 1024 + data_len;
    let mut out = Vec::with_capacity(file_len);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(file_len as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(54u32 + 1024).to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(h as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&8u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(data_len as u32).to_le_bytes());
    out.extend_from_slice(&2835i32.to_le_bytes());
    out.extend_from_slice(&2835i32.to_le_bytes());
    out.extend_from_slice(&256u32.to_le_bytes());
    out.extend_from_slice(&256u32.to_le_bytes());
    for i in 0..256u32 {
        out.extend_from_slice(&[i as u8, i as u8, i as u8, 0]);
    }
    let pad = row - w;
    for y in (0..h).rev() {
        out.extend_from_slice(&px[y * w..(y + 1) * w]);
        out.extend(std::iter::repeat(0u8).take(pad));
    }
    out
}
