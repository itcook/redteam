---
name: result
description: 取回 breakpoint 评审作业的结果
---

取回 breakpoint 评审结果（参数为 job id，省略则取最近一次；原样转发给 `brk result`）：

```bash
${CODEX_PLUGIN_ROOT}/bin/brk-darwin-arm64 result $ARGUMENTS
```

把 `verdict` 与 `findings` 原样呈现给用户。
