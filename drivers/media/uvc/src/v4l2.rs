//! UVC V4L2 ioctl 分发 — 面向 [`UvcDevice`] 的 `IoctlOps` + `V4L2DriverOps`。

use alloc::{sync::Arc, vec::Vec};
use core::{future::poll_fn, task::Poll};

use ax_task::future::block_on;
use axpoll::{IoEvents, PollSet};
use log::*;
use v4l2_core::{
    IoctlOps, V4L2DriverOps, V4l2Error,
    filehandler::V4l2Fh,
    interface::{
        buffer,
        capability::{Capabilities, Capability},
        colorspace,
        common::{BufType, Field, Memory, Timeval},
        event::{CtrlChange, EventSubscription},
        format::{
            self, Fmtdesc, Format, FrameIntervalEnum, FrameIntervalType, FrameSizeEnum,
            FrameSizeType,
        },
        stream::{StreamParm, StreamParmCap},
    },
};

use crate::{UvcDevice, UvcHandle, VideoFormat};

const PIX_FMT_MJPEG: u32 = 0x47504a4d; // 'MJPG'
const PIX_FMT_YUYV: u32 = 0x56595559; // 'YUYV'

// ── V4L2DriverOps 实现 ────────────────────────────────────────────────────

impl<H: UvcHandle> V4L2DriverOps for UvcDevice<H> {
    fn mmap(&self, offset: u64, length: u64) -> Option<(Vec<usize>, usize)> {
        self.queue.mmap(offset, length)
    }

    fn is_readable(&self) -> bool {
        self.queue.is_readable()
    }

    fn is_error(&self) -> bool {
        self.queue.is_error()
    }

    fn vb_poll_set(&self) -> Option<Arc<PollSet>> {
        Some(self.queue.vb_poll_set().clone())
    }

    fn release(&self) {
        // 停止流（close_stream）+ vb2 清理。
        self.close_stream();
        self.queue.streamoff();
        *self.state.lock() = crate::UvcDeviceState::Configured;
    }
}

// ── LegacyIoctlOps 实现 ──────────────────────────────────────────────────

impl<H: UvcHandle> v4l2_core::LegacyIoctlOps for UvcDevice<H> {}

// ── IoctlOps 实现 ─────────────────────────────────────────────────────────

impl<H: UvcHandle> IoctlOps for UvcDevice<H> {
    fn querycap(&self, cap: &mut Capability) -> v4l2_core::Result<()> {
        let driver = b"uvc\0\0\0\0\0\0\0\0\0\0\0\0\0";
        let card = b"Starry UVC Camera\0\0\0\0\0\0\0\0\0\0\0\0\0\0";
        // bus_info 必须用标准前缀（usb-/pci-/platform-…）：v4l2-compliance
        // 的 bus_info 检查只认前缀列表（Linux uvcvideo 为 "usb-<bus>-<port>"）。
        let bus = b"usb-sg2002\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0";

        // capabilities 必须含 DEVICE_CAPS 位（声明 device_caps 字段有效）——
        // v4l2-compliance 682 行 `!(caps & V4L2_CAP_DEVICE_CAPS)` 检查；
        // EXT_PIX_FORMAT（0x00200000）声明支持扩展像素格式，Linux uvcvideo
        // 亦设置（compliance 688 行检查）。
        cap.capabilities = Capabilities::VIDEO_CAPTURE
            | Capabilities::STREAMING
            | Capabilities::DEVICE_CAPS
            | Capabilities::EXT_PIX_FORMAT;
        cap.device_caps = Capabilities::VIDEO_CAPTURE | Capabilities::STREAMING;

        cap.driver[..driver.len()].copy_from_slice(driver);
        cap.card[..card.len()].copy_from_slice(card);
        cap.bus_info[..bus.len()].copy_from_slice(bus);
        // version 高 16 位 = 主版本号。v4l2-compliance 要求 >= 3（674 行
        // `(vcap.version >> 16) < 3`）；0x00060000 = 6.0.0。
        cap.version = 0x00060000;
        // reserved 必须清零：结构体从用户态复制进来，残留垃圾值会被
        // v4l2-compliance 的 check_0(vcap.reserved) 判 FAIL。
        cap.reserved = [0; 3];

        Ok(())
    }

