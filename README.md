## Introduce

本仓库用于为 Tgoskits 适配 v4l2 子系统，以满足在 StarryOS 上使用摄像头、视频采集、视频编码等需求。

由于 StarryOS 目前的 USB 协议栈对于流式采集/实时采集的支持不够完善，因此我在一定程度上重构了上游的 DWC2 USB 主机后端实现，以支持 UVC 摄像头的实时采集。

**主线合并进度：**
- **DWC2 USB 主机后端重构**：https://github.com/rcore-os/tgoskits/pull/2066

## Directory

目前涉及的主要目录和文件：

- `os/StarryOS/kernel/src/pseudofs/dev/video.rs`: v4l2 设备在文件系统中的接口委托实现
- `os/StarryOS/kernel/src/pseudofs/dev/uvc_camera.rs`: 用于为 UVC 驱动提供 USB Handle，防止驱动依赖内核、内核依赖驱动，这样的依赖循环
- `drivers/media/v4l2-core`: v4l2 驱动框架，包含 ioctl 接口层、vb2 缓冲区管理子系统
- `drivers/media/uvc`: UVC 摄像头驱动实现，主要参考 `drivers/usb/usb-device/uvc`
- `drivers/media/vivid`: vivid 测试驱动实现，主要参考 `linux/drivers/media/test-drivers`
- `drivers/usb/usb-host/src/backend/kmod/dwc2`: DWC2 USB 主机后端实现，主要参考 `linux/drivers/usb/host/dwc2`

包括 2 个内核与驱动间的胶水模块和 2 个驱动模块 1 个 v4l2 核心和整个 dwc2 后端的重构，代码量统计如下：

