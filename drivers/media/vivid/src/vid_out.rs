//! vivid-vid-out — 视频输出节点（镜像 Linux vivid-vid-out.c）
//!
//! 为 V4L2 输出设备实现 `IoctlOps` + `V4L2DriverOps`，
//! 接受来自用户空间的帧。

use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicU32, Ordering};

use axpoll::PollSet;
use v4l2_core::{
    self, IoctlOps, V4L2DriverOps,
    error::V4l2Error,
    interface::{
        buffer::{BufFlags, Buffer, Requestbuffers},
        capability::{Capabilities, Capability},
        colorspace::Colorspace,
        common::{BufType, Field, Memory, Timeval},
        format::{self, Fmtdesc, Format, FrameIntervalEnum, FrameSizeEnum},
        inout::{self, OutputType},
        stream::StreamParm,
    },
};
use videobuffer::{Vb2Queue, VirtualAllocator};

use crate::vid_common;

/// Vivid 视频输出设备。
pub struct VividOutput {
    fmt_width: AtomicU32,
    fmt_height: AtomicU32,
    fmt_pixelformat: AtomicU32,
    fmt_bytesperline: AtomicU32,
    fmt_sizeimage: AtomicU32,
    interval_num: AtomicU32,
    interval_den: AtomicU32,
    /// vb2 队列（缓冲生命周期、状态机、mmap 解码全在此）。
    queue: Vb2Queue<VirtualAllocator>,
}

impl VividOutput {
    pub fn new() -> Self {
        Self {
            fmt_width: AtomicU32::new(640),
            fmt_height: AtomicU32::new(480),
            fmt_pixelformat: AtomicU32::new(0x56595559),
            fmt_bytesperline: AtomicU32::new(1280),
            fmt_sizeimage: AtomicU32::new(614400),
            interval_num: AtomicU32::new(1),
            interval_den: AtomicU32::new(30),
            queue: Vb2Queue::new(VirtualAllocator::new(), 1, 4),
        }
    }
}

impl Default for VividOutput {
    fn default() -> Self {
        Self::new()
    }
}

impl IoctlOps for VividOutput {
    fn querycap(&self, cap: &mut Capability) -> v4l2_core::Result<()> {
        // EXT_PIX_FORMAT：声明支持扩展像素格式（Linux vivid 亦设置，
        // v4l2-compliance 688 行检查）。
        cap.capabilities = Capabilities::VIDEO_OUTPUT
            | Capabilities::STREAMING
            | Capabilities::DEVICE_CAPS
            | Capabilities::EXT_PIX_FORMAT;
        cap.device_caps = Capabilities::VIDEO_OUTPUT | Capabilities::STREAMING;
        let driver = b"vivid\0\0\0\0\0\0\0\0\0\0\0";
        let card = b"vivid-vid-out\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0";
        let bus = b"platform:vivid\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0";
        cap.driver[..driver.len()].copy_from_slice(driver);
        cap.card[..card.len()].copy_from_slice(card);
        cap.bus_info[..bus.len()].copy_from_slice(bus);
        cap.version = 0x00060000;
        cap.reserved = [0; 3];
        Ok(())
    }

    fn enum_fmt(&self, f: &mut Fmtdesc) -> v4l2_core::Result<()> {
        vid_common::enum_format(f)
    }

    fn enum_framesizes(&self, f: &mut FrameSizeEnum) -> v4l2_core::Result<()> {
        let sizes = vid_common::SUPPORTED_SIZES;
        if f.index as usize >= sizes.len() {
            return Err(V4l2Error::InvalidArgument);
        }
        let (w, h) = sizes[f.index as usize];
        f.ty = format::FrameSizeType::Discrete;
        f.size.discrete.width = w;
        f.size.discrete.height = h;
        Ok(())
    }

