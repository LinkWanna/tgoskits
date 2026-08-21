//! vivid-vid-cap — 视频采集节点（镜像 Linux vivid-vid-cap.c）
//!
//! 实现 `IoctlOps` + `V4L2DriverOps`，用于格式协商、
//! 控件、裁剪、输入选择以及缓冲管理
//! （基于 Vb2Queue；STREAMON 同步预填充测试图案）。

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use axpoll::PollSet;
use v4l2_core::{
    self, IoctlOps, V4L2DriverOps,
    ctrls::{CtrlHandler, class::UserClassCtrl},
    error::V4l2Error,
    filehandler::V4l2Fh,
    interface::{
        BufType, Field, Memory, Timeval,
        buffer::{BufFlags, Buffer, Requestbuffers},
        capability::{Capabilities, Capability},
        colorspace::{Colorspace, Quantization, XferFunc, YcbcrEncoding},
        crop::{Crop, Cropcap, Selection, SelectionTarget},
        event::{Event, EventSubscription},
        format::{
            Fmtdesc, Format, FrameIntervalEnum, FrameIntervalType, FrameSizeEnum, FrameSizeType,
        },
        inout::InputType,
        stream::StreamParm,
    },
};
use videobuffer::{BufferState, Vb2Queue, VirtualAllocator};

use crate::{
    ctrls::{TEST_PATTERN_NAMES, VividCtrl},
    tpg::{self, Pattern, PixelFormat},
    vid_common,
};

// ── 共享状态（Arc 共享） ────────────

/// 注册一个静态控件配置；失败属于编程错误（一次性初始化断言）。
fn reg(name: &str, r: v4l2_core::Result<()>) {
    r.expect(name);
}

/// ioctl 路径与填充路径之间共享的格式 + 控件状态。
struct VividCaptureState {
    fmt_width: AtomicU32,
    fmt_height: AtomicU32,
    fmt_pixelformat: AtomicU32,
    fmt_bytesperline: AtomicU32,
    fmt_sizeimage: AtomicU32,
    interval_num: AtomicU32,
    interval_den: AtomicU32,
    crop_left: AtomicU32,
    crop_top: AtomicU32,
    crop_width: AtomicU32,
    crop_height: AtomicU32,
    ctrls: CtrlHandler,
    streaming: AtomicBool,
}

impl VividCaptureState {
    fn new(events: Arc<ax_sync::Mutex<Vec<Event>>>) -> Self {
        let mut ctrls = CtrlHandler::new();
        // 控件配置为静态硬编码，注册失败属于编程错误（expect 为一次性初始化断言）。
        reg(
            "Brightness",
            ctrls.new_int(
                UserClassCtrl::Brightness as u32,
                "Brightness",
                0,
                255,
                1,
                128,
                None,
            ),
        );
        reg(
            "Contrast",
            ctrls.new_int(
                UserClassCtrl::Contrast as u32,
                "Contrast",
                0,
                255,
                1,
                128,
                None,
            ),
        );
        reg(
            "Saturation",
            ctrls.new_int(
                UserClassCtrl::Saturation as u32,
                "Saturation",
                0,
                255,
                1,
                128,
                None,
            ),
        );
        reg(
            "Hue",
            ctrls.new_int(UserClassCtrl::Hue as u32, "Hue", -128, 128, 1, 0, None),
        );
        reg(
            "Autogain",
            ctrls.new_bool(UserClassCtrl::Autogain as u32, "Autogain", false, None),
        );
        reg(
            "Gain",
            ctrls.new_int(UserClassCtrl::Gain as u32, "Gain", 0, 255, 1, 128, None),
        );
        reg(
            "Hflip",
            ctrls.new_bool(UserClassCtrl::Hflip as u32, "Horizontal Flip", false, None),
        );
        reg(
            "Vflip",
            ctrls.new_bool(UserClassCtrl::Vflip as u32, "Vertical Flip", false, None),
        );
        reg(
            "TestPattern",
            ctrls.new_menu(
                VividCtrl::TestPattern as u32,
                "Test Pattern",
                TEST_PATTERN_NAMES.len() as u32,
                0,
                TEST_PATTERN_NAMES,
                None,
            ),
        );
        reg(
            "Disconnect",
            ctrls.new_bool(VividCtrl::Disconnect as u32, "Disconnect", false, None),
        );
        reg(
            "DqbufError",
            ctrls.new_bool(VividCtrl::DqbufError as u32, "DQBUF Error", false, None),
        );
        reg(
            "QueueError",
            ctrls.new_bool(VividCtrl::QueueError as u32, "Queue Error", false, None),
        );
        reg(
            "PercDropped",
            ctrls.new_int(
                VividCtrl::PercDropped as u32,
                "Percentage Dropped",
                0,
                100,
                1,
                0,
                None,
            ),
        );
        // 控件值变化事件由框架统一生成（S_CTRL / S_EXT_CTRLS 应用后触发）。
        ctrls.set_change_notify(Box::new(move |ev| events.lock().push(ev)));

        Self {
            fmt_width: AtomicU32::new(640),
            fmt_height: AtomicU32::new(480),
            fmt_pixelformat: AtomicU32::new(0x56595559),
            fmt_bytesperline: AtomicU32::new(1280),
            fmt_sizeimage: AtomicU32::new(614400),
            interval_num: AtomicU32::new(1),
            interval_den: AtomicU32::new(30),
            crop_left: AtomicU32::new(0),
            crop_top: AtomicU32::new(0),
            crop_width: AtomicU32::new(640),
            crop_height: AtomicU32::new(480),
            ctrls,
            streaming: AtomicBool::new(false),
        }
    }