    fn enum_fmt(&self, f: &mut Fmtdesc) -> v4l2_core::Result<()> {
        let format_index = f.index as usize;
        let format = self
            .formats
            .get(format_index)
            .ok_or(V4l2Error::InvalidArgument)?;

        let description = format.description();
        let desc_bytes = description.as_bytes();
        let max_len = desc_bytes.len().min(f.description.len());
        f.pixelformat = format.pixelformat();
        f.description[..max_len].copy_from_slice(&desc_bytes[..max_len]);
        f.flags = if format.is_compressed() {
            format::FmtFlag::COMPRESSED
        } else {
            format::FmtFlag::empty()
        };

        Ok(())
    }

    fn enum_framesizes(&self, f: &mut FrameSizeEnum) -> v4l2_core::Result<()> {
        let pixel_format = f.pixel_format;
        let format = self
            .formats
            .iter()
            .find(|fmt| fmt.pixelformat() == pixel_format)
            .ok_or(V4l2Error::InvalidArgument)?;

        f.ty = FrameSizeType::Discrete;
        f.size.discrete.width = format.width as u32;
        f.size.discrete.height = format.height as u32;
        Ok(())
    }

    fn enum_frameintervals(&self, f: &mut FrameIntervalEnum) -> v4l2_core::Result<()> {
        // 从 descriptor 取该格式的默认帧率（对齐 Linux uvcvideo 返回实际帧间隔，
        // 不硬编码 30fps——YUYV 640x480 实测 ~17fps）。
        let fps = self
            .formats
            .iter()
            .find(|fmt| fmt.pixelformat() == f.pixel_format)
            .map(|fmt| fmt.frame_rate)
            .filter(|&fps| fps != 0)
            .unwrap_or(30);
        f.ty = FrameIntervalType::Discrete;
        f.interval.discrete.numerator = 1;
        f.interval.discrete.denominator = fps;
        Ok(())
    }

    fn g_fmt(&self, f: &mut Format) -> v4l2_core::Result<()> {
        if f.ty != BufType::VideoCapture {
            return Err(V4l2Error::InvalidArgument);
        }
        let current = self
            .current_format
            .as_ref()
            .ok_or(V4l2Error::AccessDenied)?;

        f.ty = BufType::VideoCapture;
        f.fmt.pix.width = current.width as u32;
        f.fmt.pix.height = current.height as u32;
        f.fmt.pix.pixelformat = current.pixelformat();
        f.fmt.pix.field = Field::NoField;
        f.fmt.pix.bytesperline = current.bytes_per_line() as u32;
        f.fmt.pix.sizeimage = current.frame_bytes() as u32;
        f.fmt.pix.colorspace = current.colorspace();
        f.fmt.pix.ycbcr_enc = colorspace::YcbcrEncoding::Default as u32;
        f.fmt.pix.quantization = colorspace::Quantization::FullRange;
        f.fmt.pix.xfer_func = colorspace::XferFunc::Default;
        Ok(())
    }

    fn s_fmt(&mut self, f: &mut Format) -> v4l2_core::Result<()> {
        if f.ty != BufType::VideoCapture {
            return Err(V4l2Error::InvalidArgument);
        }
        let pix = unsafe { f.fmt.pix };
        let width = pix.width as u16;
        let height = pix.height as u16;
        let pixelformat = pix.pixelformat;

        if pixelformat != PIX_FMT_MJPEG && pixelformat != PIX_FMT_YUYV {
            return Err(V4l2Error::InvalidArgument);
        }

        let format = VideoFormat {
            format_type: pixelformat.into(),
            width,
            height,
            frame_rate: self
                .current_format
                .as_ref()
                .map(|f| f.frame_rate)
                .unwrap_or(30),
            format_index: 0,
            frame_index: 0,
        };

        self.set_format(format)
            .map_err(|_| V4l2Error::InvalidArgument)?;

        let current = self.current_format.as_ref().unwrap();
        f.fmt.pix.width = current.width as u32;
        f.fmt.pix.height = current.height as u32;
        f.fmt.pix.pixelformat = current.pixelformat();
        f.fmt.pix.field = Field::NoField;
        f.fmt.pix.bytesperline = current.bytes_per_line() as u32;
        f.fmt.pix.sizeimage = current.frame_bytes() as u32;
        f.fmt.pix.colorspace = current.colorspace();
        f.fmt.pix.ycbcr_enc = colorspace::YcbcrEncoding::Default as u32;
        f.fmt.pix.quantization = colorspace::Quantization::FullRange;
        f.fmt.pix.xfer_func = colorspace::XferFunc::Default;
        Ok(())
    }

