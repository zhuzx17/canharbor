# 压测主机计时与来源证据

该功能只使用虚拟 CAN、模拟原生库和本地服务验证；没有向真实适配器发送帧。报告升级为 **v7**。不改变 CAN 线格式，不增加探测帧，不新增动态库加载入口；普通 TX 限额、压测私有租约、临近 I/O 的撤销检查及未知不重试规则保持有效。

## 计划与资源边界

新增可选 `timing: { sample_every, max_pending }`，默认不启用，省略字段保持旧计划指纹兼容。sample_every 必须大于零，按 `sequence % sample_every == 0` 选择；max_pending 为 1–4096。请求和显式 RTT 响应的提交计时分开，匹配窗口共享容量上限；responder-only 不把收到的请求错配成本机发出的请求。

预检把请求/响应 Begin、Settled、RX 记录及已有原始采样合并计入最多 2000 条记录/秒的保守预算；时间匹配状态、去重位图、记录带宽、分段数和磁盘字节均纳入计划预算。过小预算在开始前拒绝。任务结束释放在线时间匹配状态和位图，只保留摘要；窗口不足仅降低计时覆盖率，不能改变完整在线帧校验，也不会补发测试帧。

新增 `topology` 可保存 bus、wiring 和选定通道的 aliases；字段长度、重复角色/别名及未知字段均检查。它们参与 plan_hash，修改必须重新预检。声明只描述人工核对的信息，不是自动电气检查、USB 拓扑认证或稳定设备序列号。

## 时间点与分布

采样提交记录相同 run 单调时钟原点的 preparation_started、prepared、dispatched、enqueued、worker_started、call_started、call_completed、completed。可用时间点必须单调且有界；未观测的字段为 null，不能填零。

- preparation：生成帧、准备许可及绑定任务授权，直到本次 prepared job 准备完成。
- dispatch_wait：prepared 到 Runtime 开始提交。
- worker_queue：native 入队调用边界到 worker 取出。入队失败或尚未取出时不产生队列时延样本；只留下缺失点和未知计数。
- submit_call：驱动 submit 操作入口到返回；包括其内部编码、守卫和同步调用。它不是硬件实际发帧时间，也不证明发生了 CAN 写入。
- completion_wait：驱动操作完成到 Runtime 提交返回前的最后观测，包含原生结果回传和必要的故障处理。
- total：本次准备开始到 Runtime 返回前观测。

PEAK/Kvaser 所用 DriverBackend、gs_usb 设备 actor 和 SLCAN serial worker 均在各自原 worker 中打点；Registry 保留观测接口的路由。虚拟或未提供该接口的自定义后端默认仅有 backend-boundary 调用计时，明确标 `worker_queue_unknown`。不会把整个 submit await 冒充原生排队时间。正常手工发送及未启用计时的压测不分配 trace；接口不能授予、续期或绕过 Permit。

固定容量直方图保存样本数、溢出、p50/p95/p99 上界和 max。队列样本数可能小于测量提交数；比较结果必须带上样本数及 scope。未知提交在截止时冻结已知时间点，迟到的后台完成不能回填成已知成功、补全此前未知分布或触发重试。

## RX 匹配、负延迟与覆盖率

仅对本流有效、首次观测的采样 RX 进行主机时间匹配；echo、无关帧、重复和非法 payload 不伪装成新匹配。原在线校验器仍独立统计所有帧及错误。

关联在提交前登记，允许 RX 先于提交返回。延迟定义为 **RX 的主机接收时间减后端 call_completed 时间**。负值保留在独立负值绝对值桶中，合并分位上界按有符号次序计算；min/max 保留符号，不截成零，也不因此判定设备时间倒退。此口径包括 USB、主机与后端处理，不能用于 CAN 线上单向延迟或跨主机时钟相减。

每 receiver 保存 sampled_rx、matched、unmatched_rx、missing_sampled_submitted。匹配覆盖率分母为采样中已知成功提交集合；另外必须同时看“采样成功提交数 / 全部成功提交数”。Web 同时展示两层分母和 sample_every，不把采样覆盖率 100% 解释成捕获了所有帧。

窗口满时淘汰最旧关联；极晚 RX 计 unmatched，不重新打开历史关联。记录 window_high_water、evicted、abandoned（淘汰/结束时尚未取得提交结论的关联）和 pending。未结束时 missing 指标包括尚待关联的项；finalized 后，每 receiver 的 sampled_rx = matched + unmatched_rx。没有匹配可计算时分位数为 unknown。

请求 RX 匹配限同一服务中该请求的 expected receivers。RTT 的 response 提交有独立准备/队列/调用分布，但不把请求接收点当作 response 的对端 RX；RTT 仍使用原有显式 responder 协议及发起端时钟。

## 来源证据

服务记录软件 package version、OS、架构，以及首次捕获时磁盘可执行文件的 SHA-256/字节数。选定 PEAK/Kvaser 库在首次显式加载时计算磁盘映像 SHA-256；缓存跟随原驻留 Library，后续通道共享该加载来源，不重新读取可能已经被替换的路径冒充原映像。

读取限制为普通文件、最多 512 MiB，检查读取长度与前后元数据，拒绝增长、截断及读取中的变化；hash helper 不跟随符号链接，并以非阻塞方式打开以防 FIFO 卡住。Library 本身仍沿用原有明确绝对路径、canonicalize 和驻留所有权规则。不能建立一致快照时 hash 为 unknown。导出不包含完整库路径、主机名或授权 token。

每通道记录实际角色 ID、device_id、backend、simulated 及库来源。固件未由压测查询，明确为 unknown；无法提供选定动态库证据的后端为空并标 unavailable/not applicable。没有自动采集传递依赖库、构建源码提交或进程内存映像，不声称文件 hash 是签名、运行内存证明或硬件认证。

来源快照在配置/开始前保存到私有 identity，且写入证据流。v7 完成报告必须与两者一致；恢复保留原启动身份来源，不以新进程的可执行文件重新替换历史来源。

## 证据、复算和兼容

`metrics-*.ndjson` 增加 Provenance 和 Timing(Begin/Settled/Received/Finish) 记录；沿用全局分段编号、私有目录、hash 与只读恢复。复算独立消费原始时间点及 RX 观测，重建窗口淘汰、直方图、分位数和覆盖率，再与最终 TimingReport 完整比较。计时里的提交 outcome 还必须与独立 submissions/replies 流的采样集合相同，不能仅相信计时记录自报成功。

拒绝重复 Begin/Settled/RX、非法角色、非采样序号、时间回退、缺少必要点及 Finish 后的新记录；坏记录不推进 reducer。已知成功必须有调用完成和 Runtime 完成点；未知调用可以保留缺失点。启用后记录器失败按原证据失败策略处理，不能保留 pass。

中断/损坏恢复返回有效前缀的计时统计及已保存来源，保留总体 measurement/counters 不完整标记；不能凭一个 Finish 记录宣称整个 run 的证据齐全。未启用计时或旧报告中缺失的指标保持 unavailable，不能推导零延迟。非默认 timing/topology 计划需要 v7，协调器在 validate 阶段拒绝旧节点。

采样的调用完成时间可用于查看样本耗时，尚不作为所有提交完成速率的完整秒窗判据；这需要另行记录完整的完成时间窗聚合。原第一批的 min_submitted_ppm 仍只是计划槽位成功提交比例。
