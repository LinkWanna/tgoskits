#![no_std]
#[cfg(test)]
extern crate std;

#[macro_use]
extern crate alloc;

use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, Ordering};

use anyhow::anyhow;
use ax_sync::Mutex;
use crab_usb::{
    err::USBError,
    usb_if::{
        endpoint::TransferRequest,
        host::ControlSetup,
        transfer::{Recipient, RequestType},
    },
};
use log::*;
pub use stream::IsoPending;
use v4l2_core::{
    ctrls::CtrlHandler,
    interface::{colorspace, format},
};
use videobuffer::{Vb2Queue, VirtualAllocator};

use crate::{
    frame::FrameParser,
    helper::{parse_stream_control, parse_uvc_device},
    stream::{FrameAssembler, ISO_BATCH, ISO_DEPTH, IsoStream},
};

pub(crate) mod controls;
pub(crate) mod descriptors;
pub(crate) use descriptors::*;
pub(crate) mod frame;
pub(crate) mod helper;
pub(crate) mod stream;
pub(crate) mod v4l2;

/// USB 设备句柄 — 控制传输、接口声明/释放、ISO 批提交。
///
/// 由 OS 侧（StarryOS usbfs 的 `UsbDeviceHandle`）实现：控制路径为同步
/// 控制传输，ISO 路径为多批在飞提交（`submit_endpoint_transfer`），完成由
/// 返回的 [`IsoPending`] 在任务上下文轮询。
pub trait UvcHandle: Send + Sync + 'static {
    fn claim_interface(&self, interface: u8, alternate: u8) -> Result<(), USBError>;

    fn release_interface(&self, interface: u8) -> Result<(), USBError>;

    fn control_in(&self, param: ControlSetup, data: &mut [u8]) -> Result<usize, USBError>;

    fn control_out(&self, param: ControlSetup, data: &[u8]) -> Result<(), USBError>;

    /// 提交一批传输（含 ISO IN 的等长槽模型）。返回在飞批句柄：`poll`
    /// 等待完成（硬件事件唤醒，任务上下文），`cancel` 停批并唤醒。可连续
    /// 提交多批在飞，环满时返回 `SlotLimitReached` 作为反压。
    fn submit_endpoint_transfer(
        &self,
        endpoint: u8,
        request: TransferRequest,
    ) -> Result<IsoPending, USBError>;
}

#[derive(Debug, Clone)]
pub(crate) struct VideoFormat {
    pub format_type: VideoFormatType,
    pub width: u16,
    pub height: u16,
    pub frame_rate: u32, // 帧率 (fps)
    pub format_index: u8,
    pub frame_index: u8,
}

// 目前默认 uncompressed 格式为 YUYV 4:2:2
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum VideoFormatType {
    Uncompressed(UncompressedFormat),
    Mjpeg,
    H264,
}

impl From<u32> for VideoFormatType {
    fn from(value: u32) -> Self {
        match value {
            format::PIX_FMT_YUYV => VideoFormatType::Uncompressed(UncompressedFormat::Yuy2),
            format::PIX_FMT_NV12 => VideoFormatType::Uncompressed(UncompressedFormat::Nv12),
            format::PIX_FMT_RGB24 => VideoFormatType::Uncompressed(UncompressedFormat::Rgb24),
            format::PIX_FMT_RGB32 => VideoFormatType::Uncompressed(UncompressedFormat::Rgb32),
            format::PIX_FMT_MJPEG => VideoFormatType::Mjpeg,
            format::PIX_FMT_H264 => VideoFormatType::H264,
            _ => VideoFormatType::Uncompressed(UncompressedFormat::Yuy2), // 默认回退为 YUY2
        }
    }
}

impl VideoFormat {
    /// 每行字节数：未压缩格式 = 宽 × 每像素字节；压缩格式驱动不知道行宽，返回 0。
    pub(crate) fn bytes_per_line(&self) -> usize {
        match self.format_type {
            VideoFormatType::Uncompressed(t) => {
                let pixel_size = match t {
                    UncompressedFormat::Yuy2 => 2,  // YUY2 每像素2字节
                    UncompressedFormat::Nv12 => 1,  // NV12 每像素1字节 (平均)
                    UncompressedFormat::Rgb24 => 3, // RGB24 每像素3字节
                    UncompressedFormat::Rgb32 => 4, // RGB32 每像素4字节
                };
                (self.width as usize) * pixel_size
            }
            VideoFormatType::Mjpeg | VideoFormatType::H264 => 0,
        }
    }