    fn try_fmt(&self, f: &mut Format) -> v4l2_core::Result<()> {
        if f.ty != BufType::VideoCapture {
            return Err(V4l2Error::InvalidArgument);
        }
        let pix = unsafe { f.fmt.pix };
        let pixelformat = pix.pixelformat;
        // 保留用户请求的像素格式（若支持）——对齐 Linux TRY_FMT 语义：驱动只
        // 调整不支持的字段，不擅自改格式类型。与 s_fmt 支持集合一致。
        if pixelformat != PIX_FMT_MJPEG && pixelformat != PIX_FMT_YUYV {
            return Err(V4l2Error::InvalidArgument);
        }
        // 用 descriptor 中该格式的帧参数回填（优先精确分辨率，退化为首个同格式
        // 帧）——对齐 s_fmt 经 find_format_indices 的实际协商结果。未解析到
        // 格式时退化为请求值 clamp。
        let negotiated = self
            .formats
            .iter()
            .find(|fmt| {
                fmt.pixelformat() == pixelformat
                    && fmt.width as u32 == pix.width
                    && fmt.height as u32 == pix.height
            })
            .or_else(|| {
                self.formats
                    .iter()
                    .find(|fmt| fmt.pixelformat() == pixelformat)
            });
        let (w, h, sizeimage, bytesperline, colorspace) = match negotiated {
            Some(fmt) => (
                fmt.width as u32,
                fmt.height as u32,
                fmt.frame_bytes() as u32,
                fmt.bytes_per_line() as u32,
                fmt.colorspace(),
            ),
            None => (
                pix.width.clamp(160, 1920),
                pix.height.clamp(120, 1080),
                pix.width * pix.height * 2,
                0,
                colorspace::Colorspace::Srgb,
            ),
        };
        f.fmt.pix.width = w;
        f.fmt.pix.height = h;
        f.fmt.pix.pixelformat = pixelformat;
        f.fmt.pix.field = Field::NoField;
        f.fmt.pix.bytesperline = bytesperline;
        f.fmt.pix.sizeimage = sizeimage;
        f.fmt.pix.colorspace = colorspace;
        f.fmt.pix.ycbcr_enc = colorspace::YcbcrEncoding::Default as u32;
        f.fmt.pix.quantization = colorspace::Quantization::FullRange;
        f.fmt.pix.xfer_func = colorspace::XferFunc::Default;
        Ok(())
    }

    fn reqbufs(&mut self, req: &mut buffer::Requestbuffers) -> v4l2_core::Result<()> {
        if req.memory != Memory::Mmap {
            return Err(V4l2Error::InvalidArgument);
        }
        let sizeimage = self
            .current_format
            .as_ref()
            .map(|f| f.frame_bytes() as u32)
            .unwrap_or(300 * 1024);
        let q = &self.queue;
        q.reqbufs(req.count, &[sizeimage]).inspect_err(|e| {
            error!(
                "[UVC] reqbufs: count={} size={} failed: {:?}",
                req.count, sizeimage, e
            );
        })?;
        req.count = q.num_buffers();
        req.capabilities = buffer::BufCapabilities::SUPPORTS_MMAP;
        info!(
            "[UVC] reqbufs: count={} size={} done ({} buffers)",
            req.count,
            sizeimage,
            q.num_buffers()
        );
        Ok(())
    }

    fn querybuf(&self, buf: &mut buffer::Buffer) -> v4l2_core::Result<()> {
        let q = &self.queue;
        let vb = q
            .buffer_snapshot(buf.index)
            .ok_or(V4l2Error::InvalidArgument)?;

        buf.length = vb.planes.first().map(|p| p.length).unwrap_or(0);
        buf.flags = buffer::BufFlags::MAPPED;
        buf.memory = Memory::Mmap;
        buf.m.offset = q
            .buffer_snapshot(buf.index)
            .and_then(|vb| vb.planes.first().map(|p| p.offset as u32))
            .unwrap_or(0);
        Ok(())
    }

