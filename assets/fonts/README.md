# 字体资源目录

文件职责：说明 Argus 内置字体资源目录的使用方式。  
创建日期：2026-06-09  
修改日期：2026-10-29
作者：Argus 开发团队  
主要功能：存放经授权后可随程序分发的界面字体与日志阅读字体文件。

## 界面字体

目录树、连接树、欢迎页、页签、设置、弹窗及分析表格的界面文字统一使用
`src/fonts.rs` 中的 `ARGUS_UI_FONT_FAMILY`，与 VS Code 工作台的字体选择对齐：

- macOS：通过 GPUI 的 `.SystemUIFont` 使用原生系统 UI 字体（SF Pro），中文由系统回退到苹方等字体。
- Windows：使用 `Segoe UI`，中文由系统字体回退补齐。
- 其他平台：使用 GPUI 的默认界面字体入口；当前 Linux 实现优先 `IBM Plex Sans`，缺失时回退，尚不保证与桌面配置的字体一致。

字体由平台提供，不需要下载或随安装包分发。改变的是字体族，原字号和行高保持原值；
文字宽度可能不同，显示和文本选择测量都使用同一字体常量。更新后重启生效，无需配置迁移；
回滚此字体选择并重新构建即可恢复原显示。

VS Code 字体来源：[工作台样式](https://github.com/microsoft/vscode/blob/main/src/vs/workbench/browser/media/style.css)。

## JetBrains Mono

日志正文、SQL/堆栈原文、终端内容及代码片段使用 `JetBrains Mono` 字体族，保留等宽对齐，
并由仓库内置的常规字重文件提供：

```text
assets/fonts/JetBrainsMono-Regular.ttf
```

`src/fonts.rs` 通过 `include_bytes!` 将该文件编译进可执行程序，启动时直接注册内嵌字节。
安装包不依赖构建机源码目录、运行目录或外部字体文件。字体注册失败会明确报错。
该字体用于日志行渲染和文本选择命中计算，确保显示宽度与拖选复制位置保持一致。