    /// V4L2 色彩空间（对齐 Linux uvcvideo：压缩格式 JPEG、未压缩 sRGB）。
    pub(crate) fn colorspace(&self) -> colorspace::Colorspace {
        if self.is_compressed() {
            colorspace::Colorspace::Jpeg
        } else {
            colorspace::Colorspace::Srgb
        }
    }

    pub(crate) fn frame_bytes(&self) -> usize {
        match self.format_type {
            VideoFormatType::Uncompressed(_) => self.bytes_per_line() * (self.height as usize),
            VideoFormatType::Mjpeg => {
                // MJPEG 压缩后大小不定，这里返回一个估算值（假设压缩比为10:1）
                ((self.width as usize) * (self.height as usize) * 3) / 10
            }
            VideoFormatType::H264 => {
                // H.264 压缩后大小不定，这里返回一个估算值（假设压缩比为20:1）
                ((self.width as usize) * (self.height as usize) * 3) / 20
            }
        }
    }

    /// 获取视频格式的描述信息
    pub(crate) fn description(&self) -> String {
        match self.format_type {
            VideoFormatType::Uncompressed(_t) => "YUYV 4:2:2".into(),
            VideoFormatType::Mjpeg => "MJPEG".into(),
            VideoFormatType::H264 => "H.264".into(),
        }
    }

    /// 获取对应的 V4L2 pixel format
    pub(crate) fn pixelformat(&self) -> u32 {
        match self.format_type {
            VideoFormatType::Uncompressed(t) => match t {
                UncompressedFormat::Yuy2 => format::PIX_FMT_YUYV, // 'YUY2' 小端序
                UncompressedFormat::Nv12 => format::PIX_FMT_NV12, // 'NV12' 小端序
                UncompressedFormat::Rgb24 => format::PIX_FMT_RGB24, // 'RGB24' 小端序
                UncompressedFormat::Rgb32 => format::PIX_FMT_RGB32, // 'RGB32' 小端序
            },
            VideoFormatType::Mjpeg => format::PIX_FMT_MJPEG, // 'MJPG' 小端序
            VideoFormatType::H264 => format::PIX_FMT_H264,   // 'H264' 小端序
        }
    }

    /// 检查视频格式是否为压缩格式
    pub(crate) fn is_compressed(&self) -> bool {
        matches!(
            self.format_type,
            VideoFormatType::Mjpeg | VideoFormatType::H264
        )
    }
}

/// 未压缩视频格式类型
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum UncompressedFormat {
    /// YUY2 (YUYV) 格式
    Yuy2,
    /// NV12 格式
    Nv12,
    /// RGB24 格式
    Rgb24,
    /// RGB32 格式
    Rgb32,
}

/// UVC 设备状态
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub(crate) enum UvcDeviceState {
    /// 未配置
    Unconfigured,
    /// 已配置但未开始流传输
    Configured,
    /// 正在进行流传输
    Streaming,
    /// 错误状态
    Error(String),
}

/// UVC Stream Control 结构体 (参考 UVC 规范 4.3.1.1)
#[derive(Debug, Clone)]
pub(crate) struct StreamControl {
    hint: u16,                      // bmHint
    format_index: u8,               // bFormatIndex
    frame_index: u8,                // bFrameIndex
    frame_interval: u32,            // dwFrameInterval（100ns 单位）
    key_frame_rate: u16,            // wKeyFrameRate
    p_frame_rate: u16,              // wPFrameRate
    comp_quality: u16,              // wCompQuality
    comp_window_size: u16,          // wCompWindowSize
    delay: u16,                     // wDelay
    max_video_frame_size: u32,      // dwMaxVideoFrameSize
    max_payload_transfer_size: u32, // dwMaxPayloadTransferSize
}

/// UVC 设备的备用设置信息
#[derive(Debug, Clone)]
pub(crate) struct AlternateSetting {
    pub alt_setting: u8,           // 编号
    pub ep: u8,                    // 端点地址
    pub mps: u16,                  // 最大包大小 (Max Packet Size)
    pub packets_per_uframe: usize, // 每微帧的总字节数
    pub interval: u8,              // USB bInterval（HS ISO：实际间隔 = 2^(bInterval-1) 微帧）
}