    fn fmt_tpg(&self) -> PixelFormat {
        match self.fmt_pixelformat.load(Ordering::Acquire) {
            0x33524742 => PixelFormat::Rgb24,
            _ => PixelFormat::Yuyv,
        }
    }

    fn pattern_index(&self) -> Pattern {
        let idx = self
            .ctrls
            .find(VividCtrl::TestPattern as u32)
            .map(|c| c.value())
            .unwrap_or(0) as u32;
        tpg::pattern_from_index(idx).unwrap_or(Pattern::ColorBars)
    }

    fn fmt_dimensions(&self) -> (u32, u32) {
        (
            self.fmt_width.load(Ordering::Acquire),
            self.fmt_height.load(Ordering::Acquire),
        )
    }

    fn fmt_dimensions_and_size(&self) -> (u32, u32, u32) {
        (
            self.fmt_width.load(Ordering::Acquire),
            self.fmt_height.load(Ordering::Acquire),
            self.fmt_sizeimage.load(Ordering::Acquire),
        )
    }

    fn proc_amps(&self) -> (u32, u32, u32, i32) {
        (
            self.ctrls
                .find(UserClassCtrl::Brightness as u32)
                .map(|c| c.value() as u32)
                .unwrap_or(128),
            self.ctrls
                .find(UserClassCtrl::Contrast as u32)
                .map(|c| c.value() as u32)
                .unwrap_or(128),
            self.ctrls
                .find(UserClassCtrl::Saturation as u32)
                .map(|c| c.value() as u32)
                .unwrap_or(128),
            self.ctrls
                .find(UserClassCtrl::Hue as u32)
                .map(|c| c.value() as i32)
                .unwrap_or(0),
        )
    }

    fn update_crop(&self) {
        let w = self.fmt_width.load(Ordering::Acquire);
        let h = self.fmt_height.load(Ordering::Acquire);
        self.crop_width.store(w, Ordering::Release);
        self.crop_height.store(h, Ordering::Release);
        self.crop_left.store(0, Ordering::Release);
        self.crop_top.store(0, Ordering::Release);
    }
}

// ── Vb2Ops 实现 ──────────────────────────────────────────────────────

// ── VividCapture（采集设备） ────────────────────────────────────────────────────

/// Vivid 视频采集设备。
pub struct VividCapture {
    state: Arc<VividCaptureState>,
    queue: Vb2Queue<VirtualAllocator>,
    events: Arc<ax_sync::Mutex<Vec<v4l2_core::interface::event::Event>>>,
}

