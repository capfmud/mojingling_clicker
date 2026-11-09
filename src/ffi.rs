//! 现成函数 FFI 层：LoadLibraryA 动态加载塔防老马助手自带原生 DLL，
//! GetProcAddress 运行时解析导出符号，不依赖任何硬编码地址。
//!
//! 签名说明：这些函数是 TFJL.exe 通过 P/Invoke 调用的现成导出（cdecl 约定）。
//! 参数布局基于导出名语义 + OpenVINO/OpenCV 惯例推断，已在注释标注；
//! 若与实际调用约定不符，只需调整对应的函数类型，无需改动主逻辑。

use crate::winapi::{FreeLibrary, GetProcAddress, HMODULE, LoadLibraryA};
use std::ffi::CString;
use std::path::Path;
use std::ptr;

/// 统一检测结果
#[derive(Debug, Clone, Copy, Default)]
pub struct Detect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub score: f32,
}

// ---------- MyOV2026.dll（OpenVINO 统一 C API）----------
// 导出名：MYOV_InitModel / MYOV_InitModelEncrypted / MYOV_PredictDetect
//         MYOV_PredictClassify / MYOV_PredictPose / MYOV_IsModelInited
type MyovInitFn = unsafe extern "C" fn(model_path: *const u8) -> i32;
type MyovInitEncFn = unsafe extern "C" fn(enc_data: *const u8, enc_len: i32) -> i32;
type MyovIsInitedFn = unsafe extern "C" fn() -> i32;
/// img: BGRA 像素首地址, w/h/ch, out_x/out_y/out_score: 输出数组；返回检测数量(<0 失败)
type MyovPredictDetectFn = unsafe extern "C" fn(
    img: *const u8,
    w: i32,
    h: i32,
    ch: i32,
    out_x: *mut f32,
    out_y: *mut f32,
    out_score: *mut f32,
) -> i32;
type MyovPredictClassifyFn =
    unsafe extern "C" fn(img: *const u8, w: i32, h: i32, ch: i32, out_score: *mut f32) -> i32;
type MyovPredictPoseFn = unsafe extern "C" fn(
    img: *const u8,
    w: i32,
    h: i32,
    ch: i32,
    out_x: *mut f32,
    out_y: *mut f32,
    out_score: *mut f32,
) -> i32;

// ---------- MyOV2025.dll / MyOV.dll ----------
// 导出名：initOVDectct + OVDectct、initOVhandcard + handcard、initOVClassify160 + classify160
type InitModelDirFn = unsafe extern "C" fn(model_dir: *const u8) -> i32;
/// 检测输出：每 5 个 f32 一组 = [x, y, w, h, score]
type DetectFn = unsafe extern "C" fn(
    img: *const u8,
    w: i32,
    h: i32,
    ch: i32,
    out: *mut f32,
    out_cap: i32,
) -> i32;

/// 已加载的现成函数集合
pub struct Engine {
    _lib: HMODULE,
    pub myov_init: Option<MyovInitFn>,
    pub myov_init_enc: Option<MyovInitEncFn>,
    pub myov_is_inited: Option<MyovIsInitedFn>,
    pub myov_predict_detect: Option<MyovPredictDetectFn>,
    pub myov_predict_classify: Option<MyovPredictClassifyFn>,
    pub myov_predict_pose: Option<MyovPredictPoseFn>,
    pub init_dectct: Option<InitModelDirFn>,
    pub dectct: Option<DetectFn>,
}

impl Drop for Engine {
    fn drop(&mut self) {
        unsafe {
            FreeLibrary(self._lib);
        }
    }
}

/// 解析导出符号为指定函数指针类型；找不到返回 None
unsafe fn symbol<T>(lib: HMODULE, name: &str) -> Option<T> {
    let c = CString::new(name).ok()?;
    let addr = GetProcAddress(lib, c.as_ptr() as *const u8);
    if addr.is_null() {
        return None;
    }
    // 函数指针大小与 usize 相同，转换是安全的（调用约定由 T 描述）
    Some(std::mem::transmute_copy::<*mut core::ffi::c_void, T>(&addr))
}

impl Engine {
    /// 加载指定 DLL 并解析全部现成符号。
    /// # Safety
    /// 加载第三方 DLL；调用方需保证来源可信（本目录为塔防老马助手自带）。
    pub unsafe fn load(dll_path: &Path) -> Result<Self, String> {
        if !dll_path.exists() {
            return Err(format!("DLL 不存在: {}", dll_path.display()));
        }
        let path_c = CString::new(dll_path.to_string_lossy().as_bytes())
            .map_err(|_| "路径含 NUL".to_string())?;
        let lib = LoadLibraryA(path_c.as_ptr() as *const u8);
        if lib.is_null() {
            return Err(format!("LoadLibraryA 失败: {}", dll_path.display()));
        }

        Ok(Self {
            _lib: lib,
            myov_init: symbol(lib, "MYOV_InitModel"),
            myov_init_enc: symbol(lib, "MYOV_InitModelEncrypted"),
            myov_is_inited: symbol(lib, "MYOV_IsModelInited"),
            myov_predict_detect: symbol(lib, "MYOV_PredictDetect"),
            myov_predict_classify: symbol(lib, "MYOV_PredictClassify"),
            myov_predict_pose: symbol(lib, "MYOV_PredictPose"),
            init_dectct: symbol(lib, "initOVDectct"),
            dectct: symbol(lib, "OVDectct"),
        })
    }

    /// 符号解析报告
    pub fn symbol_report(&self) -> Vec<(&'static str, bool)> {
        vec![
            ("MYOV_InitModel", self.myov_init.is_some()),
            ("MYOV_InitModelEncrypted", self.myov_init_enc.is_some()),
            ("MYOV_IsModelInited", self.myov_is_inited.is_some()),
            ("MYOV_PredictDetect", self.myov_predict_detect.is_some()),
            ("MYOV_PredictClassify", self.myov_predict_classify.is_some()),
            ("MYOV_PredictPose", self.myov_predict_pose.is_some()),
            ("initOVDectct", self.init_dectct.is_some()),
            ("OVDectct", self.dectct.is_some()),
        ]
    }

    /// 用现成函数对 BGRA 图像做一次检测。
    /// # Safety
    /// 依赖第三方 DLL 内部模型，输入像素缓冲区须有效且长度为 w*h*ch。
    pub unsafe fn predict(&self, img: &[u8], w: i32, h: i32, ch: i32) -> Vec<Detect> {
        let mut out = Vec::new();
        if let Some(f) = self.myov_predict_detect {
            let mut xs = [0f32; 64];
            let mut ys = [0f32; 64];
            let mut ss = [0f32; 64];
            let n = f(img.as_ptr(), w, h, ch, xs.as_mut_ptr(), ys.as_mut_ptr(), ss.as_mut_ptr());
            if n > 0 {
                for i in 0..(n as usize).min(64) {
                    out.push(Detect { x: xs[i], y: ys[i], w: 0.0, h: 0.0, score: ss[i] });
                }
            }
        } else if let Some(f) = self.dectct {
            let mut raw = [0f32; 64 * 5];
            let n = f(img.as_ptr(), w, h, ch, raw.as_mut_ptr(), 64);
            if n > 0 {
                for i in 0..(n as usize).min(64) {
                    out.push(Detect {
                        x: raw[i * 5],
                        y: raw[i * 5 + 1],
                        w: raw[i * 5 + 2],
                        h: raw[i * 5 + 3],
                        score: raw[i * 5 + 4],
                    });
                }
            }
        }
        let _ = ptr::null::<u8>(); // 保持引用以明确边界
        out
    }
}
