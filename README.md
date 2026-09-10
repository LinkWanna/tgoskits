## Introduce

本仓库用于为 Tgoskits 适配 v4l2 子系统，以满足在 StarryOS 上使用摄像头、视频采集、视频编码等需求。

由于 StarryOS 目前的 USB 协议栈对于流式采集/实时采集的支持不够完善，因此我在一定程度上重构了上游的 DWC2 USB 主机后端实现，以支持 UVC 摄像头的实时采集。

**主线合并进度：**
- **DWC2 USB 主机后端重构**：https://github.com/rcore-os/tgoskits/pull/2066
- **UVC 驱动实现**：https://github.com/rcore-os/tgoskits/pull/2219

## Hack 说明

由于当前的 v4l 实现依旧存在一些问题，还在积极处理中，同时主线内核的调度器也没有办法完全支持摄像头的实时采集，因此这里提供一个 Hack 版本的 v4l 子系统。

Hack 部分仅涉及 DWC2 驱动和 UVC 驱动，主要是将图像的拼帧工作从 task 中移动到硬中断的上下文中，以减少调度器的干扰，从而实现更低的采集延迟。

## Usage

### 内核构建

```sh
# 启用默认配置
cargo starry defconfig aka-00-sg2002

# 修改 tmp/axbuild/config/starryos/build-riscv64gc-unknown-none-elf.toml 下的 features
features = [
  "starry-kernel/sg2002",
  "starry-kernel/uvc",
  "axplat-dyn/thead-mae",
  "ax-driver/serial",
  "ax-driver/sg2002-dwc2",
  "ax-driver/cv181x-sdhci",
]
log = "Info"
target = "riscv64gc-unknown-none-elf"

# 构建内核
cargo starry build

# 利用 mkimage 生成 uImage
sh scripts/mk-boot-sd.sh

# 将生成的 uImage 拷贝到 SD 卡中替代 boot 分区原有的 boot.sd
```

### 根文件系统构建

仓库 [sg2002_inference](https://github.com/LinkWanna/sg2002_inference) 的 Release 中提供了一个根文件系统。

## 限制

下面是当前测试所使用的摄像头的格式列表，如果使用其他摄像头，并且不是 YUYV422 格式，那么 `sg2002_inference` 的代码就需要进行修改。
```sh
➜  uvc-hack git:(uvc-hack) ✗ v4l2-ctl -d /dev/video2 --list-formats-ext
ioctl: VIDIOC_ENUM_FMT
        Type: Video Capture

        [0]: 'MJPG' (Motion-JPEG, compressed)
                Size: Discrete 1280x720
                        Interval: Discrete 0.017s (60.000 fps)
                Size: Discrete 640x480
                        Interval: Discrete 0.017s (60.000 fps)
        [1]: 'YUYV' (YUYV 4:2:2)
                Size: Discrete 1280x720
                        Interval: Discrete 0.100s (10.000 fps)
                Size: Discrete 640x480
                        Interval: Discrete 0.033s (30.000 fps)
```
