# 字体资源目录

文件职责：说明 Argus 内置字体资源目录的使用方式。  
创建日期：2026-06-09  
修改日期：2026-06-11  
作者：Argus 开发团队  
主要功能：存放经授权后可随程序分发的界面字体与日志阅读字体文件。

## Microsoft YaHei Mono

程序界面和来源树使用 `Microsoft YaHei Mono` 字体族。

如需真正内置该字体，请将具备分发授权的字体文件放置为：

```text
assets/fonts/MicrosoftYaHeiMono.ttf
```

启动时 `src/fonts.rs` 会读取该文件并注册到 GPUI 文本系统。若文件不存在，程序仍会声明使用
`Microsoft YaHei Mono` 字体族，系统已安装同名字体时可直接命中，否则由平台字体回退机制兜底。

## JetBrains Mono

日志正文显示使用 `JetBrains Mono` 字体族，并由仓库内置的常规字重文件提供：

```text
assets/fonts/JetBrainsMono-Regular.ttf
```

该字体用于日志行渲染和文本选择命中计算，确保显示宽度与拖选复制位置保持一致。