impl VividCapture {
    pub fn new() -> Self {
        let events = Arc::new(ax_sync::Mutex::new(Vec::new()));
        let state = Arc::new(VividCaptureState::new(Arc::clone(&events)));
        Self {
            state,
            queue: Vb2Queue::new(VirtualAllocator::new(), 2, 4),
            events,
        }
    }

    pub fn event_source(&self) -> Arc<ax_sync::Mutex<Vec<v4l2_core::interface::event::Event>>> {
        Arc::clone(&self.events)
    }
}

impl Default for VividCapture {
    fn default() -> Self {
        Self::new()
    }
}

// ── 缓冲填充辅助 ──────────────────────────────────────────────

pub fn fill_with_proc_amps(
    pat: Pattern,
    fmt: PixelFormat,
    w: u32,
    h: u32,
    frame: u32,
    buf: &mut [u8],
    amps: (u32, u32, u32, i32),
) {
    let (bright, contrast, sat, hue) = amps;
    if matches!(fmt, PixelFormat::Rgb24) {
        for y in 0..h {
            for x in 0..w {
                let c = tpg::pattern_color(pat, x, y, w, h, frame);
                let (r, g, b) =
                    vid_common::apply_proc_amps(c.r, c.g, c.b, bright, contrast, sat, hue);
                let idx = ((y * w + x) * 3) as usize;
                buf[idx] = r;
                buf[idx + 1] = g;
                buf[idx + 2] = b;
            }
        }
    } else {
        crate::tpg::fill_buffer(pat, fmt, w, h, frame, buf);
    }
}

// ── IoctlOps 实现 ────────────────────────────────────────────────────────

impl IoctlOps for VividCapture {
    fn querycap(&self, cap: &mut Capability) -> v4l2_core::Result<()> {
        // EXT_PIX_FORMAT：声明支持扩展像素格式（Linux vivid 亦设置，
        // v4l2-compliance 688 行检查）。
        cap.capabilities = Capabilities::VIDEO_CAPTURE
            | Capabilities::STREAMING
            | Capabilities::DEVICE_CAPS
            | Capabilities::EXT_PIX_FORMAT;
        cap.device_caps = Capabilities::VIDEO_CAPTURE | Capabilities::STREAMING;
        let driver = b"vivid\0\0\0\0\0\0\0\0\0\0\0";
        let card = b"vivid-vid-cap\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0";
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
        f.ty = FrameSizeType::Discrete;
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
        f.ty = FrameIntervalType::Discrete;
        f.interval.discrete.numerator = num;
        f.interval.discrete.denominator = den;
        Ok(())
    }

    fn g_fmt(&self, f: &mut Format) -> v4l2_core::Result<()> {
        if f.ty != BufType::VideoCapture {
            return Err(V4l2Error::InvalidArgument);
        }
        f.ty = BufType::VideoCapture;
        vid_common::fill_g_fmt(
            f,
            self.state.fmt_width.load(Ordering::Acquire),
            self.state.fmt_height.load(Ordering::Acquire),
            self.state.fmt_pixelformat.load(Ordering::Acquire),
            self.state.fmt_bytesperline.load(Ordering::Acquire),
            self.state.fmt_sizeimage.load(Ordering::Acquire),
        );
        Ok(())
    }

    fn s_fmt(&mut self, f: &mut Format) -> v4l2_core::Result<()> {
        if f.ty != BufType::VideoCapture {
            return Err(V4l2Error::InvalidArgument);
        }
        let w = unsafe { f.fmt.pix.width };
        let h = unsafe { f.fmt.pix.height };
        let pf = unsafe { f.fmt.pix.pixelformat };
        let (w, h, pf, _fmt) = vid_common::validate_format(w, h, pf)?;
        let (bpl, sz) =
            vid_common::compute_line_size(pf, w, h).ok_or(V4l2Error::InvalidArgument)?;
        self.state.fmt_width.store(w, Ordering::Release);
        self.state.fmt_height.store(h, Ordering::Release);
        self.state.fmt_pixelformat.store(pf, Ordering::Release);
        self.state.fmt_bytesperline.store(bpl, Ordering::Release);
        self.state.fmt_sizeimage.store(sz, Ordering::Release);
        self.state.update_crop();
        f.fmt.pix.bytesperline = bpl;
        f.fmt.pix.sizeimage = sz;
        Ok(())
    }

