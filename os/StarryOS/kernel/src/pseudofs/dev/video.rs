use core::any::Any;

use ax_sync::Mutex;
use ax_usb::{class::uvc::UvcCamera, imgcat::print_image};
use axfs_ng_vfs::{NodeFlags, VfsError, VfsResult};
use linux_raw_sys::ioctl::{
    VIDIOC_DQBUF, VIDIOC_ENUM_FMT, VIDIOC_ENUM_FRAMESIZES, VIDIOC_G_CTRL, VIDIOC_G_FMT,
    VIDIOC_QBUF, VIDIOC_QUERYBUF, VIDIOC_QUERYCAP, VIDIOC_REQBUFS, VIDIOC_S_CTRL, VIDIOC_S_FMT,
    VIDIOC_STREAMOFF, VIDIOC_STREAMON,
};
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
    camera: Mutex<UvcCamera>,
    jpeg: Mutex<Option<alloc::vec::Vec<u8>>>,
}

impl Video {
    pub fn new() -> Self {
        let host = ax_usb::usb_host();
        let mut host = host.lock();

        // 查找 UVC 设备
        let uvc_info = host
            .find_device_by_class(0xEF)
            .expect("UVC device not found on USB bus");

        let device_id = uvc_info.device_id;
        debug!(
            "UVC: dev#{} VID={:04x} PID={:04x}",
            device_id, uvc_info.descriptor.vendor_id, uvc_info.descriptor.product_id
        );

        let device = host.open(device_id).expect("Failed to open UVC device");
        let camera = UvcCamera::probe(device).expect("UVC probe/commit failed");

        info!("UVC: camera ready, frame_size={}", {
            camera.probe_result().max_video_frame_size
        });

        Self {
            camera: Mutex::new(camera),
            jpeg: Mutex::new(None),
        }
    }

    /// 抓取一帧并缓存 JPEG 数据。
    pub fn capture(&self) -> VfsResult<()> {
        let mut cam = self.camera.lock();
        match cam.capture_frame() {
            Ok(jpeg) => {
                info!("UVC: captured {} bytes", jpeg.len());
                print_image(&jpeg);
                self.jpeg.lock().replace(jpeg);
                Ok(())
            }
            Err(e) => {
                error!("UVC: capture failed: {:?}", e);
                Err(VfsError::InvalidData)
            }
        }
    }
}

impl DeviceOps for Video {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        if offset == 0 || self.jpeg.lock().is_none() {
            self.capture()?;
        }

        let jpeg = self.jpeg.lock();
        if let Some(data) = jpeg.as_ref() {
            let len = buf.len().min(data.len().saturating_sub(offset as usize));
            buf[..len].copy_from_slice(&data[offset as usize..offset as usize + len]);
            Ok(len)
        } else {
            Err(VfsError::ConnectionRefused)
        }
    }

    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        trace!("[uvc]: write_at called");
        Err(VfsError::InvalidInput)
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        match cmd {
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
            VIDIOC_ENUM_FMT => todo!(),
            VIDIOC_ENUM_FRAMESIZES => todo!(),
            VIDIOC_G_FMT => todo!(),
            VIDIOC_S_FMT => todo!(),
            VIDIOC_REQBUFS => todo!(),
            VIDIOC_QUERYBUF => todo!(),
            VIDIOC_QBUF => todo!(),
            VIDIOC_DQBUF => todo!(),
            VIDIOC_G_CTRL => todo!(),
            VIDIOC_S_CTRL => todo!(),
            VIDIOC_STREAMON => todo!(),
            VIDIOC_STREAMOFF => todo!(),
            _ => return Err(VfsError::InvalidInput),
        }
        Ok(0)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::empty()
    }

    fn mmap(&self, _offset: u64) -> DeviceMmap {
        DeviceMmap::None
    }
}
