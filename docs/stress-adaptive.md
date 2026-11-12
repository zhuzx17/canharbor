# 自适应压测与负载形状

该功能实现单 Runtime、单 scheduled stream 的自适应加压，以及可用于普通单流/多流的有界 burst、固定 seed 间隔。选择规则、候选值、判据、负载形状、最大资源预算在开始前冻结；没有向真实 CAN 设备发送帧。

## 自适应计划

新增可选 `adaptive`：`rates` 为 1–8 个严格递增的候选物理 fps，`exploration_seconds` 为每个候选的固定探索窗口，`measurement_seconds` 为固定测量窗口，`min_observed_ppm` 为显式声明的实际主机完成速率门槛（1–1000000）。要求 `count=fps=0`、无 stages/additional flows/RTT；原有固定、阶梯、多流及 FD/RTT 继续支持。各阶段复用 parent 的冻结 criteria、traffic、shape、角色和公共配置。

从最低候选开始逐级探索；候选满足冻结的接收/工作负载判据、硬完整性规则及实际速率门槛后，选择它并继续下一个候选。遇到第一个**完整捕获但不合格**的候选，结束探索，选用此前最后合格的候选；若没有合格值则结束失败，不启动测量。全部合格时选择最高候选。不会生成列表之外的速率或修改判据。

随后创建独立的 `measurement` 步骤，以选定 fps 和预声明时长固定运行；测量失败只报告失败，**不会降速重测、重新搜索或补发**。探索步骤的失败保留为失败；整次 adaptive 的 pass 只表示选定配置的固定测量通过，不表示所有探索速率或所有探索帧都通过。

停止、未知提交、已知提交错误、租约/epoch 失效、采集/证据缺口及 RSS 故障均中止整次程序，不作为可以回退的普通容量结果。每个子步骤使用独立发送门，继承父门及父协调截止；子步骤超时不能延长父授权截止。设备集合只原子预留一次，每唯一通道获取一次 acquisition，整个程序共享 Runtime 全局准入槽；未知后台 I/O 结清前保留原设备集合和槽。没有通过启动多个 Executor 规避这些限制。

子步骤名称为 `explore-0` … `explore-N` 和 `measurement`。第 i 个探索步骤使用 `run_tag + i + 1`，measurement 使用 `run_tag + rates.len() + 1`，u32 按回绕运算，最多九个值互不相同。Classic pattern 的 seed 另按 `root_seed XOR (index+1)` 派生，保持不同阶段 payload 域互不重叠，避免迟到数据污染固定测量；间隔调度 seed 则保留用户声明值。所有实际派生值写入步骤计划并由复算器重新推导。

## 实际负载和窗口

有负载形状或属于 adaptive 的 scheduled step 使用完整的冻结活动窗口；即使最后一帧较早提交，也等待窗口结束，再进入 drain。`load` 保存父 run 单调时钟下的 scheduled start、实际 end、声明窗口长度、是否完整经历窗口、known submitted、skipped 和实际完成 fps（千分位）。shaped 固定计数计划的声明窗口按 count/fps 精确到纳秒计算，不额外补成整秒空闲；窗口不包含正常 drain/cleanup；中断窗口单独标记，启动门之前停止允许零活动时长，不伪造一次完整测量。

实际 fps 为 `known backend submitted / actual active-window elapsed`，不是名义 fps、计划槽位数或物理 CAN ACK。`min_observed_ppm` 使用原始整数计数和纳秒交叉相乘比较，不根据显示值的三位小数判断。质量合格且所有计划提交完成，仍可能因实际窗口超时、主机完成速率不足而不合格。窗口事件、完整提交区间和生命周期边界必须一致。

## 负载形状

`shape` 省略或显式 `uniform` 保持旧计划指纹和旧调度行为。新增：

