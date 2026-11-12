# CANHarbor API 与事件契约

状态：v0.1 设计草案；完整接口仍在实施。M0 已实现 payload 的 JSON Schema、TypeScript 生成与验证见 [api-schema.md](api-schema.md)，实际范围见 [status.md](status.md)。OpenAPI 路由生成尚未实现。本文优先定义整体语义。REST 控制面与只读 WebSocket 数据面共享同一个服务核心。

## 1. 地址、版本与认证

浏览器生产入口为 `http://127.0.0.1:<实际分配端口>/`，HTTP API 前缀 `/api/v1`；UDS 承载同样的 HTTP 路由。默认端口由 OS 分配，服务经 UDS 向 CLI 提供本实例 origin；不硬编码“已知端口即可获得控制”。默认不监听 IPv6、0.0.0.0 或局域网。

HTTP 连接使用 cookie 会话、严格 Host/Origin 与 CSRF 验证，详见 [Web UI 设计](web-ui.md)。UDS 使用 peer UID 建立 native client，不允许 HTTP 请求用自报 header 模拟 native 身份。除公开静态资源、配对接口和无敏感信息的版本探针外，读取同样需要认证。

返回 `api_version`、`server_instance_id` 和必要的资源 revision。major 变更不兼容，minor 只增添可忽略的响应字段/事件类型；未知输入字段拒绝，未知响应字段忽略。未知流事件必须计数并提示兼容问题，不能伪装成已成功理解。

## 2. 路由约定

以下路由为首版目标，除标注后续者外进入 v0.1：

- `GET /api/v1/version`：公开的最小版本与构建信息；不含设备、路径或 token。
- `POST /api/v1/auth/pair`：兑换一次性配对码，设 cookie，返回 CSRF 值与 client ID。
- `GET /api/v1/auth/session`：已认证客户端取自身身份和 CSRF 值，用于页面刷新。
- `DELETE /api/v1/auth/session`：注销并撤销该 client 的 lease、关闭其 WS。
- `GET /api/v1/devices`：列出设备、channel ID、后端与支持状态。
- `GET /api/v1/channels/{id}/capabilities`：返回能力与位时序边界。
- `POST /api/v1/sessions`：按 channel 与初始配置保留硬件会话；默认 listen-only，返回 201 和 session ID，不启动通道。同一通道已有会话则 409 并提供可挂接的 ID。
- `GET /api/v1/sessions`、`GET /api/v1/sessions/{id}`：读取资源与当前状态。
- `POST /api/v1/sessions/{id}/lease`：获取独占控制租约，已占用返回 409。
- `POST /api/v1/sessions/{id}/lease/renew`：校验持有者与 generation 后续租。
- `DELETE /api/v1/sessions/{id}/lease`：释放租约并执行停止策略。
- `PATCH /api/v1/sessions/{id}/config`：仅 configured（已停止）状态、有效租约与匹配 revision 可修改。
- `POST /api/v1/sessions/{id}/start`、`POST /api/v1/sessions/{id}/stop`：有效租约下切换状态。
- `DELETE /api/v1/sessions/{id}`：有效租约下停止并释放资源，不能由观察者删除。
- `POST /api/v1/sessions/{id}/tx`：主动模式下提交单帧，返回 202 与 receipt。
- `GET /api/v1/sessions/{id}/tx/{receipt}`：查询发送阶段与已知错误。
- `GET /api/v1/sessions/{id}/stream`：升级 WS，首版只订阅单一 session。
- `POST /api/v1/captures`、`GET /api/v1/captures`：为正在运行的 session 创建服务端录制，或列出当前用户录制。
- `POST /api/v1/captures/{id}/stop`、`GET /api/v1/captures/{id}`：停止自己创建的录制，或查询状态与完整性；录制不是发送控制，不需通道 lease。
- `GET /api/v1/captures/{id}/download`：对已完成或已结束的不完整录制提供下载，响应标明完整性。
- `GET /api/v1/diagnostics`：已认证只读诊断。
- `GET /api/v1/legal`：许可证、第三方声明、构建提交和源码位置。
- v0.2：调度/主动回放任务接口；先以 ADR 定义停止与授权规则，再加入 API。

