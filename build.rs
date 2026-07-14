// 文件职责：处理平台相关构建资源。
// 创建日期：2026-06-15
// 修改日期：2026-07-14
// 作者：Argus 开发团队
// 主要功能：在 Windows 目标构建时嵌入应用图标资源，供 GPUI 窗口类读取。

/// Cargo 构建脚本入口。
///
/// 参数说明：无。
/// 返回值：无；Windows 资源编译失败会中断构建，以避免打包出缺少应用图标的可执行文件。
fn main() {
    println!("cargo:rerun-if-changed=resources/windows/app.rc");
    println!("cargo:rerun-if-changed=resources/windows/app.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // 资源文件使用相对于自身目录的图标路径；显式加入资源目录后，Windows RC.EXE 与
        // 非 Windows 主机上的 llvm-rc 都能按相同规则定位图标文件。
        embed_resource::compile(
            "resources/windows/app.rc",
            embed_resource::ParamsIncludeDirs(["resources/windows"]),
        )
        .manifest_optional()
        .expect("嵌入 Windows 应用图标资源失败");
    }
}
