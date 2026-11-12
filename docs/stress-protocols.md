# CAN FD、CRC、RTT 与多主机协调

这些软件路径使用虚拟 CAN、模拟驱动和本地代理验证。本次没有向真实 CAN 设备发送帧，没有连接外部 SSH 主机。物理 FD/BRS、跨机网络故障和独立分析仪验收仍需在受控台架完成。此版本不发布物理总线占用或跨主机单向延迟的计量能力。

## 兼容性与 FD 计划

旧 Classic 计划继续使用 8 字节 run tag + u32 序号，默认新增字段不参与序列化，原计划指纹保持兼容。FD/RTT/协调任务的报告版本为 4。示例见 [fd-mixed.json](../examples/stress/fd-mixed.json)。

`traffic.format=fd_v1` 明确给出 `data_bitrate`、`stream`、非零 `epoch`、`seed` 和 `variants`。每个 variant 指定实际字节数及 BRS，按序号循环选择。长度支持 8、12、16、20、24、32、48、64，分别使用合法 FD DLC；不进行隐式填充。32/48/64 字节使用完整 FD v1，短帧使用独立的标签/序号格式，报告能力为 `mixed_crc32c_and_short_sequence_only`，不能声称所有帧经过应用 CRC。0–7 字节没有容纳标签与序号的空间，当前明确拒绝，不猜测帧身份。

每个通道预检 FD 和固定数据速率能力，配置 nominal/data bitrate 后才能获得 FD data lane。lane 不能把 Classic 配置升级成 FD，也不能跨越停止、重启、租约或服务 fence。标准/扩展 ID 均按计划固定使用。

FD v1 的固定字节序为大端：

```text
0..4    43 48 01 kind       kind=0 请求/数据，kind=1 RTT 响应
4..8    run_tag:u32
8..10   stream:u16
10..12  reserved=0
12..16  epoch:u32
16..24  sequence:u64
24..28  seed:u32
28..N-4 deterministic pattern(seed, sequence, byte offset)
N-4..N  CRC32C:u32
```

线格式携带 u64 序号，当前执行器和位图仍限制单次任务在 u32 序号空间内。CRC 输入依次是 ASCII `canharbor-fd-v1`、4 字节 CAN ID、1 字节 extended、1 字节 FD=1、1 字节 BRS、1 字节实际长度，以及排除 CRC 的全部报文数据。CRC32C 使用 reflected Castagnoli 多项式 `0x82f63b78`，初值及 xorout 都是 `0xffffffff`；实现使用固定查表，无额外依赖。黄金向量覆盖 32 字节头及 64 字节模式数据，并包含标准 `123456789 → e3069283`。

CRC 验证后还会按计划重建报文，检查 stream/epoch/seed、保留位、长度和模式数据。CRC 错误和模式错误单独计数，不计入有效唯一接收；发生错误不能通过。CRC 只用于误码检测，不是消息认证。每次新执行应更换 run_tag/epoch；probe 与 responder 共享这组新身份，CRC 不抵抗身份被复用时的重放。标签本身损坏可能使帧无法关联，此时只能通过缺失/无关流量反映，不能声称已定位物理损坏位置。

## RTT 协议与响应预算

示例为 [rtt-loopback.json](../examples/stress/rtt-loopback.json)、[rtt-probe.json](../examples/stress/rtt-probe.json)、[rtt-responder.json](../examples/stress/rtt-responder.json)。RTT 要求所有 variant 至少 32 字节、单个显式 active 接收角色，以及与请求 ID 不同的响应 ID。

- `loopback`：同一任务的接收角色是显式 responder，发起角色测量 RTT。
- `probe`：只发请求、收响应；可与另一个服务上的 responder 配对。
- `responder`：不生成主动请求，只响应符合计划的请求；达到计划计数或任务截止后结束。

每个任务仍有一个 sender 和一个 receiver 通道，独立 responder 使用 receiver 发响应、sender 作为本地已预留角色。跨主机 probe/responder 必须使用匹配的 tag、stream、epoch、seed、ID、variant、计数和速率，并实际连接同一总线。虚拟测试使用四通道共享总线和两个独立 Runtime 验证，不借用跨机时钟。

响应由独立有界队列和 worker 发出，队列容量 64，只响应首次收到的有效序号；响应 ID/kind 不会再触发响应。RTT 限制最多 1000 请求/秒，另有最多 1000 响应/秒；responder 的发送间隔受计划峰值速率约束，响应负载独立计数。窗口最多 1024、超时 10–10000 ms。窗口满、流缺口、队列溢出和未知响应提交均不能通过；不通过重试弥补。

