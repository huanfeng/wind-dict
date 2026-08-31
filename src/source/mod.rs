//! 查询源的实现：词典与译源。
//!
//! 术语表把查询源分为**两类且仅两类**：
//!
//! - **词典**（[`crate::domain::Dictionary`]）——查出词条，有出处，查不到就是查不到。
//! - **译源**（[`crate::domain::TranslationSource`]）——产出译文或 AI 生成文本，无出处。
//!
//! 二者不统一抽象（docs/adr/0008）：可信度不同，数据形状也不同。译源尚未实现。

pub mod offline;
pub mod user;
