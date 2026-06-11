//! 文件职责：Argus 桌面客户端启动入口。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：初始化 GPUI 应用并打开界面占位主窗口。

use argus::app::ArgusApp;
use argus::assets::ArgusAssetSource;
use argus::fonts::register_argus_fonts;
use gpui::{
    App, AppContext, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, point, px,
    size,
};

/// 启动 Argus GPUI 应用并创建透明原生标题栏的主窗口。
fn main() {
    Application::new()
        .with_assets(ArgusAssetSource::new())
        .run(|cx: &mut App| {
            if let Err(error) = register_argus_fonts(cx) {
                eprintln!("Argus 内置字体注册失败：{error}");
            }

            let bounds = Bounds::centered(None, size(px(1280.0), px(820.0)), cx);

            cx.open_window(
                WindowOptions {
                    titlebar: Some(TitlebarOptions {
                        title: None,
                        appears_transparent: true,
                        traffic_light_position: Some(point(px(20.0), px(14.0))),
                    }),
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |_, cx| cx.new(|_| ArgusApp::new()),
            )
            .expect("打开 Argus 主窗口失败");

            cx.activate(true);
        });
}
