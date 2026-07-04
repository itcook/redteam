---
name: result
description: 取回 redteam 评审作业的结果
---

取回 redteam 评审结果（参数为**必填** job id；原样转发给 `redteam result`）：

```bash
${CODEX_PLUGIN_ROOT}/bin/redteam result $ARGUMENTS
```

把 `verdict` 与 `findings` 原样呈现给用户。