impl AlternateSetting {
    pub(crate) fn buf_len(&self) -> usize {
        self.mps as usize * self.packets_per_uframe
    }
}

pub(crate) struct IsoStreamWorker {
    task: ax_task::AxTaskRef,
    cancel: Arc<AtomicBool>,
    iso: Arc<Mutex<IsoStream>>,
}

pub struct UvcDevice<H: UvcHandle> {
    handle: Arc<H>,
    vs_iface_num: u8,
    vc_iface_num: u8,
    formats: Vec<VideoFormat>,
    alt_settings: Vec<AlternateSetting>,
    active_format: usize,
    active_alt_setting: usize,
    pub(crate) ctrls: CtrlHandler,
    pub(crate) queue: Arc<Vb2Queue<VirtualAllocator>>,
    pub(crate) state: Mutex<UvcDeviceState>,
    stream: Mutex<Option<IsoStreamWorker>>,
    events: Arc<Mutex<Vec<v4l2_core::interface::event::Event>>>,
}

impl<H: UvcHandle> UvcDevice<H> {
    /// 创建 UVC 设备驱动。
    pub fn new(handle: H, descriptor_blob: &[u8]) -> Result<Self, USBError> {
        let parsed = parse_uvc_device(descriptor_blob)?;

        handle
            .claim_interface(parsed.vc_iface_num, 0)
            .map_err(|e| {
                anyhow!(
                    "Failed to claim VC interface {}: {e:?}",
                    parsed.vc_iface_num
                )
            })?;
        handle
            .claim_interface(parsed.vs_iface_num, 0)
            .map_err(|e| {
                anyhow!(
                    "Failed to claim VS interface {}: {e:?}",
                    parsed.vs_iface_num
                )
            })?;

        let mut device = Self {
            handle: Arc::new(handle),
            vs_iface_num: parsed.vs_iface_num,
            vc_iface_num: parsed.vc_iface_num,
            ctrls: v4l2_core::ctrls::CtrlHandler::new(),
            formats: parsed.formats,
            alt_settings: parsed.alt_settings,
            active_format: 0,
            active_alt_setting: 0,
            state: Mutex::new(UvcDeviceState::Configured),
            queue: Arc::new(Vb2Queue::new(VirtualAllocator::new(), 2, 8)),
            stream: Mutex::new(None),
            events: Arc::new(Mutex::new(Vec::new())),
        };

        for fmt in &device.formats {
            info!(
                "Supported format: {:?}, {}x{}, {} fps, format_index={}, frame_index={}",
                fmt.format_type,
                fmt.width,
                fmt.height,
                fmt.frame_rate,
                fmt.format_index,
                fmt.frame_index
            );
        }
        info!(
            "[UVC] VC units: camera_terminal={:?} processing_unit={:?}",
            parsed.vc_units.camera_terminal_id, parsed.vc_units.processing_unit_id
        );
        device.register_controls(&parsed.vc_units);
        info!("[UVC] registered {} controls", device.ctrls.len());
        // 控件值变化事件由框架统一生成（S_CTRL / S_EXT_CTRLS 应用后触发），
        // 经驱动共享事件队列由 glue 排空到 fh。
        let ev = Arc::clone(&device.events);
        device
            .ctrls
            .set_change_notify(Box::new(move |event| ev.lock().push(event)));

        Ok(device)
    }

    /// V4L2 事件源：驱动投递的事件（如控件变更）
    pub fn event_source(&self) -> Arc<Mutex<Vec<v4l2_core::interface::event::Event>>> {
        Arc::clone(&self.events)
    }

    pub(crate) fn active_format_ref(&self) -> &VideoFormat {
        &self.formats[self.active_format]
    }

