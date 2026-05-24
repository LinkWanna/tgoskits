use core::{any::Any, slice};

use ax_sync::Mutex;
use ax_usb::{
    class::uvc::{self, UvcStreamSelection},
    imgcat,
    topology::UvcEnumerated,
};
use axfs_ng_vfs::{NodeFlags, VfsError, VfsResult};
use linux_raw_sys::ioctl::{
    VIDIOC_DQBUF, VIDIOC_ENUM_FMT, VIDIOC_ENUM_FRAMESIZES, VIDIOC_G_CTRL, VIDIOC_G_FMT,
    VIDIOC_QBUF, VIDIOC_QUERYBUF, VIDIOC_QUERYCAP, VIDIOC_REQBUFS, VIDIOC_S_CTRL, VIDIOC_S_FMT,
    VIDIOC_STREAMOFF, VIDIOC_STREAMON,
};
// sg200x-bsp 的设计中，所有访问 endpoint 的操作都在 dwc2::ep0 模块中进行
// 后续设计最好还是将 endpoint 访问抽象成一个结构体
use sg200x_bsp::usb::host::dwc2::ep0;
use starry_vm::VmMutPtr;

use crate::{
    file::FileLike,
    pseudofs::{DeviceMmap, DeviceOps},
};

#[repr(C)]
#[allow(non_camel_case_types, dead_code)]
struct v4l2_capability {
    driver: [u8; 16],
    card: [u8; 32],
    bus_info: [u8; 32],
    version: u32,
    capabilities: u32,
    device_caps: u32,
    reserved: [u32; 3],
}

pub struct Video {
    camera: UvcEnumerated,
    selection: UvcStreamSelection,
    jpeg: Mutex<Option<&'static [u8]>>,
}

impl Video {
    pub fn new() -> Self {
        let extras = ax_usb::device_list();

        let cam = extras.uvc.expect("UVC not found");
        debug!(
            "UVC: addr={} VID={:04x} PID={:04x} ep0_mps={}",
            cam.addr, cam.vid, cam.pid, cam.ep0_mps
        );

        // --- UVC 配置描述符解析 + 流协商 ---
        let dev = u32::from(cam.addr);
        let ep0 = cam.ep0_mps;
        let cfg_buf =
            uvc::read_configuration_descriptor(dev, ep0, 1).expect("读取 UVC 配置描述符失败");
        let cfg_total = u16::from_le_bytes([cfg_buf[2], cfg_buf[3]]) as usize;
        let mut sel =
            uvc::parse_uvc_video_stream(&cfg_buf[..cfg_total.min(cfg_buf.len())], cfg_total)
                .expect("UVC 解析视频流失败");

        if let Some(entities) =
            uvc::parse_uvc_control_entities(&cfg_buf[..cfg_total.min(cfg_buf.len())], cfg_total)
        {
            let tune = uvc::UvcImageTuning {
                brightness: Some(96),
                ..uvc::UvcImageTuning::default()
            };
            let _ = uvc::uvc_init_camera_controls(dev, ep0, &entities, &tune);
        }

        uvc::uvc_start_video_stream(dev, ep0, &mut sel)
            .expect("UVC PROBE/COMMIT 或 SET_INTERFACE 失败");
        debug!(
            "UVC: 视频流就绪 {}x{} payload={} frame_size={}",
            sel.frame_w, sel.frame_h, sel.negotiated_payload_size, sel.negotiated_frame_size
        );

        Self {
            camera: cam,
            selection: sel,
            jpeg: Mutex::new(None),
        }
    }

    /// 抓取一帧并缓存 JPEG 数据，供后续 read_at 调用返回。
    pub fn capture(&self) -> VfsResult<()> {
        let dev = u32::from(self.camera.addr);

        const MAX_TRIES: u32 = 8;
        const MIN_VALID_BYTES: usize = 4096;
        let mut last_n: usize = 0;
        let mut last_msg: Option<&'static str> = None;
        for attempt in 0..MAX_TRIES {
            let n = uvc::uvc_capture_one_frame(dev, self.camera.ep0_mps, &self.selection)
                .expect("UVC 抓帧失败");
            last_n = n;
            let s = ep0::dma_rx_slice(uvc::UVC_ASSEMBLED_JPEG_DMA_OFF, n).expect("DMA 切片越界");
            let starts_jpeg = n >= 2 && s[0] == 0xff && s[1] == 0xd8;
            let ends_jpeg = n >= 2 && s[n - 2] == 0xff && s[n - 1] == 0xd9;
            if starts_jpeg && ends_jpeg && n >= MIN_VALID_BYTES {
                self.jpeg.lock().replace(s);
                imgcat::print_image(s);
                return Ok(());
            }
            last_msg = Some(if !starts_jpeg {
                "首字节非 ff d8"
            } else if !ends_jpeg {
                "末字节非 ff d9（被截断）"
            } else {
                "尺寸过小"
            });
            debug!(
                "UVC: 帧无效 (try #{}/{}, size={}, {}); 重置 FID",
                attempt + 1,
                MAX_TRIES,
                n,
                last_msg.unwrap_or("?")
            );
            uvc::reset_frame_continuity();
        }
        debug!(
            "UVC: 重试 {} 次仍未拿到完整 JPEG，size={} {}",
            MAX_TRIES,
            last_n,
            last_msg.unwrap_or("?")
        );

        let s = ep0::dma_rx_slice(uvc::UVC_ASSEMBLED_JPEG_DMA_OFF, last_n).expect("DMA 切片越界");
        self.jpeg.lock().replace(s);
        imgcat::print_image(s);

        Ok(())
    }
}

