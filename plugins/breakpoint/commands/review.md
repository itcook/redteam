---
description: 对当前改动或指定目标做模型无关的对抗式评审（计费重操作，仅用户触发）
disable-model-invocation: true
---

运行 breakpoint 对抗式评审。把用户在命令后给出的全部参数原样转发给 `brk review`（例如目标路径、`--base <分支>`、`--mode code|doc`、`--background` 等；为空则评审当前工作树改动）：

```bash
${CLAUDE_PLUGIN_ROOT}/bin/brk review $ARGUMENTS
```

把输出的 `verdict` 与 `findings` 原样呈现给用户。这是计费重操作，未经用户要求不要自动重复调用；若用户传了 `--background`，改用 `/breakpoint:status` 与 `/breakpoint:result` 跟进。