    /// 设置视频格式
    pub(crate) fn set_format(&mut self, format: VideoFormat) -> Result<(), USBError> {
        debug!("Setting video format: {format:?}");

        // 参考 libuvc 实现，需要先 probe 然后 commit
        // 1. 构建 VS stream control 结构（同时解析出 `formats` 下标）
        let (mut stream_ctrl, pos) = self.build_stream_control(&format)?;

        // 2. 先发送 PROBE 控制请求
        self.send_vs_control(VideoStreamingControl::Probe as u8, &stream_ctrl)?;

        // 3. 获取设备的 PROBE 响应
        let probe_response = self.get_vs_control(VideoStreamingControl::Probe as u8, 26)?;
        stream_ctrl = parse_stream_control(&probe_response)?;
        let payload = stream_ctrl.max_payload_transfer_size as usize;
        self.active_alt_setting = self.select_alt_index(payload);
        info!(
            "[UVC] PROBE: fmt_ix={} frm_ix={} interval={} max_frame={} max_payload={} \
             active_alt={}",
            stream_ctrl.format_index,
            stream_ctrl.frame_index,
            stream_ctrl.frame_interval,
            stream_ctrl.max_video_frame_size,
            stream_ctrl.max_payload_transfer_size,
            self.active_alt_setting
        );

        // 4. 发送 COMMIT 控制请求
        self.send_vs_control(VideoStreamingControl::Commit as u8, &stream_ctrl)?;

        debug!("Video format set successfully");
        self.active_format = pos;
        Ok(())
    }

    pub(crate) fn start_streaming(&mut self) -> Result<(), USBError> {
        let best = self.alt_settings[self.active_alt_setting].clone();
        log::info!(
            "[UVC] Selected alt={} ep=0x{:02x} mps={} mult={} bInterval={}",
            best.alt_setting,
            best.ep,
            best.mps,
            best.packets_per_uframe,
            best.interval,
        );
        self.handle
            .claim_interface(self.vs_iface_num, best.alt_setting)
            .map_err(|e| {
                anyhow!(
                    "Failed to claim interface {} alt {}: {:?}",
                    self.vs_iface_num,
                    best.alt_setting,
                    e
                )
            })?;

        let slot_len = best.buf_len();
        info!(
            "[UVC] start_streaming: iso worker ep={:#x} batch={} slot_len={} depth={} buf={}",
            best.ep,
            ISO_BATCH,
            slot_len,
            ISO_DEPTH,
            slot_len * ISO_BATCH * ISO_DEPTH
        );
        let iso = alloc::sync::Arc::new(Mutex::new(IsoStream::new(slot_len, ISO_DEPTH)));
        let cancel = Arc::new(AtomicBool::new(false));
        let worker = {
            let handle = self.handle.clone();
            let queue = self.queue.clone();
            let endpoint = best.ep;
            let iso = iso.clone();
            let cancel = cancel.clone();
            let fmt = self.active_format_ref();
            let expected = (!fmt.is_compressed()).then_some(fmt.frame_bytes());
            ax_task::spawn_with_name(
                move || {
                    ax_task::future::block_on(async move {
                        let mut parser = FrameParser::new();
                        let mut dest = None;
                        // `dest` 为 `Option<ActiveFrame>`，由 `FrameAssembler` 通过 DriverQueue 统一管理
                        {
                            let mut iso = iso.lock();
                            if let Err(err) = iso.fill(&*handle, endpoint) {
                                error!("[UVC] stream: initial submit err={err:?}");
                                queue.set_error();
                                return;
                            }
                            if iso.in_flight() == 0 {
                                error!("[UVC] stream: iso pipeline has no in-flight batch");
                                queue.set_error();
                                return;
                            }
                        }
                        loop {
                            if cancel.load(Ordering::Acquire) {
                                let _ = iso.lock().cancel_all();
                                break;
                            }
                            let res = core::future::poll_fn(|cx| {
                                let mut assembler =
                                    FrameAssembler::new(&mut parser, &mut dest, expected, &queue);
                                iso.lock().poll_next(cx, &mut assembler)
                            })
                            .await;
                            match res {
                                Ok(()) => {
                                    if cancel.load(Ordering::Acquire) {
                                        let _ = iso.lock().cancel_all();
                                        break;
                                    }
                                    let mut iso = iso.lock();
                                    if let Err(err) = iso.fill(&*handle, endpoint) {
                                        error!("[UVC] stream: submit after complete err={err:?}");
                                        let _ = iso.cancel_all();
                                        queue.set_error();
                                        break;
                                    }
                                    if iso.in_flight() == 0 {
                                        error!("[UVC] stream: pipeline drained");
                                        queue.set_error();
                                        break;
                                    }
                                }
                                Err(err) => {
                                    let _ = iso.lock().cancel_all();
                                    if cancel.load(Ordering::Acquire)
                                        || matches!(
                                            err,
                                            USBError::TransferError(
                                                crab_usb::usb_if::err::TransferError::Cancelled
                                            )
                                        )
                                    {
                                        break;
                                    }
                                    error!("[UVC] stream: iso batch failed err={err:?}");
                                    queue.set_error();
                                    break;
                                }
                            }
                        }
                    });
                },
                alloc::string::String::from("uvc-stream"),
            )
        };
        *self.stream.lock() = Some(IsoStreamWorker {
            task: worker,
            cancel,
            iso,
        });
        info!("[UVC] start_streaming: iso worker armed");
        *self.state.lock() = UvcDeviceState::Streaming;
        Ok(())
    }