    fn enum_frameintervals(&self, f: &mut FrameIntervalEnum) -> v4l2_core::Result<()> {
        let max_den = vid_common::max_fps_for_size(f.width, f.height);
        let intervals: Vec<_> = vid_common::SUPPORTED_INTERVALS
            .iter()
            .filter(|&&(_, d)| d <= max_den)
            .collect();
        if f.index as usize >= intervals.len() {
            return Err(V4l2Error::InvalidArgument);
        }
        let &(num, den) = intervals[f.index as usize];
        f.ty = format::FrameIntervalType::Discrete;
        f.interval.discrete.numerator = num;
        f.interval.discrete.denominator = den;
        Ok(())
    }

    fn g_fmt(&self, f: &mut Format) -> v4l2_core::Result<()> {
        if f.ty != BufType::VideoOutput {
            return Err(V4l2Error::InvalidArgument);
        }
        f.ty = BufType::VideoOutput;
        vid_common::fill_g_fmt(
            f,
            self.fmt_width.load(Ordering::Acquire),
            self.fmt_height.load(Ordering::Acquire),
            self.fmt_pixelformat.load(Ordering::Acquire),
            self.fmt_bytesperline.load(Ordering::Acquire),
            self.fmt_sizeimage.load(Ordering::Acquire),
        );
        Ok(())
    }

    fn s_fmt(&mut self, f: &mut Format) -> v4l2_core::Result<()> {
        let w = unsafe { f.fmt.pix.width };
        let h = unsafe { f.fmt.pix.height };
        let pf = unsafe { f.fmt.pix.pixelformat };
        let (w, h, pf, _fmt) = vid_common::validate_format(w, h, pf)?;
        let (bpl, sz) =
            vid_common::compute_line_size(pf, w, h).ok_or(V4l2Error::InvalidArgument)?;
        self.fmt_width.store(w, Ordering::Release);
        self.fmt_height.store(h, Ordering::Release);
        self.fmt_pixelformat.store(pf, Ordering::Release);
        self.fmt_bytesperline.store(bpl, Ordering::Release);
        self.fmt_sizeimage.store(sz, Ordering::Release);
        f.fmt.pix.bytesperline = bpl;
        f.fmt.pix.sizeimage = sz;
        Ok(())
    }

    fn try_fmt(&self, f: &mut Format) -> v4l2_core::Result<()> {
        let w = unsafe { f.fmt.pix.width };
        let h = unsafe { f.fmt.pix.height };
        let pf = unsafe { f.fmt.pix.pixelformat };
        let pf = if vid_common::fmt_by_fourcc(pf).is_some() {
            pf
        } else {
            0x56595559
        };
        let (w, h, pf, _fmt) = vid_common::validate_format(w, h, pf)?;
        let (bpl, sz) = vid_common::compute_line_size(pf, w, h).unwrap_or((w * 2, w * h * 2));
        f.fmt.pix.width = w;
        f.fmt.pix.height = h;
        f.fmt.pix.pixelformat = pf;
        f.fmt.pix.field = Field::NoField;
        f.fmt.pix.bytesperline = bpl;
        f.fmt.pix.sizeimage = sz;
        f.fmt.pix.colorspace = Colorspace::Srgb;
        Ok(())
    }

    fn g_parm(&self, parm: &mut StreamParm) -> v4l2_core::Result<()> {
        let out = unsafe { &mut parm.parm.output };
        out.timeperframe.numerator = self.interval_num.load(Ordering::Acquire);
        out.timeperframe.denominator = self.interval_den.load(Ordering::Acquire);
        out.writebuffers = 4;
        Ok(())
    }

    fn s_parm(&mut self, parm: &StreamParm) -> v4l2_core::Result<()> {
        let num = unsafe { parm.parm.output.timeperframe.numerator };
        let den = unsafe { parm.parm.output.timeperframe.denominator };
        let w = self.fmt_width.load(Ordering::Acquire);
        let h = self.fmt_height.load(Ordering::Acquire);
        let (num, den) =
            vid_common::clamp_interval(num, den, w, h).ok_or(V4l2Error::InvalidArgument)?;
        self.interval_num.store(num, Ordering::Release);
        self.interval_den.store(den, Ordering::Release);
        Ok(())
    }