首版服务仅面向同一 OS 用户，不建立团队账户权限。浏览器每次独立配对建立 client；多个标签共享同一 cookie 身份，视为同一 client，前端仍要防止重复提交。不同 client 之间不能转移 lease；native CLI 每个进程有独立 client，退出主动释放，异常退出依 TTL 清理。

创建的 stopped 会话没有控制者时 60 秒自动回收。租约到期会话停止后保留最多 60 秒供诊断，再关闭；观察者不能无限占有未运行设备。运行中有有效服务端录制时租约失效仍按停止策略执行，并把录制标为中断。

## 3. 请求与响应

创建会话示例，所有枚举值均为 schema 定义的字符串：

```json
{
  "channel_id": "ch_01",
  "config": {
    "mode": "listen_only",
    "nominal_bitrate": 500000,
    "fd": { "enabled": true, "data_bitrate": 2000000, "iso": true }
  }
}
```

位时序由后端能力求解，返回实际 bitrate、sample point 与误差。请求无法满足时 422，不能无提示替换为“最接近”的配置；允许误差由显式策略限制。切换 Classic/FD 或 listen-only/active 前先 stop。

TX 示例（请求发往主动模式会话）：

```json
{
  "client_request_id": "req_f62e",
  "frame": {
    "kind": "fd_data",
    "id": 291,
    "extended": false,
    "brs": true,
    "data_hex": "0102030405060708090A0B0C"
  }
}
```

`id` 用 JSON 整数，最多 29 位，JS 可以精确表示；`data_hex` 为偶数个十六进制字符，无空格，服务端规范化为大写。64 位 sequence、generation、纳秒与计数器均用十进制字符串，前端需要计算时转 BigInt。禁止 JSON 内传 NaN/Infinity。

REST 控制是同一服务内部 actor 的命令。`start/stop/reconfigure` 在收到设备层结果后返回最终状态或明确超时；TX 仅承诺 receipt 已创建及命令已进入队列。队列满返回 429，未产生 receipt 时不能声称 accepted。

修改配置使用 `If-Match` 对应资源 ETag，缺失返回 428，冲突返回 412。控制 lease 获取成功后返回十进制字符串 generation；续租、释放、配置、start/stop、TX 与删除统一使用 `X-CANHarbor-Lease-Generation` header 携带，示例 TX 需同时发送该 header。服务在处理 actor 命令前再次校验，不能把 body 自报 client ID 当身份。

### TX 重试与结果不确定性

同一 client、session、generation 和 `client_request_id` 在 5 分钟缓存窗口内去重：相同 payload 返回同一 receipt；不同 payload 返回 409。receipt 存储满时拒绝新提交，不能通过提前丢去重记录悄悄缩短窗口。

该机制不保证跨服务重启或缓存窗口的 exactly-once。HTTP 丢响应、设备断开或服务重启后无法查明结果时显示 `outcome_unknown`，不得自动重发。服务不承诺持久化发送队列，也不在重启后重放队列。lease 过期时保留已有 receipt 状态，但禁止新的发送操作。

## 4. 流协议

浏览器同源认证后升级 WS。服务先发送 `hello`，内容包括 API 版本、server instance、session epoch、当前状态、队列限额与 heartbeat。客户端只可发送 `subscribe`、`unsubscribe` 和应用级 `ping`；所有控制与发送仍走 REST。

```json
{
  "type": "subscribe",
  "protocol_version": "1.0",
  "csrf_token": "<current-session-csrf-token>",
  "after_sequence": null,
  "filter": { "ids": [291], "extended": null, "direction": "rx" }
}
```

过滤器作用于该订阅者；不能修改硬件接收策略影响其他订阅者。语法错误应返回明确错误，不能退化成全量订阅。