    pub(crate) fn close_stream(&self) {
        if let Some(worker) = self.stream.lock().take() {
            worker.cancel.store(true, Ordering::Release);
            let _ = worker.iso.lock().cancel_all();
            worker.task.join();
        }
        let _ = self.handle.claim_interface(self.vs_iface_num, 0);
        *self.state.lock() = UvcDeviceState::Configured;
    }

    /// 发送 VS 控制请求
    fn send_vs_control(
        &mut self,
        control_selector: u8,
        stream_ctrl: &StreamControl,
    ) -> Result<(), USBError> {
        let vs_interface_num = self.vs_iface_num;

        // 序列化 StreamControl 到字节数组
        let data = helper::serialize_stream_control(stream_ctrl);
        let setup = ControlSetup {
            request_type: RequestType::Class,
            recipient: Recipient::Interface,
            request: RequestCode::SetCur.into(),
            value: (control_selector as u16) << 8,
            index: vs_interface_num as u16,
        };

        debug!(
            "Sending VS control: selector=0x{:02x}, data_len={}",
            control_selector,
            data.len()
        );

        // 底层 control_out 即 URB 控制传输的同步封装，直接使用。
        self.handle
            .control_out(setup, &data)
            .map_err(|e| anyhow!("Failed to send VS control: {:?}", e))?;

        Ok(())
    }

    /// 获取 VS 控制响应
    fn get_vs_control(&mut self, control_selector: u8, length: usize) -> Result<Vec<u8>, USBError> {
        let vs_interface_num = self.vs_iface_num;

        let setup = ControlSetup {
            request_type: RequestType::Class,
            recipient: Recipient::Interface,
            request: RequestCode::GetCur.into(),
            value: (control_selector as u16) << 8,
            index: vs_interface_num as u16,
        };

        let mut buffer = vec![0u8; length];
        self.handle
            .control_in(setup, &mut buffer)
            .map_err(|e| anyhow!("Failed to get VS control: {:?}", e))?;

        debug!(
            "Received VS control response: selector=0x{:02x}, data_len={}",
            control_selector,
            buffer.len()
        );

        Ok(buffer)
    }

