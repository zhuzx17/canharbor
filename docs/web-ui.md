# CANHarbor Web UI 设计

状态：产品与安全设计。已实现功能见[当前状态](status.md)；本文中的性能目标、目录建议和后续交互不代表已通过验收。Web 工作台与本地 Rust 服务同源发布。

## 1. 用户流程与页面

启动 `canharbor serve --open` 后打开本机页面，兑换一次性配对码；初次只显示设备与能力，不自动打开、配置或发送。用户选择通道和位率后创建会话，取得控制权再启动，默认 listen-only。

设备页显示硬件标识、后端、通道、固件、能力、占用情况和“实机验证/模拟验证/未验证”等证据状态。不能以 VID/PID 匹配就标记“完全支持”。无设备时提供虚拟总线演示，不能用模拟数据冒充实机数据。

工作台采用顶部通道/状态/控制权栏、中部虚拟帧列表或按 ID 聚合视图、右侧帧详情与单帧发送面板、底部统计与错误事件。需要保留原始顺序、时间质量、RX/TX、Classic/FD、BRS/ESI、ID 类型和 DLC。列表有明确容量，达到上限显示淘汰计数。

录制页列出运行状态、事件数、文件字节、gap、完整性和下载。诊断页显示故障、能力和版本。About 提供许可证、第三方声明与本版本源码。首版不增加账号、项目协作或云同步。

## 2. 操作语义

- “暂停显示”只暂停视图，服务仍接收并录制；持续更新接收/丢失计数。
- “停止通道”需要控制权，会影响所有观察者；按钮显示该影响。
- “开始录制”创建服务端任务，页面关闭本身不会取消任务，仍可从 CLI 或新页面管理；如果离开的页面是控制者，租约到期会停止通道并中断录制。界面必须同时说明任务与通道生命周期。
- “发送一帧”要求主动模式、有效 lease 与完整校验，不使用浏览器定时器发送。
- “回放查看”只加载历史；v0.2 的“回放发送”单独命名，提供进度、速率限制和服务端停止。
- 多页面冲突显示当前控制者类别与到期时间，不提供隐式抢占。刷新和重连后保持只读，必须重新取得控制权。

UI 不能用按钮禁用代替服务端校验。所有状态变化以服务响应/事件为准；UI 不把“点击成功”先乐观显示为“总线发送成功”。

## 3. 前端结构与性能

推荐目录为 `web/src/app/`、`features/devices/`、`features/monitor/`、`features/transmit/`、`features/captures/`、`features/diagnostics/`、`api/` 和 `workers/`。API 类型从版本化 schema 生成，领域不变量仍由 Rust 决定。

接收路径为 WS → 校验/解码 → 有界 ring buffer/聚合 → 10..20Hz UI snapshot → 虚拟列表。不要每帧触发 React setState，也不要给完整 payload 使用无界全局状态。首版显示历史上限 10000 条或 16 MiB，先达者淘汰；按 ID 聚合表也有容量和淘汰策略。

先用批量 JSON 基准验证；只有解析成为瓶颈时才移入 Web Worker。UI 能降低展示刷新率/采样，不能暗中降低服务端录制精度。十进制 u64 字符串保留原值，排序/计算使用 BigInt；显示格式化发生在渲染边界。

离线回放较大文件需增量读取和有界缓冲，不把整个文件 JSON.parse 后放入内存。首版只提供服务端录制下载，导入/回放编辑后置。产品界面优先使用“连接中”“接收中”“只读”“控制权到期”等用户可理解状态，不展示 actor 或 Rust feature 等内部实现概念。

## 4. 浏览器配对与会话

本地服务能发送 CAN 帧，不能因为绑定 loopback 就免认证。普通外部网页不能读取帧或发起控制操作。首版不支持局域网访问、反向代理或远程部署；将来另立认证、TLS 和权限 ADR。