发起端在授权提交前记录本机时间，在后端收到响应时记录本机时间，允许响应先于提交调用返回。两者之差包含主机排队、驱动、USB、CAN 和 responder 开销，不是纯线路传播时间。固定 25 桶直方图边界为 1 µs 乘 2 的幂，报告 p50/p95/p99 的桶上界、实际最大值、样本数、越界和负值计数。负差不会截成零；超时、重复、迟到/未匹配及损坏响应分开计数。

停止会同步撤销请求和响应的子许可。已进入不可撤回底层调用的响应结果不明时，记录 unknown 和响应序号；后台调用继续持有设备及执行器预留，不能启动替代任务，也不会自动重试。

## 证据与复算

FD 样本增加实际 CAN ID、extended、FD、BRS 元数据，保留原始数据，旧 Classic 样本仍可读。损坏但可关联的帧也保留，以便独立重建错误计数。逐帧采样时，最终接收位图、CRC/模式错误、重复/乱序和分阶段计数必须与重新读取的样本一致。

`rtt-NNNNNN.ndjson` 分别记录 RTT 观测和每次响应提交结论。最终报告和重启恢复会独立复算直方图与响应计数，检查重复记录、序号、超时范围和报告一致性。各类流仍使用独立的有效前缀边界，源文件不会被恢复器改写。抽样关闭时不宣称已离线重放全部接收数据。

## 多主机协调

入口是 `stress coordinate --manifest PATH --active --output EVIDENCE.ndjson`。参考 [multihost.json](../examples/stress/multihost.json)，每次新任务应换用新的 group UUID。每个 host 指定名字、runtime_dir、计划、可选 SSH 主机别名和可执行文件；本地代理必须指定可执行文件路径。普通 host 使用不同 run tag；唯一允许共享 tag 的情况是一对配置匹配的 probe/responder。

`stress agent` 是有界的单次 stdin/stdout JSON RPC，通过现有同 UID Unix socket 访问服务。远端可由 SSH 承载，不增加远程 HTTP 监听、自动发现、凭据存储或开放网络端口。控制器使用 BatchMode/连接超时，参数独立引用，SSH 目标拒绝选项注入；请求和响应限制 1 MiB，stderr 限制 64 KiB，单次 RPC 截止 8 秒，最多 8 个参与者。清单可执行程序属于显式受信任配置，应由台架操作者维护。

流程为：

1. 所有节点只读验证计划，取得本机规范指纹。
2. 所有节点 prepare 成功后才并行 commit。prepare 预留压测准入，不启动控制器或发送帧；外部手动控制仍可能导致 commit 时设备不可用，错误必须显式返回。
3. commit 由服务持有，不依赖请求连接存活。慢磁盘或调度延迟不能重置已准备的租约截止时间；请求与响应的排队执行许可也受当前协调截止时间约束，只能缩短、不能延长旧许可。计划有本地 10 秒启动保护间隔，协调心跳租约 15 秒；控制器定期并行续约并读取结果。
4. 任一响应丢失、RPC 错误、控制器截止或停止时，中止所有已知参与者，记录未确认的结果，不重复 commit。控制器进程崩溃或证据写入失败时，未续约的服务任务也会自行停止。
5. 旧租约过期不能续活，重启只恢复 interrupted 证据，不恢复协调会话或发送。原生 status 可查询历史 group。所有节点结束且各自报告通过，协调报告才通过。

服务原生端点为 `POST /api/v1/stress/coordinate`，操作包括 prepare、commit、heartbeat、abort、status。浏览器身份不能创建或控制协调会话。并发准备有独立令牌和单槽准入；重复 commit 返回已有任务，不能创建第二次执行。控制器证据不保存会话令牌。

这是有界失效的协调协议，不是全局原子发送或精确同时启动。网络分区或部分 commit 时，某些节点可能已执行；保护间隔和心跳不能证明所有物理总线都没有帧。不可撤回底层 I/O 的限制见[停止与恢复](stress-recovery.md)。报告只记录控制器本地 RPC 往返时间、各服务实例和各节点结果；这些 RPC 时间包含 SSH/进程启动，不是 CAN RTT。

时钟质量目前必须显式声明 `unsynchronized`。不提供墙钟同步假设，也不把不同主机单调时间相减；跨主机单向延迟为 unavailable/null。未来接入经过验证的硬件时间同步需要独立协议和计量验收。