    fn try_fmt(&self, f: &mut Format) -> v4l2_core::Result<()> {
        if f.ty != BufType::VideoCapture {
            return Err(V4l2Error::InvalidArgument);
        }
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
        f.fmt.pix.ycbcr_enc = YcbcrEncoding::Default as u32;
        f.fmt.pix.quantization = Quantization::Default;
        f.fmt.pix.xfer_func = XferFunc::Default;
        Ok(())
    }

    fn g_parm(&self, parm: &mut StreamParm) -> v4l2_core::Result<()> {
        let cap = unsafe { &mut parm.parm.capture };
        cap.timeperframe.numerator = self.state.interval_num.load(Ordering::Acquire);
        cap.timeperframe.denominator = self.state.interval_den.load(Ordering::Acquire);
        cap.readbuffers = 4;
        Ok(())
    }

    fn s_parm(&mut self, parm: &StreamParm) -> v4l2_core::Result<()> {
        let num = unsafe { parm.parm.capture.timeperframe.numerator };
        let den = unsafe { parm.parm.capture.timeperframe.denominator };
        let w = self.state.fmt_width.load(Ordering::Acquire);
        let h = self.state.fmt_height.load(Ordering::Acquire);
        let (num, den) =
            vid_common::clamp_interval(num, den, w, h).ok_or(V4l2Error::InvalidArgument)?;
        self.state.interval_num.store(num, Ordering::Release);
        self.state.interval_den.store(den, Ordering::Release);
        Ok(())
    }

    fn cropcap(&self, c: &mut Cropcap) -> v4l2_core::Result<()> {
        let w = self.state.fmt_width.load(Ordering::Acquire);
        let h = self.state.fmt_height.load(Ordering::Acquire);
        c.bounds.left = 0;
        c.bounds.top = 0;
        c.bounds.width = w;
        c.bounds.height = h;
        c.defrect = c.bounds;
        Ok(())
    }

    fn g_selection(&self, s: &mut Selection) -> v4l2_core::Result<()> {
        let fmt_w = self.state.fmt_width.load(Ordering::Acquire);
        let fmt_h = self.state.fmt_height.load(Ordering::Acquire);
        match s.target {
            SelectionTarget::Crop | SelectionTarget::CropDefault | SelectionTarget::CropBounds => {
                s.r.left = self.state.crop_left.load(Ordering::Acquire) as i32;
                s.r.top = self.state.crop_top.load(Ordering::Acquire) as i32;
                s.r.width = self.state.crop_width.load(Ordering::Acquire);
                s.r.height = self.state.crop_height.load(Ordering::Acquire);
                Ok(())
            }
            SelectionTarget::Compose
            | SelectionTarget::ComposeDefault
            | SelectionTarget::ComposeBounds
            | SelectionTarget::ComposePadded => {
                s.r.left = 0;
                s.r.top = 0;
                s.r.width = fmt_w;
                s.r.height = fmt_h;
                Ok(())
            }
            _ => Err(V4l2Error::InvalidArgument),
        }
    }

    fn s_selection(&mut self, s: &Selection) -> v4l2_core::Result<()> {
        match s.target {
            SelectionTarget::Crop | SelectionTarget::CropDefault | SelectionTarget::CropBounds => {
                let fmt_w = self.state.fmt_width.load(Ordering::Acquire);
                let fmt_h = self.state.fmt_height.load(Ordering::Acquire);
                let cw = s.r.width.min(fmt_w);
                let ch = s.r.height.min(fmt_h);
                let cl = s.r.left.max(0).min((fmt_w - cw) as i32) as u32;
                let ct = s.r.top.max(0).min((fmt_h - ch) as i32) as u32;
                self.state.crop_left.store(cl, Ordering::Release);
                self.state.crop_top.store(ct, Ordering::Release);
                self.state.crop_width.store(cw, Ordering::Release);
                self.state.crop_height.store(ch, Ordering::Release);
                Ok(())
            }
            SelectionTarget::Compose
            | SelectionTarget::ComposeDefault
            | SelectionTarget::ComposeBounds
            | SelectionTarget::ComposePadded => Err(V4l2Error::InvalidArgument),
            _ => Err(V4l2Error::InvalidArgument),
        }
    }

