//! 文件职责：提供时间展示格式化工具。
//! 创建日期：2026-06-11
//! 修改日期：2026-06-11
//! 作者：Argus 开发团队
//! 主要功能：把文件系统时间戳转换为界面中稳定可读的日期时间文本。

use std::time::SystemTime;

use chrono::{DateTime, Local};

/// 将文件修改时间格式化为选择器列表使用的日期时间。
///
/// 参数说明：
/// - `time`：来自文件系统元数据的系统时间。
///
/// 返回值：`YYYY-MM-DD HH:mm` 格式的本地时间文本。
pub fn format_modified_time(time: SystemTime) -> String {
    let datetime = DateTime::<Local>::from(time);
    datetime.format("%Y-%m-%d %H:%M").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    /// 验证修改时间格式固定，便于选择器列宽稳定。
    #[test]
    fn formats_system_time_for_file_picker() {
        let time = UNIX_EPOCH + Duration::from_secs(1_704_067_200);
        let formatted = format_modified_time(time);

        assert_eq!(formatted.len(), 16);
        assert_eq!(&formatted[4..5], "-");
        assert_eq!(&formatted[7..8], "-");
        assert_eq!(&formatted[10..11], " ");
        assert_eq!(&formatted[13..14], ":");
    }
}