    /// 构建 Stream Control 结构体
    ///
    /// 此函数参考了 libuvc 的 uvc_get_stream_ctrl_format_size 实现，包括：
    /// 1. 通过遍历设备描述符来查找匹配的格式和帧索引（而不是使用硬编码的值）
    /// 2. 正确计算帧间隔（frame interval），使用100ns为单位
    /// 3. 根据不同的格式类型估算最大帧大小
    /// 4. 设置适当的 bmHint 标志位
    ///
    /// libuvc 参考：
    /// - src/stream.c:uvc_get_stream_ctrl_format_size（第 474-524 行）
    /// - src/stream.c:_uvc_find_frame_desc_stream_if（第 415-444 行）
    fn build_stream_control(
        &self,
        format: &VideoFormat,
    ) -> Result<(StreamControl, usize), USBError> {
        debug!("Building stream control for format: {format:?}");

        // 查找匹配的格式位置（参考 libuvc 的实现逻辑）
        let pos = self.find_format_index(format).ok_or_else(|| {
            warn!("Failed to find matching format for: {format:?}");
            anyhow!("No matching format found")
        })?;
        let negotiated = &self.formats[pos];
        let format_index = negotiated.format_index;
        let frame_index = negotiated.frame_index;
        info!(
            "Found format_index={} frame_index={} for format: {format:?} at pos={}",
            format_index, frame_index, pos
        );

        // 计算帧间隔 (100ns 单位)，优先使用请求帧率，缺省则用描述符帧率
        let effective_fps = if format.frame_rate != 0 {
            format.frame_rate
        } else {
            negotiated.frame_rate
        };
        let frame_interval = 10_000_000u32.checked_div(effective_fps).unwrap_or(333333); // 默认 30fps (10,000,000 / 30)

        // 根据协商格式类型估算最大帧大小（对齐量化后的实际格式）
        let width = negotiated.width as u32;
        let height = negotiated.height as u32;

        let max_frame_size = match negotiated.format_type {
            VideoFormatType::Mjpeg => {
                // MJPEG 压缩格式：参考 libuvc，通常为未压缩大小的一半左右
                width * height * 2
            }
            VideoFormatType::Uncompressed(uncompressed_format) => {
                // 未压缩格式：根据具体格式计算
                match uncompressed_format {
                    UncompressedFormat::Yuy2 => width * height * 2, // YUY2: 每像素 2 字节
                    UncompressedFormat::Nv12 => width * height * 3 / 2, // NV12: 每像素 1.5 字节
                    UncompressedFormat::Rgb24 => width * height * 3, // RGB24: 每像素 3 字节
                    UncompressedFormat::Rgb32 => width * height * 4, // RGB32: 每像素 4 字节
                }
            }
            VideoFormatType::H264 => {
                // H264 压缩格式：估算为未压缩大小的 1/4 到 1/8
                width * height / 2
            }
        };

        Ok((
            StreamControl {
                hint: 0x0001, // bmHint: dwFrameInterval 字段应保持不变（参考 libuvc）
                format_index,
                frame_index,
                frame_interval,
                key_frame_rate: 0,   // 默认为 0，让设备决定
                p_frame_rate: 0,     // 默认为 0，让设备决定
                comp_quality: 0,     // 默认为 0，让设备决定
                comp_window_size: 0, // 默认为 0
                delay: 0,            // 默认为 0
                max_video_frame_size: max_frame_size,
                max_payload_transfer_size: 0, // 让设备决定，参考 libuvc
            },
            pos,
        ))
    }

    /// 查找格式位置
    ///
    /// 此函数参考了 libuvc 的 _uvc_find_frame_desc_stream_if 实现，提供了：
    /// 1. 精确的格式类型匹配（包括未压缩格式的子类型）
    /// 2. 分辨率匹配检查
    /// 3. 优雅的降级策略（exact match -> format type match -> default）
    /// 4. 返回 `formats` 的下标（非 UVC `bFormatIndex`）
    ///
    /// libuvc 参考：
    /// - src/stream.c:_uvc_find_frame_desc_stream_if（第 415-444 行）
    /// - src/stream.c:uvc_find_frame_desc（第 444-474 行）
    fn find_format_index(&self, target: &VideoFormat) -> Option<usize> {
        // 遍历所有支持的格式，寻找匹配的格式和帧配置
        for (idx, format) in self.formats.iter().enumerate() {
            // 检查格式类型是否匹配
            if format.format_type != target.format_type {
                continue;
            }

            // 对于未压缩格式，还需要检查具体的子格式
            if let (
                VideoFormatType::Uncompressed(format_type),
                VideoFormatType::Uncompressed(target_type),
            ) = (&format.format_type, &target.format_type)
                && format_type != target_type
            {
                continue;
            }

            // 检查分辨率是否匹配
            if format.width == target.width && format.height == target.height {
                debug!(
                    "Found matching format: pos={} format_index={}, frame_index={}",
                    idx, format.format_index, format.frame_index
                );
                return Some(idx);
            }
        }

        // 如果没有找到完全匹配的，尝试找到相同格式类型的第一个配置
        for (idx, format) in self.formats.iter().enumerate() {
            if format.format_type == target.format_type {
                info!(
                    "Using fallback format: pos={} format_index={}, frame_index={}",
                    idx, format.format_index, format.frame_index
                );
                return Some(idx);
            }
        }

        // 如果还是没有找到，使用默认值（参考 libuvc 的错误处理）
        debug!("No matching format found, using default indices");
        None
    }

    fn select_alt_index(&self, payload: usize) -> usize {
        if self.alt_settings.is_empty() {
            return 0;
        }
        let mut best_index = 0;
        for (index, alt) in self.alt_settings.iter().enumerate() {
            let total = alt.buf_len();
            if total >= payload {
                return index;
            }
            let best_total = self.alt_settings[best_index].buf_len();
            if total > best_total {
                best_index = index;
            }
        }
        best_index
    }
}