    // ── 控件事件（控件查询 / G/S/TRY_EXT_CTRLS 由核心经 CtrlHandler 处理）──

    fn subscribe_event(
        &mut self,
        fh: &mut V4l2Fh,
        sub: &EventSubscription,
    ) -> v4l2_core::Result<()> {
        self.state.ctrls.subscribe_event(fh, sub)
    }

    // ── 视频输入 ─────────────────────────────────────────────────

    fn enum_input(&self, input: &mut v4l2_core::interface::inout::Input) -> v4l2_core::Result<()> {
        if input.index != 0 {
            return Err(V4l2Error::InvalidArgument);
        }
        let name = b"Camera\0";
        let len = name.len().min(31);
        input.name[..len].copy_from_slice(&name[..len]);
        input.ty = InputType::Camera;
        input.status = v4l2_core::interface::inout::InStatus::empty();
        Ok(())
    }

    fn g_input(&self) -> v4l2_core::Result<u32> {
        Ok(0)
    }

    fn s_input(&mut self, index: u32) -> v4l2_core::Result<()> {
        if index != 0 {
            return Err(V4l2Error::InvalidArgument);
        }
        Ok(())
    }

    // ── 缓冲操作 ──────────────────────────────────────────────────

    fn reqbufs(&mut self, req: &mut Requestbuffers) -> v4l2_core::Result<()> {
        if req.memory != Memory::Mmap {
            return Err(V4l2Error::InvalidArgument);
        }
        let q = &self.queue;
        let sizeimage = self.state.fmt_sizeimage.load(Ordering::Acquire);
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
        buf.length = vb.planes.first().map(|p| p.length).unwrap_or(0);
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
        Ok(())
    }

    fn prepare_buf(&mut self, buf: &mut Buffer) -> v4l2_core::Result<()> {
        let q = &self.queue;
        q.prepare_buf(buf.index)?;
        buf.flags = BufFlags::PREPARED;
        buf.memory = Memory::Mmap;
        Ok(())
    }

    fn dqbuf(&mut self, buf: &mut Buffer) -> v4l2_core::Result<()> {
        let q = &self.queue;
        let idx = q.dqbuf()?;

        let vb = q.buffer_snapshot(idx);
        let (plane_vaddr, plane_size, seq, timestamp, timestamp_flags) = match &vb {
            Some(vb) => (
                vb.planes.first().map(|p| p.cookie),
                vb.planes.first().map(|p| p.length as usize),
                vb.sequence,
                vb.timestamp,
                vb.timestamp_flags,
            ),
            None => (None, None, 0, 0, 0),
        };

        if let (Some(vaddr), Some(size)) = (plane_vaddr, plane_size)
            && vaddr != 0
        {
            let pat = self.state.pattern_index();
            let tpg_fmt = self.state.fmt_tpg();
            let (w, h) = self.state.fmt_dimensions();
            let (bright, contrast, sat, hue) = self.state.proc_amps();
            let slice = unsafe { core::slice::from_raw_parts_mut(vaddr as *mut u8, size) };
            fill_with_proc_amps(pat, tpg_fmt, w, h, seq, slice, (bright, contrast, sat, hue));
        }

        let (_, _, sizeimage) = self.state.fmt_dimensions_and_size();
        buf.index = idx;
        buf.flags = BufFlags::DONE
            | BufFlags::KEYFRAME
            // 时间戳源标志（buffer_done 时由队列填 CLOCK_MONOTONIC）。
            | BufFlags::from_bits_retain(timestamp_flags);
        buf.bytesused = sizeimage;
        buf.timestamp = Timeval {
            tv_sec: (timestamp / 1_000_000_000) as i64,
            tv_usec: ((timestamp / 1_000) % 1_000_000) as i64,
        };
        buf.sequence = seq;
        buf.field = Field::NoField;
        buf.memory = Memory::Mmap;
        Ok(())
    }