    fn reqbufs(&mut self, req: &mut Requestbuffers) -> v4l2_core::Result<()> {
        if req.memory != Memory::Mmap {
            return Err(V4l2Error::InvalidArgument);
        }
        let q = &self.queue;
        let sizeimage = self.fmt_sizeimage.load(Ordering::Acquire);
        q.reqbufs(req.count, &[sizeimage])?;
        req.capabilities = v4l2_core::interface::buffer::BufCapabilities::SUPPORTS_MMAP;
        req.count = q.num_buffers(); // 协商后的实际缓冲数
        Ok(())
    }

    fn querybuf(&self, buf: &mut Buffer) -> v4l2_core::Result<()> {
        let q = &self.queue;
        let vb = q
            .buffer_snapshot(buf.index)
            .ok_or(V4l2Error::InvalidArgument)?;
        let plane = vb.planes.first().ok_or(V4l2Error::InvalidArgument)?;
        buf.length = plane.length;
        buf.flags = BufFlags::MAPPED;
        buf.memory = Memory::Mmap;
        buf.m.offset = q
            .buffer_snapshot(buf.index)
            .and_then(|vb| vb.planes.first().map(|p| p.offset as u32))
            .unwrap_or(0);
        Ok(())
    }

    fn qbuf(&mut self, buf: &mut Buffer) -> v4l2_core::Result<()> {
        let q = &self.queue;
        q.qbuf(buf.index)?;
        buf.flags = BufFlags::QUEUED;
        // output 设备无真实硬件消费：入队后立即完成（对齐 Linux vivid
        // 同步完成语义），使 DQBUF 立即可取回。
        let sizeimage = self.fmt_sizeimage.load(Ordering::Acquire);
        q.buffer_done(buf.index, videobuffer::BufferState::Done, sizeimage, 0)?;
        Ok(())
    }

    fn dqbuf(&mut self, buf: &mut Buffer) -> v4l2_core::Result<()> {
        let q = &self.queue;
        let idx = q.dqbuf()?;
        let vb = q.buffer_snapshot(idx).ok_or(V4l2Error::InvalidArgument)?;
        buf.index = idx;
        buf.flags = BufFlags::DONE
            | BufFlags::KEYFRAME
            // 时间戳源标志（buffer_done 时由队列填 CLOCK_MONOTONIC）。
            | BufFlags::from_bits_retain(vb.timestamp_flags);
        buf.bytesused = self.fmt_sizeimage.load(Ordering::Acquire);
        buf.timestamp = Timeval {
            tv_sec: (vb.timestamp / 1_000_000_000) as i64,
            tv_usec: ((vb.timestamp / 1_000) % 1_000_000) as i64,
        };
        buf.sequence = vb.sequence;
        buf.field = Field::NoField;
        buf.memory = Memory::Mmap;
        Ok(())
    }

    fn streamon(&mut self, _ty: BufType) -> v4l2_core::Result<()> {
        self.queue.streamon()
    }

    fn streamoff(&mut self, _ty: BufType) -> v4l2_core::Result<()> {
        self.queue.streamoff();
        Ok(())
    }

    fn enum_output(&self, output: &mut inout::Output) -> v4l2_core::Result<()> {
        if output.index != 0 {
            return Err(V4l2Error::InvalidArgument);
        }
        let name = b"Output\0";
        let len = name.len().min(31);
        output.name[..len].copy_from_slice(&name[..len]);
        output.ty = OutputType::Analog;
        Ok(())
    }

    fn g_output(&self) -> v4l2_core::Result<u32> {
        Ok(0)
    }

    fn s_output(&mut self, index: u32) -> v4l2_core::Result<()> {
        if index != 0 {
            return Err(V4l2Error::InvalidArgument);
        }
        Ok(())
    }
}

impl v4l2_core::LegacyIoctlOps for VividOutput {}

impl V4L2DriverOps for VividOutput {
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
        self.queue.reqbufs(0, &[]).ok();
    }
}
