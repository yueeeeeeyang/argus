# 图标资源目录

文件职责：保存 Argus 应用图标源图。  
创建日期：2026-06-09  
修改日期：2026-09-24  
作者：Argus 开发团队  
主要功能：`app-icon.png` 为 1024×1024 源图，设计元素取自 Jstack 线程频率矩阵——
暗色玻璃底板上的 4×4 格子，整行绿色代表持续 RUNNABLE 的热线程，散点绿格代表偶发线程；
修改源图后运行 `scripts/generate_icons.sh` 重新生成 `resources/macos/AppIcon.icns`
与 `resources/windows/app.ico`，供各平台打包脚本使用。