    fn streamon(&mut self, _ty: BufType) -> v4l2_core::Result<()> {
        let q = &self.queue;
        q.streamon()?;

        // 用测试图案预填充已入队的缓冲（vivid 同步填充）
        let pat = self.state.pattern_index();
        let tpg_fmt = self.state.fmt_tpg();
        let (w, h) = self.state.fmt_dimensions();
        let (_, _, sizeimage) = self.state.fmt_dimensions_and_size();
        let count = q.num_buffers();

        for i in 0..count {
            if q.buffer_snapshot(i)
                .is_some_and(|b| b.state == BufferState::Active)
            {
                if let Some(vaddr) = q
                    .buffer_snapshot(i)
                    .and_then(|vb| vb.planes.first().cloned())
                    .map(|p| p.cookie)
                    .filter(|&vaddr| vaddr != 0)
                {
                    let sz = q
                        .buffer_snapshot(i)
                        .and_then(|vb| vb.planes.first().map(|p| p.length as usize))
                        .unwrap_or(0);
                    let slice = unsafe { core::slice::from_raw_parts_mut(vaddr as *mut u8, sz) };
                    crate::tpg::fill_buffer(pat, tpg_fmt, w, h, 0, slice);
                }
                q.buffer_done(i, BufferState::Done, sizeimage, Field::NoField as u32)?;
                // 唤醒由 buffer_done 内建（DQBUF 阻塞与 poll 共用队列 vb_poll_set）。
            }
        }

        self.state.streaming.store(true, Ordering::Release);
        Ok(())
    }

    fn streamoff(&mut self, _ty: BufType) -> v4l2_core::Result<()> {
        self.state.streaming.store(false, Ordering::Release);
        self.queue.streamoff();
        Ok(())
    }
}

// ── LegacyIoctlOps 实现 ────────────────────────────────────────────────

impl v4l2_core::LegacyIoctlOps for VividCapture {
    // 旧裁剪 API（G_CROP/S_CROP）——Linux core 已用 selection 模拟，
    // 此处保留显式实现以支持旧式用户态。
    fn g_crop(&self, c: &mut Crop) -> v4l2_core::Result<()> {
        c.c.left = self.state.crop_left.load(Ordering::Acquire) as i32;
        c.c.top = self.state.crop_top.load(Ordering::Acquire) as i32;
        c.c.width = self.state.crop_width.load(Ordering::Acquire);
        c.c.height = self.state.crop_height.load(Ordering::Acquire);
        Ok(())
    }

    fn s_crop(&mut self, c: &Crop) -> v4l2_core::Result<()> {
        let w = self.state.fmt_width.load(Ordering::Acquire);
        let h = self.state.fmt_height.load(Ordering::Acquire);
        let cw = c.c.width.min(w);
        let ch = c.c.height.min(h);
        let cl = c.c.left.max(0).min((w - cw) as i32) as u32;
        let ct = c.c.top.max(0).min((h - ch) as i32) as u32;
        self.state.crop_left.store(cl, Ordering::Release);
        self.state.crop_top.store(ct, Ordering::Release);
        self.state.crop_width.store(cw, Ordering::Release);
        self.state.crop_height.store(ch, Ordering::Release);
        Ok(())
    }
}

// ── V4L2DriverOps 实现 ───────────────────────────────────────────────────

impl V4L2DriverOps for VividCapture {
    fn is_readable(&self) -> bool {
        self.queue.is_readable()
    }

    fn is_error(&self) -> bool {
        self.queue.is_error()
    }

    fn vb_poll_set(&self) -> Option<Arc<PollSet>> {
        Some(self.queue.vb_poll_set().clone())
    }

    fn mmap(&self, offset: u64, length: u64) -> Option<(Vec<usize>, usize)> {
        self.queue.mmap(offset, length)
    }

    fn release(&self) {
        self.state.streaming.store(false, Ordering::Release);
        let q = &self.queue;
        if q.num_buffers() > 0 {
            q.reqbufs(0, &[]).ok();
        }
    }

    fn ctrl_handler(&self) -> Option<&v4l2_core::ctrls::CtrlHandler> {
        Some(&self.state.ctrls)
    }
}
