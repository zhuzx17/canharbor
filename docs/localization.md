# Localization

The workbench embeds English and Simplified Chinese resources. The language
selector offers Auto, English and 简体中文. Auto follows the browser's first
supported language; unsupported languages fall back to English. Resource loading
requires no external network request.

The current origin's explicit choice is cached in localStorage. A paired browser
also saves the default through `PUT /api/v1/preferences`, using the existing
Origin, cookie and CSRF checks. The preference is injected into the next HTML
bootstrap so it survives a service restart with a different HTTP port. Existing
pages are not changed by another page saving a default.

The service stores `preferences.json` under `CANHARBOR_CONFIG_DIR`, or under
`~/Library/Application Support/CANHarbor` on macOS and
`$XDG_CONFIG_HOME/canharbor` (otherwise `~/.config/canharbor`) on Linux. Writes
require a private, user-owned directory and atomically replace the settings file.
A failed save leaves the current page's language usable and reports the failure.
Preferences do not contain pairing tokens, device configuration or control state.

Language updates must not remount the workbench, acquire or renew a lease,
resubmit a frame, restart a task, change a stress-plan hash or reinterpret an
unknown submission. Wire identifiers, JSON keys, HEX data and raw evidence stay
language independent. CSS selectors must not depend on translated labels.

Implementation proceeds in separate changes: foundation and preferences, core
connection/monitor/transmission views, recording/offline/task/stress views,
structured error presentation, CLI and launch instructions, then acceptance.
The browser views and CLI presentation use embedded catalogs. Raw technical
details and user data retain their original representation.

Run `bun run --cwd web i18n:check` to check both catalogs and locale selection.
Browser tests cover switching, pairing continuity, reload and a new service port.

The core connection, channel configuration, live monitor, frame submission,
hardware guidance and operation recovery views use translated resources. Notices
retain message keys and interpolation values so an existing notice changes
language with the page. Known API failures have translated summaries; raw service
details remain available separately. Number and time formatting use the selected
UI locale. Recording, offline analysis, tasks and stress testing use their own namespaces.

Core browser regressions switch an active virtual session between both languages
and verify that its configuration, WebSocket, received rows and invalid frame
draft survive. Switching does not pair again or submit a frame. An unpaired
browser can also translate existing recovery guidance.

Recording, offline analysis and periodic tasks now use both catalogs. Recording
names and file content remain user data. Offline validation keeps the error object
so an existing error follows language changes. Task admission and recovery notices
retain message keys. Tests check preserved drafts, raw frame columns, offline
filters and task results, with one admission across both languages.

Stress-plan controls, flow editing, verification summaries and charts use the
selected language. Frozen plans, hashes, JSON evidence and technical identifiers
retain their original representation. The bilingual regression checks one
validation and one admission, unchanged plan/hash/authorization across switching,
and continuity of an active virtual run.

## Command line and packages

`canharbor --lang en --help` and `canharbor --lang zh-CN --help` select the
help language. The global option also works after a subcommand. `auto` (the
default) uses an explicit saved UI preference, then the first nonempty `LC_ALL`,
`LC_MESSAGES` or `LANG`; unsupported environment languages use English. CLI
selection is read-only and does not change the saved browser default.

Help descriptions, service readiness, device presentation and task/sequence/stress
progress use the selected language. Chinese errors include a summary based on the
existing exit classification and retain the original diagnostic as technical
detail. Clap's original validation diagnostic, protocol codes and identifiers are
retained. An unknown outcome remains exit 7 and must be queried using the original
request identifiers; selecting a language never authorizes transmission.

JSON on stdout, JSON RPC, NDJSON, evidence, exported plans, URLs, command names,
flags and exit codes are language independent. Package `START.txt` and the launcher
message include English and Chinese instructions. Embedded catalogs require no
translation service or network access.

## 简体中文使用说明

启动软件后，在顶部“语言”选择“自动”、English 或“简体中文”。切换即时生效，
不会重新配对、重连接收流、重启任务或发送帧。已配对浏览器会保存默认偏好，
服务重启并更换 HTTP 端口后仍可使用。保存失败时，当前页面仍可切换，并显示提示。

命令行示例：

```sh
./canharbor --lang zh-CN --help
./canharbor send --lang zh-CN --help
./canharbor --lang en devices --json
```

录制名称、用户输入、CAN ID、HEX 数据与技术详情保留原样。离线文件只在浏览器内
检查；切换语言不会改写文件。JSON 输出不会翻译。发送结果未知时，请保留原始
会话、请求标识及代次，先查询原结果，不要自动重发。

Acceptance checks cover unavailable localStorage, failed preference writes, Auto
selection, a narrow Chinese viewport and embedded-only resource loading. The
Package verifier checks both help languages, invariant device JSON and authenticated
preference updates reflected in HTML, in addition to archive/source checks,
launcher pairing, virtual exchange and clean shutdown on each native platform.
