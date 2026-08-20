#![no_std]
#[cfg(test)]
extern crate std;

#[macro_use]
extern crate alloc;

use alloc::{string::String, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, Ordering};

use anyhow::anyhow;
use ax_sync::Mutex;
use crab_usb::{
    err::USBError,
    usb_if::{
        host::ControlSetup,
        transfer::{Recipient, RequestType},
    },
};
use log::*;
use v4l2_core::{
    ctrls::CtrlHandler,
    interface::{colorspace, format},
};
use videobuffer::{Vb2Queue, VirtualAllocator};

use crate::stream::{ISO_BATCH, ISO_DEPTH, IsoBatchPipeline, IsoStreamHandle, UvcTrace};

// 导入描述符解析模块
pub mod controls;
pub mod descriptors;
pub use descriptors::*;

pub mod frame;
pub mod helper;
pub mod stream;
pub mod v4l2;

use crate::helper::{parse_stream_control, parse_uvc_device};

/// USB 设备句柄 — 控制传输、接口声明/释放、ISO 批提交。
///
/// 由 OS 侧（StarryOS usbfs 的 `UsbDeviceHandle`）实现：控制路径为同步
/// 控制传输，ISO 路径为多批在飞提交（`submit_iso_batch`），完成由返回的
/// [`IsoStreamHandle`] 在任务上下文轮询。
pub trait UvcHandle: Send + Sync + 'static {
    fn claim_interface(&self, interface: u8, alternate: u8) -> Result<(), USBError>;

    fn release_interface(&self, interface: u8) -> Result<(), USBError>;

    fn control_in(&self, param: ControlSetup, data: &mut [u8]) -> Result<usize, USBError>;

    fn control_out(&self, param: ControlSetup, data: &[u8]) -> Result<(), USBError>;

    /// 提交一批 ISO IN 传输（等长槽模型：`data` 大小 = `packet_lengths`
    /// 之和，每槽一个包）。返回在飞批句柄：`poll` 等待完成（硬件事件
    /// 唤醒，任务上下文），`cancel` 停批并唤醒。可连续提交多批在飞，
    /// 环满（QueueFull）时返回 `SlotLimitReached` 作为反压。
    fn submit_iso_batch(
        &self,
        endpoint: u8,
        data: &mut [u8],
        packet_lengths: &[usize],
    ) -> Result<Arc<dyn IsoStreamHandle>, USBError>;
}

#[derive(Debug, Clone)]
pub struct VideoFormat {
    pub format_type: VideoFormatType,
    pub width: u16,
    pub height: u16,
    pub frame_rate: u32, // 帧率 (fps)
    pub format_index: u8,
    pub frame_index: u8,
}