- `burst { frames }`：每组最多 frames 个连续物理槽位使用同一目标时刻，最后一组可以不足额；组间隔为 frames/fps，至少 10 ms。frames 实际上受 20,000 fps 峰值上限限制为 1–200，不能通过低平均速率安排无限突发。预留峰值为 frames×100，不能把峰值当平均发送率。
- `seeded_intervals { seed, jitter_ppm }`：每个名义槽位增加 `0..period*jitter_ppm/1000000` 的确定性偏移，jitter 范围 1–500000 ppm；相邻理想间隔因此有界且顺序不回退。按 stage-local ordinal 使用固定 u32 hash：`x=seed XOR ordinal*0x9e3779b9`，依次 `x^=x>>16`、乘 `0x7feb352d`、`x^=x>>15`、乘 `0x846ca68b`、`x^=x>>16`，乘法回绕；偏移为 x 对最大偏移加一取模。预留每量化窗口最多 `ceil(fps/100)+1` 帧的保守峰值。

理想 deadline 再按既有 **10 ms 主机量化**调度；高 fps 下 seed 间隔可能落入同一量化批次，不能宣称微秒级线间隔。已经迟到的槽位跳过，不追赶或重试；相同计划和 seed 得到相同理想 deadline/槽位归属，主机实际完成时刻仍受调度和后端影响。

所有计数、速率、burst 大小均是物理帧，包含 Classic checkpoint；该功能 adaptive 与 shaped scheduled stream 均不组合 RTT，避免把异步回复包装成满足 burst/间隔的同一负载；原有 uniform RTT 继续支持。多流可给各流独立 shape，仍合并检查峰值、量化批次、内存、采样和磁盘预算。

## 预算、证据与恢复

预检新增 `load_budget`：所有候选探索加上最大候选的测量帧数、最大持续时间、预留物理峰值、最大量化批次及证据估算。顺序执行使用最大单步骤跟踪内存，已完成步骤释放逐帧状态，只保留有界报告；证据容量按全部可能步骤保守累加。最多 72 小时、既有 256 MiB/20,000 fps/256 batch/磁盘/采样限制继续生效，adaptive 分段至少 256 KiB，并检查报告/来源 metadata 上限。

相关计划输出 v10；旧普通计划和 Classic pattern 计划维持原版本。`adaptive.steps` 保存各步骤冻结计划、接收统计、提交/跳过结果、实际窗口、判据评估、是否合格、选定值、下一步和稳定的选择原因；最终 `planned` 是实际执行路径帧数，`reserved_planned` 是开始前授权的最大上界，两者不混用。

`FlowRecord` / `FlowEnd` 对各步骤命名空间封装，新增 `LoadWindow` 及 `AdaptiveDecision`。独立复算逐步骤检查完整提交集合及声明范围内 RX/计时证据，再从冻结规则计算预期选择。未有已验证步骤结论和选择记录，不能进入下一个候选或 measurement；修改 fps、窗口、判据、实际负载、选择原因、顺序或最终汇总会失去 verified。完成步骤的精确位图在验证后释放，复算不把所有候选逐帧状态同时保留。

RX 范围继续显式声明：关闭采样时质量复算基于完整在线聚合，不能声称离线重放全部 RX；`sample_every=1` 才支持完整匹配 RX 重放，并受 2,000 records/s 及证据容量限制。Classic pattern 继续要求完整 raw 证据。主机完成负载和选择规则可以由提交/窗口记录重算，不发布分析仪测得占用率，也不将估计标为测量值。

服务重启只读恢复已证实前缀；没有完整决策时不推测下一步，不继续探索或测量，也不重新配置或打开通道。损坏结果清除 pass 和 workload 声明，保留选择/步骤下界及原始证据。

协调器要求参与新模式的节点支持 v10，预先检查所有派生步骤 tag 及生产 ID 冲突。现有 prepare/commit、心跳、丢响应不重试和整组停止规则继续生效；各节点独立探索和选定本地固定测量配置，不宣称跨主机锁步搜索、统一总线负载平台或同步一程计时。

## 使用与验证

Web 新增 adaptive 参数、负载形状、burst/seed/jitter 设置与预留预算，任何参数变化撤销预检。结果显示每步输入、实际主机负载、判据与选择原因，区分探索失败和固定测量失败，刷新只查询。JSON 计划可由现有 CLI `stress validate/run` 使用，无新增绕过授权的发送入口。

示例：[adaptive.json](../examples/stress/adaptive.json)、[burst.json](../examples/stress/burst.json)、[seeded-intervals.json](../examples/stress/seeded-intervals.json)。
