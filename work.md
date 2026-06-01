## 概述

1. 完成了 cv181xsd SD 卡的适配
2. 完成了 ArceOS 的 USB 主机协议栈，并基于 sg200x-bsp 适配了 dwc2 驱动

## 基础设施

为了让 ArceOS 以及 根文件系统能跑在 sg2002 的板子上，首先就需要考虑烧录问题。方案有两种：
1. 利用 SD 卡烧录，每次更新内核时，手动将内核文件复制到 SD 卡中，插入板子后，板子从 SD 卡启动。
2. 利用 Uboot + TFTP(Wifi) 的方式烧录，每次更新内核时，将内核文件上传到 TFTP 服务器上，板子通过网络从 TFTP 服务器下载内核并启动。

理论上，方案二不用拔插 SD 卡，可以节约很多的时间，但是由于每一次进行 reset 都是直接断电启动，ext4 根文件系统经常损坏，导致下一次内核启动时，没有办法挂载根文件系统，直接导致内核 panic。

所以依旧是使用方案一，利用 SD 卡烧录的方式来进行开发。
```sh
# 编译并烧录内核到 SD 卡中
sh run.sh build sg2002
```

### 内核镜像构建

官方的启动流程是这样的 `bootrom(bl1)` 判断 sd 卡第一个 FAT 分区内是否拥有 `fip.bin`，如果有，加载 `fip.bin(bl2)` 里面的代码到 0x0C000000(TPU SRAM)，运行并初始化 `clock`, `DRAM`，加载 `opensbi` 到 DRAM，执行，然后加载 `uboot` 到 DRAM，执行，最后加载 `boot.sd` 到 DRAM，执行。

其中，`bootrom(bl1)` 内置在芯片中，无法修改；`fip.bin(bl2)` 包含 `opensbi` 和 `uboot`，需要使用官方提供的工具进行构建；`boot.sd` 是我们自己编译的内核镜像。

这里我利用官方提供的镜像，将 `boot.sd` 替换为我们自己编译的内核镜像，来启动 ArceOS。参考 `run.sh` 中构建 FIT 镜像的部分。

### SD 分区构建与文件系统修复

官方提供的 SD 卡镜像中，包含一个 FAT 分区和一个 ext4 分区。FAT 分区正常使用，但是 ext4 分区太大了，而且经常损坏，需要经常性地进行修复。

为了节约时间，我重新构建 SD 卡分区，使用一个 1GB 的 ext4 分区来存放根文件系统，这样就不会经常损坏了。参考 `disk` 目录下的脚本。

## SD 卡适配

已经有其他的 SD 适配的代码可以参考，这部分实现起来并不复杂

相关文件如下：
1. `components/axdriver_crates/axdriver_block/src/cv181xsd.rs`: 适配 cv181xsd 驱动，提供 SD 卡的读写接口。
2. `components/axdriver_crates/axdriver_block/src/partition/mbr.rs`: 适配 MBR 分区表，提供分区表的解析和构建功能。（彭泽辰）
3. `os/arceos/modules/axdriver/src/drivers.rs`: 将 cv181xsd 驱动注册到 ArceOS 的设备树中，使得内核能够识别和使用 SD 卡设备。


## USB 主机协议栈适配

在这部分中，我主要参考了 CrabUsb 的设计，抽象出了一个通用的 USB 设备接口。

目前的进度是实现了基于 dwc2 驱动的适配层，使得上层协议栈能够通过这个接口来访问不同类型的 USB 设备，并且实现了 UVC 设备的 Class，提供了一个 `/dev/video0` 的接口，用户态的应用可以通过这个接口来访问 UVC 设备，获取视频流数据。

我留了一些坑，有一些 `trade-off` 的细节： 
1. ArceOS 没有 `pinctrl` 驱动和 `phy` 驱动，所以在 `axdriver-usb` 中直接把 `sg2002` 板级特定的代码放到 `axdriver-usb` 里面做平台初始化了。
2. 在 `os/StarryOS/kernel/src/pseudofs/dev/video.rs` 中，我构建了一个 `/dev/video0` 的接口，用户态的应用可以通过这个接口来访问 UVC 设备，一般来说，访问设备是通过 `ioctl` 进行的，但是为了我测试方便，我实现了 `read_at` 来拍一张照片。
3. DMA 我没有太能够弄明白，目前 DMA 相关的访问还是经由 `sg200x-bsp` 的 dwc2 驱动来完成的，我还没有把 DMA 相关接口抽象出来。

### USB 设备抽象