1. 服务启动或 native CLI 请求 `web open` 时，生成至少 256 位随机、60 秒有效、单次使用配对码。码只存在内存，限量、限速验证。
2. 使用 `http://127.0.0.1:<port>/#pair=<code>` 打开页面。fragment 不发送给 HTTP 服务；页面立即在内存取出并用 history.replaceState 清除。不得将其写入 localStorage、日志、遥测或 query string。
3. 页面向同源 `POST /api/v1/auth/pair` 发送码。服务严格校验实际 Host/Origin、一次性码和有效期，然后设 HttpOnly、SameSite=Strict、Path=/ 的 session cookie，返回独立随机 CSRF 值。
4. 所有有副作用 HTTP 请求必须使用 JSON content type、自定义 CSRF header 和精确 Origin。拒绝缺失/`null`/非白名单 Origin，不通过“字符串包含 localhost”判断。公开静态页面可通过正常导航访问，不能因此得到设备数据。
5. 会话 cookie 不设置 Domain。默认本地 HTTP 不宣称具备 TLS 或 Secure cookie 保护；cookie 名不用要求 Secure 的 `__Host-` 前缀。若未来启用 HTTPS，必须启用 Secure，并重新评估入口。威胁边界不包括同用户恶意本地进程或恶意浏览器扩展。

cookie 对端口不隔离：服务为每个实例生成独立 cookie 名，只接受本实例随机会话值；不能声称这能防御控制同一主机的恶意本地进程。不同服务实例的配对码、cookie、CSRF、lease 都不通用。

WS upgrade 也必须验证 cookie 与精确 Origin，不能只靠 CORS。首条 subscribe 消息额外携带当前 CSRF 值，5 秒内未验证即关闭；认证完成前不发送设备状态或帧。URL 不承载 bearer token。注销和会话到期撤销 WS，不允许已有长连接无限延长权限。

## 5. 网络与内容防护

Host 只接受服务实际绑定的 `127.0.0.1:<port>`，拒绝攻击者域名的 DNS rebinding。Origin 同样精确匹配生产 origin；默认不返回跨源允许头。读接口不依赖 Origin 必然存在，但始终需要 cookie，带恶意 Origin 的读取请求也拒绝。

响应使用 `Cache-Control: no-store`（API/配对）、`Referrer-Policy: no-referrer`、`X-Content-Type-Options: nosniff`、禁止 framing 的策略。CSP 限制资源为自身，`object-src 'none'`、`base-uri 'none'`、`frame-ancestors 'none'`；connect-src 允许自身及实际绑定的 WS origin，不用通配。脚本不使用 inline/eval，正式模式不启用 permessage-deflate，限制消息大小和连接数。

ID 标签、设备字符串、文件名、未来 DBC 注释均以文本渲染，不使用未清理 HTML。下载通过 capture ID 解析，拒绝路径穿越与任意本机路径读取。受信任本地用户仍可按文件权限直接读录制，Web 安全策略不能改变 OS 权限。

开发模式由 Vite 同源代理 API/WS，并且后端仅在显式 `--dev-origin` 时接受具体开发 Origin；必须同时在 Host 规则中配置代理实际 Host。绑定仍为 loopback，认证/CSRF 不关闭，配对入口使用显式开发 origin。发行构建拒绝启用该模式。

不注册 Service Worker，避免旧页面后台继续持有数据或缓存过期 API。静态文件使用内容哈希，index 不长期缓存。服务/UI build ID 不一致时停止控制并提示刷新。

## 6. 验收与界面原型范围

M0 使用真实虚拟后端完成设备 → 会话 → 监视 → 单帧发送的纵向原型。该原型只证明用户流程，不计作 gs_usb 实机支持。M1 用同一页面换成真实后端，不改变领域语义。

Playwright 覆盖配对成功/过期/重复、刷新、共享标签、控制冲突、listen-only 拒发、WS断线、慢客户端、gap 和录制下载。恶意 Origin、缺失 CSRF、Host rebinding、未认证 WS、超大请求与路径穿越用服务集成测试验证。需在实际支持的 macOS 浏览器上验收，不能把 Chromium 自动化结果当作 Safari 兼容结论。

