# Classic pattern 与检查点

该功能新增 `traffic.format = "classic_pattern_v1"`，使用独立 CAN ID 的检查点关联 8 字节模式数据；保留原 Classic sequence-v1 的线格式、默认字段省略规则及计划指纹。没有向真实 CAN 设备发送帧。

## 冻结的线协议及预算

每对物理槽位为 **checkpoint、data**。物理槽位 `2k` 使用 `checkpoint_can_id`，8 字节为大端 `u32 run_tag + u32 k`；槽位 `2k+1` 使用原 `can_id`，8 字节全部由 seed、stream、run_tag 和 k 确定生成。数据帧本身不携带可被当作已知序号的 tag/sequence 头。两种 ID 使用相同标准/扩展位宽。

模式生成冻结为：`x = u64(k) XOR ((u64(seed)<<32) OR run_tag) XOR (u64(stream)<<48)`；依次执行 `x=(x XOR (x>>30))*0xbf58476d1ce4e5b9`、`x=(x XOR (x>>27))*0x94d049bb133111eb`、`x=x XOR (x>>31)`，乘法按 u64 回绕，输出大端 8 字节。同一流的变换是双射，避免不同 k 的模式碰撞；接收端仍必须使用收到的检查点，不反推序号。黄金向量：seed=42、stream=7、run_tag=17、k=0，数据为 `C3E5DC0949AADF8A`，检查点为 `0000001100000000`。

**count、fps 和阶梯速率均指物理帧，包含控制帧**，不是数据帧数/数据 fps。固定 count、各编译阶段 count 和 fps 必须为偶数，数据与控制各占一半；不会额外偷偷发送控制帧。例如 count=100、fps=100 表示 50 个检查点及 50 帧模式数据。提交集合、阶段判据、跳过槽位、最后尝试序号及计时都使用物理槽位，序号不回绕。检查点丢失/未知不自动重试，重试不会被隐藏在负载外。

`checkpoint_can_id` 必须合法且不同于数据 ID；预检、多流及多主机协调将其作为实际生产者检查 ID 冲突，数据和控制分别分配一半峰值预算。两个发送 lane 共用同一 acquisition、设备预留、Runtime 全局准入与父/子发送门，停止规则相同。

`max_pending` 范围 1–4096，分别限制每 receiver 保留的检查点映射和待关联数据；编译时额外预留每 receiver 每窗口条目 256 字节。保持精确提交/RX 位图和原有全局内存、证据、分段及吞吐上限。

该模式要求 `evidence.sample_every=1`，保存所选数据/控制 ID 的完整接收 data-frame 证据，避免离线复算靠猜测补齐检查点上下文。受现有最多 2,000 records/s 采样预算限制；单 receiver 无额外计时时最多 2,000 物理 fps，多个 receiver、计时及多流需要共同分配该预算。容量不足在开始前拒绝；运行中证据失败关闭发送门。它不宣称全总线捕获、应用 CRC、认证或达到任何真实线速。Classic pattern 暂不与 RTT 组合；FD CRC/RTT 和原 Classic sequence 保持支持。

示例见 [classic-pattern.json](../examples/stress/classic-pattern.json)。

## 有界关联、丢失和歧义

接收端只接受本流 ID/位宽，忽略 TX echo/RTR。只有收到匹配 run_tag 的检查点后，才将某段数据与该检查点生成的完整 8 字节模式精确比较；唯一匹配后记为该物理 data 槽位的已验证接收。数据先到可在窗口内等待控制帧，使用原始接收时间作主机计时，不伪造为关联完成时刻。

检查点缓存按首次接收顺序淘汰；重复控制帧计重复，但不会刷新已淘汰窗口。待关联数据超限或最终排空时退休，计 `unverified_data`；后续控制帧不能回写已退休数据。无法匹配、且存在多个可能控制上下文时，同时计 `ambiguous_data`，不挑选序号、不把它计作唯一接收或某个猜测序号的 payload error。非法 FD/长度包络单独计 `invalid_envelopes`。同一 ID 上不能识别的外来数据也无法被当成已验证数据。

报告保存 `checkpoints_unique`、`data_verified_unique`、`missing_checkpoints`、`unverified_data`、`ambiguous_data`、`invalid_envelopes`、窗口高水位/待关联数/淘汰数及 finalized 状态。原 receiver.unique 是已确认控制槽位与已验证数据槽位之和，duplicates/reordered 以确认关联的槽位顺序统计；它不是硬件分析仪测量的总线重排。缺失检查点、未验证数据、歧义或非法包络均为硬失败，不能用普通接收丢失容差改成 pass。单纯数据丢失继续按冻结的接收质量判据处理；这些比例判据的分母同样是物理帧，数据/控制分项另列，默认仍要求零丢失。

## 报告、复算、协调和 Web

包含 pattern 的报告版本为 **9**，其他计划继续输出 v8。`classic_pattern_transmit` 分别保存数据与控制的 submitted/skipped/failed/unknown；父多流报告保留物理总数，分项在各子流中。运行中的分项为最近快照，终态与完整提交区间逐项核对。

新增 `PatternFrame` 原始证据，保存 ID/位宽/FD/BRS、payload 和主机接收时间，不接受记录中预设的数据序号。复算器用同一冻结协议、独立关联状态从原始接收顺序重建关联、退休、歧义和位图，并与最终 receiver、逐阶段集合及分项发送计数比对。修改计数、移除检查点或更改原始帧不能保持 verified。多流中记录继续使用流命名空间，必须依赖父证据验证。

完整证据重启后只读复算，损坏/中断仅恢复已证实前缀及发送分项下界，不打开通道、不补发检查点或数据。多主机 prepare 前检查控制 ID 冲突，并要求 pattern 节点支持 v9；不把 v8 节点的未知能力当支持。

Web 提供 Classic pattern-v1 选择、控制 ID、窗口设置、强制完整原始证据提示及各 receiver 的关联结果；添加反向流自动选择独立数据和控制 ID。配置变化使预检失效，开始仍需显式发送授权。终态及刷新恢复展示原冻结配置，逐流/整组停止沿用既有授权隔离。
