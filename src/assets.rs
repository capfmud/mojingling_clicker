//! 内嵌资源：魔精灵模板库
//!
//! `templates.bin` 在**编译期**经 `include_bytes!` 打进 exe 的只读数据段，
//! 因此运行时不依赖任何外部文件（图片、模型、配置），交付物就是单个 exe。

pub static TEMPLATES_BIN: &[u8] = include_bytes!("../assets/templates.bin");
