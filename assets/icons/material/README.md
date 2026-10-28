# Material Icon Theme 子集

- 来源：<https://github.com/material-extensions/vscode-material-icon-theme>
- 版本：5.38.1（与 BaoCode Fast IDE 使用同一版本）
- 许可：MIT，版权与许可全文见 [LICENSE](LICENSE)
- 用途：Argus 来源树节点图标

## 运行时格式

GPUI 的 `svg()` 渲染为单色蒙版，无法保留 Material 图标的多色填充，因此运行时使用
64×64 透明 PNG，通过 `include_bytes!` 编译期嵌入，由 `img()` 渲染。SVG 仅作为可回溯的源素材。

重新生成：

```sh
scripts/generate_material_icons.sh
```

脚本需要 `rsvg-convert`（macOS 可执行 `brew install librsvg`）。

## 图标范围

目录统一使用第一版的棕色文件夹：

| 本地文件 | 上游图标 | 用途 |
|----------|----------|------|
| `folder-base.png` / `folder-base-open.png` | `folder-base` / `folder-base-open` | 所有目录（收起/展开） |
| `file.png` | `file` | 未知文件 |

文件类型：`log`、`zip`、`settings`、`xml`、`json`、`yaml`、`toml`、`document`、
`markdown`、`console`、`jar`、`table`、`database`、`lock`。

文件名：`docker`、`makefile`、`git`、`license`、`readme`、`maven`、`go-mod`。

连接树协议：`console`（SSH）、`database`（SMB）、`git`（Git）、`changelog`（SVN）。

解析规则与 BaoCode 一致：精确文件名 → 压缩包/日志特判 → 最长扩展名 → 默认 `file`；
目录只区分收起/展开，统一使用棕色文件夹。