// 目前默认 uncompressed 格式为 YUYV 4:2:2
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VideoFormatType {
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
    pub fn bytes_per_line(&self) -> usize {
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
    pub fn colorspace(&self) -> colorspace::Colorspace {
        if self.is_compressed() {
            colorspace::Colorspace::Jpeg
        } else {
            colorspace::Colorspace::Srgb
        }
    }

    pub fn frame_bytes(&self) -> usize {
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
    pub fn description(&self) -> String {
        match self.format_type {
            VideoFormatType::Uncompressed(_t) => "YUYV 4:2:2".into(),
            VideoFormatType::Mjpeg => "MJPEG".into(),
            VideoFormatType::H264 => "H.264".into(),
        }
    }

    /// 获取对应的 V4L2 pixel format
    pub fn pixelformat(&self) -> u32 {
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
    pub fn is_compressed(&self) -> bool {
        matches!(
            self.format_type,
            VideoFormatType::Mjpeg | VideoFormatType::H264
        )
    }
}

/// 未压缩视频格式类型
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UncompressedFormat {
    /// YUY2 (YUYV) 格式
    Yuy2,
    /// NV12 格式
    Nv12,
    /// RGB24 格式
    Rgb24,
    /// RGB32 格式
    Rgb32,
}

/// 视频控制事件
#[derive(Debug, Clone)]
pub enum VideoControlEvent {
    /// 视频格式变更
    FormatChanged(VideoFormat),
    /// 亮度调整
    BrightnessChanged(i16),
    /// 对比度调整
    ContrastChanged(i16),
    /// 色调调整
    HueChanged(i16),
    /// 饱和度调整
    SaturationChanged(i16),
    /// 错误事件
    Error(String),
}

/// 视频数据帧
#[derive(Debug)]
pub struct VideoFrame {
    /// 帧数据
    pub data: Vec<u8>,
    /// 时间戳
    pub timestamp: u64,
    /// 帧序号
    pub frame_number: u32,
    /// 数据格式
    pub format: VideoFormat,
    /// 是否是帧结束标志
    pub end_of_frame: bool,
}

/// UVC 设备状态
#[derive(Debug, Clone, PartialEq)]
pub enum UvcDeviceState {
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
pub struct StreamControl {
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
pub struct AlternateSetting {
    pub alt_setting: u8,           // 编号
    pub ep: u8,                    // 端点地址
    pub mps: u16,                  // 最大包大小 (Max Packet Size)
    pub packets_per_uframe: usize, // 每微帧的总字节数
    pub interval: u8,              // USB bInterval（HS ISO：实际间隔 = 2^(bInterval-1) 微帧）
}

impl AlternateSetting {
    pub fn buf_len(&self) -> usize {
        self.mps as usize * self.packets_per_uframe
    }
}

/// 常驻 ISO 流 worker（STREAMON 创建、STREAMOFF 取消后 join）。
pub(crate) struct IsoStreamWorker {
    /// worker 任务句柄（join 等待退出）。
    task: ax_task::AxTaskRef,
    /// STREAMOFF 停止标志（worker 每轮循环检查）。
    cancel: Arc<AtomicBool>,
    /// 当前在飞批快照（close 取走并 cancel 全部以唤醒阻塞中的 worker；
    /// worker 每轮提交后写入、结算后清空）。
    in_flight: Arc<Mutex<Vec<Arc<dyn IsoStreamHandle>>>>,
}

pub struct UvcDevice<H: UvcHandle> {
    handle: Arc<H>,
    vs_iface_num: u8,
    vc_iface_num: u8,
    formats: Vec<VideoFormat>,
    alt_settings: Vec<AlternateSetting>,
    current_format: Option<VideoFormat>,
    pub(crate) ctrls: CtrlHandler,
    pub(crate) queue: Arc<Vb2Queue<VirtualAllocator>>,
    pub(crate) state: Mutex<UvcDeviceState>,
    need_payload: usize,
    stream: Mutex<Option<IsoStreamWorker>>,
    trace: Arc<UvcTrace>,
    /// V4L2 事件源：驱动投递事件（如控件变更），由 glue 排空到 fh。
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
            current_format: None,
            state: Mutex::new(UvcDeviceState::Configured),
            queue: Arc::new(Vb2Queue::new(VirtualAllocator::new(), 2, 8)),
            need_payload: 0,
            stream: Mutex::new(None),
            trace: Arc::new(UvcTrace::default()),
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
        crate::controls::register_uvc_controls(
            &mut device.ctrls,
            &device.handle,
            device.vc_iface_num,
            &parsed.vc_units,
        );
        info!("[UVC] registered {} controls", device.ctrls.count());

        Ok(device)
    }

    /// V4L2 事件源：驱动投递的事件（如控件变更），由 glue 在 ioctl 后排空到 fh。
    pub fn event_source(&self) -> Arc<Mutex<Vec<v4l2_core::interface::event::Event>>> {
        Arc::clone(&self.events)
    }

    /// 设置视频格式
    pub fn set_format(&mut self, format: VideoFormat) -> Result<(), USBError> {
        debug!("Setting video format: {format:?}");

        // 参考 libuvc 实现，需要先 probe 然后 commit
        // 1. 构建 VS stream control 结构
        let mut stream_ctrl = self.build_stream_control(&format)?;

        // 2. 先发送 PROBE 控制请求
        self.send_vs_control(VideoStreamingControl::Probe as u8, &stream_ctrl)?;

        // 3. 获取设备的 PROBE 响应
        let probe_response = self.get_vs_control(VideoStreamingControl::Probe as u8, 26)?;
        stream_ctrl = parse_stream_control(&probe_response)?;
        let payload = stream_ctrl.max_payload_transfer_size as usize;
        self.need_payload = payload;
        info!(
            "[UVC] PROBE: fmt_ix={} frm_ix={} interval={} max_frame={} max_payload={}",
            stream_ctrl.format_index,
            stream_ctrl.frame_index,
            stream_ctrl.frame_interval,
            stream_ctrl.max_video_frame_size,
            stream_ctrl.max_payload_transfer_size
        );

        // 4. 发送 COMMIT 控制请求
        self.send_vs_control(VideoStreamingControl::Commit as u8, &stream_ctrl)?;

        debug!("Video format set successfully");
        self.current_format = Some(format);
        Ok(())
    }

    /// 选择最优 alt setting + SET_INTERFACE + spawn 流 worker（任务侧轮询模型）。
    pub(crate) fn start_streaming(&mut self) -> Result<(), USBError> {
        let best = self.find_best_alt();
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

        // 流 worker：预填充 `ISO_DEPTH` 个等长槽批（dwc2 环容量内），
        // 完成后结算并立即回填，形成持续在飞的流水线。
        let slot_len = best.buf_len();
        info!(
            "[UVC] start_streaming: iso worker ep={:#x} batch={} slot_len={} depth={} buf={}",
            best.ep,
            ISO_BATCH,
            slot_len,
            ISO_DEPTH,
            slot_len * ISO_BATCH * ISO_DEPTH
        );
        let cancel = Arc::new(AtomicBool::new(false));
        let in_flight: Arc<Mutex<Vec<Arc<dyn IsoStreamHandle>>>> = Arc::new(Mutex::new(Vec::new()));
        let worker = {
            let handle = self.handle.clone();
            let queue = self.queue.clone();
            let trace = self.trace.clone();
            let endpoint = best.ep;
            let cancel = cancel.clone();
            let in_flight = in_flight.clone();
            let mut session = crate::stream::CaptureSession::new();
            let mut pipeline = IsoBatchPipeline::new(slot_len, ISO_DEPTH);
            ax_task::spawn_with_name(
                move || {
                    loop {
                        if cancel.load(Ordering::Acquire) {
                            break;
                        }
                        if let Err(err) = pipeline.submit_pending(&*handle, endpoint) {
                            error!("[UVC] stream: submit_iso_batch err={err:?}");
                            let _ = pipeline.cancel_all();
                            queue.set_error();
                            break;
                        }
                        *in_flight.lock() = pipeline.in_flight_handles();
                        if cancel.load(Ordering::Acquire) {
                            let _ = pipeline.cancel_all();
                            *in_flight.lock() = Vec::new();
                            break;
                        }
                        if pipeline.in_flight() == 0 {
                            error!("[UVC] stream: iso pipeline has no in-flight batch");
                            queue.set_error();
                            break;
                        }
                        let outcome = ax_task::future::block_on(core::future::poll_fn(|cx| {
                            pipeline.poll_process(cx, &mut session, &queue, &trace)
                        }));
                        *in_flight.lock() = Vec::new();
                        if let Err(err) = outcome {
                            let _ = pipeline.cancel_all();
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
                },
                alloc::string::String::from("uvc-stream"),
            )
        };
        *self.stream.lock() = Some(IsoStreamWorker {
            task: worker,
            cancel,
            in_flight,
        });
        info!("[UVC] start_streaming: iso worker armed");
        // 重置启动 profiling 指标（set-once，跨 STREAMON 会话失效需清零）。
        self.trace.first_data_batch.store(0, Ordering::Relaxed);
        self.trace.first_frame_batch.store(0, Ordering::Relaxed);
        *self.state.lock() = UvcDeviceState::Streaming;
        Ok(())
    }

    /// 停采集（取消 + join 流 worker）→ SET_INTERFACE(alt=0)。
    ///
    /// 调用者必须随后执行 `queue.streamoff()`——它清队列状态并唤醒全部
    /// 等待者（阻塞 DQBUF / poll），是停流路径的唤醒权威。
    pub(crate) fn close_stream(&self) {
        if let Some(worker) = self.stream.lock().take() {
            // 置取消标志 + cancel 全部在飞批（halt 通道 → CHHLTD 唤醒 worker）。
            worker.cancel.store(true, Ordering::Release);
            let in_flight = core::mem::take(&mut *worker.in_flight.lock());
            for pending in in_flight {
                let _ = pending.cancel();
            }
            worker.task.join();
            self.log_stream_summary();
        }
        let _ = self.handle.claim_interface(self.vs_iface_num, 0);
        *self.state.lock() = UvcDeviceState::Configured;
    }

    /// 打印本次采集会话的完成事件摘要（worker 侧只更新原子，这里统一打印）。
    fn log_stream_summary(&self) {
        let trace = &self.trace;
        info!(
            "[UVC] stream trace: batches={} frames={} err_packets={} bytes={}",
            trace.batches.load(Ordering::Relaxed),
            trace.frames_done.load(Ordering::Relaxed),
            trace.err_packets.load(Ordering::Relaxed),
            trace.bytes_received.load(Ordering::Relaxed),
        );
    }

    /// 发送单元控制请求（SET_CUR）——UVC 单元控制通道（4.2.2 类特定请求）。
    pub(crate) fn send_pu_control(
        handle: &H,
        vc_iface: u8,
        unit_id: u8,
        control_selector: u8,
        data: &[u8],
    ) -> Result<(), USBError> {
        let setup = ControlSetup {
            request_type: RequestType::Class,
            recipient: Recipient::Interface,
            request: RequestCode::SetCur.into(),
            value: (control_selector as u16) << 8,
            index: ((unit_id as u16) << 8) | vc_iface as u16,
        };
        handle
            .control_out(setup, data)
            .map_err(|e| anyhow!("Failed to send unit control: {e:?}"))?;
        Ok(())
    }

    /// 读取单元控制请求（GET_CUR）——UVC 单元控制通道（4.2.2 类特定请求）。
    pub(crate) fn get_pu_control(
        handle: &H,
        vc_iface: u8,
        unit_id: u8,
        control_selector: u8,
        request: RequestCode,
        data: &mut [u8],
    ) -> Result<(), USBError> {
        let setup = ControlSetup {
            request_type: RequestType::Class,
            recipient: Recipient::Interface,
            request: request.into(),
            value: (control_selector as u16) << 8,
            index: ((unit_id as u16) << 8) | vc_iface as u16,
        };
        handle
            .control_in(setup, data)
            .map_err(|e| anyhow!("Failed to get unit control: {e:?}"))?;
        Ok(())
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
    fn build_stream_control(&mut self, format: &VideoFormat) -> Result<StreamControl, USBError> {
        debug!("Building stream control for format: {format:?}");

        // 查找匹配的格式和帧索引（参考 libuvc 的实现逻辑）
        let (format_index, frame_index) = self
            .find_format_indices(&self.formats, format)
            .ok_or_else(|| {
                warn!("Failed to find matching format for: {format:?}");
                anyhow!("No matching format found")
            })?;
        info!(
            "Found format_index={} frame_index={} for format: {format:?}",
            format_index, frame_index
        );

        // 计算帧间隔 (100ns 单位)，参考 libuvc 的计算方式
        let frame_interval = 10_000_000u32
            .checked_div(format.frame_rate)
            .unwrap_or(333333); // 默认 30fps (10,000,000 / 30)

        // 根据格式类型估算最大帧大小
        let width = format.width as u32;
        let height = format.height as u32;

        let max_frame_size = match format.format_type {
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

        Ok(StreamControl {
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
        })
    }

    /// 查找格式和帧索引
    ///
    /// 此函数参考了 libuvc 的 _uvc_find_frame_desc_stream_if 实现，提供了：
    /// 1. 精确的格式类型匹配（包括未压缩格式的子类型）
    /// 2. 分辨率匹配检查
    /// 3. 优雅的降级策略（exact match -> format type match -> default）
    /// 4. 符合 UVC 规范的索引计算（从1开始）
    ///
    /// libuvc 参考：
    /// - src/stream.c:_uvc_find_frame_desc_stream_if（第 415-444 行）
    /// - src/stream.c:uvc_find_frame_desc（第 444-474 行）
    fn find_format_indices(
        &self,
        formats: &[VideoFormat],
        target: &VideoFormat,
    ) -> Option<(u8, u8)> {
        // 遍历所有支持的格式，寻找匹配的格式和帧配置
        for format in formats {
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
                let format_index = format.format_index;
                let frame_index = format.frame_index;

                debug!(
                    "Found matching format: format_index={}, frame_index={}",
                    format_index, frame_index
                );
                return Some((format_index, frame_index));
            }
        }

        // 如果没有找到完全匹配的，尝试找到相同格式类型的第一个配置
        for format in formats {
            if format.format_type == target.format_type {
                let format_index = format.format_index;
                let frame_index = format.frame_index;

                info!(
                    "Using fallback format: format_index={}, frame_index={}",
                    format_index, frame_index
                );
                return Some((format_index, frame_index));
            }
        }

        // 如果还是没有找到，使用默认值（参考 libuvc 的错误处理）
        debug!("No matching format found, using default indices");
        None
    }

    /// 选择最优的备用设置 (Alternate Setting)
    fn find_best_alt(&self) -> AlternateSetting {
        let target = self.need_payload;
        let mut best_index = 0;
        for (index, alt) in self.alt_settings.iter().enumerate() {
            let total = (alt.mps as usize) * alt.packets_per_uframe;
            if total >= target {
                return alt.clone();
            }
            let best_total = self.alt_settings[best_index].mps as usize
                * self.alt_settings[best_index].packets_per_uframe;
            if total > best_total {
                best_index = index;
            }
        }
        self.alt_settings[best_index].clone()
    }
}
