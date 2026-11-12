# 压测判据

新增可选 `criteria`；省略或全默认时序列化仍省略该字段，保留旧计划指纹。所有数值均为整数，未知字段拒绝，开始后不可修改；改变判据必须重新验证并得到新 plan_hash，同一请求 ID 不允许换计划。

- `min_submitted_ppm` 默认 `1000000`，范围 `1..1000000`。每个编译阶段（包括 warmup）和整个 run 均以 `submitted / scheduled` 检查。阶段的固定时长、fps 和序号范围在编译时冻结，检查的是各阶段计划槽位中成功提交的比例。它不是按完成时间计算的实际 fps，也不是任意滑动秒窗速率；真正完成速率的时间窗门槛依赖调用/完成计时。此项不把 arming/drain 计入分母。
- `max_scheduler_skipped` 默认 `0`，范围为 u32。要容许跳过，必须同时显式降低提交率要求，并考虑计划完整性缺失门槛；不会自动放宽其他条件。
- `max_missing_expected_ppm`、`max_missing_submitted_ppm` 默认 `0`，范围 `0..999999`。分别以 planned 和 known-submitted 为分母，每个 receiver 独立检查，不能跨 receiver 平均。零已知提交时后者只允许零缺失；工作负载要求仍然不能通过。
- `max_duplicates`、`max_reordered`、`max_payload_errors`、`max_out_of_range` 默认 `0`，范围为 u32，每个 receiver 独立检查。payload 错误为 CRC 与 pattern 错误之和；降级的短帧/Classic sequence 不因此获得独立 CRC 能力。错误帧不计作 unique，若缺失也超标则另报。
- `max_dispatch_lateness_ns` 默认 `null`（不增加迟到硬门槛），允许 `0..259200000000000`。它测量主机量化调度的 dispatch lateness，不是 CAN 线上的发送延迟或 worker 排队时间。Web 使用整数毫秒输入，API/CLI 支持整数纳秒。

比例通过 u128 交叉乘积比较，不先舍入 ppm，不把 1/3 误当成 333333 ppm。计数总和及阶段形状检查不因容差而放宽。评估成本只与最多 64 个编译阶段、7 个 receiver 有关，不在逐帧路径上评估判据。

非默认判据要求报告版本至少为 **6**，保存可复算的 `assessment`：`passed`、`workload_met`（提交比例及跳过预算）和有界失败项（指标、接收通道/阶段、实测计数、上限、比例分母）。它只是工作负载/质量判据的结果；**criteria met 不等于 run pass**。通过还要求 completed、完整测量与计数、原身份/停止规则、无未知提交、无已知提交错误、无 bus-off/RX overflow、完整证据和 RTT/内存监控等既有硬约束。用户停止、证据缺口、服务重启不得通过容差转成 pass。

最终 Metric 新增累计 `max_dispatch_lateness_ns`；复算校验它与最终报告一致并检查指标不回退。提交比例和阶段统计继续由独立提交区间复算。采样范围及证明边界没有扩大；没有完整 RX 样本时，不宣称离线重新捕获了全部帧。

中断/损坏恢复清除最终 assessment 和 workload_achieved，只保留下界与已有明确违规；不以恢复前的“评估通过”冒充恢复后的完整证据。旧版本报告仍按默认严格口径读取；非默认 criteria 必须使用 v6，协调器在 validate 阶段拒绝较旧节点，避免带着未知判据进入 prepare/commit。

CLI 沿用 JSON 计划，无新增发送命令。多主机按各参与者冻结的 criteria 重新计算终态，仍要求 plan/hash/服务实例/提交证据边界一致。Web 参数变化立即使预检失效，结果显示实际冻结配置及失败项。

示例见 [criteria.json](../examples/stress/criteria.json)。示例只表示显式允许的测试误差，不推荐作为所有适配器的统一生产验收标准；默认仍为严格零容差。