impl DeviceOps for Video {
    /// Reads data from the device.
    /// On first read (offset == 0), captures a frame and caches it.
    /// Subsequent reads return data from the cache.
    /// In a real V4L2 implementation, frame data should be obtained via VIDIOC_DQBUF ioctl.
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        if offset == 0 || self.jpeg.lock().is_none() {
            self.capture()?;
        }

        let jpeg = self.jpeg.lock();
        if let Some(data) = *jpeg {
            let len = buf.len().min(data.len().saturating_sub(offset as usize));
            buf[..len].copy_from_slice(&data[offset as usize..offset as usize + len]);
            Ok(len)
        } else {
            Err(VfsError::ConnectionRefused)
        }
    }

    /// Writes data to the device (not supported for UVC)
    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        trace!("[uvc]: write_at called");
        Err(VfsError::InvalidInput)
    }

    /// Handles ioctl commands for the device
    fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        match cmd {
            // 查询设备能力，返回一个 v4l2_capability 结构体
            VIDIOC_QUERYCAP => {
                let driver: [u8; 16] = {
                    let data = b"uvcvideo";
                    let mut driver = [0u8; 16];
                    driver[..data.len()].copy_from_slice(data);
                    driver
                };
                let card: [u8; 32] = {
                    let data = b"Integrated Camera: Integrated C";
                    let mut card = [0u8; 32];
                    card[..data.len()].copy_from_slice(data);
                    card
                };
                let bus_info: [u8; 32] = {
                    let data = b"platform:uvcvideo";
                    let mut bus_info = [0u8; 32];
                    bus_info[..data.len()].copy_from_slice(data);
                    bus_info
                };

                let cap = v4l2_capability {
                    driver,
                    card,
                    bus_info,
                    version: 1,
                    capabilities: 0x84a00001,
                    device_caps: 0,
                    reserved: [0; 3],
                };
                (arg as *mut v4l2_capability).vm_write(cap)?;
            }
            // 枚举设备支持的像素格式，返回一个 v4l2_fmtdesc 结构体
            VIDIOC_ENUM_FMT => todo!(),
            // 枚举设备支持的帧大小，返回一个 v4l2_frmsizeenum 结构体
            VIDIOC_ENUM_FRAMESIZES => todo!(),
            // 获取当前视频格式，返回一个 v4l2_format 结构体
            VIDIOC_G_FMT => todo!(),
            // 设置视频格式，参数是一个 v4l2_format 结构体
            VIDIOC_S_FMT => todo!(),
            // 请求缓冲区，参数是一个 v4l2_requestbuffers 结构体，返回实际分配的缓冲区数量
            VIDIOC_REQBUFS => todo!(),
            // 查询缓冲区状态，参数是一个 v4l2_buffer 结构体，返回缓冲区的物理地址和长度
            VIDIOC_QUERYBUF => todo!(),
            // 将缓冲区放入队列，参数是一个 v4l2_buffer 结构体，表示缓冲区的索引和状态
            VIDIOC_QBUF => todo!(),
            // 从队列中取出一个缓冲区，参数是一个 v4l2_buffer 结构体，返回缓冲区的索引和状态
            VIDIOC_DQBUF => todo!(),
            // 查询亮度/对比度/白平衡等
            VIDIOC_G_CTRL => todo!(),
            // 设置亮度/对比度/白平衡等
            VIDIOC_S_CTRL => todo!(),
            // 启动视频流，参数是一个 v4l2_buf_type 枚举值，表示缓冲区类型
            VIDIOC_STREAMON => todo!(),
            // 停止视频流，参数是一个 v4l2_buf_type 枚举值，表示缓冲区类型
            VIDIOC_STREAMOFF => todo!(),
            _ => return Err(VfsError::InvalidInput),
        }
        Ok(0)
    }

    /// Returns a reference to the object as Any for dynamic type checking
    fn as_any(&self) -> &dyn Any {
        self
    }

    /// Returns the node flags for the device
    fn flags(&self) -> NodeFlags {
        // 不支持使用 Read/Write 访问设备，只能通过 ioctl 进行交互，因此不设置 STREAM 标志
        NodeFlags::empty()
    }

    /// Maps an exported GEM buffer selected by `handle << PAGE_SHIFT`.
    fn mmap(&self, offset: u64) -> DeviceMmap {
        DeviceMmap::None
    }
}
