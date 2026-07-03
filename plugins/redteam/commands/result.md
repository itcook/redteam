---
description: 取回 redteam 评审作业的结果
---

取回 redteam 评审结果（参数为 job id，省略则取最近一次；原样转发给 `redteam result`）：

```bash
${CLAUDE_PLUGIN_ROOT}/bin/redteam result $ARGUMENTS
```

把 `verdict` 与 `findings` 原样呈现给用户。
