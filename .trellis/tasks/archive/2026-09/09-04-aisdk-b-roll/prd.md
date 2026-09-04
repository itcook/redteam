# B roll：aisdk 三方 SDK 替换自建 provider 收发层

## Goal

怀疑自建请求/响应处理对规范化输出（structured output）与边缘情况（finish_reason=max_tokens 等）处理不周。引入 lazy-hq/aisdk（rustls-only 已核）在 b-roll/aisdk 分支替换 AnthropicBackend/OpenAiBackend 的 HTTP/SSE 层做对照测试：同 PR181 bundle 三模型 B roll，对比 iterations/finish_reason 语义/耗时/findings。不合入 dev，结论驱动去留。

## Requirements

- TBD

## Acceptance Criteria

- [ ] TBD

## Notes

- Keep `prd.md` focused on requirements, constraints, and acceptance criteria.
- Lightweight tasks can remain PRD-only.
- For complex tasks, add `design.md` for technical design and `implement.md` for execution planning before `task.py start`.