    fn qbuf(&mut self, buf: &mut buffer::Buffer) -> v4l2_core::Result<()> {
        self.queue.qbuf(buf.index)?;
        buf.flags = buffer::BufFlags::QUEUED;
        Ok(())
    }

    fn dqbuf(&mut self, buf: &mut buffer::Buffer) -> v4l2_core::Result<()> {
        let q = &self.queue;
        // 对齐 Linux `__vb2_wait_for_done_vb` 循环入口检查顺序：
        // `!streaming → -EINVAL`（"streaming off, will not wait for
        // buffers"）；`error → -EIO`。快路径短路：从未 STREAMON 或
        // STREAMOFF 后的 DQBUF 立即返回，绝不睡眠——否则等待条件
        // readable/error 永假，阻塞挂死。
        if !q.is_streaming() {
            return Err(V4l2Error::InvalidArgument);
        }
        if q.is_error() {
            return Err(V4l2Error::Io);
        }
        // 阻塞等待采集完成帧。等待源是 vb2 队列内建的 vb_poll_set
        // （buffer_done/set_error/streamoff 发布状态后唤醒，IRQ 安全）——
        // DQBUF 阻塞与 VFS poll 共用同一唤醒源（对齐 Linux vb2 done_wq）。
        // 等待条件对齐 Linux：done 非空 || 队列错误 || 停流。
        if !q.is_readable() {
            let wait = poll_fn(|cx| {
                if q.is_readable() || q.is_error() || !q.is_streaming() {
                    return Poll::Ready(());
                }
                // SAFETY: ioctl 任务上下文；register 不持队列锁（is_readable
                // 的锁已释放），满足 PollSet::register 的上下文与锁约束。
                unsafe {
                    q.vb_poll_set()
                        .register(cx.waker(), IoEvents::IN | IoEvents::ERR)
                };
                if q.is_readable() || q.is_error() || !q.is_streaming() {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            });
            block_on(wait);
            // 唤醒后重查（Linux 循环回到入口）：等待期间 STREAMOFF →
            // EINVAL；置错 → EIO。
            if !q.is_streaming() {
                return Err(V4l2Error::InvalidArgument);
            }
            if q.is_error() {
                return Err(V4l2Error::Io);
            }
        }
        let idx = q.dqbuf()?;
        let vb = q.buffer_snapshot(idx).ok_or(V4l2Error::InvalidArgument)?;
        let (bytesused, sequence, timestamp, timestamp_flags) =
            (vb.bytesused, vb.sequence, vb.timestamp, vb.timestamp_flags);

        buf.index = idx;
        buf.flags = buffer::BufFlags::DONE
            | buffer::BufFlags::KEYFRAME
            | buffer::BufFlags::from_bits_retain(timestamp_flags);
        buf.bytesused = bytesused;
        buf.timestamp = Timeval {
            tv_sec: (timestamp / 1_000_000_000) as i64,
            tv_usec: ((timestamp / 1_000) % 1_000_000) as i64,
        };
        buf.field = Field::NoField;
        buf.sequence = sequence;
        buf.memory = Memory::Mmap;
        Ok(())
    }

    fn streamon(&mut self, ty: BufType) -> v4l2_core::Result<()> {
        if ty != BufType::VideoCapture {
            return Err(V4l2Error::InvalidArgument);
        }
        self.start_streaming().map_err(|_| V4l2Error::Io)?;
        self.queue.streamon()?;
        Ok(())
    }

    fn streamoff(&mut self, ty: BufType) -> v4l2_core::Result<()> {
        if ty != BufType::VideoCapture {
            return Err(V4l2Error::InvalidArgument);
        }
        // 先停 worker（join）再清队列——避免 worker 与队列状态机并发（buffer_done
        // 对非 Active 缓冲是 no-op，但 join 保证顺序干净）。
        self.close_stream();
        self.queue.streamoff();
        Ok(())
    }

    fn g_parm(&self, p: &mut StreamParm) -> v4l2_core::Result<()> {
        let cap = unsafe { &mut p.parm.capture };
        cap.capability = StreamParmCap::TIMEPERFRAME;
        cap.timeperframe.numerator = 1;
        cap.timeperframe.denominator = self
            .current_format
            .as_ref()
            .map(|f| f.frame_rate)
            .filter(|&fps| fps != 0)
            .unwrap_or(30);
        cap.readbuffers = 8;
        Ok(())
    }

    fn s_parm(&mut self, _p: &StreamParm) -> v4l2_core::Result<()> {
        Ok(())
    }

    // ── 控件（UVC 硬件代理，注册在 UvcDevice::ctrls）──────────────

    fn queryctrl(&self, q: &mut v4l2_core::interface::ctrl::QueryCtrl) -> v4l2_core::Result<()> {
        self.ctrls.queryctrl(q)
    }

    fn query_ext_ctrl(
        &self,
        q: &mut v4l2_core::interface::ctrl::QueryExtCtrl,
    ) -> v4l2_core::Result<()> {
        self.ctrls.query_ext_ctrl(q)
    }

    fn querymenu(&self, q: &mut v4l2_core::interface::ctrl::Querymenu) -> v4l2_core::Result<()> {
        self.ctrls.querymenu(q)
    }

    fn g_ctrl(&self, ctrl: &mut v4l2_core::interface::ctrl::Control) -> v4l2_core::Result<()> {
        self.ctrls.g_ctrl(ctrl)
    }

    fn s_ctrl(&mut self, ctrl: &v4l2_core::interface::ctrl::Control) -> v4l2_core::Result<()> {
        if self.ctrls.s_ctrl(ctrl)?.is_some()
            && let Some(ev) = self.ctrls.change_event(ctrl.id, CtrlChange::VALUE)
        {
            self.events.lock().push(ev);
        }
        Ok(())
    }

    fn subscribe_event(
        &mut self,
        fh: &mut V4l2Fh,
        sub: &EventSubscription,
    ) -> v4l2_core::Result<()> {
        self.ctrls.subscribe_event(fh, sub)
    }
}

#[cfg(test)]
mod tests {
    use v4l2_core::interface::{buffer::Buffer, common};

