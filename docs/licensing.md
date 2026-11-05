# CANHarbor 许可与发布设计

CANHarbor 自有代码、Web UI 与新增文档采用 AGPL-3.0-or-later。仓库现有 LICENSE 为 AGPLv3 正文；新增 README 明确选择 or-later，实施时 Cargo/前端包元数据与文件 SPDX 标识需保持一致。第三方文件继续保留原许可，不能只因加入仓库就改写其授权。

## 1. Web UI 与源码交付

Web UI 固定提供 About / Licenses / Source 入口；`GET /api/v1/legal` 返回后端与前端的实际 build ID、commit、dirty 标记、许可和对应源码位置。不能只链接默认分支首页并假设满足所有版本的相应源码要求。

release 归档覆盖实际二进制与 UI 的源码、锁文件、构建/安装脚本、补丁和必要配置。使用在线相应源码交付路径时，在二进制下载处提供清晰且等效的源码入口，并按适用条款维持可用性。UI 的压缩 JS 不替代可修改的 TS/TSX 源码和构建材料。

对支持远程网络交互的修改版本，按 AGPL 第13条向交互用户显著提供获取该版本 Corresponding Source 的机会。本项目即使默认仅本地访问，也统一提供源码入口以简化发行；这不等于声称一切内部运行都必须向全世界公开。

本地 dirty 构建不伪称对应官方 tag。修改者应配置实际源码位置或本机可提供的源码归档；legal 页面明确报告缺失，不靠“官方源码”链接掩盖本地修改。源码归档服务只提供指定构建的完整归档，不能变成浏览任意本机文件的接口。

## 2. 已研究的代码准入方向

- `sorrowfeng/gsusb-canfd`：根 MIT，协议、时序、C ABI 与测试参考。固定提交 `6785cc82d66da93cb3ff5af66a6b3498f1118c66`；引入时核对每个实际片段的来源。
- `mac-can/MacCAN-Core`：BSD-2-Clause OR GPL-3.0-or-later，优先选 BSD。固定提交 `d08c9e40dc1608a976e557a7a371b00a8f0f5b65`。
- `cliffrubio/macOS-CAN-Drivers`：PeakUSB 为 GPL-2.0-or-later，可选 GPLv3 路径与 AGPLv3 模块组合，保留 GPL 部分；KvaserUSB 选 BSD-3-Clause 分支。固定提交 `d2d06333ce988916c971c25d4707afb25fde76ae`。还需核对实际使用的厂商原始来源。
- `OpenCyphal/libcanard`：MIT，可在 M3 按需引入；前期未固定提交，使用前完成固定与逐文件审查。
- `OpenCANMonitor` 和 `canDrive`：主要借鉴交互与分析流程；根 MIT 不覆盖所有捆绑二进制、固件或第三方资产。
- `PCBUSB-Utilities`：工具源、独立二进制与底层闭源库分别授权。默认不依赖、不打包闭源 libPCBUSB。
- `awesome-canbus`：列表 CC0，仅作发现入口，不代表被链接资源统一授权。

Linux `drivers/net/can/usb/gs_usb.c` 当前文件头为 GPL-2.0-only，不能直接翻译并入 AGPL 程序。协议事实与独立实现、复制受版权保护的实现是不同问题；Rust 翻译不抹除上游版权。实现前优先使用许可清楚的参考与自制协议向量。

以上是研究与准入方向，不构成对任意版本或整个上游仓库的合规背书。实际导入文件及固定版本以 provenance 清单和 THIRD_PARTY_NOTICES 为准；上游变更或导入文件不同，必须重新核查。

## 3. 依赖和资产

计划使用 nusb（MIT OR Apache-2.0）、Tokio/Axum、React/Vite 等；最终按锁定版本和实际 feature 生成依赖清单，声明要覆盖传递依赖、构建工具和最终分发资产。若引入 libusb，单独核对 LGPL 与打包/链接要求。

建立 `LICENSES/`、`THIRD_PARTY_NOTICES` 与 provenance 清单，记录源仓库、完整 SHA、原路径、哈希、原许可、双许可选用分支、修改及发布关联。使用 cargo-deny/cargo-audit 和前端依赖扫描辅助检查；这些工具不替代 C/C++、字体、图标、DBC、固件与示例流量审查。

不默认打包厂商 DBC、受限制诊断数据、真实车辆流量或未知来源截图。测试流量与 golden vectors 优先自制并记录来源。

## 4. 社区与商业使用

AGPL 允许商业使用、收费分发和服务；不添加“禁止商用”或“商业用途必须付费”条款。覆盖作品的源码、声明和适用安装材料义务按具体行为判断。进程隔离、动态链接或 HTTP 调用都不能自动作为 copyleft 豁免。

贡献建议采用 DCO 与 inbound=outbound，记录贡献者的权利声明。未来若提供闭源商业例外，需拥有相关权利或取得授权，不能单方面把外来的 GPL/AGPL 代码商业化改许可。商标与官方发行身份另设政策，不影响用户的许可证权利。

本文件是工程合规设计，不能保证没有第三方专利、版权争议或所有司法辖区的风险。商业发行前审查实际产物与依赖，必要时由专业人员复核。

## 5. 发布验收

- 源码归档对应实际后端/UI 构建，包含构建材料且可按说明重建。
- CLI 与 About 可查看许可和来源；所有借入文件的声明保留。
- SBOM 与实际 release feature、静态资源和本地 dylib 一致。
- release 下载处与运行界面的源码入口可访问，符合选定交付路径。
- 未把闭源组件、无许可资产或不兼容实现混入发行物。

依据：[仓库许可证](../LICENSE)、[GNU 许可 FAQ](https://www.gnu.org/licenses/gpl-faq.en.html#AGPLGPL)、[OSI 开源定义](https://opensource.org/osd)。