```json
{
  "type": "events",
  "server_instance_id": "srv_01",
  "session_epoch": "ep_01",
  "events": [
    {
      "sequence": "184",
      "type": "frame_received",
      "host_monotonic_ns": "23500000000",
      "device_timestamp_us": "123456",
      "timestamp_quality": "hardware_unmapped",
      "frame": {
        "kind": "fd_data",
        "id": 291,
        "extended": false,
        "brs": true,
        "esi": false,
        "dlc": 9,
        "data_hex": "0102030405060708090A0B0C"
      }
    }
  ]
}
```

`timestamp_quality` 初始支持 `host_only`、`hardware_unmapped`、`hardware_mapped_estimate`、`uncertain`。最后一种用于回绕/复位等无法消歧的情形。RX 事件含 ESI，TX 请求不允许客户端设置不支持的 ESI。payload、DLC 与 kind 一致性由 schema 与核心构造器双重验证。

每通道 sequence 在过滤前分配，过滤导致的序号跳跃不是丢包。缓冲溢出时发送独立 `gap`，包含 cause、范围（可知时）、丢弃数与匹配过滤后的丢弃计数；物理设备报告的未知丢失量保留 unknown。gap 控制信息不能与普通数据一起无声丢弃；如果无法可靠投递 gap，关闭连接并要求重新取快照。

首版默认不保存可重放的 WS backlog。`after_sequence = null` 只接实时数据；客户端请求续传时返回 `resume_unavailable`，展示 discontinuity，再用新订阅开始。未来增加有限 backlog 也必须区分未保留与过滤排除，不能承诺无限续传。精确历史来自录制文件。

每 10 秒 heartbeat，30 秒未响应关闭连接。注销/会话过期及时关闭 WS；配对会话默认空闲 30 分钟、绝对寿命 8 小时，已认证活动可延长空闲时间但不能越过绝对寿命。重连重新读取状态；不能自动恢复 lease 或 TX。

## 5. 错误与限额

统一错误结构：`error.code`、`message`、`retryable`、`request_id`、安全的 `details`。原始 OS/USB 细节进入经清理的诊断，不泄露路径、配对码或凭证。稳定 code 至少包括 `device_busy`、`device_disconnected`、`unsupported`、`invalid_frame`、`invalid_state`、`lease_conflict`、`lease_expired`、`revision_conflict`、`queue_full`、`disk_full`、`outcome_unknown`。

使用 400 表示格式错误、401 未认证、403 权限/来源/CSRF 不符、404 不存在、409 状态或租约冲突、412 revision 不符、422 能力或参数语义不可满足、428 缺少 revision、429 资源限额、503 后端暂不可用。`retryable` 不授权自动重试有副作用的操作。

初始限额：普通 REST body 64 KiB，WS 入站 8 KiB，单帧 TX 每 client 100 请求/秒且 burst 20，最多 8 个浏览器会话、16 条 WS、8 个硬件会话和 4 个录制任务。TX actor 队列每通道 256 条、排队期限默认 1 秒；lease 失效/超期的未提交项被拒绝并更新 receipt。导入文件 v0.2 单独定义，不能复用无上限 body。

WS 出站批量和队列按总体设计执行。限额配置有硬上限与全局预算，不能只信客户端声明。上述值可在 M0/M1 测量后调整，调整须同步契约与测试。

## 6. 契约验收

后端模型 → DTO → JSON Schema/TypeScript 的生成与 snapshot diff 加入 CI；示例 JSON 必须验证。Rust 与 TS 分别覆盖超过 2^53 的序号、非法 DLC/FD/RTR 组合、未知输入字段、日期/时间单位和过滤语义。REST 的成功/拒绝场景与 WS hello/events/gap/断开都使用同一虚拟后端 fixture。

### 已实现的发送结果查询

当前发送收据、取消等待、限流和只读恢复语义见
[transmission-receipts.md](transmission-receipts.md)。除收据 UUID 查询外，
`GET /api/v1/sessions/{id}/tx?client_request_id=...&generation=...` 可按原请求
查找记录；generation 是读取过滤条件，不授予控制权。多条匹配返回 409。
本层硬件提交结果仍为 backend_submitted，未提供逐请求的固件回显确认。