    use super::*;
    use crate::{helper::test_util::build_uvc_blob, stream::test_util::MockUvc};

    fn make_device() -> UvcDevice<MockUvc> {
        let blob = build_uvc_blob(0x01, 0x02);
        UvcDevice::new(MockUvc {}, &blob).unwrap()
    }

    /// 构造全零输入的 UAPI Buffer（dqbuf 只写输出字段、不读输入字段）。
    fn zeroed_buffer() -> Buffer {
        Buffer {
            index: 0,
            ty: BufType::VideoCapture,
            bytesused: 0,
            flags: buffer::BufFlags::empty(),
            field: Field::Any,
            timestamp: Timeval {
                tv_sec: 0,
                tv_usec: 0,
            },
            timecode: common::Timecode {
                ty: 0,
                flags: 0,
                frames: 0,
                seconds: 0,
                minutes: 0,
                hours: 0,
                userbits: [0; 4],
            },
            sequence: 0,
            memory: Memory::Mmap,
            m: buffer::BufferM { offset: 0 },
            length: 0,
            reserved2: 0,
            request_fd: 0,
        }
    }

    /// 回归：从未 STREAMON 时 DQBUF 必须立即返回 EINVAL，不得永久睡眠。
    /// 对齐 Linux `__vb2_wait_for_done_vb`：`!q->streaming → -EINVAL`
    /// （"streaming off, will not wait for buffers"）。
    /// 修复前：等待条件 readable/error 永假 → 阻塞挂死（内核上永久
    /// 睡眠；host 测试上 block_on 因无调度器 panic）。
    /// STREAMOFF 后场景走同一检查点（同一 `!is_streaming` 分支），
    /// 停流唤醒由 videobuffer 的 `streamoff_wakes_waiters` 回归覆盖。
    #[test]
    fn dqbuf_without_streaming_returns_einval_not_hang() {
        let mut dev = make_device();
        let mut buf = zeroed_buffer();

        let err = IoctlOps::dqbuf(&mut dev, &mut buf).unwrap_err();

        assert_eq!(err, V4l2Error::InvalidArgument);
    }
}