这部分是核心的难点，主要是如何抽象出一个通用的 USB 设备接口，使得上层协议栈能够通过这个接口来访问不同类型的 USB 设备。我可以参考的实现有 Linux 内核中的 USB 设备抽象：
```c
static struct hc_driver dwc2_hc_driver = {
	.description = "dwc2_hsotg",
	.product_desc = "DWC OTG Controller",
	.hcd_priv_size = sizeof(struct wrapper_priv_data),

	.irq = _dwc2_hcd_irq,   // 中断处理函数
	.flags = HCD_MEMORY | HCD_USB2 | HCD_BH,

	.start = _dwc2_hcd_start,   // 启动主机控制器
	.stop = _dwc2_hcd_stop,     // 停止主机控制器
	.urb_enqueue = _dwc2_hcd_urb_enqueue,   // 将 URB（USB 请求块）加入到主机控制器的传输队列中
	.urb_dequeue = _dwc2_hcd_urb_dequeue,   // 从主机控制器的传输队列中移除 URB
	.endpoint_disable = _dwc2_hcd_endpoint_disable, // 禁用 USB 端点
	.endpoint_reset = _dwc2_hcd_endpoint_reset,     // 重置 USB 端点
	.get_frame_number = _dwc2_hcd_get_frame_number, // 获取当前的 USB 帧编号

	.hub_status_data = _dwc2_hcd_hub_status_data,   // 获取 USB 集线器的状态数据
	.hub_control = _dwc2_hcd_hub_control,           // 控制 USB 集线器的操作，如端口复位、端口启用/禁用等
	.clear_tt_buffer_complete = _dwc2_hcd_clear_tt_buffer_complete, // 清除事务翻译器（Transaction Translator）缓冲区完成状态

	.bus_suspend = _dwc2_hcd_suspend,   // 挂起主机控制器
	.bus_resume = _dwc2_hcd_resume,     // 恢复主机控制器

	.map_urb_for_dma	= dwc2_map_urb_for_dma,       // 将 URB 映射到 DMA 地址空间中，以便主机控制器能够访问 URB 中的数据
	.unmap_urb_for_dma	= dwc2_unmap_urb_for_dma,   // 将 URB 从 DMA 地址空间中解除映射
};
```

Linux 利用了一个 URB 队列来做为 USB 设备和主机控制器之间的通信桥梁，URB 中包含了 USB 设备的地址、端点信息、数据缓冲区等信息。特定驱动的主机控制器通过处理 URB 来完成对 USB 设备的访问。

上层只需要关心 URB 的构建和提交，而不需要关心底层的 USB 设备细节，这样就实现了设备抽象。

CrabUsb 也有类似的设计，不过更加地深入 USB 协议的细节：
```rust
pub(crate) trait DeviceOp: Send + Any + 'static {
    fn id(&self) -> usize;
    fn backend_name(&self) -> &str;
    fn descriptor(&self) -> &DeviceDescriptor;
    fn configuration_descriptors(&self) -> &[ConfigurationDescriptor];

    fn ctrl_ep_ref(&self) -> &Endpoint;

    fn ctrl_ep_mut(&mut self) -> &mut Endpoint;

    fn claim_interface<'a>(
        &'a mut self,
        interface: u8,
        alternate: u8,
    ) -> BoxFuture<'a, Result<(), USBError>>;

    fn set_configuration<'a>(
        &'a mut self,
        configuration_value: u8,
    ) -> BoxFuture<'a, Result<(), USBError>>;

    fn endpoint(&mut self, desc: &EndpointDescriptor) -> Result<ep::Endpoint, USBError>;

    fn update_hub(&mut self, params: HubParams) -> BoxFuture<'_, Result<(), USBError>>;
}

pub(crate) trait EndpointOp: Send + Any + 'static {
    fn submit_request(&mut self, request: TransferRequest) -> Result<RequestId, TransferError>;

    fn reclaim_request(
        &mut self,
        id: RequestId,
    ) -> Option<Result<TransferCompletion, TransferError>>;

    fn register_waker(&self, id: RequestId, cx: &mut Context<'_>);

    fn cancel_request(&mut self, _id: RequestId) -> Result<(), TransferError> {
        Err(TransferError::NotSupported)
    }
}
```

我基本上是参考了 CrabUsb 的设计抽象出 Trait 来定义 USB 设备和端点的操作接口。

### usb 协议栈适配

axusb 部分承载的是仅协议相关的实现，考虑到 ArceOS 要求的分层设计，axusb 不能被 axdriver-usb 依赖，同时还需要为上层应用提供语义化的接口，所以我现在的设计是把 usb class 相关实现放在 axusb 中，通过 usb class 的接口来构造 `TransferRequest`，然后通过 `EndpointOps` 的接口来提交请求。

在 USB 协议接口的部分，我使用了 `usb-if` 这个仓库，这个仓库实现了 USB 协议规范的定义和解析，包含了 USB 设备描述符、配置描述符、接口描述符等的定义和解析功能，这样我就不需要自己去实现这些协议细节了。

目前只实现了 UVC 设备的 Class，基本上是基于 sg200x-bsp 的代码横向迁移过来的。

### 测试方案

我写了一个 `os/StarryOS/kernel/src/pseudofs/dev/video.rs` 在 `/dev/video0` 上提供了一个视频设备的接口，用户态的应用可以通过这个接口来访问 UVC 设备，获取视频流数据。

实现了 `read_at` 来拍一张照片，获取一帧视频流数据，在 capture 时，通过 iTerm2 Inline Images Protocol 把图片字节流写到控制台。

### 参考仓库
- [linux](https://github.com/torvalds/linux): Linux 内核的源代码仓库，包含了 USB 协议栈的实现和 dwc2 驱动及其适配。
- [CrabUsb](https://github.com/drivercraft/CrabUSB): Rust 的 USB 协议栈实现
- [usb-if](https://github.com/drivercraft/CrabUSB/tree/main/usb-if): USB 协议规范的实现，包含了 USB 设备描述符、配置描述符、接口描述符等的定义和解析。