初始性能目标：虚拟源 10000 events/s 连续 10 分钟，10000 行显示上限下 UI 可交互，操作响应 p95 < 200ms。此为指定基准机、版本和负载下待测目标；同时记录 CPU、进程内存、浏览器内存与各层丢弃数，不能把模拟吞吐等同实机 CAN 吞吐。

参考：[OWASP WebSocket Security Cheat Sheet](https://github.com/OWASP/CheatSheetSeries/blob/master/cheatsheets/WebSocket_Security_Cheat_Sheet.md)。本地 HTTP 是明确受限部署模型；远程访问必须使用另外设计的 TLS 与访问控制。

## Error recovery

The workbench retains the service error message and adds a next step for expired
pairing, unavailable service, control/configuration conflicts and invalid input.
“Check connection and session status” reads the current browser authentication,
devices and sessions. It never redeems an old pairing link, acquires control,
starts a channel, or repeats a failed submission. A failed pairing attempt needs
a fresh `canharbor web open` link. Uncertain operations must be reconciled using
session state or submission history before a user submits another operation.
Nominal and FD data bitrate inputs report invalid whole-number ranges inline.

Recovery guidance uses the operation context and service error code. Access
rejection (403) is distinguished from missing authentication (401), and expired
control is distinguished from a busy device. Recording and stress panels use
the same recovery presentation; their refresh actions only read current lists
and reports. Refreshing status never repeats a mutation or infers nonexecution
from an absent result.

Workbench drafts retain only the selected view, session identifier, display
filters and frame input. Restored selections are checked against current
service sessions; control, credentials and transmission are never restored.
Changing session or channel while this page holds a lease requires closing the
controlled session first. A new subscription resumes the display with a fresh,
fixed elapsed-time origin. The origin does not move when old rows leave the
500-frame buffer. Frame fields are validated against the selected session's
CAN FD configuration before either single or periodic submission.

## Observing state and releasing control

A transient list-query failure does not revoke control or stop renewal. The
workbench labels stale status with the last successful query and renewal times.
A stream disconnect does not establish that the channel stopped; same-session
reconnect preserves displayed history and marks the unavailable interval.
Status checks are read-only and preserve the existing monitor and pause state.
Authentication is read again only if the page has no authenticated identity.
A failed renewal with an unknown outcome blocks sending and pauses further
renewal attempts until current ownership is established or expires.

“Stop and release control” stops acquisition and retains the session until idle
cleanup. Stopping affects all observers and ends active recordings; “Close” also
removes the session. Neither operation resumes other work.

Configuration inputs are drafts. The applied values are shown separately.
External configuration changes preserve an edited draft and require explicit
review before applying it. Loading current values discards the draft; keeping a
draft against current values does not apply it. Saving remains a separate action.

## Task navigation and display windows

The live, recordings, offline and stress views have explicit URL fragments and
matching navigation state. Offline inspection and stress planning do not display
the live channel configuration form. A running channel collapses configuration;
Show channel settings reveals it without changing the session. Connection/control
query timestamps are available under Connection and control details, automatically
expanded when status is uncertain.

Recent frames retains 500 frames. By ID retains at most 2048 groups keyed by ID,
identifier width, frame kind and direction, preserving counts and latest payload
until the group is evicted. Both use exact ID filters with optional `0x`. Counts
exclude paused updates; display evictions, paused skips and group evictions are
shown separately from stream gaps. Clear resets the visible frame/group history;
received and loss counters remain session totals. Neither view replaces recording.

Analyze recording downloads a stopped service capture with a 64 MiB bound and
opens it directly in the read-only offline viewer. Existing local files can be up
to 1 GiB. Offline filtering explicitly uses ID substring matching; a time selector
switches between monotonic seconds and raw nanoseconds without rounding u64 values.