```sh
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
 Language              Files        Lines         Code     Comments       Blanks
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
 TOML                      4           64           57            1            6
─────────────────────────────────────────────────────────────────────────────────
 Rust                     76        19936        17099          704         2133
 |- Markdown              74         1604            0         1487          117
 (Total)                            21540        17099         2191         2250
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
 Total                    80        21604        17156         2192         2256
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

## Milestone

- [✅] 初步完成 v4l2 驱动框架，从 Linux 迁移 ioctl 接口层，部分实现 vivid 测试驱动，并适配到 StarryOS `/dev` 中
- [✅] 初步完成 vb2 缓冲区管理子系统，将内存管理机制从驱动层委托到 vb2 中
- [✅] 完成基础的用户态测试程序，包括 ffmpeg、v4l2-ctl 和自己实现的综合测试程序
- [✅] 以 sg200x-bsp 作为后端、v4l2 作为 ioctl 前端实现 UVC 摄像头驱动
- [✅] 利用 JPU 将 JPEG 格式转换为 YUYV 格式以加速 YUYV 输出
- [✅] 以 sg200x-bsp 作为 USB endpoint 后端，CrabUSB 作为 USB 控制器，v4l2 作为 ioctl 前端实现 UVC 摄像头驱动
- [✅] 删除 sg200x-bsp 依赖，仅使用 CrabUSB 协议栈作为后端，v4l2 作为 ioctl 前端实现 UVC 摄像头驱动
- [✅] 支持 USB isochronous 传输在 endpoint 上的多 transaction 传输（mult>1）
- [✅] 支持摄像头的原生 YUYV 格式输出
- [✅] 重构并优化当前的 vb2 缓冲区实现，以支持 DMA buffer 的零拷贝传输
- [✅] 重构 DWC2 USB 主机后端实现，以支持 DDMA 传输
- [✅] 重构 vb2 实现，基于缓冲区来驱动图像采样
- [✅] 完善 uvc 驱动对 v4l2 框架的适配，通过 `v4l2-ctl` 工具的测试，确保摄像头的采集、编码、输出等功能正常
- [✅] 稳定 v4l2-core 驱动框架，完善文件句柄和事件机制，保证多线程安全 
- [✅] DWC2 重构主线合并
- [  ] uvc 驱动通过 `v4l2-compliance` 全量测试
- [  ] 完善 v4l2 相关文档及其说明

## 上游修改

这部分中，我完成了对 DWC2 USB 主机后端的重构，重构前状态：所有模块位于 `mod.rs` 中，利用常量与寄存器偏移量来实现对寄存器的访问，模块划分不显著，且不支持 Isochronous 传输，也没有 DDMA 支持，无法满足实时采集的需求。

### DWC2 USB 主机后端重构

这部分的核心工作如下：

1. 将 `mod.rs` 中的所有模块拆分为独立的模块文件，便于维护和扩展。
2. 添加 Descriptor DMA 支持，以处理 Isochronous 传输下 125us 的硬性传输间隔要求。破坏性更新，考虑到只有最古老的 DWC2 的版本（2.90a）不支持 DDMA，可以考虑不  Buffer DMA 回退路径。
  - DWC2 的 DDMA 类似与网卡的 DMA，传输数据时需要将 Descriptor 写入到 Descriptor DMA 中，DWC2 会根据 Descriptor 的内容来进行数据传输。
  - 在我的实现中，我设置了 128 大小的 DMA 描述符表，每次处理 64 个，将中断间隔从 125us 扩展到 125us * 64 = 8ms，避免了频繁中断
3. 添加了 Isochronous 传输的状态机

### usbfs API 修改

我尽可能做了最小化修改，仅仅将 `submit_endpoint_transfer` 接口暴露了出来，但是这部分依旧需要与上游讨论，`SubmittedTransfer` 是 `usbfs::manager` 的对象，因此，如果内核外部驱动需要使用，则必须做依赖反转。

```rust
pub(crate) fn submit_endpoint_transfer(
    &self,
    endpoint: u8,
    request: TransferRequest,
) -> StarryResult<SubmittedTransfer> {
    self.lease.submit_endpoint_transfer(endpoint, request)
}
```

## 图像采集

### 传输层：利用 DDMA + TransferRequest Pipeline 将数据抽象为“流”

在 USB 的四种传输策略中，Control、Bulk、Interrupt 都可以视为事务型传输，数据传输的间隔不固定，传输的速率也不固定。而 Isochronous 则是流式传输，数据传输的间隔固定，传输的速率也基本固定（取决于设备），而且没有重传机制，丢包后不会重传。

在我实现了 DDMA 后，将用户请求切分为 non_isochronous 和 isochronous 两种传输策略，Isochronous 传输策略下，端点长期占用 usb host 通道，用户侧多次发起 TransferRequest 后，isochronous 状态机会在 TransferRequest 缓冲区非空时，自动填充新的 DMA 描述符链表，保证数据的连续性和实时性。

### 采集层：利用 videobuffer 把“流”构造成“帧”

相比一周前，videobuffer 这部分已经趋于完善了，将通用的内存管理逻辑从具体的驱动中抽象出来，形成了一个通用的缓冲区管理子系统，驱动只需要关心如何从缓冲区中获取数据，提供一个能够从 USB Package 中解码数据的回调函数即可。

### 接口层：v4l2 驱动 ioctl 框架

已经完成了最主要的两大功能，图像采集和控制接口，并且完成了对 UVC 驱动的适配，能够通过 v4l2-ctl 工具进行测试，确保摄像头的采集、编码、输出等功能正常。
目前正在逐步通过 `v4l2-compliance` 工具的全面测试，确保 v4l2 驱动的接口符合规范。测试仓库：https://github.com/LinkWanna/v4l2-test

## 已知缺陷

当前还是处于快速迭代的时候，有一些机制并非刻意设计，而是为了快速实现功能而临时设计的，有一些又是因为基础设施限制，后续会进行重构和优化。

1. usbfs 的一些设计适应不了 UVC 驱动的需求，毕竟之前也没有具体的设备使用过，我遇到的问题如下：
  - 依赖方向有问题，当前 UVC 是第一个在内核外基于 USB 协议栈实现的驱动，不可能直接依赖 usbfs 的 `UsbDeviceHandle`，因为这会导致依赖循环，只能用 trait 来让依赖反转。
2. DWC2 的 zero-copy 机制还不完善，当前的实现是 DWC2 内部分配 DMA Buffer，然后通过 DMA 将数据拷贝到 TransferRequest 的缓冲区中，如果要求改为 zero-copy，则会遇到这样的问题：
  - 用户提供的缓冲区是虚拟地址，无法保证缓冲区的物理连续性，因此无法直接传给 DWC2 进行 DMA 传输，这部分要求用户做约束
3. 无法保证实时性，当前的 uvc 驱动是中断 wakeup + task 重填的方式来实现的，在 高负载情况下无法保证实时性
